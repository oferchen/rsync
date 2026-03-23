impl<'a> CopyContext<'a> {
    /// Records a skip event for a non-regular file (e.g. socket, unknown type).
    pub(super) fn record_skipped_non_regular(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
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

    /// Records the elapsed time spent transferring the file list over the wire.
    #[allow(dead_code)]
    pub(super) const fn record_file_list_transfer(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_transfer(elapsed);
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
    pub(super) fn defer_deletion(
        &mut self,
        destination: PathBuf,
        relative: Option<PathBuf>,
        keep: Vec<OsString>,
    ) {
        self.deferred_ops.deletions.push(DeferredDeletion {
            destination,
            relative,
            keep,
        });
    }

    /// Executes all queued deferred deletions, unless I/O errors suppress them.
    pub(super) fn flush_deferred_deletions(&mut self) -> Result<(), LocalCopyError> {
        if !self.deletions_allowed() {
            // I/O errors occurred and --ignore-errors is not set;
            // suppress deletions to prevent data loss.
            self.deferred_ops.deletions.clear();
            return Ok(());
        }
        let pending = std::mem::take(&mut self.deferred_ops.deletions);
        for entry in pending {
            self.enforce_timeout()?;
            let relative = entry.relative.as_deref();
            delete_extraneous_entries(self, entry.destination.as_path(), relative, &entry.keep)?;
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
