impl<'a> CopyContext<'a> {
    /// Creates a new copy context from the execution mode, options, and
    /// optional event observer. Initialises the bandwidth limiter, filter
    /// program, buffer pool, and deferred-sync strategy.
    pub(super) fn new(
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        observer: Option<&'a mut dyn LocalCopyRecordHandler>,
        destination_root: PathBuf,
    ) -> Self {
        let burst = options.bandwidth_burst_bytes();
        let limiter =
            BandwidthLimitComponents::new(options.bandwidth_limit_bytes(), burst).into_limiter();
        let collect_events = options.events_enabled();
        let stop_at_wallclock = options.stop_at();
        let stop_deadline = stop_at_wallclock.map(|deadline| {
            let now = std::time::SystemTime::now();
            match deadline.duration_since(now) {
                Ok(duration) => Instant::now() + duration,
                Err(_) => Instant::now(),
            }
        });
        let filter_program = options.filter_program().cloned();
        let dir_merge_layers = filter_program
            .as_ref()
            .map(|program| vec![Vec::new(); program.dir_merge_rules().len()])
            .unwrap_or_default();
        let dir_merge_marker_layers = filter_program
            .as_ref()
            .map(|program| vec![Vec::new(); program.dir_merge_rules().len()])
            .unwrap_or_default();
        let dir_merge_ephemeral = Vec::new();
        let dir_merge_marker_ephemeral = Vec::new();
        let timeout = options.timeout();

        let buffer_pool = global_buffer_pool();

        let deferred_sync = if options.fsync_enabled() {
            DeferredSync::new(SyncStrategy::Batched(100))
        } else {
            DeferredSync::new(SyncStrategy::None)
        };

        // When batch mode is active, create a temp file to buffer per-file
        // delta data. The flist entries go directly to the batch writer during
        // the walk, but file data (NDX + iflags + sum_head + tokens + checksum)
        // must come AFTER the flist end marker in the batch stream.
        let batch_delta_buf = options
            .get_batch_writer()
            .map(|_| std::io::Cursor::new(Vec::new()));

        let batch_ndx_codec = options.get_batch_writer().map(|batch_writer_arc| {
            let guard = batch_writer_arc.lock().unwrap();
            let proto_version = guard.config().protocol_version;
            drop(guard);
            protocol::codec::NdxCodecEnum::new(proto_version as u8)
        });

        let batch_flist_writer = options.get_batch_writer().map(|batch_writer_arc| {
            let guard = batch_writer_arc.lock().unwrap();
            let proto_version = guard.config().protocol_version;
            let compat_flags_val = guard.config().compat_flags;
            drop(guard);
            let protocol = protocol::ProtocolVersion::try_from(proto_version as u8)
                .unwrap_or(protocol::ProtocolVersion::NEWEST);
            // upstream: io.c:start_write_batch() - compat_flags are written to the batch
            // header. The flist writer must use the same compat_flags to ensure the wire
            // encoding (varint flags, safe file list) matches what the reader will expect
            // when decoding the batch body.
            let writer = if let Some(cf) = compat_flags_val {
                let compat = protocol::CompatibilityFlags::from_bits(cf as u32);
                protocol::flist::FileListWriter::with_compat_flags(protocol, compat)
            } else {
                protocol::flist::FileListWriter::new(protocol)
            };
            writer
                .with_preserve_uid(options.preserve_owner())
                .with_preserve_gid(options.preserve_group())
                .with_preserve_links(options.links_enabled())
                .with_preserve_devices(options.devices_enabled())
                .with_preserve_specials(options.specials_enabled())
                .with_preserve_hard_links(options.hard_links_enabled())
                .with_preserve_atimes(options.preserve_atimes())
                .with_preserve_crtimes(options.preserve_crtimes())
        });

        Self {
            mode,
            options,
            hard_links: HardLinkTracker::new(),
            limiter,
            summary: LocalCopySummary::default(),
            events: if collect_events {
                Some(Vec::new())
            } else {
                None
            },
            filter_program,
            dir_merge_layers: Rc::new(RefCell::new(dir_merge_layers)),
            dir_merge_marker_layers: Rc::new(RefCell::new(dir_merge_marker_layers)),
            observer,
            dir_merge_ephemeral: Rc::new(RefCell::new(dir_merge_ephemeral)),
            dir_merge_marker_ephemeral: Rc::new(RefCell::new(dir_merge_marker_ephemeral)),
            deferred_ops: DeferredOperationQueue::default(),
            timeout,
            stop_deadline,
            stop_at: stop_at_wallclock,
            last_progress: Instant::now(),
            destination_root,
            safety_depth_offset: 0,
            use_buffer_pool: true,
            buffer_pool,
            deferred_sync,
            checksum_cache: None,
            io_errors_occurred: false,
            verified_parents: HashSet::new(),
            batch_flist_writer,
            batch_delta_buf,
            batch_delta_entries: Vec::new(),
            batch_entry_sort_data: Vec::new(),
            batch_current_delta_idx: 0,
            batch_flist_index: 0,
            batch_ndx_codec,
        }
    }

    /// Records that forward progress was made, resetting the timeout clock.
    pub(super) fn register_progress(&mut self) {
        self.last_progress = Instant::now();
    }

    /// Returns an error if the inactivity timeout or wall-clock deadline has
    /// been exceeded.
    pub(super) fn enforce_timeout(&mut self) -> Result<(), LocalCopyError> {
        if let Some(limit) = self.timeout
            && self.last_progress.elapsed() > limit
        {
            return Err(LocalCopyError::timeout(limit));
        }
        if let Some(deadline) = self.stop_deadline
            && Instant::now() >= deadline
        {
            let target = self.stop_at.unwrap_or_else(std::time::SystemTime::now);
            return Err(LocalCopyError::stop_at_reached(target));
        }
        Ok(())
    }

    /// Returns the execution mode (real or dry-run).
    pub(super) const fn mode(&self) -> LocalCopyExecution {
        self.mode
    }

    /// Returns a reference to the full set of copy options.
    pub(super) const fn options(&self) -> &LocalCopyOptions {
        &self.options
    }

    /// Returns whether `--one-file-system` (`-x`) is enabled.
    pub(super) const fn one_file_system_enabled(&self) -> bool {
        self.options.one_file_system_enabled()
    }

    /// Returns the `--one-file-system` nesting level (0, 1, or 2).
    pub(super) const fn one_file_system_level(&self) -> u8 {
        self.options.one_file_system_level()
    }

    /// Records a hard-link source if `--hard-links` is enabled.
    pub(super) fn record_hard_link(&mut self, metadata: &fs::Metadata, destination: &Path) {
        if self.options.hard_links_enabled() {
            self.hard_links.record(metadata, destination);
        }
    }

    /// Returns the existing hard-link target for a file, if one was previously
    /// recorded with the same inode/device.
    pub(super) fn existing_hard_link_target(&self, metadata: &fs::Metadata) -> Option<PathBuf> {
        if self.options.hard_links_enabled() {
            self.hard_links.existing_target(metadata)
        } else {
            None
        }
    }

    /// Returns whether `--delay-updates` is enabled.
    pub(super) const fn delay_updates_enabled(&self) -> bool {
        self.options.delay_updates_enabled()
    }

    /// Returns whether a bandwidth limiter is active.
    #[cfg(target_os = "macos")]
    pub(super) const fn has_bandwidth_limiter(&self) -> bool {
        self.limiter.is_some()
    }

    /// Returns the root destination directory for the transfer.
    pub(super) fn destination_root(&self) -> &Path {
        &self.destination_root
    }

    /// Finalization template:
    /// 1. Register newly created path.
    /// 2. Apply POSIX/stat metadata.
    /// 3. Conditionally sync xattrs/ACLs (Strategy-style via feature flags).
    /// 4. Record as hard-link source and remove the original if requested.
    pub(super) fn apply_metadata_and_finalize(
        &mut self,
        destination: &Path,
        params: FinalizeMetadataParams<'_>,
    ) -> Result<(), LocalCopyError> {
        let FinalizeMetadataParams {
            metadata,
            metadata_options,
            mode,
            path_context,
            #[cfg(unix)]
            fd,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls,
        } = params;

        let MetadataPathContext {
            source,
            relative,
            file_type,
            destination_previously_existed,
        } = path_context;

        self.register_created_path(
            destination,
            CreatedEntryKind::File,
            destination_previously_existed,
        );

        // Use fd-based metadata operations when an open fd is available (Unix).
        // Stat the destination first to skip redundant chown/chmod/utimensat
        // when values already match - upstream rsync.c:set_file_attrs() does the
        // same comparison before calling chown/chmod.
        #[cfg(unix)]
        {
            if let Some(fd) = fd {
                if let Ok(existing) = std::fs::metadata(destination) {
                    ::metadata::apply_file_metadata_with_fd_if_changed(
                        destination,
                        metadata,
                        &existing,
                        &metadata_options,
                        fd,
                    )
                    .map_err(map_metadata_error)?;
                } else {
                    ::metadata::apply_file_metadata_with_fd(
                        destination,
                        metadata,
                        &metadata_options,
                        fd,
                    )
                    .map_err(map_metadata_error)?;
                }
            } else {
                apply_file_metadata_with_options(destination, metadata, &metadata_options)
                    .map_err(map_metadata_error)?;
            }
        }
        #[cfg(not(unix))]
        {
            apply_file_metadata_with_options(destination, metadata, &metadata_options)
                .map_err(map_metadata_error)?;
        }

        #[cfg(all(unix, feature = "xattr"))]
        {
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                self.filter_program.as_ref(),
            )?;

            // Sync NFSv4 ACLs separately (stored in system.nfs4_acl xattr)
            sync_nfsv4_acls_if_requested(
                self.options.preserve_nfsv4_acls(),
                mode,
                source,
                destination,
                true,
            )?;
        }

        #[cfg(all(any(unix, windows), feature = "acl"))]
        {
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
        }

        #[cfg(not(any(all(unix, feature = "xattr"), all(any(unix, windows), feature = "acl"))))]
        let _ = mode;

        self.record_hard_link(metadata, destination);
        remove_source_entry_if_requested(self, source, relative, file_type)?;

        // Register file for deferred sync (runtime-selected via fsync_enabled)
        self.deferred_sync
            .register(destination.to_path_buf())
            .map_err(|error| LocalCopyError::io("register deferred sync", destination, error))?;
        self.deferred_sync.flush_if_threshold().map_err(|error| {
            LocalCopyError::io("flush deferred sync threshold", PathBuf::new(), error)
        })?;

        Ok(())
    }

    /// Searches `--link-dest` directories for a file matching the source,
    /// returning the first candidate that passes the quick-check comparison.
    pub(super) fn link_dest_target(
        &self,
        relative: &Path,
        source: &Path,
        metadata: &fs::Metadata,
        size_only: bool,
        ignore_times: bool,
        checksum: bool,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest candidate",
                        candidate,
                        error,
                    ));
                }
            };

            if !candidate_metadata.file_type().is_file() {
                continue;
            }

            if should_skip_copy(CopyComparison {
                source_path: source,
                source: metadata,
                destination_path: candidate.as_path(),
                destination: &candidate_metadata,
                size_only,
                ignore_times,
                checksum,
                checksum_algorithm: self.options.checksum_algorithm(),
                modify_window: self.options.modify_window(),
                prefetched_match: None,
            }) {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
    }

    /// Returns the configured `--link-dest` / `--copy-dest` / `--compare-dest`
    /// reference directories.
    pub(super) fn reference_directories(&self) -> &[ReferenceDirectory] {
        self.options.reference_directories()
    }

    /// Sets the checksum cache for the current directory.
    ///
    /// The cache should be populated via parallel checksum prefetching
    /// before processing files in the directory.
    pub(super) fn set_checksum_cache(&mut self, cache: super::executor::ChecksumCache) {
        self.checksum_cache = Some(cache);
    }

    /// Looks up a source path in the checksum cache.
    ///
    /// Returns `Some(true)` if checksums match (skip copy), `Some(false)` if
    /// checksums differ (need copy), or `None` if not in cache.
    pub(super) fn lookup_checksum(&self, source: &Path) -> Option<bool> {
        self.checksum_cache
            .as_ref()
            .and_then(|cache| cache.lookup(source))
    }

    /// Clears the checksum cache to free memory after directory processing.
    pub(super) fn clear_checksum_cache(&mut self) {
        if let Some(ref mut cache) = self.checksum_cache {
            cache.clear();
        }
    }

    /// Queues a deferred update for `--delay-updates` and records the hard-link
    /// source. The staging directory is tracked for cleanup after commit.
    pub(super) fn register_deferred_update(&mut self, update: DeferredUpdate) {
        // Track the `.~tmp~` staging directory for cleanup after all updates
        // are committed.
        if let Some(parent) = update.guard.staging_path().parent() {
            if parent
                .file_name()
                .is_some_and(|name| name == super::options::staging::DELAY_UPDATES_PARTIAL_DIR)
            {
                self.deferred_ops.delay_staging_dirs.insert(parent.to_path_buf());
            }
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

    /// Renames or copies an existing destination entry to the backup location
    /// when `--backup` is enabled.
    pub(super) fn backup_existing_entry(
        &mut self,
        destination: &Path,
        _relative: Option<&Path>,
        file_type: fs::FileType,
    ) -> Result<(), LocalCopyError> {
        if !self.options.backup_enabled() || self.mode.is_dry_run() {
            return Ok(());
        }

        if file_type.is_dir() {
            return Ok(());
        }

        // Always derive the relative path from destination/destination_root
        // rather than using the source-relative path. The source-relative path
        // may not include the source directory basename (e.g., "nested/file.txt"
        // instead of "source/nested/file.txt"), causing backup files to be
        // placed at the wrong location when --backup-dir is used.
        // upstream: backup.c:get_backup_name() uses the destination-relative path
        let backup_path = compute_backup_path(
            self.destination_root(),
            destination,
            None,
            self.options.backup_directory(),
            self.options.backup_suffix(),
        );

        if let Some(parent) = backup_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|error| LocalCopyError::io("create backup directory", parent, error))?;
        }

        match fs::rename(destination, &backup_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if let Err(remove_error) = fs::remove_file(&backup_path)
                    && remove_error.kind() != io::ErrorKind::NotFound
                {
                    return Err(LocalCopyError::io(
                        "remove existing backup",
                        backup_path,
                        remove_error,
                    ));
                }
                fs::rename(destination, &backup_path).map_err(|rename_error| {
                    LocalCopyError::io("create backup", backup_path.clone(), rename_error)
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                copy_entry_to_backup(destination, &backup_path, file_type)?;
            }
            Err(error) => {
                return Err(LocalCopyError::io("create backup", backup_path, error));
            }
        }

        Ok(())
    }

    /// Forcibly removes a destination entry (backing it up first if needed),
    /// and records the deletion in the summary.
    pub(super) fn force_remove_destination(
        &mut self,
        destination: &Path,
        relative: Option<&Path>,
        metadata: &fs::Metadata,
    ) -> Result<(), LocalCopyError> {
        let file_type = metadata.file_type();

        if self.mode.is_dry_run() {
            self.summary_mut().record_deletion();
            if let Some(path) = relative {
                self.record(LocalCopyRecord::new(
                    path.to_path_buf(),
                    LocalCopyAction::EntryDeleted,
                    0,
                    None,
                    Duration::default(),
                    None,
                ));
            }
            self.register_progress();
            return Ok(());
        }

        self.backup_existing_entry(destination, relative, file_type)?;

        let context = if file_type.is_dir() {
            "remove existing directory"
        } else {
            "remove existing destination"
        };

        let removal_result = if file_type.is_dir() {
            fs::remove_dir_all(destination)
        } else {
            fs::remove_file(destination)
        };

        match removal_result {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    context,
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        self.summary_mut().record_deletion();
        if let Some(path) = relative {
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
        self.register_progress();

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

        guard.commit()?;

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
                #[cfg(unix)]
                fd: None, // No fd available for deferred updates
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(any(unix, windows), feature = "acl"))]
                preserve_acls,
            },
        )
    }

    /// Returns the configured delete timing (before, during, after, or delay).
    pub(super) const fn delete_timing(&self) -> Option<DeleteTiming> {
        self.options.delete_timing()
    }

    /// Returns the `--min-size` limit, if set.
    pub(super) const fn min_file_size_limit(&self) -> Option<u64> {
        self.options.min_file_size_limit()
    }

    /// Returns the `--max-size` limit, if set.
    pub(super) const fn max_file_size_limit(&self) -> Option<u64> {
        self.options.max_file_size_limit()
    }

    /// Returns an Arc reference to the shared buffer pool.
    ///
    /// The Arc is returned so that [`BufferGuard`] can hold an owned reference,
    /// avoiding borrow checker issues when the context is mutably borrowed.
    pub(super) fn buffer_pool(&self) -> Arc<BufferPool> {
        Arc::clone(&self.buffer_pool)
    }

    /// Returns whether the buffer pool should be used for I/O operations.
    pub(super) const fn use_buffer_pool(&self) -> bool {
        self.use_buffer_pool
    }

    /// Flushes all pending sync operations.
    pub(super) fn flush_deferred_syncs(&mut self) -> Result<(), LocalCopyError> {
        self.deferred_sync
            .flush()
            .map_err(|error| LocalCopyError::io("flush syncs", PathBuf::new(), error))
    }

    /// Records that an I/O error occurred during the transfer.
    ///
    /// When I/O errors are recorded and `--ignore-errors` is not set,
    /// deletion operations are suppressed to prevent data loss.
    pub(super) fn record_io_error(&mut self) {
        self.io_errors_occurred = true;
    }

    /// Reports whether deletions should proceed despite I/O errors.
    ///
    /// Returns `true` if:
    /// - No I/O errors occurred, OR
    /// - `--ignore-errors` is enabled
    pub(super) const fn deletions_allowed(&self) -> bool {
        !self.io_errors_occurred || self.options.ignore_errors_enabled()
    }
}
