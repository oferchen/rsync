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
        let dynamic_dir_merge_stack: Vec<DynamicDirMergeFrame> = Vec::new();
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

        let adaptive_level = if options.compress_enabled() && options.adaptive_compress_enabled() {
            let initial = options.compression_level();
            let level_i32 = match initial {
                CompressionLevel::None => 0,
                CompressionLevel::Fast => 1,
                CompressionLevel::Default => 6,
                CompressionLevel::Best => 9,
                CompressionLevel::Precise(v) => i32::from(v.get()),
            };
            match options.compression_algorithm() {
                CompressionAlgorithm::Zlib => Some(
                    compress::strategy::adaptive_level::AdaptiveLevelController::for_zlib(
                        level_i32,
                    ),
                ),
                #[cfg(feature = "zstd")]
                CompressionAlgorithm::Zstd => Some(
                    compress::strategy::adaptive_level::AdaptiveLevelController::for_zstd(
                        level_i32,
                    ),
                ),
                _ => None,
            }
        } else {
            None
        };

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
            dynamic_dir_merge_stack: Rc::new(RefCell::new(dynamic_dir_merge_stack)),
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
            destination_metadata_cache: HashMap::new(),
            io_errors_occurred: false,
            io_error_delete_warning_emitted: false,
            iconv_conversion_error: false,
            multi_source: false,
            verified_parents: HashMap::new(),
            batch_flist_writer,
            batch_delta_buf,
            batch_delta_entries: Vec::new(),
            batch_entry_sort_data: Vec::new(),
            batch_current_delta_idx: 0,
            batch_flist_index: 0,
            batch_ndx_codec,
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
    /// generator.c:1540 exits before `set_file_attrs()` and therefore never
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

        // upstream: rsync.c:954-965 - when `-p`/`--chmod` are off, the
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
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
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

    /// Searches `--link-dest` directories for the BEST file matching the source.
    ///
    /// upstream: generator.c:954-983 `try_dests_reg()` scans every basis dir and
    /// tracks the highest match_level (2 = data matches `quick_check_ok`, 3 = data
    /// and attributes both match `unchanged_attrs`), breaking early only on an
    /// exact (level-3) match. Returning the first data-only candidate instead
    /// would let an earlier match_level-2 basis (attrs differ) shadow a later
    /// exact one, forcing an unnecessary copy + attr reapply where upstream would
    /// hard-link the exact basis with no reapply. The caller re-derives the
    /// winning candidate's level to choose hard-link vs copy, so returning the
    /// best candidate is sufficient to mirror upstream.
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

        let metadata_options = self.metadata_options();
        let preserve_xattrs = {
            #[cfg(all(unix, feature = "xattr"))]
            {
                self.options.preserve_xattrs()
            }
            #[cfg(not(all(unix, feature = "xattr")))]
            {
                false
            }
        };

        let mut best: Option<(PathBuf, u8)> = None;
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

            if !should_skip_copy(CopyComparison {
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
                continue;
            }

            // At least match_level 2 (data matches); match_level 3 when the
            // preserved attributes also match, which the caller hard-links.
            let level = if crate::local_copy::reference_attrs_unchanged(
                &candidate,
                source,
                metadata,
                &metadata_options,
                preserve_xattrs,
            ) {
                3
            } else {
                2
            };

            if best.as_ref().is_none_or(|(_, best_level)| level > *best_level) {
                best = Some((candidate, level));
            }
            // upstream: generator.c:979 - an exact match ends the scan.
            if level == 3 {
                break;
            }
        }

        Ok(best.map(|(candidate, _)| candidate))
    }

    /// Locates a `--link-dest` basis symlink at `relative` that points at the
    /// same `target`.
    ///
    /// Returns the basis symlink path when a link-dest entry holds a symlink
    /// with a matching target, so the receiver can hard-link the symlink into
    /// place (`hL`) instead of recreating it.
    ///
    /// upstream: generator.c:1117-1134 try_dests_non() - LINK_DEST hard-links a
    /// matching symlink from the basis when CAN_HARDLINK_SYMLINK is supported.
    pub(super) fn link_dest_symlink_target(
        &self,
        relative: &Path,
        target: &Path,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest symlink",
                        candidate,
                        error,
                    ));
                }
            };

            if !candidate_metadata.file_type().is_symlink() {
                continue;
            }

            match fs::read_link(&candidate) {
                Ok(basis_target) if basis_target == target => return Ok(Some(candidate)),
                Ok(_) => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "read link-dest symlink",
                        candidate,
                        error,
                    ));
                }
            }
        }

        Ok(None)
    }

    /// Locates a `--link-dest` basis device or special file at `relative` that
    /// exactly matches the source node, returning its path so the receiver can
    /// hard-link it into place (`hD`/`hS` + blank) instead of recreating it.
    ///
    /// Mirrors upstream `generator.c:1052-1140` try_dests_non(): a `LINK_DEST`
    /// basis entry of the same file-type bucket (`FT_DEVICE`/`FT_SPECIAL`) whose
    /// device number (devices) or `_S_IFMT` (specials) matches
    /// (`generator.c:657-671` quick_check_ok) AND whose preserved attributes are
    /// unchanged (`generator.c:461-500` unchanged_attrs) reaches match_level 3
    /// and is hard-linked, itemizing as an exact match. `CAN_HARDLINK_SPECIAL`
    /// is defined on Linux, so devices and specials participate.
    #[cfg(unix)]
    pub(super) fn link_dest_special_target(
        &self,
        relative: &Path,
        metadata: &fs::Metadata,
        metadata_options: &MetadataOptions,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        let source_type = metadata.file_type();
        let source_is_device = source_type.is_block_device() || source_type.is_char_device();
        let source_is_special = source_type.is_fifo() || source_type.is_socket();
        if !source_is_device && !source_is_special {
            return Ok(None);
        }

        let modify_window = self.options.modify_window();

        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::symlink_metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest special",
                        candidate,
                        error,
                    ));
                }
            };

            let cand_type = candidate_metadata.file_type();

            // upstream: generator.c:1076 - the basis must share the source's
            // file-type bucket, and generator.c:657-671 quick_check_ok compares
            // st_rdev (devices) or _S_IFMT (specials, i.e. fifo vs socket).
            if source_is_device {
                if !(cand_type.is_block_device() || cand_type.is_char_device()) {
                    continue;
                }
                if metadata.rdev() != candidate_metadata.rdev() {
                    continue;
                }
            } else if source_type.is_fifo() != cand_type.is_fifo()
                || source_type.is_socket() != cand_type.is_socket()
            {
                continue;
            }

            // upstream: generator.c:461-500 unchanged_attrs - preserved mtime,
            // perms and ownership must match for the match_level-3 hard-link.
            if metadata_options.times()
                && !mtimes_within_window(metadata, &candidate_metadata, modify_window)
            {
                continue;
            }
            if metadata_options.permissions()
                && (metadata.mode() & 0o7777) != (candidate_metadata.mode() & 0o7777)
            {
                continue;
            }
            if metadata_options.owner() && metadata.uid() != candidate_metadata.uid() {
                continue;
            }
            if metadata_options.group() && metadata.gid() != candidate_metadata.gid() {
                continue;
            }

            return Ok(Some(candidate));
        }

        Ok(None)
    }

    /// Non-Unix stub: device and special nodes cannot be materialised, so no
    /// `--link-dest` basis can ever match one.
    #[cfg(not(unix))]
    pub(super) fn link_dest_special_target(
        &self,
        _relative: &Path,
        _metadata: &fs::Metadata,
        _metadata_options: &MetadataOptions,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
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
                self.deferred_ops
                    .delay_staging_dirs
                    .insert(parent.to_path_buf());
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
    ///
    /// Emits an `--info=BACKUP` notice mirroring upstream rsync 3.4.1
    /// (backup.c:352) under `INFO_GTE(BACKUP, 1)` once the backup has been
    /// placed successfully. The wording matches upstream byte-for-byte:
    /// `backed up <fname> to <buf>`.
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

        // Track which backup strategy succeeded so we can emit the matching
        // upstream `--debug=BACKUP` trace (RENAME, COPY, or SYMLINK).
        // upstream: backup.c:link_or_rename and the fall-through copy_file path.
        let strategy = match fs::rename(destination, &backup_path) {
            Ok(()) => BackupStrategy::Rename,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            // upstream: backup.c:247-256 - link_or_rename failing with EEXIST or
            // EISDIR is recoverable: lstat the target and call delete_item with
            // DEL_RECURSE, then retry. EISDIR fires when the backup-dir already
            // contains a directory at the path we need (e.g. user pre-created
            // it, or a previous backup left a tree there); without this arm,
            // backup test 4 from upstream backup.test fails as exit-23 fatal.
            // Windows reports renaming a file onto an existing directory as
            // ERROR_ACCESS_DENIED (PermissionDenied) rather than EEXIST/EISDIR,
            // so it must enter the same recovery arm; the inner symlink_metadata
            // re-stat below still gates removal on the target actually existing,
            // so a genuine permission error falls through to the retry-and-fail
            // path unchanged.
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    || error.kind() == io::ErrorKind::IsADirectory
                    || error.kind() == io::ErrorKind::PermissionDenied =>
            {
                match fs::symlink_metadata(&backup_path) {
                    Ok(meta) if meta.is_dir() => {
                        fs::remove_dir_all(&backup_path).map_err(|remove_error| {
                            LocalCopyError::io(
                                "remove existing backup directory",
                                backup_path.clone(),
                                remove_error,
                            )
                        })?;
                    }
                    Ok(_) => {
                        if let Err(remove_error) = fs::remove_file(&backup_path)
                            && remove_error.kind() != io::ErrorKind::NotFound
                        {
                            return Err(LocalCopyError::io(
                                "remove existing backup",
                                backup_path,
                                remove_error,
                            ));
                        }
                    }
                    Err(meta_error) if meta_error.kind() == io::ErrorKind::NotFound => {}
                    Err(meta_error) => {
                        return Err(LocalCopyError::io(
                            "stat existing backup",
                            backup_path,
                            meta_error,
                        ));
                    }
                }
                fs::rename(destination, &backup_path).map_err(|rename_error| {
                    LocalCopyError::io("create backup", backup_path.clone(), rename_error)
                })?;
                BackupStrategy::Rename
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                // upstream: backup.c:290 - when copying across devices, the
                // symlink fallback honours --safe-links. Unsafe symlinks are
                // not recreated at the backup location and SYMSAFE,1 fires.
                if file_type.is_symlink()
                    && self.options.safe_links_enabled()
                    && let Ok(target) = fs::read_link(destination)
                {
                    let safety_rel = destination
                        .strip_prefix(self.destination_root())
                        .unwrap_or(destination);
                    if !symlink_target_is_safe(&target, safety_rel) {
                        // upstream: backup.c:291 - INFO_GTE(SYMSAFE, 1)
                        info_log!(
                            Symsafe,
                            1,
                            "not backing up unsafe symlink \"{}\" -> \"{}\"",
                            destination.display(),
                            target.display()
                        );
                        return Ok(());
                    }
                }
                copy_entry_to_backup(destination, &backup_path, file_type)?;
                if file_type.is_symlink() {
                    BackupStrategy::Symlink
                } else {
                    BackupStrategy::Copy
                }
            }
            Err(error) => {
                return Err(LocalCopyError::io("create backup", backup_path, error));
            }
        };

        // upstream: backup.c:201-202,216-217,299-300,333-334 - DEBUG_GTE(BACKUP, 1)
        // emits one of HLINK/RENAME/SYMLINK/COPY per success path. oc-rsync's
        // local-copy executor uses rename as the primary strategy and falls back
        // to copy or symlink across filesystem boundaries.
        let destination_display = destination.display().to_string();
        match strategy {
            BackupStrategy::Rename => trace_make_backup_rename(&destination_display),
            BackupStrategy::Copy => trace_make_backup_copy(&destination_display),
            BackupStrategy::Symlink => trace_make_backup_symlink(&destination_display),
        }

        // upstream: backup.c:353 - rprintf(FINFO, "backed up %s to %s\n", fname, buf)
        // emits fname and buf as the rsync-relative paths (e.g. "deep/name1"),
        // not absolute filesystem paths. Strip the destination_root prefix so
        // the message matches upstream byte-for-byte and grep-by-relative-path
        // assertions in the upstream backup.test pass.
        let dest_root = self.destination_root();
        let destination_rel = destination.strip_prefix(dest_root).unwrap_or(destination);
        let backup_rel = backup_path
            .strip_prefix(dest_root)
            .unwrap_or(backup_path.as_path());
        info_log!(
            Backup,
            1,
            "backed up {} to {}",
            destination_rel.display(),
            backup_rel.display()
        );

        Ok(())
    }

    /// Forcibly removes a type-conflicting destination entry (backing it up
    /// first if needed) to make room for an incoming item of a different type.
    ///
    /// upstream: generator.c:1240 recv_generator() clears the conflicting
    /// destination with `delete_item(fname, mode, del_opts | DEL_FOR_FILE)`.
    /// delete.c:178-194 `delete_item()` suppresses `log_delete()` and the
    /// `stats.deleted_files` bump whenever `flags & DEL_MAKE_ROOM` is set, so
    /// the make-room removal of the conflicting entry itself is silent and
    /// uncounted (unlike a genuine delete-pass deletion). When that entry is a
    /// directory, `delete_dir_contents()` (delete.c:83) recurses with
    /// DEL_MAKE_ROOM stripped, so its contents are itemized (`*deleting`) and
    /// counted like ordinary deletions while the directory node stays silent.
    pub(super) fn force_remove_destination(
        &mut self,
        destination: &Path,
        relative: Option<&Path>,
        metadata: &fs::Metadata,
    ) -> Result<(), LocalCopyError> {
        let file_type = metadata.file_type();

        if self.mode.is_dry_run() {
            if file_type.is_dir() {
                self.record_make_room_contents(destination, relative)?;
            }
            self.register_progress();
            return Ok(());
        }

        self.backup_existing_entry(destination, relative, file_type)?;

        if file_type.is_dir() {
            self.record_make_room_contents(destination, relative)?;
        }

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

        self.register_progress();

        Ok(())
    }

    /// Itemizes and counts the contents of a conflicting directory that is
    /// being cleared to make room for an incoming item, mirroring upstream's
    /// `delete_dir_contents()` recursion (delete.c:83): the children are
    /// reported like delete-pass deletions (DEL_MAKE_ROOM stripped) while the
    /// directory node itself is removed silently by the caller.
    fn record_make_room_contents(
        &mut self,
        destination: &Path,
        relative: Option<&Path>,
    ) -> Result<(), LocalCopyError> {
        let mut subtree_path = destination.to_path_buf();
        let mut subtree_relative = relative.map(Path::to_path_buf).unwrap_or_else(|| {
            destination
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_default()
        });
        record_directory_subtree(self, &mut subtree_path, &mut subtree_relative)
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
                // upstream: rsync.c:954-965 - deferred updates have already
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
    // upstream: generator.c:298-305 delete_in_dir() prints "IO error
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

/// Returns `true` when two nodes' modification times are equal within
/// `--modify-window`.
///
/// upstream: util1.c:1478 same_time() - a whole-second delta within
/// `modify_window` counts as unchanged; with a zero window the sub-second
/// component must also match.
#[cfg(unix)]
fn mtimes_within_window(
    source: &fs::Metadata,
    candidate: &fs::Metadata,
    modify_window: Duration,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    let delta = source.mtime().abs_diff(candidate.mtime());
    let window = modify_window.as_secs();
    if window == 0 {
        delta == 0 && source.mtime_nsec() == candidate.mtime_nsec()
    } else {
        delta <= window
    }
}
