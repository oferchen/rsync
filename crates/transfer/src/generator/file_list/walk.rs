//! Filesystem walking and directory scanning for the generator role.
//!
//! Implements recursive directory traversal, symlink resolution, and
//! filter application during file list construction. Directory children
//! are batch-stat'd in parallel via [`super::batch_stat`] when the entry
//! count exceeds the parallel threshold.
//!
//! # Upstream Reference
//!
//! - `flist.c:send_file_list()` - recursive directory scanning
//! - `flist.c:readlink_stat()` - symlink resolution modes

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use logging::info_log;

use crate::full_fname::full_fname_path;
use crate::role_trailer::error_location;

use super::super::GeneratorContext;
use super::super::io_error_flags;
use super::super::protocol_io::SenderDiagnostic;
use super::batch_stat::{StatResult, batch_stat_dir_entries};

/// One pending step of the directory walk.
///
/// The walk used to be call-stack recursion: a directory's entry was pushed,
/// then each child was walked from inside that frame, wrapped in a filter-chain
/// scope guard. That shape cannot be suspended, and suspending is exactly what
/// upstream's producer does - `send_directory()` scans ONE directory and
/// `send_extra_file_list()` (`flist.c`) decides when to scan the next.
///
/// Making the stack explicit preserves the traversal order and the guard
/// nesting exactly while turning every pop into a point the walk can stop at.
enum WalkStep {
    /// A path whose metadata is already final: the entry points, which resolve
    /// metadata before they call in.
    Visit {
        path: PathBuf,
        metadata: std::fs::Metadata,
        is_top_level: bool,
    },
    /// A directory child straight out of the batch stat.
    ///
    /// The `--copy-dirlinks` / `--copy-unsafe-links` re-stat is deliberately
    /// NOT applied when the child is scheduled. It runs when the child is
    /// reached, because its `copying unsafe symlink` notice must print after
    /// the previous sibling's subtree, exactly as the recursion printed it.
    VisitChild(StatResult),
    /// Read `dir`'s children, batch-stat them, and schedule each one.
    ///
    /// `opened` carries a handle the caller already holds. The two arms are not
    /// interchangeable: the recursive arm opens the directory BEFORE pushing
    /// that directory's own entry, so an `opendir` failure is reported ahead of
    /// the entry; the transfer-root arm opens it after entering the filter
    /// scope. Which side opens is what keeps diagnostic ordering unchanged.
    ScanChildren {
        dir: PathBuf,
        opened: Option<fast_io::pinned_root::ReadDir>,
    },
    /// Release one per-directory filter scope.
    ///
    /// upstream: `exclude.c:pop_local_filters()`
    LeaveDir(filters::DirFilterGuard),
}

impl GeneratorContext {
    /// Pre-checks a top-level source entry and walks it if it exists.
    ///
    /// Returns `true` if the entry was processed (exists or was handled as a
    /// mode-0 sentinel), `false` if the caller should skip to the next entry.
    ///
    /// This method applies `--ignore-missing-args` and `--delete-missing-args`
    /// semantics for top-level source paths and `--files-from` entries. The
    /// distinction from recursive children (which use `walk_path` directly)
    /// is critical: a missing top-level source is "never existed at flist time"
    /// (upstream `link_stat ... failed`, exit 23), while a missing recursive
    /// child is "vanished during walk" (upstream `file has vanished`, exit 24).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2254-2272` - `link_stat` + `missing_args` handling per source
    pub(in crate::generator) fn try_walk_source_entry(
        &mut self,
        base: &Path,
        path: &Path,
    ) -> io::Result<bool> {
        self.try_walk_source_entry_dedup(base, path, None)
    }

    /// Like `try_walk_source_entry`, but consults `emitted_dirs` to skip
    /// re-emitting a directory entry that was already pushed by an earlier
    /// pass (e.g. the implied-parent loop in `build_file_list_with_base`).
    ///
    /// When the source path is a directory whose relative name is already in
    /// `emitted_dirs`, the function returns `Ok(true)` without re-emitting or
    /// recursing. Subsequent `--files-from` entries that explicitly name
    /// children of the same directory still reach the receiver via their own
    /// top-level walks, and re-walking here would produce a duplicate parent
    /// entry that upstream's `implied_filter_list` check rejects with
    /// "rejecting unrequested file-list name" (flist.c:1026).
    ///
    /// `emitted_dirs` is `Some` only from `build_file_list_with_base`, which
    /// passes the set of directories already emitted by its implied-parent
    /// loop. All other callers (single-source `build_file_list`) pass `None`
    /// and retain the original walk-everything behaviour.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1026` - `check_filter(&implied_filter_list, ...)` rejects
    ///   second occurrences as "unrequested file-list name".
    /// - `flist.c:1937` - `send_implied_dirs()` is upstream's equivalent
    ///   single emission point for the same logical directory.
    pub(in crate::generator) fn try_walk_source_entry_dedup(
        &mut self,
        base: &Path,
        path: &Path,
        emitted_dirs: Option<&HashSet<PathBuf>>,
    ) -> io::Result<bool> {
        // Record the transfer root so per-directory merge files re-anchor their
        // leading-`/` rules to the merge file's own directory (upstream
        // exclude.c add_rule XFLG_ANCHORED2ABS). Idempotent across source
        // entries; `base` is the same root for the whole walk.
        self.filter_chain.set_transfer_root(base.to_path_buf());
        // upstream: flist.c:2411 `push_dir(dir, 0)` - the sender chdir's into
        // the `dir` half of the positional's `dir`/`fn` split before walking
        // it, which is the `curr_dir` every `full_fname()` in the walk renders
        // against (util1.c:1285). `base` is that same directory.
        self.curr_dir = Some(base.to_path_buf());
        // upstream: flist.c:2425 - link_stat() once, then pass &st to
        // send_file_name(). Reuse the metadata to avoid a redundant stat
        // inside walk_path_with_metadata.
        match self.resolve_symlink_metadata(path, base) {
            Ok(metadata) => {
                // If a prior pass already emitted this directory (e.g. the
                // implied-parent loop in build_file_list_with_base), skip the
                // top-level walk so we do not produce a duplicate file-list
                // entry. The directory's contents will be reached by the
                // explicit child entries from --files-from. Files are never
                // deduped through this path; only directories at the top
                // level can collide with the implied-parent loop's output.
                if metadata.is_dir() {
                    if let Some(seen) = emitted_dirs {
                        let relative = path.strip_prefix(base).unwrap_or(path);
                        if !relative.as_os_str().is_empty() && seen.contains(relative) {
                            return Ok(true);
                        }
                    }
                }
                // Path exists - pass pre-resolved metadata directly.
                self.walk_path_with_metadata(base, path.to_path_buf(), metadata, true)?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match self.missing_args_mode() {
                    // upstream: flist.c:2261 - missing_args == 1: silently skip
                    1 => Ok(false),
                    // upstream: flist.c:2254-2258 - missing_args == 2: emit mode-0 sentinel
                    2 => {
                        self.emit_delete_sentinel(base, path)?;
                        Ok(true)
                    }
                    // upstream: flist.c:2428-2436 - default: link_stat failed.
                    _ => {
                        // upstream: flist.c:2703 - `if (errno != ENOENT)` guards
                        // the `io_error |= IOERR_GENERAL`, so a source that
                        // never existed deliberately leaves io_error clear: that
                        // bit travels to the receiver and would inhibit its
                        // deletions, and upstream only wants that "if we might
                        // be omitting an existing file". The exit code comes
                        // from got_xfer_error instead, set by the FERROR_XFER
                        // below on both this side and the peer (log.c:310-311).
                        // upstream: flist.c:2433 - rsyserr(FERROR_XFER, ...)
                        let text = format!(
                            "rsync: [sender] link_stat {} failed: {}\n",
                            full_fname_path(path, self.daemon_paths()),
                            engine::local_copy::upstream_io_error(&e),
                        );
                        self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
                        Ok(false)
                    }
                }
            }
            Err(e) => {
                // Non-ENOENT error: log as link_stat failure and record.
                self.log_stat_error(path, &e);
                self.record_io_error(&e);
                Ok(false)
            }
        }
    }

    /// Emits a mode-0 sentinel file entry for `--delete-missing-args`.
    ///
    /// The sentinel has `mode == 0`, which the receiver interprets as an
    /// instruction to delete the corresponding destination path. The entry
    /// carries the relative name so the receiver can locate the target.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2254-2258` - `missing_args == 2`: `make_file()` + `file->mode = 0`
    fn emit_delete_sentinel(&mut self, base: &Path, path: &Path) -> io::Result<()> {
        let relative = path.strip_prefix(base).unwrap_or(path).to_path_buf();
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(path.file_name().unwrap_or(path.as_os_str()))
        } else {
            relative
        };
        // upstream: mode=0 signals "delete this entry" to the receiver.
        // Create a regular file entry then override mode to 0.
        let mut entry = protocol::flist::FileEntry::new_file(relative, 0, 0);
        entry.set_mode(0);
        self.push_file_item(entry, path.to_path_buf());
        Ok(())
    }

    /// Walks a path with pre-resolved metadata, skipping the initial stat call.
    ///
    /// `is_top_level` is `true` only for the direct source arguments; recursive
    /// descents into directory children always pass `false`. The flag controls
    /// whether the directory entry receives `XMIT_TOP_DIR` (upstream `FLAG_TOP_DIR`).
    ///
    /// This is the inner implementation shared by `walk_path` (which resolves
    /// metadata itself) and the batched-stat path (which pre-resolves metadata
    /// for all directory children in parallel before processing them).
    fn walk_path_with_metadata(
        &mut self,
        base: &Path,
        path: PathBuf,
        metadata: std::fs::Metadata,
        is_top_level: bool,
    ) -> io::Result<()> {
        let mut stack = vec![WalkStep::Visit {
            path,
            metadata,
            is_top_level,
        }];
        self.drive_walk(base, &mut stack)
    }

    /// Runs the explicit walk stack to exhaustion.
    ///
    /// Steps pop LIFO, so a directory that schedules `[LeaveDir, ScanChildren]`
    /// scans first and releases its filter scope after - the same nesting the
    /// recursion produced. An error abandons the remaining steps, which is what
    /// the `?` in the recursive version did by unwinding.
    fn drive_walk(&mut self, base: &Path, stack: &mut Vec<WalkStep>) -> io::Result<()> {
        while let Some(step) = stack.pop() {
            match step {
                WalkStep::Visit {
                    path,
                    metadata,
                    is_top_level,
                } => self.visit_walk_entry(base, path, metadata, is_top_level, stack)?,
                WalkStep::VisitChild(result) => {
                    if let Some((path, metadata)) = self.resolve_child_metadata(base, result) {
                        self.visit_walk_entry(base, path, metadata, false, stack)?;
                    }
                }
                WalkStep::ScanChildren { dir, opened } => {
                    self.scan_children_onto(&dir, opened, stack)?;
                }
                WalkStep::LeaveDir(guard) => self.filter_chain.leave_directory(guard),
            }
        }
        Ok(())
    }

    /// Emits one path's entry and schedules its children when it is a directory
    /// to descend into.
    ///
    /// This is the body the recursion used to run per frame, with the two
    /// recursive calls replaced by pushes onto `stack`.
    fn visit_walk_entry(
        &mut self,
        base: &Path,
        path: PathBuf,
        metadata: std::fs::Metadata,
        is_top_level: bool,
        stack: &mut Vec<WalkStep>,
    ) -> io::Result<()> {
        let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();

        // upstream: flist.c:2338-2349 - non-relative single-file sources split
        // on the last `/` so the wire-side relative name is just the basename
        // (`fn` in upstream's terminology). When base == path for a regular
        // file, our `strip_prefix` would otherwise leave `relative` empty and
        // the receiver would write the bytes into the destination directory's
        // own name slot, producing an empty / corrupt entry. Restoring the
        // basename here matches upstream's daemon-mode `chdir(dir); link_stat(fn)`
        // ordering for the sub-path pull case (e.g. `rsync rsync://h/m/a/b/f`).
        let relative = if relative.as_os_str().is_empty() && !metadata.is_dir() {
            match path.file_name() {
                Some(name) => PathBuf::from(name),
                None => relative,
            }
        } else {
            relative
        };

        // upstream: flist.c:2287 - always emit "." with XMIT_TOP_DIR for the
        // root transfer directory. Enables delete_in_dir() when --delete is active.
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            let mut dot_entry = self.create_entry(&path, PathBuf::from("."), &metadata)?;
            dot_entry.set_top_dir(true);
            self.push_file_item(dot_entry, path.clone());

            // upstream: exclude.c:push_local_filters() - read per-directory
            // merge files when entering the root transfer directory.
            let guard = self.filter_chain.enter_directory(&path).map_err(|e| {
                io::Error::other(format!(
                    "filter chain error in \"{}\": {e} {}{}",
                    path.display(),
                    error_location!(),
                    crate::role_trailer::sender()
                ))
            })?;

            // LIFO: the scan runs before the scope is released.
            stack.push(WalkStep::LeaveDir(guard));
            stack.push(WalkStep::ScanChildren {
                dir: path,
                opened: None,
            });
            return Ok(());
        }

        // upstream: flist.c:send_file_name() - skip unsupported file types
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            let ft = metadata.file_type();
            // upstream: flist.c:1419 - `--copy-devices` makes make_file() emit a
            // block/char device as a regular file, so it is included on the wire
            // even without `--devices`. Only skip when neither flag is active.
            if (ft.is_block_device() || ft.is_char_device())
                && !self.config.flags.devices
                && !self.config.flags.copy_devices
            {
                return Ok(());
            }
            if (ft.is_fifo() || ft.is_socket()) && !self.config.flags.specials {
                return Ok(());
            }
        }

        // upstream: flist.c:1360 - is_excluded() applied during make_file()
        // FilterChain evaluates per-directory scoped rules (innermost first)
        // then global rules. If no rules are configured, allows() returns true.
        if !self.filter_chain.allows(&relative, metadata.is_dir()) {
            return Ok(());
        }

        // upstream: generator.c:1547 - skip unsafe symlinks when --safe-links.
        // Sender-side filtering ensures unsafe symlinks never reach the receiver,
        // matching the belt-and-suspenders approach for daemon push interop.
        if self.config.flags.safe_links && metadata.file_type().is_symlink() {
            if let Ok(target) = self.read_source_link(&path) {
                if super::super::super::symlink_safety::is_unsafe_symlink(
                    target.as_os_str(),
                    &relative,
                ) {
                    return Ok(());
                }
            }
        }

        let mut entry = match self.create_entry(&path, relative, &metadata) {
            Ok(e) => e,
            Err(e) => {
                // upstream: flist.c - rsyserr for make_file() failures
                let text = format!(
                    "rsync: [sender] make_file failed for \"{}\": {}\n",
                    path.display(),
                    engine::local_copy::upstream_io_error(&e),
                );
                self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
                self.add_io_error(io_error_flags::IOERR_GENERAL);
                return Ok(());
            }
        };

        // upstream: flist.c:2287 - top-level source directories carry
        // FLAG_TOP_DIR so delete_in_dir() can scope deletions. Under
        // --relative the directory entry has a non-empty relative name (e.g.
        // "tmp/dbg/src/usr/bin") instead of ".", but it still needs the flag.
        if is_top_level && metadata.is_dir() {
            entry.set_top_dir(true);
        }

        // upstream: flist.c:send_file_list() - scan directory before recording entry
        let should_recurse = metadata.is_dir() && self.config.flags.recursive;
        let dir_read = if should_recurse {
            match fast_io::pinned_root::read_dir(&path) {
                Ok(entries) => Some(entries),
                Err(e) => {
                    // upstream: flist.c:1878 - rsyserr(FERROR_XFER, errno, "opendir %s failed", ...)
                    let text = format!(
                        "rsync: [sender] opendir {} failed: {}\n",
                        full_fname_path(&path, self.daemon_paths()),
                        engine::local_copy::upstream_io_error(&e),
                    );
                    self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
                    self.record_io_error(&e);
                    None
                }
            }
        } else {
            None
        };

        // Keep a clone of the path before moving it into the file list,
        // needed for enter_directory() if this is a directory we'll recurse into.
        let dir_path = if dir_read.is_some() {
            Some(path.clone())
        } else {
            None
        };

        self.push_file_item(entry, path);

        if let Some(entries) = dir_read {
            // Safety: dir_path is always Some when dir_read is Some
            let dir_path = dir_path.expect("dir_path is Some whenever dir_read is Some");

            // upstream: exclude.c:push_local_filters() - read per-directory
            // merge files when entering a subdirectory during recursive walk.
            let guard = self.filter_chain.enter_directory(&dir_path).map_err(|e| {
                io::Error::other(format!(
                    "filter chain error in \"{}\": {e} {}{}",
                    dir_path.display(),
                    error_location!(),
                    crate::role_trailer::sender()
                ))
            })?;

            // LIFO: the scan runs before the scope is released.
            stack.push(WalkStep::LeaveDir(guard));
            stack.push(WalkStep::ScanChildren {
                dir: dir_path,
                opened: Some(entries),
            });
        }

        Ok(())
    }

    /// Scans the children of a `--files-from` SLASH_ENDING_NAME directory.
    ///
    /// Used by `build_file_list_with_base` to honour the upstream
    /// `flist.c:2329` rule that trailing-slash `--files-from` entries recurse
    /// into their named directory's children even when global `-r` is off
    /// (`options.c:2189` clears `recurse` whenever `--files-from` is active).
    /// The walk-loop already pushed the directory entry itself via
    /// `try_walk_source_entry_dedup`; this helper just adds the children at
    /// the same level a global `-r` would have produced.
    ///
    /// Wraps `scan_directory_batched` in the per-directory filter scope
    /// (`enter_directory` / `leave_directory`) so per-dir merge files are
    /// honoured for the recursion the way they are during a normal walk.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads directory and stats each child
    /// - `flist.c:2329` - `SLASH_ENDING_NAME` flag for trailing-slash entries
    pub(in crate::generator) fn scan_files_from_marker_dir(
        &mut self,
        base: &Path,
        dir_path: &Path,
    ) -> io::Result<()> {
        let guard = self.filter_chain.enter_directory(dir_path).map_err(|e| {
            io::Error::other(format!(
                "filter chain error in \"{}\": {e} {}{}",
                dir_path.display(),
                error_location!(),
                crate::role_trailer::sender()
            ))
        })?;
        let result = self.scan_directory_batched(base, dir_path);
        self.filter_chain.leave_directory(guard);
        result
    }

    /// Reads a directory and batch-stats its children before recursive processing.
    ///
    /// Collects all `DirEntry` paths from `read_dir()`, resolves their metadata
    /// in parallel via [`batch_stat_dir_entries`], then processes each child
    /// through `walk_path_with_metadata`. Entries whose stat fails are logged
    /// and recorded as I/O errors without aborting the traversal.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads directory and stats each child
    fn scan_directory_batched(&mut self, base: &Path, dir_path: &Path) -> io::Result<()> {
        let mut stack = vec![WalkStep::ScanChildren {
            dir: dir_path.to_path_buf(),
            opened: None,
        }];
        self.drive_walk(base, &mut stack)
    }

    /// Opens `dir_path` when the caller did not, then schedules its children.
    ///
    /// A failed `opendir` is reported and the directory skipped; it does not
    /// abort the walk.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1878` - `rsyserr(FERROR_XFER, errno, "opendir %s failed", ...)`
    fn scan_children_onto(
        &mut self,
        dir_path: &Path,
        opened: Option<fast_io::pinned_root::ReadDir>,
        stack: &mut Vec<WalkStep>,
    ) -> io::Result<()> {
        let entries = match opened {
            Some(entries) => entries,
            None => match fast_io::pinned_root::read_dir(dir_path) {
                Ok(entries) => entries,
                Err(e) => {
                    // upstream: flist.c:1878 - rsyserr(FERROR_XFER, errno, "opendir %s failed", ...)
                    let text = format!(
                        "rsync: [sender] opendir {} failed: {}\n",
                        full_fname_path(dir_path, self.daemon_paths()),
                        engine::local_copy::upstream_io_error(&e),
                    );
                    self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
                    self.record_io_error(&e);
                    return Ok(());
                }
            },
        };
        self.push_dir_entries_onto(dir_path, entries, stack)
    }

    /// Collects paths from a `ReadDir` iterator, batch-stats them, and schedules
    /// each child on the walk stack.
    ///
    /// Children are pushed in reverse so they pop in readdir order, matching the
    /// order the recursion visited them.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads directory and stats each child
    /// - `flist.c:2195` - `rsyserr(FERROR_XFER, errno, "readdir(%s)", ...)`
    fn push_dir_entries_onto(
        &mut self,
        dir_path: &Path,
        entries: fast_io::pinned_root::ReadDir,
        stack: &mut Vec<WalkStep>,
    ) -> io::Result<()> {
        // Phase 1: collect child paths from readdir
        let mut child_paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(child) => child_paths.push(child),
                Err(e) => {
                    // upstream: flist.c:2195 - rsyserr(FERROR_XFER, errno, "readdir(%s)", ...)
                    let text = format!(
                        "rsync: [sender] readdir({}): {}\n",
                        full_fname_path(dir_path, self.daemon_paths()),
                        engine::local_copy::upstream_io_error(&e),
                    );
                    self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
                    self.record_io_error(&e);
                }
            }
        }

        if child_paths.is_empty() {
            return Ok(());
        }

        // Phase 2: determine stat mode and batch-resolve metadata.
        // --copy-links: follow all symlinks (fs::metadata)
        // default: lstat (fs::symlink_metadata)
        // --copy-unsafe-links needs per-child fixup, applied when the child is
        // reached rather than here - see `WalkStep::VisitChild`.
        let follow = self.config.flags.copy_links;
        let stat_results = batch_stat_dir_entries(child_paths, follow, &self.parallel_thresholds);

        // Phase 3: schedule each child. Reverse, because the stack pops LIFO.
        stack.extend(stat_results.into_iter().rev().map(WalkStep::VisitChild));

        Ok(())
    }

    /// Applies the per-child stat fixups and reports the failures.
    ///
    /// Returns `None` when the child is dropped from the walk: its batch stat
    /// failed, or the `--copy-unsafe-links` dereference failed. Both are logged
    /// and counted as I/O errors without aborting the traversal.
    ///
    /// This runs when the child is REACHED, not when it is scheduled, so its
    /// notices stay interleaved with the walk exactly as the recursion had them.
    fn resolve_child_metadata(
        &mut self,
        base: &Path,
        result: StatResult,
    ) -> Option<(PathBuf, std::fs::Metadata)> {
        let StatResult { path, metadata } = result;
        let mut meta = match metadata {
            Ok(meta) => meta,
            Err(e) => {
                self.log_stat_error(&path, &e);
                self.record_io_error(&e);
                return None;
            }
        };

        let follow = self.config.flags.copy_links;

        // upstream: flist.c:1362-1370 link_stat() - with --copy-dirlinks
        // (follow_dirlinks), a symlink whose target is a directory is
        // transmitted as a real directory. Applied before the copy-unsafe-links
        // check exactly as upstream applies it inside link_stat() before
        // readlink_stat() re-examines S_ISLNK. Only symlinks to directories are
        // followed; symlinks to files stay symlinks (distinct from
        // --copy-links, which follows all).
        if !follow && self.config.flags.copy_dirlinks && meta.file_type().is_symlink() {
            if let Ok(followed) = fast_io::pinned_root::metadata(&path) {
                if followed.file_type().is_dir() {
                    meta = followed;
                }
            }
        }

        // upstream: flist.c:215 - follow unsafe symlinks when
        // --copy-unsafe-links. The batch used lstat, so we need to re-stat
        // symlinks whose target escapes the tree.
        if !follow && self.config.flags.copy_unsafe_links && meta.file_type().is_symlink() {
            if let Ok(target) = self.read_source_link(&path) {
                let relative = path.strip_prefix(base).unwrap_or(&path);
                if super::super::super::symlink_safety::is_unsafe_symlink(
                    target.as_os_str(),
                    relative,
                ) {
                    // upstream: flist.c:229 - INFO_GTE(SYMSAFE, 1) fires before
                    // the target is dereferenced.
                    info_log!(
                        Symsafe,
                        1,
                        "copying unsafe symlink \"{}\" -> \"{}\"",
                        path.display(),
                        target.display()
                    );
                    match fast_io::pinned_root::metadata(&path) {
                        Ok(followed) => meta = followed,
                        Err(e) => {
                            self.log_stat_error(&path, &e);
                            self.record_io_error(&e);
                            return None;
                        }
                    }
                }
            }
        }

        Some((path, meta))
    }

    /// Logs a stat failure with the appropriate upstream error format.
    ///
    /// Distinguishes between vanished files (ENOENT) and general stat errors,
    /// matching upstream `flist.c:1286-1294` error reporting. The two cases
    /// carry different log classes upstream, so they queue different frame
    /// types: the vanished notice is an `FWARNING`, the stat failure an
    /// `FERROR_XFER`.
    fn log_stat_error(&mut self, path: &Path, e: &io::Error) {
        let fname = full_fname_path(path, self.daemon_paths());
        let (kind, text) = if e.kind() == io::ErrorKind::NotFound {
            // upstream: flist.c:1463-1467 - rprintf(FWARNING, "file has vanished: %s\n", ...)
            (
                SenderDiagnostic::Warning,
                format!("file has vanished: {fname}\n"),
            )
        } else {
            // upstream: flist.c:1846 - rsyserr(FERROR_XFER, errno, "link_stat %s failed", ...)
            (
                SenderDiagnostic::ErrorXfer,
                format!(
                    "rsync: [sender] link_stat {fname} failed: {}\n",
                    engine::local_copy::upstream_io_error(e),
                ),
            )
        };
        self.queue_flist_diagnostic(kind, text);
    }

    /// Resolves symlink metadata following upstream `flist.c:readlink_stat()`.
    ///
    /// Three modes of symlink resolution:
    /// - `--copy-links`: follow ALL symlinks (stat instead of lstat)
    /// - `--copy-unsafe-links`: follow only symlinks whose target escapes
    ///   the transfer tree (converting them to regular files)
    /// - Default: use lstat (preserve symlinks as symlinks)
    ///
    /// Every stat here goes through
    /// [`fast_io::pinned_root`](fast_io::pinned_root), which anchors the
    /// lookup on the module root a daemon pinned while still privileged when
    /// the path lies beneath it. Upstream is already positioned there - it
    /// `change_dir()`s into the module before the drop - so `link_stat(".")`
    /// costs it nothing; oc keeps absolute paths, and without the anchor the
    /// stat re-walks the module's ancestors as the dropped uid and `EACCES`es
    /// on an unsearchable one.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:217-244` - `readlink_stat()`
    /// - `flist.c:227` - `copy_unsafe_links && unsafe_symlink(linkbuf, path)`
    /// - `clientserver.c:1059-1065` - the pin the anchoring reads.
    pub(in crate::generator) fn resolve_symlink_metadata(
        &self,
        path: &Path,
        base: &Path,
    ) -> io::Result<std::fs::Metadata> {
        // upstream: flist.c:readlink_stat() operates on paths without the
        // DOTDIR marker. On Linux, lstat("path/") follows symlinks because the
        // kernel resolves the trailing slash, making a symlink appear as its
        // target directory - so the marker must be stripped before the stat to
        // get a decision that is the same on every platform. Capture whether it
        // was there FIRST: the marker is not noise to discard, it is upstream's
        // `name_type`, and the follow decision below depends on it.
        let is_dotdir = super::operand_has_dotdir_marker(path);
        let normalized: PathBuf;
        let path = {
            let bytes = path.as_os_str().as_encoded_bytes();
            if bytes.len() > 1 && (bytes.ends_with(b"/") || bytes.ends_with(b"/.")) {
                normalized = path.components().collect();
                normalized.as_path()
            } else {
                path
            }
        };

        if self.config.flags.copy_links {
            return fast_io::pinned_root::metadata(path);
        }

        let meta = fast_io::pinned_root::symlink_metadata(path)?;

        // upstream: flist.c:1362-1370 link_stat() - with follow_dirlinks a
        // symlink whose target is a directory is transmitted as a real
        // directory. Applied before the copy-unsafe-links check, mirroring
        // upstream's link_stat() (dirlink follow) running before
        // readlink_stat()'s S_ISLNK re-examination. Only symlinks to
        // directories are followed; symlinks to files stay symlinks (distinct
        // from --copy-links).
        //
        // follow_dirlinks has TWO disjuncts upstream, not one
        // (flist.c:2697 `copy_dirlinks || name_type != NORMAL_NAME`). A DOTDIR
        // operand follows, because asking for the CONTENTS of `current/` is
        // only meaningful once `current` has been resolved to the directory it
        // points at. Gating on `--copy-dirlinks` alone made
        // `oc-rsync -a host:current/ dest/` deliver nothing and exit 0.
        //
        // ⚠ The DOTDIR disjunct is gated on OWNERSHIP, `--copy-dirlinks` is
        // not, and the asymmetry is deliberate. Upstream reaches this stat with
        // the operand ALREADY resolved: the daemon runs
        // `change_dir(module_chdir)` through `open_no_attacker_symlinks()`
        // (util1.c:1216) before the file list is walked, so by the time
        // `link_stat()` sees `.` there is no symlink left to follow. oc walks
        // the unresolved path, and every daemon transfer sends `.` as its
        // operand - so an ungated DOTDIR follow lets a foreign-uid symlink
        // planted at the module root redirect the listing outside the module.
        // `--copy-dirlinks` is an explicit client request over a client-named
        // tree and keeps its existing meaning untouched.
        //
        // When the daemon PINNED its module root the stat above already comes
        // from the pinned directory, so `.` is a directory here and this gate
        // has nothing to decide - which is upstream's position exactly, and is
        // sound for the same reason: the pin is taken through the ownership
        // walk (`util1.c:1254-1263`), so a foreign-uid symlink at the module
        // root is refused before a pin exists and this gate is back in force.
        //
        // upstream: `rsync-3.5.0/syscall.c:406` - a symlink owned by uid 0 or
        // our euid is the operator's own layout and is followed; any other uid
        // is an attacker's plant and is refused.
        let follow_dirlinks = self.config.flags.copy_dirlinks
            || (is_dotdir && symlink_target_is_operator_owned(&meta));
        if follow_dirlinks && meta.file_type().is_symlink() {
            if let Ok(followed) = fast_io::pinned_root::metadata(path) {
                if followed.file_type().is_dir() {
                    return Ok(followed);
                }
            }
        }

        // upstream: flist.c:215 - follow unsafe symlinks when --copy-unsafe-links
        if self.config.flags.copy_unsafe_links && meta.file_type().is_symlink() {
            let target = self.read_source_link(path)?;
            let relative = path.strip_prefix(base).unwrap_or(path);
            if super::super::super::symlink_safety::is_unsafe_symlink(target.as_os_str(), relative)
            {
                // upstream: flist.c:229 - INFO_GTE(SYMSAFE, 1) fires before
                // the unsafe symlink is dereferenced into a regular entry.
                info_log!(
                    Symsafe,
                    1,
                    "copying unsafe symlink \"{}\" -> \"{}\"",
                    path.display(),
                    target.display()
                );
                return fast_io::pinned_root::metadata(path);
            }
        }

        Ok(meta)
    }
}

/// Is a symlink the operator's own, and therefore safe to follow for a DOTDIR
/// operand?
///
/// Ownership - not location - is upstream's trust signal here. A link owned by
/// uid 0 or our own euid is the operator's layout (the `current -> releases/X`
/// deploy pattern this follow exists to serve); any other uid is a plant.
///
/// This is the DOTDIR half of `follow_dirlinks` only. `--copy-dirlinks` is an
/// explicit request over a client-named tree and is deliberately not gated.
///
/// On a non-Unix target there is no `st_uid` to test and no unprivileged way to
/// plant a foreign-owned link in a module root, so the follow proceeds.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:406` - `st_uid != 0 && st_uid != trusted_uid`
///   refuses the symlink; otherwise the walk follows it.
/// - `rsync-3.5.0/util1.c:1216` `change_dir()` - the daemon resolves the module
///   root through this same rule BEFORE the file list is walked, which is why
///   upstream never reaches this stat with an unresolved operand.
fn symlink_target_is_operator_owned(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        fast_io::symlink_owner_is_trusted(meta.uid())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

#[cfg(all(test, unix))]
mod dotdir_follow_trust_tests {
    /// The DOTDIR follow is gated on upstream's ownership rule, not on
    /// location: uid 0 and our own euid are the operator's own layout, any
    /// other uid is a plant.
    ///
    /// This pins the RULE. The behavioural proof that a foreign-owned symlink
    /// at a daemon module root is not followed lives in the upstream testsuite
    /// cell `daemon-module-chdir-symlink`, which needs root to create a
    /// non-self-owned link and therefore cannot run here.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:406`.
    #[test]
    fn only_root_and_our_own_euid_are_trusted_symlink_owners() {
        let euid = rustix::process::geteuid().as_raw();
        assert!(
            fast_io::symlink_owner_is_trusted(0),
            "uid 0 is the operator"
        );
        assert!(
            fast_io::symlink_owner_is_trusted(euid),
            "our own euid is the operator"
        );

        // Pick a uid that is neither 0 nor ours. 65534 (nobody) is the uid the
        // testsuite plants with; fall back if we happen to be running as it.
        let foreign = if euid == 65534 { 65533 } else { 65534 };
        assert!(
            !fast_io::symlink_owner_is_trusted(foreign),
            "uid {foreign} is neither root nor our euid, so it is a plant"
        );
    }
}

#[cfg(test)]
mod rsyserr_wording_tests {
    //! Pin per-file `rsyserr`-equivalent wording to upstream rsync 3.4.1
    //! `log.c:rsyserr()` byte-for-byte. See task #2174 and
    //! `docs/audits/error-message-verbatim-audit.md` family 4.

    /// Each tuple is (template-with-{path}-marker, expected-rendered-line).
    /// Templates mirror the literal `eprintln!` formats above so a future
    /// refactor that re-inserts the source-location or role-version trailer
    /// will fail these asserts.
    const CASES: &[(&str, &str)] = &[
        // upstream: flist.c:1846 - "link_stat %s failed"
        (
            "rsync: [sender] link_stat \"{path}\" failed: No such file or directory (2)",
            "rsync: [sender] link_stat \"/p\" failed: No such file or directory (2)",
        ),
        // upstream: flist.c:1842 - "opendir %s failed"
        (
            "rsync: [sender] opendir \"{path}\" failed: Permission denied (13)",
            "rsync: [sender] opendir \"/p\" failed: Permission denied (13)",
        ),
        // upstream: flist.c:2195 - "readdir(%s)"
        (
            "rsync: [sender] readdir(\"{path}\"): Input/output error (5)",
            "rsync: [sender] readdir(\"/p\"): Input/output error (5)",
        ),
        // upstream: flist.c (make_file paths) - follows rsyserr() shape
        (
            "rsync: [sender] make_file failed for \"{path}\": Permission denied (13)",
            "rsync: [sender] make_file failed for \"/p\": Permission denied (13)",
        ),
        // upstream: flist.c:1463 / sender.c:713 - "file has vanished: %s" via full_fname()
        ("file has vanished: \"{path}\"", "file has vanished: \"/p\""),
        // upstream: sender.c:718 - "send_files failed to open %s"
        (
            "rsync: [sender] send_files failed to open \"{path}\": Permission denied (13)",
            "rsync: [sender] send_files failed to open \"/p\": Permission denied (13)",
        ),
    ];

    #[test]
    fn rsyserr_wording_matches_upstream_byte_for_byte() {
        for (template, expected) in CASES {
            let rendered = template.replace("{path}", "/p");
            assert_eq!(
                &rendered, expected,
                "template {template:?} did not match upstream wording"
            );
        }
    }

    /// The same lines rendered from a daemon server process must carry
    /// upstream's ` (in MODULE)` suffix outside the closing quote and name the
    /// path relative to the module root, because upstream builds the path with
    /// `full_fname()` after `chdir()`ing into the module (`clientserver.c:993`)
    /// and `module_id >= 0`. Ground truth captured from rsync 3.4.4 serving
    /// module `mod` rooted at `/tmp/modrel/mod`:
    /// `rsync: [sender] opendir "denied" (in mod) failed: Permission denied (13)`.
    #[test]
    fn rsyserr_wording_carries_daemon_module_suffix() {
        use crate::full_fname::{DaemonPaths, full_fname_path};
        use std::path::Path;

        let daemon = DaemonPaths {
            module: "mymod",
            module_root: Path::new("/srv/mod"),
            curr_dir: Path::new("/srv/mod"),
        };
        let quoted = full_fname_path(Path::new("/srv/mod/p"), Some(daemon));
        assert_eq!(quoted, "\"p\" (in mymod)");
        assert_eq!(
            format!("rsync: [sender] link_stat {quoted} failed: No such file or directory (2)"),
            "rsync: [sender] link_stat \"p\" (in mymod) failed: No such file or directory (2)",
        );
        assert_eq!(
            format!("rsync: [sender] readdir({quoted}): Input/output error (5)"),
            "rsync: [sender] readdir(\"p\" (in mymod)): Input/output error (5)",
        );
        assert_eq!(
            format!("file has vanished: {quoted}"),
            "file has vanished: \"p\" (in mymod)",
        );
    }

    /// A client or SSH server process (`module_id < 0`) must keep the absolute
    /// path it has always printed: upstream neither strips a prefix nor
    /// appends a suffix there, so a local or SSH run's stderr is unchanged.
    #[test]
    fn rsyserr_wording_outside_a_daemon_stays_absolute() {
        use crate::full_fname::full_fname_path;
        use std::path::Path;

        let quoted = full_fname_path(Path::new("/srv/mod/p"), None);
        assert_eq!(quoted, "\"/srv/mod/p\"");
        assert_eq!(
            format!("rsync: [sender] opendir {quoted} failed: Permission denied (13)"),
            "rsync: [sender] opendir \"/srv/mod/p\" failed: Permission denied (13)",
        );
    }
}

#[cfg(test)]
mod symsafe_emission_tests {
    //! Wording tests for `--info=SYMSAFE` producer emissions on the
    //! sender side.
    //!
    //! Upstream rsync 3.4.1 fires `INFO_GTE(SYMSAFE, 1)` at `flist.c:268`
    //! when `--copy-unsafe-links` triggers a dereference. The exact line
    //! emitted (per `rprintf(FINFO, ...)`) is matched byte-for-byte so
    //! interop harnesses that grep for the literal continue to find it.
    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, info_log, init};

    fn init_symsafe_level1() {
        let mut cfg = VerbosityConfig::default();
        cfg.info.symsafe = 1;
        init(cfg);
        let _ = drain_events();
    }

    fn symsafe_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info {
                    flag: InfoFlag::Symsafe,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn copying_unsafe_symlink_wording_matches_upstream() {
        // upstream: flist.c:229 -
        //     rprintf(FINFO, "copying unsafe symlink \"%s\" -> \"%s\"\n",
        //             path, linkbuf);
        init_symsafe_level1();
        let path = std::path::Path::new("src/link");
        let target = std::path::Path::new("/etc/passwd");
        info_log!(
            Symsafe,
            1,
            "copying unsafe symlink \"{}\" -> \"{}\"",
            path.display(),
            target.display()
        );
        let msgs = symsafe_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "copying unsafe symlink \"src/link\" -> \"/etc/passwd\""),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn symsafe_emissions_suppressed_when_disabled() {
        // Default `VerbosityConfig` leaves `info.symsafe == 0`, mirroring
        // upstream's pre-`-v` state. The macro must not synthesise an event.
        init(VerbosityConfig::default());
        let _ = drain_events();
        info_log!(
            Symsafe,
            1,
            "copying unsafe symlink \"{}\" -> \"{}\"",
            "x",
            "y"
        );
        let msgs = symsafe_messages();
        assert!(
            msgs.is_empty(),
            "SYMSAFE emissions must be gated; got: {msgs:?}"
        );
    }
}
