// `--delay-updates` staging: recording deferred updates during the walk and
// committing them after it. upstream: receiver.c's delayed-update list.
impl<'a> CopyContext<'a> {
    /// Records a finalized directory for the final
    /// [`touch_up_dirs`](Self::touch_up_dirs) pass.
    ///
    /// Called by `apply_final_directory_metadata` during the traversal. Late
    /// in-directory mutations (the delayed-update rename sweep, deferred
    /// deletions, backup file creation) bump the directory mtime and require the
    /// directory to stay writable, so the final pass re-applies the recorded
    /// source mtime and reinstates the restricted mode last.
    ///
    /// `mtime` is the source mtime to re-apply (when times are preserved) and
    /// `restore_mode` is the restricted mode to reinstate for a directory that
    /// was kept writable during the transfer.
    pub(super) fn record_finalized_directory(
        &mut self,
        destination: &Path,
        mtime: Option<filetime::FileTime>,
        restore_mode: Option<u32>,
    ) {
        self.deferred_ops.finalized_dirs.push(FinalizedDir {
            path: destination.to_path_buf(),
            mtime,
            restore_mode,
        });
    }

    /// Queues a deferred update for `--delay-updates` and records the hard-link
    /// source. The staging directory is tracked for cleanup after commit.
    pub(super) fn register_deferred_update(&mut self, update: DeferredUpdate) {
        // Track the staging directory for rmdir once every update has been moved
        // out of it. The guard reports it only for a RELATIVE `--partial-dir`;
        // an absolute one is a reserved location upstream never removes.
        //
        // upstream: receiver.c:718 handle_partial_dir(partialptr, PDIR_DELETE)
        // after the delayed rename succeeds.
        if let Some(dir) = update.guard.partial_dir_to_remove() {
            self.deferred_ops
                .delay_staging_dirs
                .insert(dir.to_path_buf());
        }
        let metadata = update.metadata.clone();
        let destination = update.destination.clone();
        self.record_hard_link(&metadata, destination.as_path());
        self.deferred_ops.updates.push(update);
    }

    /// Commits a single deferred update matching the given destination path,
    /// if one exists in the queue.
    pub(super) fn commit_deferred_update_for(
        &mut self,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        if let Some(index) = self
            .deferred_ops
            .updates
            .iter()
            .position(|update| update.destination.as_path() == destination)
        {
            let update = self.deferred_ops.updates.swap_remove(index);
            self.finalize_deferred_update(update)?;
        }
        Ok(())
    }

    /// Commits all remaining deferred updates and removes empty staging
    /// directories.
    pub(super) fn flush_deferred_updates(&mut self) -> Result<(), LocalCopyError> {
        let updates = std::mem::take(&mut self.deferred_ops.updates);
        for update in updates {
            self.finalize_deferred_update(update)?;
        }

        // Remove empty `.~tmp~` staging directories after all deferred files
        // have been moved to their final locations. This covers both updates
        // committed here and those committed early via `commit_deferred_update_for`.
        //
        // upstream: receiver.c -- handle_partial_dir(partialptr, PDIR_DELETE)
        let dirs = std::mem::take(&mut self.deferred_ops.delay_staging_dirs);
        for dir in &dirs {
            let _ = fs::remove_dir(dir);
        }

        Ok(())
    }

    /// Commits a single deferred update: moves the staged file to its final
    /// path and applies metadata.
    pub(super) fn finalize_deferred_update(
        &mut self,
        update: DeferredUpdate,
    ) -> Result<(), LocalCopyError> {
        let DeferredUpdate {
            guard,
            metadata,
            metadata_options,
            mode,
            path_context,
            destination,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls,
        } = update;

        #[cfg(not(any(all(unix, feature = "xattr"), all(any(unix, windows), feature = "acl"))))]
        let _ = &path_context.source;

        // Deferred updates have no open fd, so the cross-device flag is unused.
        let _cross_device = guard.commit()?;

        self.apply_metadata_and_finalize(
            destination.as_path(),
            FinalizeMetadataParams {
                metadata: &metadata,
                metadata_options,
                mode,
                path_context: MetadataPathContext {
                    source: path_context.source.as_path(),
                    relative: path_context.relative.as_deref(),
                    file_type: path_context.file_type,
                    destination_previously_existed: path_context.destination_previously_existed,
                },
                // upstream: receiver.c:964 - deferred updates have already
                // committed the rename + applied dest_mode at the original
                // commit site, so there is no pre-transfer stat to recover
                // here.
                pre_transfer_meta: None,
                #[cfg(unix)]
                fd: None, // No fd available for deferred updates
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(any(unix, windows), feature = "acl"))]
                preserve_acls,
            },
        )
    }
}
