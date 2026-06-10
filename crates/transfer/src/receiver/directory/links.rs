//! Symlink and hardlink creation from the received file list.
//!
//! Handles symbolic link creation (Unix-only with `--links`) and hardlink
//! creation (leader/follower via `hardlink_idx`). Protocol 28-29 entries are
//! normalized to use `hardlink_idx` and `hlink_first` during file list
//! reception, so this module handles both protocol versions uniformly.

use std::fs;
use std::path::Path;

use logging::{debug_log, info_log};
use protocol::flist::{trace_leader_is, trace_looking_for_leader, trace_virtual_first};

use crate::generator::ItemFlags;
use crate::receiver::ReceiverContext;

impl ReceiverContext {
    /// Creates symbolic links from the file list entries.
    ///
    /// Iterates through the received file list, finds symlink entries with
    /// `preserve_links` enabled, and creates them on the destination filesystem.
    /// Existing symlinks pointing to the correct target are skipped (quick-check).
    /// Existing files/symlinks at the destination path are removed before creation.
    ///
    /// Emits MSG_INFO itemize frames for each symlink created or found up-to-date
    /// when the daemon has itemize output enabled.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1544` - `if (preserve_links && ftype == FT_SYMLINK)`
    /// - `generator.c:1591` - `atomic_create(file, fname, sl, ...)`
    #[cfg(unix)]
    pub(in crate::receiver) fn create_symlinks<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) {
        if !self.config.flags.links || self.config.flags.dry_run {
            return;
        }

        for entry in &self.file_list {
            if !entry.is_symlink() {
                continue;
            }

            let wire_target = match entry.link_target() {
                Some(t) => t,
                None => continue,
            };

            let relative_path = entry.path();

            // upstream: generator.c:1547 - skip unsafe symlinks when --safe-links
            // is set. Check stays here (not in sanitize_file_list) to preserve
            // protocol index alignment with the sender. Safe-link evaluation
            // runs against the wire target (pre-munge) so the policy decision
            // matches upstream's `flist.c` ordering where the munge prefix is
            // applied only after safety checks complete.
            if self.config.flags.safe_links
                && crate::symlink_safety::is_unsafe_symlink(wire_target.as_os_str(), relative_path)
            {
                // upstream: generator.c:1554 - log skipped unsafe symlinks
                info_log!(
                    Name,
                    1,
                    "skipping unsafe symlink \"{}\" -> \"{}\"",
                    relative_path.display(),
                    wire_target.display()
                );
                continue;
            }

            // upstream: flist.c:1122-1126 - receiver prepends `/rsyncd-munged/`
            // to the symlink target when the daemon enabled `munge symlinks`,
            // so the on-disk link cannot resolve outside the module root when
            // followed. Apply once here so both the quick-check comparison and
            // the create syscall see the prefixed form.
            let munged: std::path::PathBuf;
            let target: &std::path::Path = if self.config.munge_symlinks {
                munged = apply_symlink_munge_prefix(wire_target);
                munged.as_path()
            } else {
                wire_target.as_path()
            };

            let link_path = dest_dir.join(relative_path);

            // upstream: generator.c:1561 - quick_check_ok(FT_SYMLINK, ...)
            if let Ok(existing_target) = std::fs::read_link(&link_path) {
                if existing_target == *target {
                    // upstream: generator.c:1565 - symlink up-to-date, metadata only
                    let iflags = ItemFlags::from_raw(0);
                    let _ = self.emit_itemize(writer, &iflags, entry);
                    // upstream: log.c log_item / send_directory NAME emissions
                    // upstream: generator.c:1133 - "%s is uptodate" at INFO_GTE(NAME, 2)
                    info_log!(Name, 2, "{} is uptodate", relative_path.display());
                    continue;
                }
                // SEC-1.g: route the obstacle unlink through the sandbox
                // dirfd when the destination parent is the sandbox root,
                // so a TOCTOU swap on `link_path` between the readlink
                // above and this unlink cannot redirect the syscall to
                // an attacker-chosen parent. Falls back to path-based
                // `remove_file` otherwise.
                let _ = fast_io::unlink_via_sandbox_or_fallback(
                    sandbox,
                    dest_dir,
                    relative_path,
                    &link_path,
                    fast_io::UnlinkFlags::File,
                );
            } else if fast_io::lstat_via_sandbox_or_fallback(
                sandbox,
                dest_dir,
                relative_path,
                &link_path,
            )
            .is_ok()
            {
                // SEC-1.f: when the sandbox is plumbed and the destination
                // parent is the sandbox root, the obstacle stat goes through
                // `fstatat(AT_SYMLINK_NOFOLLOW)` so a TOCTOU symlink swap on
                // `link_path` cannot redirect the probe to a different
                // inode. Falls back to `symlink_metadata` otherwise.
                //
                // SEC-1.g: matching unlink also goes through the sandbox
                // dirfd via `unlinkat` so the obstacle-remove syscall is
                // anchored on the same parent the stat just observed.
                let _ = fast_io::unlink_via_sandbox_or_fallback(
                    sandbox,
                    dest_dir,
                    relative_path,
                    &link_path,
                    fast_io::UnlinkFlags::File,
                );
            }

            // Ensure parent directory exists for --relative paths.
            // upstream: generator.c:1317-1326 - make_path() for relative_paths
            if let Some(parent) = link_path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            // upstream: generator.c:1591 - atomic_create() -> do_symlink()
            //
            // SEC-1.h: when the sandbox is plumbed and the destination
            // parent is the sandbox root, the create goes through
            // `symlinkat(target, dirfd, leaf)` so a TOCTOU swap on a
            // mid-path component cannot redirect the create to an
            // attacker-chosen parent. Falls back to path-based
            // `std::os::unix::fs::symlink` otherwise.
            if let Err(e) = fast_io::symlinkat_via_sandbox_or_fallback(
                sandbox,
                dest_dir,
                relative_path,
                &link_path,
                target,
            ) {
                debug_log!(
                    Recv,
                    1,
                    "failed to create symlink {} -> {}: {}",
                    link_path.display(),
                    target.display(),
                    e
                );
            } else {
                // upstream: generator.c:1594 - itemize new symlink after creation
                let iflags =
                    ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);
                let _ = self.emit_itemize(writer, &iflags, entry);
            }
        }
    }

    /// No-op on non-Unix platforms where symlinks are not supported.
    #[cfg(not(unix))]
    pub(in crate::receiver) fn create_symlinks<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        _dest_dir: &Path,
        _writer: &mut W,
    ) {
    }

    /// Creates hard links for hardlink follower entries after the leader has been transferred.
    ///
    /// Hardlinked files are grouped by a shared `hardlink_idx`. The first file in
    /// each group (the "leader", with `hlink_first = true`) is transferred normally.
    /// Subsequent files ("followers") are created as hard links to the leader's
    /// destination path instead of being transferred independently.
    ///
    /// This method works uniformly for both protocol versions:
    /// - Protocol 30+: `hardlink_idx` and `hlink_first` come from the wire encoding
    /// - Protocol 28-29: `normalize_pre30_hardlinks()` assigns synthetic
    ///   `hardlink_idx` and `hlink_first` from (dev, ino) pairs during file list
    ///   reception, so both versions use the same leader/follower logic here.
    ///
    /// Uses `HardlinkApplyTracker` for leader path tracking and deferred follower
    /// resolution. Leaders committed during pipelined transfer are already recorded
    /// in the tracker; this method records any remaining leaders (e.g., files that
    /// matched quick-check and were not transferred) and links all followers.
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:hard_link_check()` - skips followers, links to leader
    /// - `hlink.c:finish_hard_link()` - creates links after leader transfer completes
    /// - `generator.c:1539` - `F_HLINK_NOT_FIRST` check before `hard_link_check()`
    pub(in crate::receiver) fn create_hardlinks<W: crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        writer: &mut W,
    ) {
        if !self.config.flags.hard_links || self.config.flags.dry_run {
            return;
        }

        // Protocol 30+: use HardlinkApplyTracker for leader/follower resolution.
        // Take the tracker temporarily to avoid borrow conflicts with self.emit_itemize().
        if let Some(mut tracker) = self.hardlink_tracker.take() {
            // First pass: ensure all leaders are recorded in the tracker.
            // Leaders committed during pipelined transfer are already recorded;
            // this covers leaders that matched quick-check (not transferred) or
            // were processed via the sync path.
            for entry in &self.file_list {
                if entry.hlink_first() {
                    let gnum = match entry.hardlink_idx() {
                        Some(idx) => idx,
                        None => continue,
                    };
                    // Skip if already recorded during pipelined commit.
                    if tracker.leader_path(gnum).is_some() {
                        continue;
                    }
                    let relative_path = entry.path();
                    let dest_path = if relative_path.as_os_str() == "." {
                        dest_dir.to_path_buf()
                    } else {
                        dest_dir.join(relative_path)
                    };
                    // No deferred followers expected here since we process
                    // followers in the second pass below.
                    let _ = tracker.record_leader(gnum, dest_path);
                }
            }

            // Second pass: link followers to their leaders.
            for (follower_ndx, entry) in self.file_list.iter().enumerate() {
                if !entry.hlinked() || entry.hlink_first() {
                    continue;
                }
                let leader_idx = match entry.hardlink_idx() {
                    Some(idx) => idx,
                    None => continue,
                };

                let entry_name = entry.path().display().to_string();
                let leader_path = match tracker.leader_path(leader_idx) {
                    Some(p) => p.to_path_buf(),
                    None => {
                        // upstream: hlink.c HLINK debug emissions
                        // No leader recorded: matches `virtual first` in upstream
                        // when a prior file in the group was skipped.
                        trace_virtual_first(follower_ndx as i32, &entry_name, leader_idx as i32);
                        debug_log!(
                            Recv,
                            1,
                            "hardlink follower {} references unknown leader index {}",
                            entry.name(),
                            leader_idx
                        );
                        continue;
                    }
                };
                // upstream: hlink.c HLINK debug emissions
                // Pre-resolution diagnostic plus the final `leader is` message.
                trace_looking_for_leader(follower_ndx as i32, &entry_name, leader_idx as i32);
                trace_leader_is(
                    follower_ndx as i32,
                    &entry_name,
                    leader_idx as i32,
                    leader_idx as i32,
                    &leader_path.display().to_string(),
                );

                let relative_path = entry.path();
                let link_path = dest_dir.join(relative_path);

                // Quick-check: if destination already hard-links to the leader, skip.
                //
                // SEC-1.f: the `link_path` stat goes through the sandbox
                // dirfd via `fstatat(AT_SYMLINK_NOFOLLOW)` when the
                // sandbox is plumbed and the relative path is a single
                // component. The `leader_path` is owned by the receiver-
                // managed `HardlinkApplyTracker` (it may live under a
                // different parent than `dest_dir` for cross-segment
                // hardlinks) and stays on the path-based stat for now.
                #[cfg(unix)]
                let link_meta_outcome = fast_io::lstat_via_sandbox_or_fallback(
                    sandbox,
                    dest_dir,
                    relative_path,
                    &link_path,
                );
                #[cfg(not(unix))]
                let link_meta_outcome = fs::symlink_metadata(&link_path);
                if let Ok(link_meta) = link_meta_outcome {
                    if let Ok(leader_meta) = fs::symlink_metadata(&leader_path) {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            if link_meta.dev() == leader_meta.dev()
                                && link_meta.ino() == leader_meta.ino()
                            {
                                // upstream: hlink.c - hardlink already correct, metadata only
                                let iflags = ItemFlags::from_raw(0);
                                let _ = self.emit_itemize(writer, &iflags, entry);
                                // upstream: hlink.c:223 - "%s is uptodate"
                                // emitted at INFO_GTE(NAME, 2) when the
                                // destination already hard-links to the leader.
                                info_log!(Name, 2, "{} is uptodate", relative_path.display());
                                continue;
                            }
                        }
                        #[cfg(not(unix))]
                        {
                            let _ = (&link_meta, leader_meta);
                        }
                    }
                    // Remove existing file so we can create the hard link.
                    //
                    // SEC-1.g: on Unix, route the unlink through the
                    // sandbox dirfd when the destination parent is the
                    // sandbox root so a TOCTOU swap between the stat
                    // above and the unlink cannot redirect the syscall
                    // to an attacker-chosen parent. Windows stays on
                    // the path-based `remove_file` per the SEC-1.l
                    // NTFS-handle audit.
                    #[cfg(unix)]
                    let _ = fast_io::unlink_via_sandbox_or_fallback(
                        sandbox,
                        dest_dir,
                        relative_path,
                        &link_path,
                        fast_io::UnlinkFlags::File,
                    );
                    #[cfg(not(unix))]
                    let _ = fs::remove_file(&link_path);
                }

                // Ensure parent directory exists.
                if let Some(parent) = link_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                // upstream: hlink.c:maybe_hard_link() -> atomic_create() -> do_link()
                //
                // SEC-1.h (Unix): when the sandbox is plumbed and the
                // follower's destination parent is the sandbox root,
                // route through `linkat(AT_FDCWD, leader, dirfd, leaf,
                // 0)` so a mid-syscall symlink swap on the follower's
                // parent cannot redirect the create to an
                // attacker-chosen directory. Falls back to
                // `fast_io::hard_link` (io_uring LINKAT on Linux 5.15+,
                // else `std::fs::hard_link`) for the multi-component
                // and no-sandbox cases, preserving the existing
                // `EXDEV` / `EPERM` error semantics. Windows stays on
                // the path-based fallback per the SEC-1.l NTFS-handle
                // audit.
                #[cfg(unix)]
                let link_result = fast_io::linkat_via_sandbox_or_fallback(
                    sandbox,
                    &leader_path,
                    dest_dir,
                    relative_path,
                    &link_path,
                );
                #[cfg(not(unix))]
                let link_result = fast_io::hard_link(&leader_path, &link_path);
                if let Err(e) = link_result {
                    debug_log!(
                        Recv,
                        1,
                        "failed to hard link {} => {}: {}",
                        link_path.display(),
                        leader_path.display(),
                        e
                    );
                } else {
                    // upstream: hlink.c:finish_hard_link() - itemize new hardlink
                    let iflags = ItemFlags::from_raw(
                        ItemFlags::ITEM_LOCAL_CHANGE
                            | ItemFlags::ITEM_XNAME_FOLLOWS
                            | ItemFlags::ITEM_IS_NEW,
                    );
                    let _ = self.emit_itemize(writer, &iflags, entry);
                    // upstream: hlink.c:236 - "%s => %s" at INFO_GTE(NAME, 1)
                    // when a hardlink follower is linked to its leader.
                    info_log!(
                        Name,
                        1,
                        "{} => {}",
                        relative_path.display(),
                        leader_path.display()
                    );
                }
            }

            // Resolve any remaining deferred followers from incremental commits.
            // upstream: hlink.c:finish_hard_link() - final pass
            let (linked, errors) = tracker.resolve_deferred();
            if linked > 0 {
                debug_log!(Recv, 2, "resolved {} deferred hardlink followers", linked);
            }
            for (path, err) in errors {
                debug_log!(
                    Recv,
                    1,
                    "failed to resolve deferred hardlink {}: {}",
                    path.display(),
                    err
                );
            }

            self.hardlink_tracker = Some(tracker);
        }

        // Protocol 28-29 hardlinks are normalized to hardlink_idx/hlink_first
        // during file list reception (normalize_pre30_hardlinks), so the
        // protocol 30+ path above handles both versions uniformly.
    }
}

/// Prepends the `/rsyncd-munged/` prefix to a symlink target.
///
/// Mirrors upstream `flist.c:1122-1126` where the receiver prepends
/// `SYMLINK_PREFIX` to the wire target so the on-disk symlink cannot resolve
/// outside the module root when followed. Only invoked when the daemon module
/// has `munge symlinks = yes` (or the `!use_chroot` auto default).
///
/// The transform is a pure byte-level prepend so a non-UTF-8 target still
/// receives the ASCII prefix without any lossy decode. The matching strip
/// happens on the sender via `strip_symlink_munge_prefix` in
/// `crate::generator::file_list::entry`.
#[cfg(unix)]
pub(in crate::receiver) fn apply_symlink_munge_prefix(target: &Path) -> std::path::PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let mut bytes = ::metadata::SYMLINK_MUNGE_PREFIX.as_bytes().to_vec();
    bytes.extend_from_slice(target.as_os_str().as_bytes());
    std::path::PathBuf::from(OsString::from_vec(bytes))
}

#[cfg(test)]
mod tests {
    use std::fs;

    /// Verifies `fast_io::hard_link` creates a valid hard link regardless of
    /// whether io_uring handles it or `std::fs::hard_link` does.
    #[test]
    fn hard_link_via_io_uring_or_fallback_creates_link() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("link_src.txt");
        let dst = dir.path().join("link_dst.txt");

        fs::write(&src, b"hardlink payload").unwrap();

        fast_io::hard_link(&src, &dst).unwrap();

        assert!(src.exists());
        assert!(dst.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"hardlink payload");

        // Verify they share the same inode (both point to same data).
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let src_meta = fs::metadata(&src).unwrap();
            let dst_meta = fs::metadata(&dst).unwrap();
            assert_eq!(src_meta.ino(), dst_meta.ino());
        }
    }

    /// Verifies that attempting to hard link to an existing destination fails
    /// with an appropriate error (io_uring or fallback path).
    #[test]
    fn hard_link_via_io_uring_or_fallback_fails_when_dst_exists() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("link_src_exists.txt");
        let dst = dir.path().join("link_dst_exists.txt");

        fs::write(&src, b"source").unwrap();
        fs::write(&dst, b"existing").unwrap();

        let result = fast_io::hard_link(&src, &dst);
        assert!(result.is_err());
    }

    /// Verifies the hardlink `%s => %s` and `%s is uptodate` emission shapes
    /// match upstream rsync byte-for-byte.
    ///
    /// upstream: log.c log_item / send_directory NAME emissions
    /// upstream: hlink.c:223 (`"is uptodate"`) and hlink.c:236 (`"=>"`).
    #[test]
    fn hardlink_name_level_emission_shape_matches_upstream() {
        use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, info_log, init};

        let mut cfg = VerbosityConfig::default();
        cfg.info.name = 2;
        init(cfg);
        let _ = drain_events();

        let follower = std::path::Path::new("dir/follower");
        let leader = std::path::Path::new("dir/leader");
        info_log!(Name, 1, "{} => {}", follower.display(), leader.display());
        info_log!(Name, 2, "{} is uptodate", follower.display());

        let messages: Vec<String> = drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info {
                    flag: InfoFlag::Name,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect();

        assert!(
            messages.iter().any(|m| m == "dir/follower => dir/leader"),
            "missing upstream hardlink => emission: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m == "dir/follower is uptodate"),
            "missing upstream `is uptodate` emission: {messages:?}"
        );
    }

    /// Verifies NAME emissions are suppressed when the flag level is below
    /// the emission level.
    #[test]
    fn hardlink_name_emissions_suppressed_at_level_zero() {
        use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, info_log, init};

        let cfg = VerbosityConfig::default();
        init(cfg);
        let _ = drain_events();

        info_log!(Name, 1, "src => dst");
        info_log!(Name, 2, "src is uptodate");

        let messages: Vec<_> = drain_events()
            .into_iter()
            .filter(|event| {
                matches!(
                    event,
                    DiagnosticEvent::Info {
                        flag: InfoFlag::Name,
                        ..
                    }
                )
            })
            .collect();

        assert!(
            messages.is_empty(),
            "NAME emissions must be gated by InfoFlag::Name level: {messages:?}"
        );
    }

    /// Verifies consistent availability: the function returns the same
    /// path (io_uring vs fallback) across multiple calls.
    #[test]
    fn hard_link_io_uring_availability_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("consistency_src.txt");
        let dst1 = dir.path().join("consistency_dst1.txt");
        let dst2 = dir.path().join("consistency_dst2.txt");
        fs::write(&src, b"data").unwrap();

        let first = fast_io::try_hard_link_via_io_uring(&src, &dst1).is_some();
        let second = fast_io::try_hard_link_via_io_uring(&src, &dst2).is_some();
        assert_eq!(
            first, second,
            "io_uring LINKAT availability must be consistent"
        );
    }

    /// Verifies the receiver-side `--debug=HLINK` follower emission shape
    /// matches upstream `hlink.c:hard_link_check()` byte-for-byte.
    ///
    /// upstream: hlink.c HLINK debug emissions
    #[test]
    fn hardlink_debug_hlink_leader_emission_matches_upstream() {
        use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};
        use protocol::flist::{trace_leader_is, trace_looking_for_leader, trace_virtual_first};

        let mut cfg = VerbosityConfig::default();
        cfg.debug.hlink = 2;
        init(cfg);
        let _ = drain_events();

        trace_looking_for_leader(4, "dir/follower", 1);
        trace_leader_is(4, "dir/follower", 1, 1, "dest/leader");
        trace_virtual_first(5, "dir/orphan", 2);

        let msgs: Vec<String> = drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Hlink,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect();

        assert!(
            msgs.iter()
                .any(|m| m == "hlink for 4 (dir/follower,1): looking for a leader")
        );
        assert!(
            msgs.iter()
                .any(|m| m == "hlink for 4 (dir/follower,1): leader is 1 (dest/leader)")
        );
        assert!(
            msgs.iter()
                .any(|m| m == "hlink for 5 (dir/orphan,2): virtual first")
        );
    }
}
