impl<'a> CopyContext<'a> {
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

    pub(super) fn record_file_list_generation(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_generation(elapsed);
        }
    }

    pub(super) fn record_file_list_entry(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            let size = encoded_path_len(path);
            self.summary.record_file_list_entry(size);
        }
    }

    #[allow(dead_code)]
    pub(super) fn record_file_list_transfer(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_transfer(elapsed);
        }
    }

    pub(super) fn into_outcome(self) -> CopyOutcome {
        CopyOutcome {
            summary: self.summary,
            events: self.events,
            destination_root: self.destination_root,
        }
    }

    pub(super) fn defer_deletion(
        &mut self,
        destination: PathBuf,
        relative: Option<PathBuf>,
        keep: Vec<OsString>,
    ) {
        self.deferred_deletions.push(DeferredDeletion {
            destination,
            relative,
            keep,
        });
    }

    pub(super) fn flush_deferred_deletions(&mut self) -> Result<(), LocalCopyError> {
        let pending = std::mem::take(&mut self.deferred_deletions);
        for entry in pending {
            self.enforce_timeout()?;
            let relative = entry.relative.as_deref();
            delete_extraneous_entries(self, entry.destination.as_path(), relative, &entry.keep)?;
        }
        Ok(())
    }

    pub(super) fn register_created_path(
        &mut self,
        path: &Path,
        kind: CreatedEntryKind,
        existed_before: bool,
    ) {
        if self.mode.is_dry_run() || existed_before {
            return;
        }
        self.created_entries.push(CreatedEntry {
            path: path.to_path_buf(),
            kind,
        });
    }

    pub(crate) fn last_created_entry_path(&self) -> Option<&Path> {
        self.created_entries
            .last()
            .map(|entry| entry.path.as_path())
    }

    pub(crate) fn pop_last_created_entry(&mut self) {
        self.created_entries.pop();
    }

    pub(super) fn rollback_on_error(&mut self, error: &LocalCopyError) {
        if matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }) {
            self.rollback_created_entries();
        }
    }

    pub(super) fn rollback_created_entries(&mut self) {
        while let Some(entry) = self.created_entries.pop() {
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
