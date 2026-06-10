//! Directory creation logic - batch and incremental modes.
//!
//! Handles `create_directories` (parallel metadata application),
//! `ensure_relative_parents` (for `--relative` paths),
//! `create_directory_incremental` (single-directory creation during
//! incremental recursion), and `touch_up_dirs` (mtime repair after
//! file writes clobber directory timestamps).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};
use metadata::{MetadataOptions, apply_metadata_with_cached_stat};
use protocol::acl::AclCache;
use protocol::flist::FileEntry;
use protocol::xattr::XattrList;

use super::FailedDirectories;
use crate::receiver::{ReceiverContext, apply_acls_from_receiver_cache};

impl ReceiverContext {
    /// Creates directories from the file list, applying metadata in parallel.
    ///
    /// Two-phase approach: directory creation is sequential (cheap, respects
    /// parent-child ordering), metadata application (`chown`/`chmod`/`utimes`)
    /// is dispatched through `crate::parallel_io::map_blocking`, which runs on
    /// rayon's work-stealing pool when the directory count exceeds the
    /// `ParallelOp::Metadata` threshold and falls back to sequential iteration
    /// below it.
    ///
    /// Returns a list of metadata errors encountered (path, error message).
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:693` - `dry_run` skips all filesystem modifications
    /// - `generator.c:1432-1500` - directory creation and metadata in `recv_generator()`
    pub(in crate::receiver) fn create_directories(
        &self,
        dest_dir: &Path,
        metadata_opts: &MetadataOptions,
        acl_cache: Option<&AclCache>,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    ) -> io::Result<Vec<(PathBuf, String)>> {
        // upstream: receiver.c:693 - dry_run skips all filesystem modifications
        if self.config.flags.dry_run {
            return Ok(Vec::new());
        }

        // upstream: generator.c:1261-1262 - check_filter(&daemon_filter_list, ...)
        // skips daemon-excluded directories before creation.
        let daemon_filters = self.daemon_filter_set();
        let dir_entries: Vec<(usize, PathBuf, PathBuf)> = self
            .file_list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_dir())
            .filter(|(_, e)| {
                if let Some(filters) = daemon_filters {
                    let name = e.name();
                    if name != "." && !name.is_empty() {
                        return filters.allows(Path::new(name), true);
                    }
                }
                true
            })
            .map(|(idx, entry)| {
                let relative_path = entry.path().to_path_buf();
                let dir_path = if relative_path.as_os_str() == "." {
                    dest_dir.to_path_buf()
                } else {
                    dest_dir.join(&relative_path)
                };
                (idx, relative_path, dir_path)
            })
            .collect();

        let mut failed_dir_paths: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        // upstream: generator.c:1337-1340 - probe each new parent directory's
        // default POSIX ACL when !preserve_perms so dest_mode() folds the bits
        // in. The probe also drives the `DEBUG_GTE(ACL, 1)` emission in
        // `acls.c:1133-1134`. Mirror the gating exactly: only probe when
        // ACLs are preserved and the user did not pass --perms.
        #[cfg(all(
            feature = "acl",
            any(target_os = "linux", target_os = "macos", target_os = "freebsd")
        ))]
        let probe_default_perms = self.config.flags.acls && !self.config.flags.perms;
        #[cfg(all(
            feature = "acl",
            any(target_os = "linux", target_os = "macos", target_os = "freebsd")
        ))]
        let mut probed_parents: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        for (_, relative_path, dir_path) in &dir_entries {
            // `relative_path` is only read on Unix (mkdirat fast path).
            #[cfg(not(unix))]
            let _ = relative_path;
            if !dir_path.exists() {
                #[cfg(all(
                    feature = "acl",
                    any(target_os = "linux", target_os = "macos", target_os = "freebsd")
                ))]
                if probe_default_perms {
                    if let Some(parent) = dir_path.parent() {
                        if probed_parents.insert(parent.to_path_buf()) {
                            // upstream: generator.c:1339 dflt_perms = default_perms_for_dir(dn)
                            // Pass umask = 0; upstream prints the ACL-derived bits, not
                            // the umask-derived fallback, so the trace value is umask-independent.
                            let _ = ::metadata::default_perms_for_dir(parent, 0);
                        }
                    }
                }
                // SEC-1.h: when the sandbox is plumbed and the new dir
                // is a single-component leaf under the sandbox root,
                // route through `mkdirat(dirfd, leaf, 0o777)` so a
                // mid-syscall symlink swap on the leaf cannot redirect
                // the create to an attacker-chosen parent. Multi-
                // component paths fall back to `fs::create_dir_all`,
                // which preserves the parent-walk for `--relative`
                // shapes that `ensure_relative_parents` did not pre-
                // create. The mode argument matches the upstream
                // `mkdir(2)` umask-handling: pass `0o777` and let the
                // active umask trim the bits.
                #[cfg(unix)]
                let create_result = fast_io::mkdirat_via_sandbox_or_fallback(
                    sandbox,
                    dest_dir,
                    relative_path,
                    dir_path,
                    0o777,
                )
                .or_else(|err| {
                    if err.kind() == io::ErrorKind::NotFound {
                        // Multi-component path needs the parent walk.
                        fs::create_dir_all(dir_path)
                    } else {
                        Err(err)
                    }
                });
                #[cfg(not(unix))]
                let create_result = fs::create_dir_all(dir_path);
                if let Err(e) = create_result {
                    if e.kind() == io::ErrorKind::PermissionDenied {
                        // upstream: receiver.c - permission denied on mkdir is non-fatal,
                        // sets io_error and continues with remaining files.
                        if self.config.flags.verbose && self.config.connection.client_mode {
                            info_log!(
                                Misc,
                                1,
                                "failed to create directory {}: {}",
                                dir_path.display(),
                                e
                            );
                        }
                        emit_lsm_audit_hint_once();
                        failed_dir_paths.insert(dir_path.clone());
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        // Build owned data for parallel metadata application, skipping failed dirs.
        let metadata_opts_clone = metadata_opts.clone();
        let entry_snapshots: Vec<(PathBuf, FileEntry, Option<XattrList>)> = dir_entries
            .into_iter()
            .filter(|(_, _, dir_path)| !failed_dir_paths.contains(dir_path))
            .map(|(idx, _, dir_path)| {
                let entry = &self.file_list[idx];
                let xattr_list = self.resolve_xattr_list(entry);
                (dir_path, entry.clone(), xattr_list)
            })
            .collect();
        let dir_creation_errors: Vec<(PathBuf, String)> = failed_dir_paths
            .into_iter()
            .map(|p| {
                let msg = format!(
                    "failed to create directory {}: Permission denied",
                    p.display()
                );
                (p, msg)
            })
            .collect();

        let acl_cache_clone = acl_cache.cloned();
        let results = crate::parallel_io::map_blocking(
            entry_snapshots,
            self.parallel_thresholds
                .for_op(crate::parallel_io::ParallelOp::Metadata),
            move |(dir_path, entry, xattr_list)| {
                if let Err(e) =
                    apply_metadata_with_cached_stat(&dir_path, &entry, &metadata_opts_clone, None)
                {
                    return Some((dir_path, e.to_string()));
                }
                // Apply cached ACLs after metadata
                if let Err(e) = apply_acls_from_receiver_cache(
                    &dir_path,
                    &entry,
                    acl_cache_clone.as_ref(),
                    true, // directories always follow symlinks
                ) {
                    return Some((dir_path, e.to_string()));
                }
                // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
                if let Some(ref xattr_list) = xattr_list {
                    if let Err(e) = metadata::apply_xattrs_from_list(&dir_path, xattr_list, true) {
                        return Some((dir_path, e.to_string()));
                    }
                }
                None
            },
        );

        let mut all_errors: Vec<(PathBuf, String)> = results.into_iter().flatten().collect();
        all_errors.extend(dir_creation_errors);
        Ok(all_errors)
    }

    /// Creates implied parent directories for `--relative` path components.
    ///
    /// When `--relative` is active, the file list may contain entries with deep paths
    /// (e.g., `a/b/c/file.txt`). If `--no-implied-dirs` was used, the intermediate
    /// directories (`a/`, `a/b/`, `a/b/c/`) may not appear as explicit directory
    /// entries in the file list. This method ensures all parent directories exist
    /// before files, symlinks, or other entries are processed.
    ///
    /// Uses a set to track already-created paths, avoiding redundant `mkdir` syscalls
    /// when many entries share common parent directories.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1317-1326` - `make_path()` for missing parents when
    ///   `relative_paths && !implied_dirs`
    /// - `generator.c:1472-1475` - retry `mkdir` after `make_path()` when
    ///   `relative_paths` and initial `mkdir` returns `ENOENT`
    pub(in crate::receiver) fn ensure_relative_parents(&self, dest_dir: &Path) {
        if !self.config.flags.relative || self.config.flags.dry_run {
            return;
        }

        let mut created: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

        for entry in &self.file_list {
            let relative_path = entry.path();
            if relative_path.as_os_str() == "." {
                continue;
            }

            // Collect all ancestor directories that need creation.
            // For path "a/b/c/file.txt", we need "a/", "a/b/", "a/b/c/".
            // For directory entry "a/b/c/", we need "a/", "a/b/".
            let target = if entry.is_dir() {
                // For directories, create parents (not the dir itself - that's handled
                // by create_directories / create_directory_incremental).
                match relative_path.parent() {
                    Some(p) if !p.as_os_str().is_empty() => p,
                    _ => continue,
                }
            } else {
                // For files/symlinks/etc., create all parent directories.
                match relative_path.parent() {
                    Some(p) if !p.as_os_str().is_empty() => p,
                    _ => continue,
                }
            };

            // Walk up the path to find the deepest ancestor that needs creation.
            // Build the list of paths to create from shallowest to deepest.
            let mut ancestors_to_create: Vec<PathBuf> = Vec::new();
            let mut current = target;
            loop {
                let abs_path = dest_dir.join(current);
                if created.contains(&abs_path) || abs_path.exists() {
                    break;
                }
                ancestors_to_create.push(abs_path);
                match current.parent() {
                    Some(p) if !p.as_os_str().is_empty() => current = p,
                    _ => break,
                }
            }

            // Create from shallowest to deepest.
            for dir_path in ancestors_to_create.into_iter().rev() {
                if let Err(e) = fs::create_dir(&dir_path) {
                    if e.kind() != io::ErrorKind::AlreadyExists {
                        debug_log!(
                            Recv,
                            1,
                            "failed to create implied parent directory {}: {}",
                            dir_path.display(),
                            e
                        );
                        break;
                    }
                }
                created.insert(dir_path);
            }
        }
    }

    /// Creates a single directory during incremental processing.
    ///
    /// Returns `Ok(None)` on failure or skip (marks dir as failed).
    /// Returns `Ok(Some(true))` when a new directory was created.
    /// Returns `Ok(Some(false))` when an existing directory had metadata applied.
    /// Only returns `Err` for unrecoverable errors.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1432` - `recv_generator()` creates directories
    /// - `generator.c:1472-1475` - retry `mkdir` after `make_path()`
    pub(in crate::receiver) fn create_directory_incremental(
        &self,
        dest_dir: &Path,
        entry: &FileEntry,
        metadata_opts: &MetadataOptions,
        failed_dirs: &mut FailedDirectories,
        acl_cache: Option<&AclCache>,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    ) -> io::Result<Option<bool>> {
        let relative_path = entry.path();
        let dir_path = if relative_path.as_os_str() == "." {
            dest_dir.to_path_buf()
        } else {
            dest_dir.join(relative_path)
        };

        // Check if parent is under a failed directory
        if let Some(failed_parent) = failed_dirs.failed_ancestor(entry.name()) {
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(
                    Skip,
                    1,
                    "skipping directory {} (parent {} failed)",
                    entry.name(),
                    failed_parent
                );
            }
            failed_dirs.mark_failed(entry.name());
            return Ok(None);
        }

        // Try to create the directory.
        //
        // SEC-1.h: when the sandbox is plumbed and the new dir is a
        // single-component leaf under the sandbox root, route through
        // `mkdirat(dirfd, leaf, 0o777)` so a mid-syscall symlink swap
        // on the leaf cannot redirect the create to an attacker-chosen
        // parent. Multi-component paths fall back to
        // `fs::create_dir_all`, which preserves the parent-walk for
        // `--relative` shapes.
        let is_new = !dir_path.exists();
        if is_new {
            #[cfg(unix)]
            let create_result = fast_io::mkdirat_via_sandbox_or_fallback(
                sandbox,
                dest_dir,
                relative_path,
                &dir_path,
                0o777,
            )
            .or_else(|err| {
                if err.kind() == io::ErrorKind::NotFound {
                    fs::create_dir_all(&dir_path)
                } else {
                    Err(err)
                }
            });
            #[cfg(not(unix))]
            let create_result = fs::create_dir_all(&dir_path);
            if let Err(e) = create_result {
                if e.kind() == io::ErrorKind::PermissionDenied {
                    // upstream: receiver.c:693-700 - permission denied on
                    // mkdir is non-fatal: increment io_error and continue
                    // with remaining entries. Matches the parallel
                    // `create_directories` path above.
                    if self.config.flags.verbose && self.config.connection.client_mode {
                        info_log!(
                            Misc,
                            1,
                            "failed to create directory {}: {}",
                            dir_path.display(),
                            e
                        );
                    }
                    failed_dirs.mark_failed(entry.name());
                    return Ok(None);
                }
                // SEC-1.h fail-loud: ELOOP from a mid-syscall symlink
                // swap, EOPNOTSUPP from a sandbox-anchored refusal, and
                // every other non-EACCES error class are security
                // boundaries. Propagate so the receiver surfaces the
                // failure with a non-zero exit code instead of silently
                // skipping the entry.
                return Err(e);
            }
        }

        // Apply metadata (non-fatal errors)
        // Skip the stat inside apply_metadata_from_file_entry: the
        // directory was just created, so pass None to apply unconditionally.
        if let Err(e) = apply_metadata_with_cached_stat(&dir_path, entry, metadata_opts, None) {
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(
                    Misc,
                    1,
                    "warning: metadata error for {}: {}",
                    dir_path.display(),
                    e
                );
            }
        } else if let Some(ref xattr_list) = self.resolve_xattr_list(entry) {
            // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
            if let Err(e) = metadata::apply_xattrs_from_list(&dir_path, xattr_list, true) {
                if self.config.flags.verbose && self.config.connection.client_mode {
                    info_log!(
                        Misc,
                        1,
                        "warning: xattr error for {}: {}",
                        dir_path.display(),
                        e
                    );
                }
            }
        }

        // Apply cached ACLs after metadata (non-fatal errors)
        if let Err(e) = apply_acls_from_receiver_cache(&dir_path, entry, acl_cache, true) {
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(
                    Misc,
                    1,
                    "warning: ACL error for {}: {}",
                    dir_path.display(),
                    e
                );
            }
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            if relative_path.as_os_str() == "." {
                info_log!(Name, 1, "./");
            } else {
                info_log!(Name, 1, "{}/", relative_path.display());
            }
        }

        Ok(Some(is_new))
    }

    /// Re-applies directory mtimes after all file transfers complete.
    ///
    /// Writing files into a directory updates the directory's mtime to the
    /// current time (OS behavior). This method walks all directory entries
    /// in reverse order (deepest first) and re-sets each mtime from the
    /// file list entry, so parent directory timestamps are not disturbed
    /// by child directory mtime updates.
    ///
    /// Gated on `preserve_times` (`-t` / `--times`). Skipped for dry-run
    /// and when backups are active (upstream skips directories that need
    /// backup handling).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2080-2133` - `touch_up_dirs(dir_flist, -1)` iterates
    ///   in reverse order to handle deepest-first ordering.
    /// - `generator.c:2398-2399` - `need_retouch_dir_times` gating:
    ///   `preserve_mtimes && !omit_dir_times`.
    pub(in crate::receiver) fn touch_up_dirs(&self, dest_dir: &Path) {
        // upstream: generator.c:2398 - need_retouch_dir_times =
        // preserve_mtimes && !omit_dir_times
        if !self.config.flags.times || self.config.flags.dry_run {
            return;
        }

        // upstream: generator.c:2101 - skip when make_backups && !backup_dir
        // (directory mtimes are changed by backup file creation)
        if self.config.flags.backup && self.config.backup_dir.is_none() {
            return;
        }

        // Iterate in reverse so deepest directories are touched first.
        // This prevents a parent's mtime from being clobbered when we
        // later utimensat a child directory under it.
        // upstream: generator.c:2083 - for (i = dir_flist->used - 1; i >= 0; i--)
        for entry in self.file_list.iter().rev() {
            if !entry.is_dir() {
                continue;
            }

            let relative_path = entry.path();
            let dir_path = if relative_path.as_os_str() == "." {
                dest_dir.to_path_buf()
            } else {
                dest_dir.join(relative_path)
            };

            let mtime = filetime::FileTime::from_unix_time(entry.mtime(), entry.mtime_nsec());

            // Only update if the current mtime differs from the desired one.
            let needs_update = match fs::metadata(&dir_path) {
                Ok(meta) => filetime::FileTime::from_last_modification_time(&meta) != mtime,
                Err(_) => false, // directory may not exist (permission denied, etc.)
            };

            if needs_update {
                if let Err(e) = filetime::set_file_mtime(&dir_path, mtime) {
                    debug_log!(
                        Recv,
                        1,
                        "touch_up_dirs: failed to set mtime on {}: {}",
                        dir_path.display(),
                        e
                    );
                }
            }
        }
    }
}

/// Emits the LSM audit-log hint at most once per process when a mandatory
/// access control LSM is active.
///
/// Called from receiver code paths that swallow a
/// `io::ErrorKind::PermissionDenied` to keep the transfer going. The hint
/// points the operator at `ausearch -m AVC -ts recent` so they can
/// correlate the EACCES with an LSM AVC denial without re-running with
/// verbose tracing. The hint is purely informational and is suppressed
/// when:
///
/// - [`fast_io::lsm::has_mandatory_lsm`] reports no mandatory LSM is
///   loaded (the kernel `EACCES` was generated by classic POSIX
///   permission checks, not an LSM policy decision worth correlating),
/// - the helper has already emitted on this process (single-shot via
///   [`OnceLock`]) so high file counts do not flood the log,
/// - the host is not Linux (no `/sys/kernel/security/lsm`, so the
///   classifier returns `false` by construction).
fn emit_lsm_audit_hint_once() {
    use std::sync::OnceLock;
    static EMITTED: OnceLock<()> = OnceLock::new();
    if EMITTED.get().is_some() {
        return;
    }
    if !fast_io::lsm::has_mandatory_lsm() {
        return;
    }
    if EMITTED.set(()).is_err() {
        // Another thread won the race; their emission counts.
        return;
    }
    info_log!(
        Misc,
        1,
        "operation denied (EACCES). If an LSM is active on this host, \
         check the audit log: ausearch -m AVC -ts recent"
    );
}

#[cfg(test)]
mod touch_up_dirs_tests {
    use std::ffi::OsString;
    use std::fs;

    use filetime::FileTime;
    use protocol::ProtocolVersion;
    use protocol::flist::FileEntry;

    use crate::config::ServerConfig;
    use crate::flags::ParsedServerFlags;
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::role::ServerRole;

    fn handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    fn config_with_times(times: bool) -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-r".to_owned(),
            flags: ParsedServerFlags {
                times,
                recursive: true,
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            ..Default::default()
        }
    }

    /// After writing files into a directory, the OS clobbers the directory
    /// mtime with the current time. `touch_up_dirs` must re-apply the
    /// original mtime from the file list entry.
    ///
    /// upstream: generator.c:2080-2133 - touch_up_dirs()
    #[test]
    fn restores_directory_mtime_after_file_writes() {
        let dir = test_support::create_tempdir();
        let sub = dir.path().join("subdir");
        fs::create_dir(&sub).unwrap();

        // Write a file into the directory to clobber its mtime.
        fs::write(sub.join("file.txt"), b"hello").unwrap();

        // The desired mtime is in the past (2020-01-01 00:00:00 UTC).
        let desired_secs: i64 = 1_577_836_800;
        let mut entry = FileEntry::new_directory("subdir".into(), 0o755);
        entry.set_mtime(desired_secs, 0);

        let hs = handshake();
        let config = config_with_times(true);
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.file_list = vec![entry];

        ctx.touch_up_dirs(dir.path());

        let meta = fs::metadata(&sub).unwrap();
        let actual = FileTime::from_last_modification_time(&meta);
        let expected = FileTime::from_unix_time(desired_secs, 0);
        assert_eq!(
            actual, expected,
            "directory mtime should be restored to the file list value"
        );
    }

    /// When `--times` is not set, `touch_up_dirs` must be a no-op.
    #[test]
    fn skipped_when_times_not_set() {
        let dir = test_support::create_tempdir();
        let sub = dir.path().join("subdir");
        fs::create_dir(&sub).unwrap();

        // Record the current mtime before touch_up_dirs.
        let before = FileTime::from_last_modification_time(&fs::metadata(&sub).unwrap());

        let desired_secs: i64 = 1_577_836_800;
        let mut entry = FileEntry::new_directory("subdir".into(), 0o755);
        entry.set_mtime(desired_secs, 0);

        let hs = handshake();
        let config = config_with_times(false);
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.file_list = vec![entry];

        ctx.touch_up_dirs(dir.path());

        let after = FileTime::from_last_modification_time(&fs::metadata(&sub).unwrap());
        assert_eq!(
            before, after,
            "directory mtime must not change when --times is off"
        );
    }

    /// Deepest directories must be processed first so that setting a parent
    /// mtime is not immediately clobbered by a child directory mtime update.
    #[test]
    fn processes_deepest_directories_first() {
        let dir = test_support::create_tempdir();
        let parent = dir.path().join("parent");
        let child = parent.join("child");
        fs::create_dir_all(&child).unwrap();

        // Write into child to clobber parent mtime.
        fs::write(child.join("file.txt"), b"data").unwrap();

        let parent_secs: i64 = 1_577_836_800;
        let child_secs: i64 = 1_577_923_200; // one day later

        let mut parent_entry = FileEntry::new_directory("parent".into(), 0o755);
        parent_entry.set_mtime(parent_secs, 0);

        let mut child_entry = FileEntry::new_directory("parent/child".into(), 0o755);
        child_entry.set_mtime(child_secs, 0);

        let hs = handshake();
        let config = config_with_times(true);
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        // Parent comes first in file list (natural order).
        ctx.file_list = vec![parent_entry, child_entry];

        ctx.touch_up_dirs(dir.path());

        let parent_actual = FileTime::from_last_modification_time(&fs::metadata(&parent).unwrap());
        let child_actual = FileTime::from_last_modification_time(&fs::metadata(&child).unwrap());

        assert_eq!(
            parent_actual,
            FileTime::from_unix_time(parent_secs, 0),
            "parent directory mtime must be restored"
        );
        assert_eq!(
            child_actual,
            FileTime::from_unix_time(child_secs, 0),
            "child directory mtime must be restored"
        );
    }

    /// The root directory entry (path = ".") must map to `dest_dir` itself.
    #[test]
    fn handles_dot_directory_entry() {
        let dir = test_support::create_tempdir();

        let desired_secs: i64 = 1_577_836_800;
        let mut entry = FileEntry::new_directory(".".into(), 0o755);
        entry.set_mtime(desired_secs, 0);

        let hs = handshake();
        let config = config_with_times(true);
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.file_list = vec![entry];

        ctx.touch_up_dirs(dir.path());

        let actual = FileTime::from_last_modification_time(&fs::metadata(dir.path()).unwrap());
        let expected = FileTime::from_unix_time(desired_secs, 0);
        assert_eq!(actual, expected, "dest_dir mtime should match '.' entry");
    }

    /// Non-directory entries in the file list must be ignored.
    #[test]
    fn ignores_non_directory_entries() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("file.txt");
        fs::write(&file_path, b"content").unwrap();

        // Backdate the file so we can detect if touch_up_dirs changes it.
        let past = FileTime::from_unix_time(1_500_000_000, 0);
        filetime::set_file_mtime(&file_path, past).unwrap();

        let mut file_entry = FileEntry::new_file("file.txt".into(), 7, 0o644);
        file_entry.set_mtime(1_577_836_800, 0);

        let hs = handshake();
        let config = config_with_times(true);
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.file_list = vec![file_entry];

        ctx.touch_up_dirs(dir.path());

        let actual = FileTime::from_last_modification_time(&fs::metadata(&file_path).unwrap());
        assert_eq!(
            actual, past,
            "touch_up_dirs must not modify non-directory entries"
        );
    }
}
