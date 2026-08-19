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
        let limiter =
            BandwidthLimitComponents::new(options.bandwidth_limit_bytes()).into_limiter();
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
        let timeout = options.timeout();

        let buffer_pool = global_buffer_pool();

        let deferred_sync = if options.fsync_enabled() {
            DeferredSync::new(SyncStrategy::Batched(100))
        } else {
            DeferredSync::new(SyncStrategy::None)
        };

        let batch_delta_buf = build_batch_delta_buffer(&options);
        let batch_ndx_codec = build_batch_ndx_codec(&options);
        let batch_flist_writer = build_batch_flist_writer(&options);
        let batch_token_encoder = build_batch_token_encoder(&options);
        let adaptive_level = build_adaptive_level_controller(&options);

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
            dir_merge: DirectoryFilterHandles::new(filter_program.as_ref()),
            delete_dir_merge: DirectoryFilterHandles::new(filter_program.as_ref()),
            delete_filter_chain: RefCell::new(Vec::new()),
            filter_program,
            observer,
            deferred_ops: DeferredOperationQueue::default(),
            timeout,
            stop_deadline,
            stop_at: stop_at_wallclock,
            last_progress: Instant::now(),
            destination_root,
            source_anchor: None,
            safety_depth_offset: 0,
            use_buffer_pool: true,
            buffer_pool,
            deferred_sync,
            checksum_cache: None,
            destination_metadata_cache: HashMap::new(),
            io_errors_occurred: false,
            io_error_delete_warning_emitted: false,
            iconv_conversion_error: false,
            unsupported_operation_skipped: false,
            sender_remove_error: false,
            delete_io_error: false,
            multi_source: false,
            verified_parents: HashMap::new(),
            emitted_implied_dirs: HashSet::new(),
            expanded_source_roots: Vec::new(),
            batch_flist_writer,
            batch_delta_buf,
            batch_delta_entries: Vec::new(),
            batch_delta_sum_head: protocol::wire::SumHead::WHOLE_FILE,
            batch_delta_sum_head_offset: 0,
            batch_entry_sort_data: Vec::new(),
            batch_current_delta_idx: 0,
            batch_flist_index: 0,
            batch_ndx_codec,
            batch_token_encoder,
            readdir_buf: Vec::new(),
            adaptive_level,
        }
    }

    /// Reserves additional capacity in the events buffer to avoid
    /// growth-copy reallocations when the entry count is known ahead of time.
    pub(super) fn reserve_event_capacity(&mut self, additional: usize) {
        if let Some(events) = &mut self.events {
            events.reserve(additional);
        }
    }

    /// Records that forward progress was made, resetting the timeout clock.
    pub(in crate::local_copy) fn register_progress(&mut self) {
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

    /// Records that the active plan carries more than one source operand.
    /// Read by [`Self::multi_source`] to switch `--delete-during` to deferred
    /// sweeps, merging per-source keep lists so a sibling source's flist
    /// entries cannot be deleted before they are written.
    pub(super) fn set_multi_source(&mut self, value: bool) {
        self.multi_source = value;
    }

    /// Returns `true` when the plan carries multiple sources.
    pub(super) const fn multi_source(&self) -> bool {
        self.multi_source
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

    /// Registers a hardlink-cohort leader keyed by the reference file the
    /// destination is being linked to. Returns `true` the first time a given
    /// reference is seen (this destination is the cohort leader) and `false`
    /// for subsequent followers that share the same inode.
    ///
    /// Used to make per-inode metadata writes (e.g. NTFS DACL writes via
    /// `SetNamedSecurityInfoW`) O(1) per cohort instead of O(N) per follower.
    ///
    /// upstream: hlink.c::hard_link_check returns 1 for followers so
    /// generator.c:1552 exits before `set_file_attrs()` and therefore never
    /// calls `set_acl()` on a follower alias.
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub(super) fn register_acl_cohort_leader(&mut self, reference: &Path) -> bool {
        self.hard_links.register_acl_cohort_leader(reference)
    }

    /// Returns whether `--delay-updates` is enabled.
    pub(super) const fn delay_updates_enabled(&self) -> bool {
        self.options.delay_updates_enabled()
    }

    /// Returns whether a bandwidth limiter is active.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub(in crate::local_copy) const fn has_bandwidth_limiter(&self) -> bool {
        self.limiter.is_some()
    }

    /// Sets the source-tree confinement anchor for the operand about to be
    /// walked.
    ///
    /// upstream: `rsync-3.5.0/sender.c` - the sender's content opens are
    /// confined beneath the transfer root, which is per source argument.
    pub(in crate::local_copy) fn set_source_anchor(&mut self, anchor: Option<PathBuf>) {
        self.source_anchor = anchor;
    }

    /// Returns the source-tree confinement anchor for the current operand.
    pub(in crate::local_copy) fn source_anchor(&self) -> Option<&Path> {
        self.source_anchor.as_deref()
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
            pre_transfer_meta,
            #[cfg(unix)]
            fd,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls,
        } = params;
        #[cfg(not(unix))]
        let _ = pre_transfer_meta;

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

        // upstream: receiver.c:964 - when `-p`/`--chmod` are off, the
        // receiver rewrites `file->mode` via `dest_mode()` BEFORE the
        // transfer; `set_file_attrs()` then chmods the freshly-renamed
        // temp file to that mode. Reproduce that chmod here so a re-
        // transferred regular file holds its pre-transfer permission bits
        // and a new regular file lands at `source_mode & dflt_perms`. The
        // call short-circuits when `-p`/`--chmod` are active so the
        // existing chmod chain owns the syscall.
        #[cfg(unix)]
        ::metadata::apply_dest_mode_pre_transfer(
            destination,
            metadata,
            &metadata_options,
            pre_transfer_meta,
        )
        .map_err(map_metadata_error)?;

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
            sync_acls_if_requested(
                preserve_acls,
                self.options.fake_super_enabled(),
                mode,
                source,
                destination,
                true,
            )?;
        }

        #[cfg(not(any(all(unix, feature = "xattr"), all(any(unix, windows), feature = "acl"))))]
        let _ = mode;

        // upstream: xattrs.c:set_stat_xattr() reads the *source* stat via
        // x_lstat() (get_stat_xattr layered over lstat), so a placeholder that
        // already carries a `user.rsync.%stat` xattr forwards those recorded
        // uid/gid/mode/rdev instead of the placeholder's own on-disk values.
        // `set_owner_like` only saw the placeholder's `fs::Metadata`, so rewrite
        // the destination xattr here from the effective source stat.
        #[cfg(all(unix, feature = "xattr"))]
        store_effective_fake_super_if_requested(&metadata_options, source, destination, metadata)?;

        self.record_hard_link(metadata, destination);
        remove_source_entry_if_requested(self, source, destination, metadata, relative, file_type)?;

        // Register file for deferred sync (runtime-selected via fsync_enabled)
        self.deferred_sync
            .register(destination.to_path_buf())
            .map_err(|error| LocalCopyError::io("register deferred sync", destination, error))?;
        self.deferred_sync.flush_if_threshold().map_err(|error| {
            LocalCopyError::io("flush deferred sync threshold", PathBuf::new(), error)
        })?;

        Ok(())
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

    /// Stores destination `lstat` metadata gathered during checksum-mode
    /// prefetch so `copy_file` can reuse it instead of re-lstat'ing.
    pub(super) fn set_destination_metadata_cache(
        &mut self,
        cache: HashMap<PathBuf, fs::Metadata>,
    ) {
        self.destination_metadata_cache = cache;
    }

    /// Removes and returns the cached destination `lstat` metadata for `dest`,
    /// if the checksum-mode prefetch recorded it. Returns `None` when absent
    /// (non-checksum mode, a non-regular destination, or already consumed), in
    /// which case the caller performs its own `lstat`.
    pub(super) fn take_cached_destination_metadata(&mut self, dest: &Path) -> Option<fs::Metadata> {
        self.destination_metadata_cache.remove(dest)
    }

    /// Returns a mutable reference to the reusable readdir buffer.
    ///
    /// Callers should `clear()` the buffer before filling it. The Vec's heap
    /// capacity persists across calls, eliminating per-directory allocations
    /// during recursive traversal.
    pub(super) fn readdir_buf(&mut self) -> &mut Vec<(OsString, PathBuf)> {
        &mut self.readdir_buf
    }

    /// Clears the checksum cache to free memory after directory processing.
    pub(super) fn clear_checksum_cache(&mut self) {
        if let Some(ref mut cache) = self.checksum_cache {
            cache.clear();
        }
        self.destination_metadata_cache.clear();
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

    /// Records that an `--iconv` filename could not be strictly transcoded and
    /// its entry was skipped.
    ///
    /// Also suppresses deletions like any other general I/O error, matching
    /// upstream where `io_error |= IOERR_GENERAL` gates the delete pass.
    // upstream: flist.c:1631 send_file1() sets io_error |= IOERR_GENERAL.
    pub(super) fn record_iconv_conversion_error(&mut self) {
        self.iconv_conversion_error = true;
        self.io_errors_occurred = true;
    }

    /// Reports whether any `--iconv` filename conversion was skipped, so the
    /// transfer can finish with exit code 23 (`RERR_PARTIAL`).
    pub(super) const fn iconv_conversion_error_occurred(&self) -> bool {
        self.iconv_conversion_error
    }

    /// Reports whether any entry was skipped because its creation is
    /// unsupported on this platform without privilege (a Windows unprivileged
    /// file symlink), so the transfer can finish with exit code 23
    /// (`RERR_PARTIAL`) even though every other entry was copied.
    pub(super) const fn unsupported_operation_skipped(&self) -> bool {
        self.unsupported_operation_skipped
    }

    /// Records that a `--remove-source-files` source could not be removed - it
    /// was refused by a safety guard (changed since it was copied, or it is the
    /// same inode as the destination) or its unlink failed. The run continues
    /// but finishes `RERR_PARTIAL` (exit 23).
    ///
    /// upstream: `sender.c:successful_send()` sets `got_xfer_error` via
    /// `FERROR_XFER` and returns without aborting; `main.c:1630` then exits
    /// `RERR_PARTIAL`.
    pub(super) fn record_sender_remove_error(&mut self) {
        self.sender_remove_error = true;
    }

    /// Reports whether any `--remove-source-files` removal was refused or
    /// failed, so the transfer can finish with exit code 23 (`RERR_PARTIAL`)
    /// even though every other entry was copied.
    pub(super) const fn sender_remove_error_occurred(&self) -> bool {
        self.sender_remove_error
    }

    /// Records that the delete emitter stepped over a genuine unlink/rmdir
    /// error during a recursive peel. The pass still deletes the rest of the
    /// tree; this flag only forces the final `RERR_PARTIAL` (exit 23) exit
    /// code.
    ///
    /// upstream: `delete.c:86-210` - `delete_dir_contents` / `delete_item`
    /// log each un-removable entry via `rsyserr(FERROR_XFER, ...)` and set
    /// `io_error |= IOERR_GENERAL`; `main.c` then exits `RERR_PARTIAL`.
    pub(super) fn record_delete_io_error(&mut self) {
        self.delete_io_error = true;
    }

    /// Reports whether a swallowed delete-pass error must force
    /// `RERR_PARTIAL` (exit 23). `--ignore-errors` suppresses it, matching
    /// upstream where the flag clears `IOERR_GENERAL` for the delete pass.
    pub(super) const fn io_error_requires_partial_exit(&self) -> bool {
        self.delete_io_error && !self.options.ignore_errors_enabled()
    }

    /// Reports whether deletions should proceed despite I/O errors.
    ///
    /// Returns `true` if:
    /// - No I/O errors occurred, OR
    /// - `--ignore-errors` is enabled
    pub(super) const fn deletions_allowed(&self) -> bool {
        !self.io_errors_occurred || self.options.ignore_errors_enabled()
    }

    /// Returns `true` when the delete pass must be skipped because a general
    /// I/O error occurred and `--ignore-errors` was not given, emitting the
    /// upstream skip notice exactly once.
    ///
    /// The message renders at the default verbosity through the `NONREG`
    /// info category (info_verbosity[0], enabled at verbose level 0), the
    /// same channel oc uses for the sibling "skipping non-regular file"
    /// notice. `--ignore-errors` keeps [`Self::deletions_allowed`] true, so
    /// neither the warning nor the skip fires in that case - matching
    /// upstream, where the flag both suppresses the notice and lets the
    /// delete pass run.
    // upstream: generator.c:304-311 delete_in_dir() prints "IO error
    // encountered -- skipping file deletion" once (guarded by a static
    // `already_warned`) and returns without deleting whenever
    // `io_error & IOERR_GENERAL && !ignore_errors`.
    pub(super) fn delete_pass_blocked_by_io_error(&mut self) -> bool {
        if self.deletions_allowed() {
            return false;
        }
        if !self.io_error_delete_warning_emitted {
            self.io_error_delete_warning_emitted = true;
            info_log!(Nonreg, 1, "IO error encountered -- skipping file deletion");
        }
        true
    }
}
