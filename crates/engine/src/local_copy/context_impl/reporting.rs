impl<'a> CopyContext<'a> {
    /// Records a skip event for a non-regular file (e.g. socket, unknown type).
    ///
    /// Emits an `--info=NONREG` notice mirroring upstream rsync 3.4.1
    /// (generator.c:1684) under `INFO_GTE(NONREG, 1)`. NONREG is part of
    /// `info_verbosity[0]` (options.c:240), so the flag is enabled by
    /// default and fires regardless of `-v`. The wording matches upstream
    /// byte-for-byte: `skipping non-regular file "%s"`.
    pub(super) fn record_skipped_non_regular(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            info_log!(Nonreg, 1, "skipping non-regular file \"{}\"", path.display());
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedNonRegular,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
    }

    /// Records a skip event for a symbolic link whose creation the platform
    /// refuses without privilege - a Windows file symlink created by an
    /// unprivileged user without Developer Mode. Emits a warning, records the
    /// skip, and sets the soft-error flag so the transfer still finishes but
    /// exits `RERR_PARTIAL` (23), mirroring upstream's `FERROR_XFER` handling of
    /// a failed `do_symlink()`.
    ///
    /// Only reachable on Windows, where `create_symlink` can hit
    /// `ERROR_PRIVILEGE_NOT_HELD` on a file link with no junction fallback.
    #[cfg(windows)]
    pub(super) fn record_skipped_unsupported_symlink(
        &mut self,
        relative: Option<&Path>,
        target: &Path,
    ) {
        if let Some(path) = relative {
            info_log!(
                Nonreg,
                1,
                "skipping symlink \"{}\" -> \"{}\" (symlink creation requires Administrator or Developer Mode)",
                path.display(),
                target.display()
            );
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedNonRegular,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
        self.unsupported_operation_skipped = true;
        self.io_errors_occurred = true;
    }

    /// Records a skip event for a directory (when `-r` is not enabled).
    pub(super) fn record_skipped_directory(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedDirectory,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
    }

    /// Records a skip event for a mount-point boundary (`--one-file-system`).
    pub(super) fn record_skipped_mount_point(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedMountPoint,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
    }

    /// Records a skip event for a symlink rejected by `--safe-links`.
    pub(super) fn record_skipped_unsafe_symlink(
        &mut self,
        relative: Option<&Path>,
        metadata: &fs::Metadata,
        target: PathBuf,
    ) {
        if let Some(path) = relative {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target));
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedUnsafeSymlink,
                0,
                None,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
    }

    /// Records the elapsed time spent generating the file list.
    pub(super) const fn record_file_list_generation(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_generation(elapsed);
        }
    }

    /// Records a file list entry and accumulates its wire-encoded size.
    pub(super) fn record_file_list_entry(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            let size = encoded_path_len(path);
            self.summary.record_file_list_entry(size);
        }
    }

    /// Consumes the context and returns the final [`CopyOutcome`].
    pub(super) fn into_outcome(self) -> CopyOutcome {
        CopyOutcome {
            summary: self.summary,
            events: self.events,
            destination_root: self.destination_root,
        }
    }

    /// Queues a directory for deferred deletion (processed after the transfer).
    /// When a deferred deletion for the same `destination` already exists, the
    /// new keep list is unioned into the existing entry so a sibling source's
    /// flist contributions cannot be unlinked by an earlier source's sweep.
    /// Mirrors upstream's shared-flist semantics where every source's entries
    /// are visible to every `delete_in_dir()` call.
    pub(super) fn defer_deletion(
        &mut self,
        destination: PathBuf,
        relative: Option<PathBuf>,
        keep: Vec<OsString>,
    ) {
        if let Some(existing) = self
            .deferred_ops
            .deletions
            .iter_mut()
            .find(|entry| entry.destination == destination)
        {
            for name in keep {
                if !existing.keep.iter().any(|existing_name| existing_name == &name) {
                    existing.keep.push(name);
                }
            }
            return;
        }
        self.deferred_ops.deletions.push(DeferredDeletion {
            destination,
            relative,
            keep,
            decided: None,
        });
    }

    /// Queues a `--delete-delay` deletion whose plan was already decided during
    /// the transfer walk (while the destination per-dir merge files were still
    /// absent). The flush executes `plan` verbatim, without re-scanning the
    /// destination or re-evaluating filters.
    ///
    /// upstream: generator.c:345 `remember_delete()` records the concrete
    /// victim during `delete_in_dir`; `do_delayed_deletions()` (generator.c:2419)
    /// later unlinks it without any further filter check.
    pub(super) fn defer_decided_deletion(
        &mut self,
        destination: PathBuf,
        relative: Option<PathBuf>,
        plan: crate::delete::DeletePlan,
    ) {
        self.deferred_ops.deletions.push(DeferredDeletion {
            destination,
            relative,
            keep: Vec::new(),
            decided: Some(plan),
        });
    }

    /// Adds `keep_name` to the keep list of every queued deferred deletion
    /// whose `destination` is an ancestor of `child_path`. Allows a sibling
    /// source's file to protect itself from a previously-queued sweep on the
    /// same parent directory, replaying the shared-flist invariant upstream
    /// `delete_in_dir()` relies on.
    pub(super) fn register_keep_under_deferred_destination(&mut self, child_path: &Path) {
        let Some(parent) = child_path.parent() else {
            return;
        };
        let Some(name) = child_path.file_name() else {
            return;
        };
        let owned_name: OsString = name.to_os_string();
        for entry in &mut self.deferred_ops.deletions {
            if entry.destination.as_path() == parent
                && !entry.keep.iter().any(|existing| existing == &owned_name)
            {
                entry.keep.push(owned_name.clone());
            }
        }
    }

    /// Restores each transferred directory's real mode and source mtime after
    /// all late in-directory mutations complete - the single final directory
    /// touch-up pass, run once from the executor finalize after the
    /// delayed-update, deletion, and sync flushes.
    ///
    /// Two repairs happen here, both undoing side effects of keeping the
    /// directory usable during the transfer:
    ///
    /// - **Permissions.** A directory whose real mode lacks owner write was
    ///   kept temporarily writable during the traversal (see
    ///   `apply_final_directory_metadata`) so the deferred deletions/updates
    ///   could still write into it. The real, restricted mode is reinstated
    ///   here - LAST, after those mutations ran - matching upstream, where a
    ///   `--delete-after` / `--delay-updates` copy into a `0555` directory that
    ///   needs a child deletion/update would otherwise fail `EACCES`.
    /// - **Mtimes.** The delayed-update rename sweep, deferred deletions, and
    ///   backup file creation each bump the mtime of the directory they act in,
    ///   clobbering the value `apply_final_directory_metadata` set during the
    ///   traversal. Each directory's mtime is re-set from the source here.
    ///
    /// Directories are re-touched deepest-first so a child's perm/mtime change
    /// cannot re-clobber a parent processed earlier, matching the receiver's
    /// reverse iteration.
    ///
    /// The mtime repair is gated on `need_retouch_dir_times` (times preserved,
    /// `--omit-dir-times` off) and skipped when `--backup` is active without a
    /// backup directory, because backup file creation legitimately changes the
    /// destination directory mtimes. The perm repair is independent of that
    /// gating and always runs for the directories that were kept writable.
    ///
    /// upstream: generator.c:2449-2451 - touch_up_dirs(dir_flist, -1) runs
    /// after handle_delayed_updates() and the delete phase; generator.c:2089
    /// touch_up_dirs(); generator.c:2122-2127 fix_dir_perms restores the real
    /// mode; generator.c:2271 need_retouch_dir_times = preserve_mtimes &&
    /// !omit_dir_times.
    pub(super) fn touch_up_dirs(&mut self) {
        let mut dirs = std::mem::take(&mut self.deferred_ops.finalized_dirs);
        let preserve_times = self.metadata_options().times() && !self.omit_dir_times_enabled();
        // upstream: generator.c - skip retouching mtimes when make_backups &&
        // !backup_dir, since in-place backup file creation changes directory
        // mtimes on purpose. The perm restore is unaffected by backups.
        let backup_without_dir =
            self.options.backup_enabled() && self.options.backup_directory().is_none();
        let do_times = preserve_times && !backup_without_dir;

        // Deepest-first, mirroring upstream's reverse flist walk, so a child's
        // perm/mtime change cannot re-clobber a parent handled earlier.
        // upstream: generator.c:2083 for (i = dir_flist->used - 1; i >= 0; i--).
        dirs.sort_by(|a, b| b.path.components().count().cmp(&a.path.components().count()));
        for dir in dirs {
            // Reinstate the real (restricted) directory mode last, after every
            // deferred deletion/update ran while the directory was kept
            // writable. This runs regardless of the mtime gating above.
            // upstream: generator.c:2124-2126 - fix_dir_perms does
            // do_chmod_at(fname, file->mode) before the mtime repair.
            #[cfg(unix)]
            if let Some(mode) = dir.restore_mode {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dir.path, fs::Permissions::from_mode(mode));
            }

            if !do_times {
                continue;
            }
            let Some(mtime) = dir.mtime else {
                continue;
            };
            // upstream: generator.c:2130 - only re-set when mtime_differs(), so
            // directories untouched by a late mutation are left alone.
            let needs_update = match fs::metadata(&dir.path) {
                Ok(meta) => filetime::FileTime::from_last_modification_time(&meta) != mtime,
                Err(_) => false,
            };
            if needs_update {
                let _ = filetime::set_file_mtime(&dir.path, mtime);
            }
        }
    }

    /// Executes all queued deferred deletions, unless I/O errors suppress them.
    ///
    /// Deletions modify the parent directory's mtime. When `-t` is active the
    /// directory mtime was already set by `apply_final_directory_metadata`.
    /// Capture each directory's mtime before deletion and restore it afterwards
    /// to match upstream rsync's behaviour where the receiver sets directory
    /// times independently of the generator's delayed-deletion phase.
    ///
    /// This per-operation restore is now redundant with the final
    /// [`touch_up_dirs`](Self::touch_up_dirs) pass (both re-apply the same
    /// source mtime). It is intentionally retained here to keep this change
    /// scoped to one concern; folding it into the final pass is a deferred DRY
    /// cleanup. Both set the identical value, so the overlap is harmless.
    // upstream: generator.c:2419 do_delayed_deletions() runs after generate_files()
    pub(super) fn flush_deferred_deletions(&mut self) -> Result<(), LocalCopyError> {
        // Nothing queued means no delete pass ran, so there is no notice to
        // emit - upstream only prints the skip notice from inside delete_in_dir
        // when a deletion was actually pending. Checking this first avoids a
        // spurious "IO error encountered -- skipping file deletion" when an
        // I/O error occurred (e.g. an unconvertible --iconv name) but --delete
        // was not requested.
        if self.deferred_ops.deletions.is_empty() {
            return Ok(());
        }
        if self.delete_pass_blocked_by_io_error() {
            // I/O errors occurred and --ignore-errors is not set; suppress
            // deletions to prevent data loss and emit the one-shot skip
            // notice. upstream: generator.c:298-305.
            self.deferred_ops.deletions.clear();
            return Ok(());
        }
        let preserve_times = self.metadata_options().times()
            && !self.omit_dir_times_enabled();
        let mut pending = std::mem::take(&mut self.deferred_ops.deletions);
        // Deferred entries are queued in post-order (a directory defers its own
        // sweep AFTER recursing into and deferring its children). Upstream's
        // `do_delete_pass` instead walks the flist forward - parent before child
        // (generator.c:368-389). Restore that pre-order so the persistent
        // destination delete-filter chain loads each parent's `.rsync-filter`
        // rules BEFORE a subdirectory's sweep, letting them inherit down the
        // destination tree (exclude.c:801). A path-ascending sort places every
        // ancestor directory before its descendants.
        pending.sort_by(|a, b| a.destination.cmp(&b.destination));
        for entry in pending {
            self.enforce_timeout()?;
            // Capture directory mtime before deletion modifies it.
            let saved_times = if preserve_times {
                fs::metadata(&entry.destination).ok().map(|m| {
                    filetime::FileTime::from_last_modification_time(&m)
                })
            } else {
                None
            };
            let relative = entry.relative.as_deref();
            match entry.decided {
                // --delete-delay: execute the plan decided during the walk.
                Some(plan) => crate::local_copy::executor::execute_decided_deletion(
                    self,
                    entry.destination.as_path(),
                    relative,
                    plan,
                )?,
                // --delete-after / multi-source downgrade: decide (rescan +
                // filter) and delete now that the destination merge files are
                // present. The persistent delete-filter chain (advanced inside
                // build_plan_for_directory) inherits each parent's rules into
                // its subdirectories because `pending` was sorted parent-first.
                None => delete_extraneous_entries(
                    self,
                    entry.destination.as_path(),
                    relative,
                    &entry.keep,
                )?,
            }
            // Restore directory mtime that deletion invalidated.
            if let Some(mtime) = saved_times {
                let _ = filetime::set_file_mtime(&entry.destination, mtime);
            }
        }
        Ok(())
    }

    /// Tracks a newly created filesystem entry for rollback on timeout.
    pub(super) fn register_created_path(
        &mut self,
        path: &Path,
        kind: CreatedEntryKind,
        existed_before: bool,
    ) {
        if self.mode.is_dry_run() || existed_before {
            return;
        }
        self.deferred_ops.created_entries.push(CreatedEntry {
            path: path.to_path_buf(),
            kind,
        });
    }

    /// Returns the path of the most recently created entry, if any.
    pub(crate) fn last_created_entry_path(&self) -> Option<&Path> {
        self.deferred_ops
            .created_entries
            .last()
            .map(|entry| entry.path.as_path())
    }

    /// Removes the most recently created entry from the rollback tracker.
    pub(crate) fn pop_last_created_entry(&mut self) {
        self.deferred_ops.created_entries.pop();
    }

    /// Rolls back created entries if the error is a timeout.
    pub(super) fn rollback_on_error(&mut self, error: &LocalCopyError) {
        if matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }) {
            self.rollback_created_entries();
        }
    }

    /// Removes all entries created during the transfer (files, dirs, symlinks,
    /// etc.) in reverse order.
    pub(super) fn rollback_created_entries(&mut self) {
        while let Some(entry) = self.deferred_ops.created_entries.pop() {
            match entry.kind {
                CreatedEntryKind::Directory => {
                    let _ = fs::remove_dir(&entry.path);
                }
                CreatedEntryKind::File
                | CreatedEntryKind::Symlink
                | CreatedEntryKind::Fifo
                | CreatedEntryKind::Device
                | CreatedEntryKind::HardLink => {
                    let _ = fs::remove_file(&entry.path);
                }
            }
        }
    }
}

/// Returns the wire-encoded byte length of a path (platform-aware, with null
/// terminator).
fn encoded_path_len(path: &Path) -> usize {
    const NULL_TERMINATOR: usize = 1;

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str().as_bytes().len() + NULL_TERMINATOR
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        path.as_os_str().encode_wide().count() * 2 + NULL_TERMINATOR
    }

    #[cfg(not(any(unix, windows)))]
    {
        path.to_string_lossy().len() + NULL_TERMINATOR
    }
}
