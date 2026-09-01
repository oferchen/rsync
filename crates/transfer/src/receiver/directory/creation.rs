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
use metadata::{AclIdMapper, MetadataOptions, apply_metadata_with_pre_transfer_stat};
use protocol::acl::AclCache;
use protocol::flist::FileEntry;
use protocol::xattr::XattrList;

use super::FailedDirectories;
use crate::receiver::{ReceiverContext, apply_acls_from_receiver_cache};

/// Outcome of classifying a directory destination before creation.
///
/// Mirrors upstream's generator dir preparation: `link_stat(fname, &sx.st,
/// keep_dirlinks && is_dir)` (`generator.c:1356`) classifies the destination,
/// then a non-directory destination is deleted via `delete_item(...,
/// del_opts | DEL_FOR_DIR)` before `do_mkdir_at()` (`generator.c:1451-1455`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirDestination {
    /// Destination is absent - create it (and honour `--existing` skipping).
    Missing,
    /// Destination already usable as a directory - reuse it. Either a real
    /// directory, or a symlink-to-directory followed under `--keep-dirlinks`.
    Existing,
    /// A conflicting symlink was removed - create a real directory in its
    /// place. The destination existed, so `--existing` does NOT skip it.
    ReplacedSymlink,
}

impl DirDestination {
    /// Whether a directory must be materialised (mkdir) for this outcome.
    const fn needs_mkdir(self) -> bool {
        matches!(self, Self::Missing | Self::ReplacedSymlink)
    }
}

/// Reports whether a directory whose final permission bits are `real_mode`
/// must be granted a temporary `u+rwx` while the receiver writes files into
/// it, with the real mode restored afterward by `touch_up_dirs`.
///
/// Mirrors upstream `generator.c:1512-1520`: when the receiver is not root,
/// is not running `--fake-super`, is preserving permissions, and the target
/// directory mode lacks full user `rwx` (`(mode & S_IRWXU) != S_IRWXU`), the
/// generator chmods the directory to `mode | S_IRWXU` so `mkstemp()` can
/// create temp files inside it, then sets `need_retouch_dir_perms` so the
/// restrictive mode is reinstated at the end of the transfer
/// (`generator.c:2122-2127`, `fix_dir_perms`). Without this, a source
/// directory with a read-only mode (for example `0555`) leaves the
/// destination directory unwritable and every file transfer into it fails
/// with `mkstemp ... Permission denied`.
#[cfg(unix)]
fn dir_needs_writable_transfer_mode(
    preserve_perms: bool,
    fake_super: bool,
    real_mode: u32,
) -> bool {
    preserve_perms
        && !fake_super
        && !metadata::am_root()
        // upstream: generator.c:1512 - (file->mode & S_IRWXU) != S_IRWXU
        && (real_mode & 0o700) != 0o700
}

impl ReceiverContext {
    /// Classifies a directory destination, removing a conflicting symlink first
    /// when required.
    ///
    /// A `.exists()`-style probe would be wrong here: it follows symlinks, so a
    /// destination symlink-to-directory would always be treated as an existing
    /// directory and never replaced, diverging from upstream when
    /// `--keep-dirlinks` is off.
    ///
    /// The removal is confined. The entry being unlinked is a symlink the peer
    /// chose the target of, sitting on the receiver's destination path, so a
    /// path-based `unlink(2)` here re-resolves every component under the peer's
    /// influence. `sandbox` may be `None`: that is a supported arm of
    /// [`fast_io::unlink_via_sandbox_or_fallback`], not a gap - it then applies
    /// upstream's three-arm `do_unlink_at()` contract, which errors rather than
    /// falling back to a plain syscall when the ownership walk refuses.
    fn classify_dir_destination(
        &self,
        dir_path: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        dest_dir: &Path,
        relative_path: &Path,
    ) -> io::Result<DirDestination> {
        // `dest_dir` / `relative_path` anchor the confined unlink, which only
        // exists on Unix.
        #[cfg(not(unix))]
        let _ = (dest_dir, relative_path);
        match fs::symlink_metadata(dir_path) {
            Ok(existing) if existing.file_type().is_symlink() => {
                let resolves_to_dir = fs::metadata(dir_path)
                    .map(|meta| meta.file_type().is_dir())
                    .unwrap_or(false);
                if self.config.flags.keep_dirlinks && resolves_to_dir {
                    // upstream: generator.c:1356 - keep_dirlinks follows the
                    // destination symlink-to-directory instead of replacing it.
                    Ok(DirDestination::Existing)
                } else {
                    // upstream: generator.c:1841 - delete_item(fname, ...,
                    // del_opts | DEL_FOR_DIR) removes the conflicting symlink
                    // before mkdir. That reaches delete.c:71-78 del_unlink(),
                    // which takes do_unlink_atfd() against a held dirfd or else
                    // robust_unlink() -> util1.c:545 -> do_unlink_at() - the
                    // CONFINED wrapper. Upstream issues no plain unlink() on
                    // this path, so neither do we.
                    if !self.config.flags.skip_dest_writes() {
                        #[cfg(unix)]
                        fast_io::unlink_via_sandbox_or_fallback(
                            sandbox,
                            dest_dir,
                            relative_path,
                            dir_path,
                            fast_io::UnlinkFlags::File,
                        )?;
                        #[cfg(not(unix))]
                        fs::remove_file(dir_path)?;
                    }
                    Ok(DirDestination::ReplacedSymlink)
                }
            }
            // An existing real directory (or any other existing non-symlink
            // entry, matching the prior `.exists()` semantics) is reused.
            Ok(_) => Ok(DirDestination::Existing),
            // A stat failure of ANY errno class classifies as `Missing`; it is
            // never raised from here.
            //
            // upstream: generator.c:1745-1746 - `statret = gen_entry_stat(...);
            // stat_errno = errno;`. Every later branch tests `statret`, not the
            // errno class, and `generator.c:1840` gates the obstruction removal
            // on `statret == 0`. A stat that failed therefore never reaches
            // `delete_item()`: it falls through to `gen_entry_mkdir()`
            // (`generator.c:1831`/`1873`), and that mkdir's error is the one
            // reported. `stat_errno` is consulted only by the `--existing`
            // ENOENT test at generator.c:1758.
            //
            // Consequently the ONLY `Err` this function may return is the
            // confined unlink above - upstream's `goto skipping_dir_contents`
            // (`generator.c:1841-1842`), which the caller treats as a hard
            // failure that skips the directory and its contents. Raising the
            // stat error here instead would route ENOTDIR - the "a plain file
            // sits where a parent directory should be" shape - into that same
            // skip arm, laundering a hard mkdir failure into `Ok(None)` and
            // hiding it from the caller's exit-code path.
            Err(_) => Ok(DirDestination::Missing),
        }
    }

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
    /// - `generator.c:1480-1483` - `itemize()` is invoked once per directory entry,
    ///   so a freshly mkdir'd dir emits `cd+++++++++ <name>/` and an existing one
    ///   emits a metadata-only `.d ...` row gated by the standard significance check
    pub(in crate::receiver) fn create_directories<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        metadata_opts: &MetadataOptions,
        acl_cache: Option<&AclCache>,
        acl_id_map: Option<&AclIdMapper>,
        writer: &mut W,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    ) -> io::Result<Vec<(PathBuf, String)>> {
        // upstream: receiver.c:693 - dry_run skips all filesystem modifications;
        // list-only suppresses the receiver entirely (generator.c:1249).
        if self.config.flags.skip_dest_writes() {
            return Ok(Vec::new());
        }

        // upstream: generator.c:1273-1287 - check_filter(&daemon_filter_list, ...)
        // skips daemon-excluded directories before creation, reporting
        // `ERROR: daemon refused to receive directory "%s"` as FERROR_XFER
        // (generator.c:1281-1283) before the `skipping_dir_contents` jump. A
        // server receiver forwards that frame to the pushing client instead of
        // writing it to the daemon's own stderr.
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
                        // upstream: generator.c:1258-1266 - a directory below an
                        // already-refused one is dropped in silence; only the
                        // outermost refusal is reported.
                        if crate::receiver::daemon_filter_refuses_ancestor(filters, name) {
                            return false;
                        }
                        if !filters.allows(Path::new(name), true) {
                            let _ = self.emit_error_xfer_line(
                                writer,
                                &format!("ERROR: daemon refused to receive directory \"{name}\"\n"),
                            );
                            // upstream: generator.c:1284-1285 jumps to
                            // `skipping_dir_contents`, whose FERROR notice
                            // (generator.c:1492) announces that the directory's
                            // contents are being dropped.
                            let _ = self.emit_error_line(
                                writer,
                                "*** Skipping any contents from this failed directory ***\n",
                            );
                            return false;
                        }
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
        // upstream: generator.c:1374-1378 - directories skipped under
        // --existing (ignore_non_existing) are NOT errors: upstream sets
        // skip_dir / FLAG_MISSING_DIR and never touches io_error. Track them
        // apart from `failed_dir_paths` (real mkdir EACCES failures) so the
        // itemize/metadata passes below skip them without folding a spurious
        // "failed to create directory" into `dir_creation_errors` (which would
        // wrongly set IOERR_GENERAL -> RERR_PARTIAL/exit 23).
        let mut skipped_existing_dirs: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        // Track whether each directory was freshly created (true) or already
        // existed (false). Drives the iflags passed to `emit_itemize` so the
        // receiver matches upstream `generator.c:1480-1483`: a new dir emits
        // `cd+++++++++ <name>/`, an existing one emits a metadata-only row
        // gated by the standard significance check.
        let mut dir_was_new: Vec<bool> = Vec::with_capacity(dir_entries.len());
        // upstream: generator.c:1349-1352 - probe each new parent directory's
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
        // upstream: generator.c:1368-1383 - with --existing (ignore_non_existing),
        // a directory that does not yet exist at the destination is never created;
        // upstream sets skip_dir = file and FLAG_MISSING_DIR so the missing dir and
        // its descendants are skipped. Because dir_entries are processed in
        // parent-first sorted order and we never create the parent, each descendant
        // path also fails the .exists() probe and is skipped the same way.
        let existing_only = self.config.file_selection.existing_only;
        // upstream: generator.c:1480-1483 - itemize() compares a directory
        // against the stat taken when the generator first reaches its flist
        // entry, before any child (which sorts after its parent) is created.
        // The mkdir loop below materialises child subdirectories first, and
        // each child mkdir bumps its parent's on-disk mtime; re-stat'ing an
        // existing directory after the loop would observe that bumped mtime and
        // emit a spurious `.d..t...... ./` for an otherwise-unchanged transfer
        // root. Capture each already-present directory's pre-mkdir stat here so
        // the itemize pass compares against the true pre-transfer state.
        let pre_mkdir_meta: Vec<Option<fs::Metadata>> = dir_entries
            .iter()
            .map(|(_, _, dir_path)| fs::metadata(dir_path).ok())
            .collect();
        for (_, relative_path, dir_path) in &dir_entries {
            // `relative_path` is only read on Unix (mkdirat fast path).
            #[cfg(not(unix))]
            let _ = relative_path;
            // upstream: generator.c:1356 / 1451-1455 - classify the destination
            // via lstat (not exists()) so a symlink-to-directory is replaced by
            // a real directory unless --keep-dirlinks is set, in which case it is
            // followed.
            //
            // upstream: generator.c:1840-1843 - a failed
            // `delete_item(..., DEL_FOR_DIR)` does `goto skipping_dir_contents`;
            // it does NOT re-probe and carry on. The previous
            // `.unwrap_or_else(|_| if dir_path.exists() { .. })` recovery was
            // wrong twice over: `exists()` FOLLOWS symlinks, so the very entry
            // whose removal just failed reported as an existing directory, and
            // the transfer then proceeded through a symlink the peer controls.
            // It also swallowed the errno, which is how a denied unlink
            // surfaced three layers later as an opaque EXDEV from the confined
            // walk. upstream generator.c:1443-1467 states the rule for the
            // whole family: a runtime denial is a real failure and is
            // deliberately NOT retried unconfined.
            let dir_dest = match self.classify_dir_destination(
                dir_path,
                #[cfg(unix)]
                sandbox,
                dest_dir,
                relative_path,
            ) {
                Ok(dest) => dest,
                Err(error) => {
                    if self.config.flags.verbose && self.config.connection.client_mode {
                        info_log!(
                            Misc,
                            1,
                            "failed to prepare directory {}: {}",
                            dir_path.display(),
                            error
                        );
                    }
                    emit_lsm_audit_hint_once();
                    // Mirrors the mkdir-failure arm below: record the directory
                    // as failed so the itemize and metadata passes skip it and
                    // `dir_creation_errors` folds IOERR_GENERAL into the exit
                    // code, matching upstream's "any errors get reported later".
                    dir_was_new.push(false);
                    failed_dir_paths.insert(dir_path.clone());
                    continue;
                }
            };
            let is_new = dir_dest.needs_mkdir();
            dir_was_new.push(is_new);
            // upstream: generator.c:1401 - --existing (ignore_non_existing) only
            // skips a genuinely absent destination (statret == -1); a symlink
            // being replaced existed, so it is not skipped.
            if dir_dest == DirDestination::Missing && existing_only {
                // upstream: generator.c:1374-1378 - "not creating new directory".
                // Record in the skip set (not `failed_dir_paths`) so the
                // itemize and metadata passes below skip this directory without
                // treating the benign --existing skip as a mkdir failure.
                if self.config.flags.verbose && self.config.connection.client_mode {
                    info_log!(
                        Skip,
                        1,
                        "not creating new directory \"{}\"",
                        dir_path.display()
                    );
                }
                skipped_existing_dirs.insert(dir_path.clone());
                continue;
            }
            if is_new {
                #[cfg(all(
                    feature = "acl",
                    any(target_os = "linux", target_os = "macos", target_os = "freebsd")
                ))]
                if probe_default_perms {
                    if let Some(parent) = dir_path.parent() {
                        if probed_parents.insert(parent.to_path_buf()) {
                            // upstream: generator.c:1351 dflt_perms = default_perms_for_dir(dn)
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

        // upstream: receiver.c:736-738 - every newly created directory
        // (ITEM_IS_NEW) bumps stats.created_dirs, independent of itemize
        // visibility, so the "Number of created files" dir sub-count is correct
        // even without -i. Runs before (and separate from) the itemize gate
        // below, which is skipped when the client did not request itemize output.
        for ((idx, _, dir_path), is_new) in dir_entries.iter().zip(dir_was_new.iter()) {
            if *is_new
                && !failed_dir_paths.contains(dir_path)
                && !skipped_existing_dirs.contains(dir_path)
            {
                self.record_created(self.file_list[*idx].mode());
            }
        }

        // upstream: generator.c:1480-1483 - emit per-directory itemize rows
        // after the mkdir pass and before metadata application, so the row
        // ordering matches upstream's recv_generator() pass over the flist.
        // Skipped dirs (PermissionDenied during mkdir) do not emit a row.
        // The `should_emit_itemize` gate avoids touching the writer when
        // the client did not request itemize output (or the receiver runs
        // in client mode, where the CLI front-end emits via local-copy
        // records instead of MSG_INFO frames).
        if self.should_emit_itemize() {
            for (pos, ((idx, _, dir_path), is_new)) in
                dir_entries.iter().zip(dir_was_new.iter()).enumerate()
            {
                if failed_dir_paths.contains(dir_path) || skipped_existing_dirs.contains(dir_path) {
                    continue;
                }
                let entry = &self.file_list[*idx];
                let iflags = if *is_new {
                    // upstream: generator.c:1481 - new dir is itemize()'d with
                    // statret < 0, which ORs ITEM_LOCAL_CHANGE | ITEM_IS_NEW.
                    crate::generator::ItemFlags::from_raw(
                        crate::generator::ItemFlags::ITEM_LOCAL_CHANGE
                            | crate::generator::ItemFlags::ITEM_IS_NEW,
                    )
                } else {
                    // upstream: generator.c:1482 - existing dir is itemize()'d
                    // with statret == 0. itemize() (generator.c:511-572) still
                    // compares the pre-apply dest stat against the sender entry
                    // and sets ITEM_REPORT_{TIME,PERMS,OWNER,GROUP} for any
                    // attribute that differs; the transfer root `.` therefore
                    // reports `.d..t......` when its mtime differs. Compare
                    // against `pre_mkdir_meta` (captured before the mkdir loop
                    // bumped parent mtimes) so an unchanged root does not report
                    // a spurious time change; fall back to a fresh stat only if
                    // the pre-mkdir capture failed. emit_itemize's standard gate
                    // drops the row when nothing differs, and the root-dir
                    // compensation still fires `cd+++++++++ ./` when the
                    // pre-flight mkdir created the root.
                    let raw = match pre_mkdir_meta.get(pos).and_then(Option::as_ref) {
                        Some(meta) => self.itemize_existing_flags(entry, dir_path, Some(meta), 0),
                        None => self.existing_dir_iflags(entry, dir_path),
                    };
                    crate::generator::ItemFlags::from_raw(raw)
                };
                // Deferred on the run_pipelined path so the dir row lands in
                // flist-index order (immediately before its children) at flush
                // time; emitted immediately on every other path.
                let _ = self.emit_or_record_itemize(writer, *idx, &iflags, entry);
                self.record_server_no_transfer_itemize(*idx, iflags.raw());
            }
        }

        // Build owned data for parallel metadata application, skipping failed dirs.
        // upstream: rsync.c:583 - `omit_dir_times && S_ISDIR(...)` adds
        // ATTRS_SKIP_MTIME, so a directory's mtime is never applied under -O.
        // `effective_omit_dir_times` also folds in the implicit
        // `--backup`-without-`--backup-dir` rule (options.c:2342-2343), so an
        // empty backed-up directory keeps its wall-clock mtime rather than the
        // source mtime. On a pull the local client IS the receiver and -O rides
        // no wire bit (options.c:2646-2647 gates 'O' on am_sender), so clear
        // preserve_times for the directory apply here, mirroring the local
        // executor's apply_final_directory_metadata.
        let metadata_opts_clone = if self.config.effective_omit_dir_times() {
            metadata_opts.clone().preserve_times(false)
        } else {
            metadata_opts.clone()
        };
        // upstream: generator.c:1512-1520 - grant a transient u+rwx to any
        // directory whose final mode is not writable by us so the receiver can
        // create temp files inside it; the real mode is restored in
        // touch_up_dirs. Captured here so the closure below stays Send.
        #[cfg(unix)]
        let preserve_perms = metadata_opts.permissions();
        #[cfg(unix)]
        let fake_super = metadata_opts.fake_super_enabled();
        let entry_snapshots: Vec<(PathBuf, FileEntry, Option<XattrList>, Option<fs::Metadata>)> =
            dir_entries
                .into_iter()
                .zip(dir_was_new.iter().copied())
                .filter(|((_, _, dir_path), _)| {
                    !failed_dir_paths.contains(dir_path)
                        && !skipped_existing_dirs.contains(dir_path)
                })
                .map(|((idx, _, dir_path), is_new)| {
                    let entry = &self.file_list[idx];
                    let xattr_list = self.resolve_xattr_list(entry);
                    // `mut` is only exercised by the Unix transient-writable-mode
                    // grant below; on other platforms the clone is never mutated.
                    #[cfg_attr(not(unix), allow(unused_mut))]
                    let mut entry = entry.clone();
                    #[cfg(unix)]
                    if dir_needs_writable_transfer_mode(
                        preserve_perms,
                        fake_super,
                        entry.permissions(),
                    ) {
                        entry.set_mode(entry.mode() | 0o700);
                    }
                    // upstream: generator.c:1465 dest_mode(..., statret == 0) -
                    // an existing dir keeps its own perms (exists=true), a new
                    // dir gets the source mode masked by dflt_perms
                    // (exists=false). Supply the pre-transfer stat only for a
                    // dir that already existed so the !perms dest_mode() apply
                    // takes the exists=true branch and never rewrites its bits.
                    let pre_transfer = if is_new {
                        None
                    } else {
                        fs::metadata(&dir_path).ok()
                    };
                    (dir_path, entry, xattr_list, pre_transfer)
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
        let acl_id_map_clone = acl_id_map.cloned();
        let xattr_filter = self.xattr_name_filter_arc();
        let results = crate::parallel_io::map_blocking(
            entry_snapshots,
            self.parallel_thresholds
                .for_op(crate::parallel_io::ParallelOp::Metadata),
            move |(dir_path, entry, xattr_list, pre_transfer)| {
                if let Err(e) = apply_metadata_with_pre_transfer_stat(
                    &dir_path,
                    &entry,
                    &metadata_opts_clone,
                    None,
                    pre_transfer,
                ) {
                    return Some((dir_path, e.to_string()));
                }
                // Apply cached ACLs after metadata
                if let Err(e) = apply_acls_from_receiver_cache(
                    &dir_path,
                    &entry,
                    acl_cache_clone.as_ref(),
                    acl_id_map_clone.as_ref(),
                    true, // directories always follow symlinks
                ) {
                    return Some((dir_path, e.to_string()));
                }
                // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
                if let Some(ref xattr_list) = xattr_list {
                    let filter = xattr_filter.as_ref().map(|set| {
                        move |name: &str| set.xattr_name_allowed(name, filters::XattrSide::Receiver)
                    });
                    let filter_ref = filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool);
                    // upstream: rsync_xal_set resolves an abbreviated value
                    // against fnamecmp; a directory is its own basis.
                    if let Err(e) = metadata::apply_xattrs_from_list(
                        &dir_path,
                        xattr_list,
                        true,
                        Some(&dir_path),
                        filter_ref,
                        None,
                    ) {
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

    /// Creates the still-missing implied parent directories of `--relative`
    /// path components.
    ///
    /// When `--relative` is active the file list may contain entries with deep
    /// paths (`a/b/c/file.txt`). With `--no-implied-dirs` at protocol < 30 the
    /// intermediate directories (`a/`, `a/b/`, `a/b/c/`) never appear as
    /// directory entries, so nothing else would create them. This method fills
    /// exactly that gap.
    ///
    /// It is a **fallback, not a pre-pass**: the caller must run it *after* the
    /// file list's own directory entries have been created and itemized. That
    /// ordering is upstream's. `recv_generator()` walks the file list in order;
    /// a directory entry is created and itemized by `gen_entry_mkdir()`
    /// (`generator.c:1873`) when its own entry is reached, and the parent
    /// `make_path()` at `generator.c:1718-1725` fires only for an entry whose
    /// `do_stat_at(dn, ...)` still reports the parent absent. Running it first
    /// pre-empts the directory pass: the directories then already exist when
    /// they are classified, so a real run reports `.d..t......` where its own
    /// `--dry-run` (which skips destination writes) reports `cd+++++++++`, and
    /// the unconfined `create_dir` displaces the confined `mkdirat` that the
    /// classified path would have issued.
    ///
    /// Uses a set to track already-created paths, avoiding redundant `mkdir`
    /// syscalls when many entries share common parent directories.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1718-1725` - `make_path(fname, MKP_DROP_NAME |
    ///   MKP_SKIP_SLASH)` for a parent that `do_stat_at()` reports missing
    /// - `generator.c:1876-1878` - retry `gen_entry_mkdir()` after `make_path()`
    ///   when `relative_paths` and the initial mkdir returns `ENOENT`
    /// - `util1.c:238` / `util1.c:277` - every component `make_path()` creates
    ///   goes through `do_mkdir_at()`, i.e. the confined mkdir, never a bare
    ///   `mkdir(2)`. `syscall.c:2066 do_mkdir_at()` opens the parent with
    ///   `owner_walk_parent()` and issues `mkdirat()` against that dirfd, so a
    ///   symlinked component cannot redirect the create out of the module.
    pub(in crate::receiver) fn ensure_relative_parents(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    ) {
        if !self.config.flags.relative || self.config.flags.skip_dest_writes() {
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
            let mut ancestors_to_create: Vec<(PathBuf, &Path)> = Vec::new();
            let mut current = target;
            loop {
                let abs_path = dest_dir.join(current);
                if created.contains(&abs_path) || abs_path.exists() {
                    break;
                }
                ancestors_to_create.push((abs_path, current));
                match current.parent() {
                    Some(p) if !p.as_os_str().is_empty() => current = p,
                    _ => break,
                }
            }

            // Create from shallowest to deepest. Each component goes through
            // the confined mkdir, mirroring `make_path()`'s use of
            // `do_mkdir_at()` (util1.c:238/277) rather than a bare `mkdir(2)`.
            for (dir_path, rel_path) in ancestors_to_create.into_iter().rev() {
                #[cfg(unix)]
                let create_result = fast_io::mkdirat_via_sandbox_or_fallback(
                    sandbox, dest_dir, rel_path, &dir_path, 0o777,
                );
                #[cfg(not(unix))]
                let create_result = {
                    let _ = rel_path;
                    fs::create_dir(&dir_path)
                };
                if let Err(e) = create_result {
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
    /// Returns `Ok(Some((true, iflags)))` when a new directory was created.
    /// Returns `Ok(Some((false, iflags)))` when an existing directory had
    /// metadata applied. In both cases `iflags` are the raw itemize flags the
    /// caller should emit: for a new dir, `ITEM_LOCAL_CHANGE | ITEM_IS_NEW`;
    /// for an existing dir, the attribute-diff flags computed against the
    /// pre-apply destination stat (`ITEM_REPORT_{TIME,PERMS,OWNER,GROUP}`),
    /// mirroring upstream's `itemize()` at `generator.c:1481` which runs before
    /// `set_file_attrs` (`generator.c:1503`). Only returns `Err` for
    /// unrecoverable errors.
    ///
    /// Under `--dry-run` / `--list-only` the destination is left untouched: no
    /// `mkdir`, no symlink removal, no metadata. The classification, the
    /// returned `iflags`, and the `is_new` flag that drives the caller's
    /// created-directory tally are still computed, so a dry run reports exactly
    /// what a real run would create.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1432` - `recv_generator()` creates directories
    /// - `generator.c:1484-1487` - retry `mkdir` after `make_path()`
    /// - `generator.c:1480-1483` - `itemize()` before metadata application
    /// - `syscall.c:1010-1016` - `do_mkdir()` is a no-op under `dry_run`, so
    ///   `itemize()` and the receiver's `created_dirs` tally still run
    pub(in crate::receiver) fn create_directory_incremental(
        &self,
        dest_dir: &Path,
        entry: &FileEntry,
        metadata_opts: &MetadataOptions,
        failed_dirs: &mut FailedDirectories,
        acl_cache: Option<&AclCache>,
        acl_id_map: Option<&AclIdMapper>,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    ) -> io::Result<Option<(bool, u32)>> {
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
        // upstream: generator.c:1356 / 1451-1455 - lstat-classify the
        // destination so a symlink-to-directory is replaced by a real directory
        // unless --keep-dirlinks follows it.
        //
        // upstream: generator.c:1840-1843 - a failed
        // `delete_item(..., DEL_FOR_DIR)` goes to `skipping_dir_contents`, so
        // the directory and its contents are skipped and the error is reported.
        // This is the incremental-recursion twin of the `create_directories`
        // arm above; both previously recovered via a FOLLOWING `exists()`,
        // which reports the un-removed symlink as an existing directory and
        // walks the transfer straight through it.
        let dir_dest = match self.classify_dir_destination(
            &dir_path,
            #[cfg(unix)]
            sandbox,
            dest_dir,
            relative_path,
        ) {
            Ok(dest) => dest,
            Err(error) => {
                if self.config.flags.verbose && self.config.connection.client_mode {
                    info_log!(
                        Misc,
                        1,
                        "failed to prepare directory {}: {}",
                        dir_path.display(),
                        error
                    );
                }
                emit_lsm_audit_hint_once();
                failed_dirs.mark_failed(entry.name());
                return Ok(None);
            }
        };
        let is_new = dir_dest.needs_mkdir();
        // upstream: generator.c:1368-1383 - with --existing (ignore_non_existing),
        // a directory missing at the destination is never created; the dir is
        // marked skipped (FLAG_MISSING_DIR) so its descendants are skipped too.
        // Marking it failed here drives the same descendant skip via the
        // failed-ancestor check above on subsequent entries.
        //
        // upstream: generator.c:1401 - --existing only skips a genuinely absent
        // destination; a replaced symlink existed and is not skipped.
        if dir_dest == DirDestination::Missing && self.config.file_selection.existing_only {
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(
                    Skip,
                    1,
                    "not creating new directory \"{}\"",
                    dir_path.display()
                );
            }
            failed_dirs.mark_failed(entry.name());
            return Ok(None);
        }
        // upstream: generator.c:1480-1483 - itemize() runs before set_file_attrs
        // (generator.c:1503), so compute the itemize flags from the pre-apply
        // destination stat here. A new dir reports ITEM_LOCAL_CHANGE|ITEM_IS_NEW
        // (`cd+++++++++`); an existing dir reports the attribute-diff flags
        // (ITEM_REPORT_{TIME,PERMS,OWNER,GROUP}) so a differing root `.` mtime
        // emits `.d..t......`. For an existing dir the stat must be read now,
        // before apply_metadata_with_pre_transfer_stat below overwrites the mtime.
        let iflags: u32 = if is_new {
            crate::generator::ItemFlags::ITEM_LOCAL_CHANGE
                | crate::generator::ItemFlags::ITEM_IS_NEW
        } else {
            self.existing_dir_iflags(entry, &dir_path)
        };
        // upstream: syscall.c:1010-1016 - `do_mkdir()` (reached from
        // `do_mkdir_at()`) returns 0 without touching the filesystem when
        // `dry_run` is set, and `rsync.c:498-499 set_file_attrs()` returns early
        // the same way, while `generator.c:1480-1483 itemize()` still runs above.
        // A dry run therefore reports the directory it *would* create - itemize
        // row and created-dir tally intact - without creating it. Placed after
        // the classification and the `iflags` computation so both survive; an
        // early return here would silence output upstream still prints.
        let skip_dest_writes = self.config.flags.skip_dest_writes();
        if is_new && !skip_dest_writes {
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

        // upstream: rsync.c:498-499 - `set_file_attrs()` returns early under
        // dry_run, so no metadata, xattrs, or ACLs reach the destination.
        if !skip_dest_writes {
            self.apply_incremental_dir_metadata(
                &dir_path,
                entry,
                metadata_opts,
                is_new,
                acl_cache,
                acl_id_map,
            );
        }

        // The plain-`-v` directory name is NOT emitted here. Its caller
        // (`run_pipelined_incremental`) decides it from `verbose_dir_name_lines`
        // against the pre-transfer stat, exactly as `run_pipelined` does, so an
        // unchanged directory stays silent (upstream names a directory only when
        // `set_file_attrs()` changed it, generator.c:1503-1505) and the name can
        // be interleaved with `--progress` in flist order. Naming it here also
        // ran after this call's own metadata apply, which is too late to observe
        // the pre-transfer state.
        Ok(Some((is_new, iflags)))
    }

    /// Applies metadata, xattrs, and cached ACLs to one directory created or
    /// reused by [`Self::create_directory_incremental`].
    ///
    /// Every failure is non-fatal and reported as a verbose warning, mirroring
    /// upstream's `set_file_attrs()` error handling: the transfer continues with
    /// the remaining entries.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1503` - `set_file_attrs()` after the directory mkdir
    /// - `generator.c:1465` - `dest_mode(..., statret == 0)` keeps an existing
    ///   directory's own permission bits when `--perms` is not in effect
    /// - `generator.c:1512-1520` - transient `u+rwx` grant, undone by
    ///   [`Self::touch_up_dirs`]
    /// - `xattrs.c:set_xattr()` - xattrs are applied after metadata
    fn apply_incremental_dir_metadata(
        &self,
        dir_path: &Path,
        entry: &FileEntry,
        metadata_opts: &MetadataOptions,
        is_new: bool,
        acl_cache: Option<&AclCache>,
        acl_id_map: Option<&AclIdMapper>,
    ) {
        // upstream: generator.c:1512-1520 - grant a transient u+rwx to a
        // read-only directory so files can be written into it; the real mode
        // is restored in touch_up_dirs.
        #[cfg(unix)]
        let tweaked_entry = dir_needs_writable_transfer_mode(
            metadata_opts.permissions(),
            metadata_opts.fake_super_enabled(),
            entry.permissions(),
        )
        .then(|| {
            let mut e = entry.clone();
            e.set_mode(e.mode() | 0o700);
            e
        });
        #[cfg(unix)]
        let apply_entry = tweaked_entry.as_ref().unwrap_or(entry);
        #[cfg(not(unix))]
        let apply_entry = entry;
        // upstream: generator.c:1465 dest_mode(..., statret == 0) - supply the
        // pre-transfer stat only for a dir that already existed so the !perms
        // dest_mode() apply keeps its own permission bits (exists=true) instead
        // of rewriting them; a freshly created dir (is_new) uses exists=false.
        let pre_transfer = if is_new {
            None
        } else {
            fs::metadata(dir_path).ok()
        };
        if let Err(e) = apply_metadata_with_pre_transfer_stat(
            dir_path,
            apply_entry,
            metadata_opts,
            None,
            pre_transfer,
        ) {
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
            let filter = self.xattr_name_filter().map(|set| {
                move |name: &str| set.xattr_name_allowed(name, filters::XattrSide::Receiver)
            });
            let filter_ref = filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool);
            // upstream: rsync_xal_set resolves an abbreviated value against
            // fnamecmp; a directory is its own basis.
            if let Err(e) = metadata::apply_xattrs_from_list(
                dir_path,
                xattr_list,
                true,
                Some(dir_path),
                filter_ref,
                None,
            ) {
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

        if let Err(e) = apply_acls_from_receiver_cache(dir_path, entry, acl_cache, acl_id_map, true)
        {
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
    }

    /// Restores directory permissions and mtimes after all file transfers
    /// complete.
    ///
    /// Two repairs happen here, both undoing side effects of the transfer:
    ///
    /// - **Permissions.** A directory whose final mode is not writable by us
    ///   was granted a transient `u+rwx` during creation (see
    ///   [`dir_needs_writable_transfer_mode`]) so the receiver could create
    ///   temp files inside it. The real, restrictive mode is reinstated here.
    /// - **Mtimes.** Writing files into a directory updates its mtime to the
    ///   current time (OS behavior). Each directory's mtime is re-set from the
    ///   file-list entry.
    ///
    /// The flist is walked in reverse (deepest first) so a parent's mtime is
    /// not clobbered when a child directory under it is later re-touched.
    ///
    /// The permission repair is gated on `-p` (`--perms`) and skipped for
    /// root / `--fake-super`; the mtime repair is gated on `-t` (`--times`)
    /// and skipped when backups without a backup-dir are active. Both are
    /// skipped for dry-run.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2093-2146` - `touch_up_dirs(dir_flist, -1)` iterates in
    ///   reverse order and repairs perms then times.
    /// - `generator.c:2122-2127` - `fix_dir_perms = !am_root && !(mode &
    ///   S_IWUSR)` restores the real directory mode.
    /// - `generator.c:2398-2399` - `need_retouch_dir_times` gating:
    ///   `preserve_mtimes && !omit_dir_times`.
    /// - `generator.c:2138-2144` - the retouch loop pokes `maybe_send_keepalive`
    ///   every `loopchk_limit` iterations so a remote sender's `--timeout` does
    ///   not fire while the generator re-sets directory mtimes/perms across a
    ///   large tree without writing anything to the socket.
    pub(in crate::receiver) fn touch_up_dirs<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        writer: &mut W,
    ) {
        if self.config.flags.skip_dest_writes() {
            return;
        }

        // upstream: generator.c:2271 - need_retouch_dir_times =
        // preserve_mtimes && !omit_dir_times. `effective_omit_dir_times` folds
        // in the implicit `--backup`-without-`--backup-dir` rule
        // (options.c:2342-2343, generator.c:2101), so the same predicate governs
        // both this retouch pass and the creation-time apply above.
        let retouch_times = self.config.flags.times && !self.config.effective_omit_dir_times();

        // upstream: generator.c:2122 - fix_dir_perms = !am_root && !(mode &
        // S_IWUSR); only meaningful when we preserve perms (otherwise the
        // directory keeps its umask-derived writable mode).
        #[cfg(unix)]
        let retouch_perms =
            self.config.flags.perms && !self.config.fake_super && !metadata::am_root();
        #[cfg(not(unix))]
        let retouch_perms = false;

        if !retouch_times && !retouch_perms {
            return;
        }

        // Iterate in reverse so deepest directories are touched first.
        // upstream: generator.c:2083 - for (i = dir_flist->used - 1; i >= 0; i--)
        for entry in self.file_list.iter().rev() {
            // upstream: generator.c:2138-2144 - poke a keepalive once the I/O
            // lull has elapsed so a remote sender does not time out while this
            // final metadata pass walks a large directory tree. A strict no-op
            // unless --timeout is set (allowed_lull None).
            let _ = writer.maybe_send_keepalive();
            if !entry.is_dir() {
                continue;
            }

            let relative_path = entry.path();
            let dir_path = if relative_path.as_os_str() == "." {
                dest_dir.to_path_buf()
            } else {
                dest_dir.join(relative_path)
            };

            // upstream: generator.c:2124-2125 - restore the real mode before
            // the mtime repair. Only directories that lack the user write bit
            // were tweaked, so only those are chmod'd back.
            #[cfg(unix)]
            if retouch_perms && (entry.permissions() & 0o200) == 0 {
                use std::os::unix::fs::PermissionsExt;
                let perms = fs::Permissions::from_mode(entry.permissions());
                if let Err(e) = fs::set_permissions(&dir_path, perms) {
                    debug_log!(
                        Recv,
                        1,
                        "touch_up_dirs: failed to restore perms on {}: {}",
                        dir_path.display(),
                        e
                    );
                }
            }

            if !retouch_times {
                continue;
            }

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
///   `OnceLock`) so high file counts do not flood the log,
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

    /// A directory skipped under `--existing` (`ignore_non_existing`) must not
    /// be reported as a "failed to create directory" error. Upstream sets
    /// `skip_dir` / `FLAG_MISSING_DIR` and never touches `io_error`
    /// (generator.c:1374-1378), so the non-incremental `create_directories`
    /// pass on a remote pull must return an empty error vec for such a dir.
    ///
    /// This is the load-bearing regression: folding the benign skip into the
    /// error set set `IOERR_GENERAL`, which surfaced as `RERR_PARTIAL` (exit
    /// 23) once the client honoured the receiver's `io_error`. That broke a
    /// plain `--existing --include='*/' --exclude='*'` pull, which must exit 0
    /// exactly like upstream.
    #[test]
    fn create_directories_existing_only_missing_dir_is_not_an_error() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let mut config = config_with_times(false);
        config.file_selection.existing_only = true;

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.file_list = vec![FileEntry::new_directory("missing".into(), 0o755)];

        let opts = metadata::MetadataOptions::default();
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        let errors = ctx
            .create_directories(
                dest,
                &opts,
                None,
                None,
                &mut writer,
                #[cfg(unix)]
                None,
            )
            .expect("create_directories succeeds");

        assert!(
            errors.is_empty(),
            "--existing skip of a missing directory must not produce an error \
             (would set IOERR_GENERAL -> exit 23): {errors:?}"
        );
        assert!(
            !dest.join("missing").exists(),
            "--existing must not create the missing directory"
        );
    }

    /// A refused obstruction removal must be REPORTED, not laundered into
    /// "the destination already exists".
    ///
    /// upstream: generator.c:1840-1843 - when
    /// `delete_item(..., del_opts | DEL_FOR_DIR)` fails, upstream does
    /// `goto skipping_dir_contents`; it never re-probes the destination and
    /// carries on. oc previously recovered with
    /// `.unwrap_or_else(|_| if dir_path.exists() { Existing } else { Missing })`,
    /// and `exists()` FOLLOWS symlinks - so the very entry whose removal had
    /// just been denied reported back as an existing directory, the transfer
    /// proceeded through a peer-controlled symlink, and the errno was
    /// discarded. That discarded errno is what made a denied `unlink` surface
    /// three layers later as an opaque EXDEV from the confined walk.
    ///
    /// The fixture denies the removal with a read-only parent, which is the
    /// portable stand-in for any refusal - a confined-walk rejection or a
    /// syscall-filter denial reaches the same arm.
    #[cfg(unix)]
    #[test]
    fn a_refused_obstruction_removal_is_reported_not_reclassified() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        if metadata::am_root() {
            // root bypasses the directory write bit, so the fixture cannot
            // deny the removal and the cell would pass vacuously.
            return;
        }

        let dir = test_support::create_tempdir();
        let dest = dir.path();
        let target = dest.join("real");
        fs::create_dir(&target).unwrap();
        symlink(&target, dest.join("d")).unwrap();

        // Deny the unlink by removing write permission from the parent.
        let original = fs::metadata(dest).unwrap().permissions();
        fs::set_permissions(dest, fs::Permissions::from_mode(0o555)).unwrap();

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, config_with_times(false));
        ctx.file_list = vec![FileEntry::new_directory("d".into(), 0o755)];

        let opts = metadata::MetadataOptions::default();
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        let errors = ctx.create_directories(
            dest,
            &opts,
            None,
            None,
            &mut writer,
            #[cfg(unix)]
            None,
        );

        // Restore before asserting so a failure does not leave the tempdir
        // undeletable.
        fs::set_permissions(dest, original).unwrap();

        let errors = errors.expect("create_directories returns the error set, not Err");
        assert!(
            !errors.is_empty(),
            "a denied obstruction removal must be reported so io_error reaches \
             the exit code; reclassifying it as an existing directory walks the \
             transfer through the surviving symlink"
        );
        assert!(
            fs::symlink_metadata(dest.join("d"))
                .expect("the entry still exists")
                .file_type()
                .is_symlink(),
            "the fixture is only meaningful while the removal actually failed"
        );
    }

    /// The twin of `surfaces_non_permission_error_from_mkdir`, for the BATCH
    /// pass: a stat failure is not an obstruction refusal, and the two must not
    /// share an arm.
    ///
    /// `classify_dir_destination` can fail for two unrelated reasons, and only
    /// one of them is upstream's `skipping_dir_contents` case:
    ///
    /// - the confined unlink of an obstructing symlink was refused - upstream
    ///   `generator.c:1841-1842` `goto skipping_dir_contents`, pinned by
    ///   `a_refused_obstruction_removal_is_reported_not_reclassified` above;
    /// - the destination could not be stat'd at all - upstream
    ///   `generator.c:1745` just records `statret = -1`, and because
    ///   `generator.c:1840` gates `delete_item()` on `statret == 0`, the entry
    ///   falls through to `gen_entry_mkdir()` and that mkdir's error is what
    ///   gets reported.
    ///
    /// Collapsing the second into the first turns a hard failure into a silent
    /// "directory skipped": ENOTDIR from a plain file sitting where a parent
    /// directory belongs stopped propagating and was folded into the failed-dir
    /// set instead. This fixture shapes exactly that - `afile` is a regular
    /// file, so stat'ing `afile/sub` fails ENOTDIR, which is neither NotFound
    /// (the parent-walk retry) nor PermissionDenied (upstream's non-fatal
    /// class) - and pins the hard error to `Err`.
    #[cfg(unix)]
    #[test]
    fn a_stat_failure_is_not_an_obstruction_refusal_and_still_fails_loud() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        // A regular file where a parent directory is expected forces ENOTDIR
        // when the receiver stats and then creates `afile/sub` beneath it.
        fs::write(dest.join("afile"), b"not a directory").expect("plant regular file");

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, config_with_times(false));
        ctx.file_list = vec![FileEntry::new_directory("afile/sub".into(), 0o755)];

        let opts = metadata::MetadataOptions::default();
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        let result = ctx.create_directories(
            dest,
            &opts,
            None,
            None,
            &mut writer,
            #[cfg(unix)]
            None,
        );

        let err = result.expect_err(
            "a non-EACCES mkdir failure must propagate as Err from the batch pass, \
             not be laundered into the failed-directory set",
        );
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::PermissionDenied,
            "EACCES takes upstream's non-fatal branch; this fixture avoids it"
        );
        assert_ne!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "NotFound would take the parent-walk retry, not the hard-error path"
        );
    }

    /// Regression: creating a child subdirectory inside an already-present
    /// transfer root bumps the root's on-disk mtime. The batch
    /// `create_directories` pass mkdir's every directory before it itemizes any,
    /// so re-stat'ing the root afterwards observed the bumped mtime and produced
    /// a spurious `.d..t...... ./` row that upstream never emits. Upstream's
    /// `itemize()` runs when the generator first reaches each entry, before its
    /// children are created (`generator.c:1480-1483`, before `set_file_attrs`),
    /// so an unchanged root reports no time change. The receiver must compare a
    /// directory against its pre-mkdir stat: an unchanged root emits no row even
    /// when a brand-new child subdir is created inside it, while a genuinely
    /// new child still itemizes as created.
    #[cfg(unix)]
    #[test]
    fn create_directories_unchanged_root_not_itemized_after_child_mkdir() {
        let dir = test_support::create_tempdir();
        let dest = dir.path();

        // Pin the pre-existing transfer-root mtime to a fixed value so the
        // sender entry below can match it exactly.
        let root_secs: i64 = 1_577_836_800; // 2020-01-01 00:00:00 UTC
        filetime::set_file_mtime(dest, FileTime::from_unix_time(root_secs, 0)).unwrap();

        let mut config = config_with_times(true);
        config.flags.info_flags.itemize = true;
        config.connection.client_mode = true;

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        // Record rendered rows under their flist index instead of writing to the
        // process stdout, so the test can inspect exactly what was itemized.
        ctx.defer_itemize = true;

        // Transfer root `.` (index 0) carries the SAME mtime as the on-disk dest
        // root; the new child subdir `sub` (index 1) is created inside it, which
        // bumps the root's on-disk mtime mid-pass.
        let mut root_entry = FileEntry::new_directory(".".into(), 0o755);
        root_entry.set_mtime(root_secs, 0);
        ctx.file_list = vec![root_entry, FileEntry::new_directory("sub".into(), 0o755)];

        let opts = metadata::MetadataOptions::default();
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        ctx.create_directories(
            dest,
            &opts,
            None,
            None,
            &mut writer,
            #[cfg(unix)]
            None,
        )
        .expect("create_directories succeeds");

        // Proof the root's on-disk mtime was bumped mid-pass: the child exists.
        assert!(dest.join("sub").is_dir(), "child subdir must be created");

        let rows = ctx.itemize_rows.borrow();
        // Root `.` (index 0) is unchanged relative to the sender, so comparing
        // against the pre-mkdir stat yields no significant flags and no row.
        if let Some(lines) = rows.get(&0) {
            assert!(
                lines.is_empty(),
                "unchanged transfer root must not itemize (got {lines:?})"
            );
        }
        // The genuinely new child subdir (index 1) still reports the creation
        // glyph - the fix must not suppress legitimate directory itemization.
        assert_eq!(
            rows.get(&1).map(Vec::as_slice).unwrap_or_default(),
            ["cd+++++++++ sub/\n"],
            "a newly created child subdir must still itemize as created"
        );
    }

    /// After writing files into a directory, the OS clobbers the directory
    /// mtime with the current time. `touch_up_dirs` must re-apply the
    /// original mtime from the file list entry.
    ///
    /// upstream: generator.c:2093-2146 - touch_up_dirs()
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

        ctx.touch_up_dirs(
            dir.path(),
            &mut crate::writer::ServerWriter::new_plain(Vec::new()),
        );

        let meta = fs::metadata(&sub).unwrap();
        let actual = FileTime::from_last_modification_time(&meta);
        let expected = FileTime::from_unix_time(desired_secs, 0);
        assert_eq!(
            actual, expected,
            "directory mtime should be restored to the file list value"
        );
    }

    /// Under `--omit-dir-times` the retouch pass must NOT re-apply the source
    /// directory mtime, leaving the directory at its (current) on-disk mtime.
    ///
    /// upstream: generator.c:2271 - `need_retouch_dir_times = preserve_mtimes
    /// && !omit_dir_times`. On a remote pull the local client IS the receiver
    /// and `-O` never rides the wire (options.c:2646-2647 gates the compact
    /// 'O' on am_sender), so the receiver config must carry `omit_dir_times`
    /// and honor it here. Regression guard for the remote pull that applied the
    /// source directory mtime despite `-O`.
    #[test]
    fn omit_dir_times_skips_directory_mtime_restore() {
        let dir = test_support::create_tempdir();
        let sub = dir.path().join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.txt"), b"hello").unwrap();

        // Source mtime is in the past; with -O it must NOT be applied.
        let source_secs: i64 = 1_577_836_800;
        let mut entry = FileEntry::new_directory("subdir".into(), 0o755);
        entry.set_mtime(source_secs, 0);

        let hs = handshake();
        let mut config = config_with_times(true);
        config.flags.omit_dir_times = true;
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        ctx.file_list = vec![entry];

        ctx.touch_up_dirs(
            dir.path(),
            &mut crate::writer::ServerWriter::new_plain(Vec::new()),
        );

        let meta = fs::metadata(&sub).unwrap();
        let actual = FileTime::from_last_modification_time(&meta);
        let source_mtime = FileTime::from_unix_time(source_secs, 0);
        assert_ne!(
            actual, source_mtime,
            "under --omit-dir-times the source directory mtime must NOT be applied"
        );
    }

    /// The writable-transfer helper mirrors upstream's `dir_tweaking` gate
    /// (`generator.c:1512`): only a non-root receiver preserving perms on a
    /// directory that lacks full user `rwx` needs the transient `u+rwx`.
    #[cfg(unix)]
    #[test]
    fn writable_transfer_mode_helper_matches_upstream_gate() {
        let root = metadata::am_root();
        // A read-only dir needs the tweak only when non-root + preserving perms.
        assert_eq!(
            super::dir_needs_writable_transfer_mode(true, false, 0o555),
            !root
        );
        // A dir that already has full user rwx never needs the tweak.
        assert!(!super::dir_needs_writable_transfer_mode(true, false, 0o755));
        // Not preserving perms, or --fake-super, disables the tweak.
        assert!(!super::dir_needs_writable_transfer_mode(
            false, false, 0o555
        ));
        assert!(!super::dir_needs_writable_transfer_mode(true, true, 0o555));
    }

    /// Regression for the `mkstemp ... Permission denied` (#250) data bug: a
    /// source directory with a read-only mode (e.g. `0555`) must still be
    /// writable while the receiver creates files inside it, then be restored
    /// to its restrictive mode afterward.
    ///
    /// upstream: generator.c:1512-1520 (grant `u+rwx`) + generator.c:2122-2127
    /// (`fix_dir_perms` restore in touch_up_dirs).
    #[cfg(unix)]
    #[test]
    fn readonly_dir_is_writable_during_transfer_then_restored() {
        use std::os::unix::fs::PermissionsExt;

        let dir = test_support::create_tempdir();
        let dest = dir.path();

        let mut config = config_with_times(false);
        config.flags.perms = true;

        let hs = handshake();
        let mut ctx = ReceiverContext::new_for_test(&hs, config);
        // Read-only directory mode: r-xr-xr-x, no user write bit.
        ctx.file_list = vec![FileEntry::new_directory("sub".into(), 0o555)];

        let opts = metadata::MetadataOptions::default();
        let mut writer = crate::writer::ServerWriter::new_plain(Vec::new());
        ctx.create_directories(
            dest,
            &opts,
            None,
            None,
            &mut writer,
            #[cfg(unix)]
            None,
        )
        .expect("create_directories succeeds");

        // During the transfer window the directory must be writable so the
        // receiver can create a temp file inside it. Under a non-root test
        // runner this only holds because of the u+rwx tweak; under root the
        // write always succeeds. Either way, creating a file must not fail.
        let sub = dest.join("sub");
        fs::write(sub.join("file.txt"), b"payload")
            .expect("must be able to create files in a read-only-mode dir mid-transfer");

        // After the transfer the restrictive mode must be reinstated (skipped
        // under root / --fake-super, matching upstream fix_dir_perms).
        ctx.touch_up_dirs(
            dest,
            &mut crate::writer::ServerWriter::new_plain(Vec::new()),
        );
        if !metadata::am_root() {
            let mode = fs::metadata(&sub).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o555,
                "restrictive directory mode must be restored after transfer"
            );
        }
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

        ctx.touch_up_dirs(
            dir.path(),
            &mut crate::writer::ServerWriter::new_plain(Vec::new()),
        );

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

        ctx.touch_up_dirs(
            dir.path(),
            &mut crate::writer::ServerWriter::new_plain(Vec::new()),
        );

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

        ctx.touch_up_dirs(
            dir.path(),
            &mut crate::writer::ServerWriter::new_plain(Vec::new()),
        );

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

        ctx.touch_up_dirs(
            dir.path(),
            &mut crate::writer::ServerWriter::new_plain(Vec::new()),
        );

        let actual = FileTime::from_last_modification_time(&fs::metadata(&file_path).unwrap());
        assert_eq!(
            actual, past,
            "touch_up_dirs must not modify non-directory entries"
        );
    }

    #[cfg(unix)]
    fn config_with_keep_dirlinks(keep: bool) -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-r".to_owned(),
            flags: ParsedServerFlags {
                keep_dirlinks: keep,
                recursive: true,
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            ..Default::default()
        }
    }

    /// Without `--keep-dirlinks`, a destination symlink standing where the
    /// source has a directory is a type conflict: upstream deletes it and
    /// creates a real directory (`generator.c:1451-1455`). The classifier must
    /// remove the symlink and report that a mkdir is needed.
    #[cfg(unix)]
    #[test]
    fn keep_dirlinks_off_replaces_dest_symlink_to_dir() {
        use std::os::unix::fs::symlink;

        let dir = test_support::create_tempdir();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = dir.path().join("d");
        symlink(&target, &link).unwrap();

        let hs = handshake();
        let ctx = ReceiverContext::new_for_test(&hs, config_with_keep_dirlinks(false));

        let decision = ctx
            .classify_dir_destination(
                &link,
                #[cfg(unix)]
                None,
                dir.path(),
                std::path::Path::new("d"),
            )
            .expect("classify succeeds");
        assert_eq!(
            decision,
            super::DirDestination::ReplacedSymlink,
            "without -K the conflicting dest symlink must be replaced"
        );
        assert!(decision.needs_mkdir(), "a real directory must be created");
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "the conflicting symlink must have been removed"
        );
    }

    /// With `--keep-dirlinks`, a destination symlink resolving to a directory is
    /// followed rather than replaced (`generator.c:1356`): the classifier keeps
    /// the symlink in place and reports no mkdir is needed.
    #[cfg(unix)]
    #[test]
    fn keep_dirlinks_on_follows_dest_symlink_to_dir() {
        use std::os::unix::fs::symlink;

        let dir = test_support::create_tempdir();
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = dir.path().join("d");
        symlink(&target, &link).unwrap();

        let hs = handshake();
        let ctx = ReceiverContext::new_for_test(&hs, config_with_keep_dirlinks(true));

        let decision = ctx
            .classify_dir_destination(
                &link,
                #[cfg(unix)]
                None,
                dir.path(),
                std::path::Path::new("d"),
            )
            .expect("classify succeeds");
        assert_eq!(
            decision,
            super::DirDestination::Existing,
            "with -K a dest symlink-to-directory is followed, not replaced"
        );
        assert!(
            !decision.needs_mkdir(),
            "no mkdir when following the symlink"
        );
        let md = fs::symlink_metadata(&link).unwrap();
        assert!(
            md.file_type().is_symlink(),
            "the dest symlink must be preserved under -K"
        );
    }

    /// A destination symlink that resolves to a non-directory (a file) is a
    /// type conflict even under `--keep-dirlinks`: `keep_dirlinks` follows only
    /// symlinks-to-directories, so the symlink is replaced.
    #[cfg(unix)]
    #[test]
    fn keep_dirlinks_on_replaces_symlink_to_non_dir() {
        use std::os::unix::fs::symlink;

        let dir = test_support::create_tempdir();
        let target = dir.path().join("target.txt");
        fs::write(&target, b"file").unwrap();
        let link = dir.path().join("d");
        symlink(&target, &link).unwrap();

        let hs = handshake();
        let ctx = ReceiverContext::new_for_test(&hs, config_with_keep_dirlinks(true));

        let decision = ctx
            .classify_dir_destination(
                &link,
                #[cfg(unix)]
                None,
                dir.path(),
                std::path::Path::new("d"),
            )
            .expect("classify succeeds");
        assert_eq!(
            decision,
            super::DirDestination::ReplacedSymlink,
            "-K follows only symlinks-to-directories; a symlink-to-file is replaced"
        );
        assert!(
            fs::symlink_metadata(&link).is_err(),
            "the symlink-to-file conflict must have been removed"
        );
    }
}
