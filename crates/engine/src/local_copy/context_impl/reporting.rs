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

    /// Executes all queued deferred deletions, unless I/O errors suppress them.
    ///
    /// Deletions modify the parent directory's mtime. When `-t` is active the
    /// directory mtime was already set by `apply_final_directory_metadata`.
    /// Capture each directory's mtime before deletion and restore it afterwards
    /// to match upstream rsync's behaviour where the receiver sets directory
    /// times independently of the generator's delayed-deletion phase.
    // upstream: generator.c:2419 do_delayed_deletions() runs after generate_files()
    pub(super) fn flush_deferred_deletions(&mut self) -> Result<(), LocalCopyError> {
        if !self.deletions_allowed() {
            // I/O errors occurred and --ignore-errors is not set;
            // suppress deletions to prevent data loss.
            self.deferred_ops.deletions.clear();
            return Ok(());
        }
        let preserve_times = self.metadata_options().times()
            && !self.omit_dir_times_enabled();
        let pending = std::mem::take(&mut self.deferred_ops.deletions);
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
            delete_extraneous_entries(self, entry.destination.as_path(), relative, &entry.keep)?;
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
