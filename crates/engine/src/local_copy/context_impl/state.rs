impl<'a> CopyContext<'a> {
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
            deferred_deletions: Vec::new(),
            deferred_updates: Vec::new(),
            timeout,
            stop_deadline,
            stop_at: stop_at_wallclock,
            last_progress: Instant::now(),
            created_entries: Vec::new(),
            destination_root,
        }
    }

    pub(super) fn register_progress(&mut self) {
        self.last_progress = Instant::now();
    }

    pub(super) fn enforce_timeout(&mut self) -> Result<(), LocalCopyError> {
        if let Some(limit) = self.timeout {
            if self.last_progress.elapsed() > limit {
                return Err(LocalCopyError::timeout(limit));
            }
        }
        if let Some(deadline) = self.stop_deadline {
            if Instant::now() >= deadline {
                let target = self
                    .stop_at
                    .unwrap_or_else(std::time::SystemTime::now);
                return Err(LocalCopyError::stop_at_reached(target));
            }
        }
        Ok(())
    }

    pub(super) fn mode(&self) -> LocalCopyExecution {
        self.mode
    }

    pub(super) fn options(&self) -> &LocalCopyOptions {
        &self.options
    }

    pub(super) fn one_file_system_enabled(&self) -> bool {
        self.options.one_file_system_enabled()
    }

    pub(super) fn record_hard_link(&mut self, metadata: &fs::Metadata, destination: &Path) {
        if self.options.hard_links_enabled() {
            self.hard_links.record(metadata, destination);
        }
    }

    pub(super) fn existing_hard_link_target(&self, metadata: &fs::Metadata) -> Option<PathBuf> {
        if self.options.hard_links_enabled() {
            self.hard_links.existing_target(metadata)
        } else {
            None
        }
    }

    pub(super) fn delay_updates_enabled(&self) -> bool {
        self.options.delay_updates_enabled()
    }

    pub(super) fn destination_root(&self) -> &Path {
        &self.destination_root
    }

    pub(super) fn apply_metadata_and_finalize(
        &mut self,
        destination: &Path,
        params: FinalizeMetadataParams<'_>,
    ) -> Result<(), LocalCopyError> {
        let FinalizeMetadataParams {
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        } = params;
        self.register_created_path(
            destination,
            CreatedEntryKind::File,
            destination_previously_existed,
        );
        apply_file_metadata_with_options(destination, metadata, metadata_options)
            .map_err(map_metadata_error)?;
        #[cfg(feature = "xattr")]
        {
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                self.filter_program.as_ref(),
            )?;
        }
        #[cfg(feature = "acl")]
        {
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
        }
        #[cfg(not(any(feature = "xattr", feature = "acl")))]
        let _ = mode;
        self.record_hard_link(metadata, destination);
        remove_source_entry_if_requested(self, source, relative, file_type)?;
        Ok(())
    }

    pub(super) fn link_dest_target(
        &self,
        relative: &Path,
        source: &Path,
        metadata: &fs::Metadata,
        metadata_options: &MetadataOptions,
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
                options: metadata_options,
                size_only,
                ignore_times,
                checksum,
                checksum_algorithm: self.options.checksum_algorithm(),
                modify_window: self.options.modify_window(),
            }) {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
    }

    pub(super) fn reference_directories(&self) -> &[ReferenceDirectory] {
        self.options.reference_directories()
    }

    pub(super) fn register_deferred_update(&mut self, update: DeferredUpdate) {
        let metadata = update.metadata.clone();
        let destination = update.destination.clone();
        self.record_hard_link(&metadata, destination.as_path());
        self.deferred_updates.push(update);
    }

    pub(super) fn commit_deferred_update_for(
        &mut self,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        if let Some(index) = self
            .deferred_updates
            .iter()
            .position(|update| update.destination.as_path() == destination)
        {
            let update = self.deferred_updates.swap_remove(index);
            self.finalize_deferred_update(update)?;
        }
        Ok(())
    }

    pub(super) fn flush_deferred_updates(&mut self) -> Result<(), LocalCopyError> {
        if self.deferred_updates.is_empty() {
            return Ok(());
        }

        let updates = std::mem::take(&mut self.deferred_updates);
        for update in updates {
            self.finalize_deferred_update(update)?;
        }
        Ok(())
    }

    pub(super) fn backup_existing_entry(
        &mut self,
        destination: &Path,
        relative: Option<&Path>,
        file_type: fs::FileType,
    ) -> Result<(), LocalCopyError> {
        if !self.options.backup_enabled() || self.mode.is_dry_run() {
            return Ok(());
        }

        if file_type.is_dir() {
            return Ok(());
        }

        let backup_path = compute_backup_path(
            self.destination_root(),
            destination,
            relative,
            self.options.backup_directory(),
            self.options.backup_suffix(),
        );

        if let Some(parent) = backup_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create backup directory", parent.to_path_buf(), error)
                })?;
            }
        }

        match fs::rename(destination, &backup_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if let Err(remove_error) = fs::remove_file(&backup_path) {
                    if remove_error.kind() != io::ErrorKind::NotFound {
                        return Err(LocalCopyError::io(
                            "remove existing backup",
                            backup_path.clone(),
                            remove_error,
                        ));
                    }
                }
                fs::rename(destination, &backup_path).map_err(|rename_error| {
                    LocalCopyError::io("create backup", backup_path.clone(), rename_error)
                })?;
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                copy_entry_to_backup(destination, &backup_path, file_type)?;
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "create backup",
                    backup_path.clone(),
                    error,
                ));
            }
        }

        Ok(())
    }

    pub(super) fn finalize_deferred_update(
        &mut self,
        update: DeferredUpdate,
    ) -> Result<(), LocalCopyError> {
        let DeferredUpdate {
            guard,
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            destination,
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        } = update;

        #[cfg(not(any(feature = "xattr", feature = "acl")))]
        let _ = &source;

        guard.commit()?;

        self.apply_metadata_and_finalize(
            destination.as_path(),
            FinalizeMetadataParams {
                metadata: &metadata,
                metadata_options,
                mode,
                source: source.as_path(),
                relative: relative.as_deref(),
                file_type,
                destination_previously_existed,
                #[cfg(feature = "xattr")]
                preserve_xattrs,
                #[cfg(feature = "acl")]
                preserve_acls,
            },
        )
    }

    pub(super) fn delete_timing(&self) -> Option<DeleteTiming> {
        self.options.delete_timing()
    }

    pub(super) fn min_file_size_limit(&self) -> Option<u64> {
        self.options.min_file_size_limit()
    }

    pub(super) fn max_file_size_limit(&self) -> Option<u64> {
        self.options.max_file_size_limit()
    }

}
