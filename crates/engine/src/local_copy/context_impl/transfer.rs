impl<'a> CopyContext<'a> {
    /// Returns an interrupt-signal error once a shutdown signal has been
    /// received.
    ///
    /// Polled inside the per-chunk copy loops so a large single-file transfer
    /// stops promptly mid-file. Unwinding then runs the destination guard's
    /// finalisation (moving the temp onto its `--partial`/`--partial-dir`
    /// destination) in normal, non-signal context. upstream: receiver.c's
    /// per-block loop checks for a pending signal via `io_flush`/`check_timeout`.
    ///
    /// The abort is reported as [`LocalCopyError::interrupted`] (exit code 20,
    /// `RERR_SIGNAL`), not a per-file `RERR_PARTIAL`, so the single diagnostic
    /// matches upstream's `received SIGINT, SIGTERM, or SIGHUP (code 20)` line.
    fn check_shutdown(&self) -> Result<(), LocalCopyError> {
        if fast_io::signal::shutdown_requested() {
            return Err(LocalCopyError::interrupted());
        }
        Ok(())
    }

    fn start_compressor(
        &self,
        compress: bool,
        source: &Path,
    ) -> Result<Option<ActiveCompressor>, LocalCopyError> {
        if !compress {
            return Ok(None);
        }

        let level = if let Some(ctrl) = &self.adaptive_level {
            // Convert the adaptive strategy's recommended i32 level to a
            // CompressionLevel. The controller clamps to codec-valid bounds.
            let recommended = ctrl.current_level();
            if recommended == 0 {
                CompressionLevel::None
            } else if let Some(nz) = std::num::NonZeroU8::new(recommended as u8) {
                CompressionLevel::Precise(nz)
            } else {
                self.compression_level()
            }
        } else {
            self.compression_level()
        };

        ActiveCompressor::new_with_workers(
            self.compression_algorithm(),
            level,
            self.compression_threads(),
        )
        .map(Some)
        .map_err(|error| LocalCopyError::io("initialise compression", source, error))
    }

    /// Feeds compression ratio feedback to the adaptive level controller
    /// after a file finishes compressing.
    fn record_adaptive_compression(&mut self, input_bytes: u64, compressed_bytes: u64) {
        if let Some(ctrl) = &mut self.adaptive_level {
            ctrl.record_input_bytes(input_bytes);
            ctrl.record_output_bytes(compressed_bytes);
            ctrl.record_file_complete();
        }
    }

    fn register_limiter_bytes(&mut self, bytes: u64) {
        if bytes == 0 {
            return;
        }

        if let Some(limiter) = self.limiter.as_mut() {
            let bounded = bytes.min(usize::MAX as u64) as usize;
            let sleep = limiter.register(bounded);
            self.summary.record_bandwidth_sleep(sleep.requested());
        }
    }

    /// Loads per-dir-merge filter files for a source directory, pushing this
    /// directory's rules onto the shared source-side filter stacks. Returns a
    /// guard that pops them on drop.
    pub(super) fn enter_directory(
        &self,
        source: &Path,
        relative_dir: Option<&Path>,
    ) -> Result<DirectoryFilterGuard, LocalCopyError> {
        self.enter_directory_for_path(source, relative_dir, true, false, &self.dir_merge)
    }

    /// Loads per-dir-merge filter files from the destination directory before a
    /// deletion scan, pushing exactly this directory's rules onto the shared
    /// per-directory filter stacks and returning a guard that pops them (and
    /// restores any inherited entries a `clear` directive wiped) on drop.
    ///
    /// upstream: delete.c:63 - `delete_dir_contents()` calls
    /// `push_local_filters(fname, dlen)` with the destination directory so the
    /// receiver applies any `: filter` rules found in the directory being
    /// scanned for extraneous entries. The returned guard pops the loaded rules
    /// when it drops, mirroring the matching `pop_local_filters()` call on
    /// `delete.c:115`.
    ///
    /// # Incremental isolation (PEX, #333)
    ///
    /// The destination scan runs while the source-visible per-directory filter
    /// stacks are already populated to the current traversal depth. Rather than
    /// cloning the entire ancestor stack before the load and restoring it
    /// verbatim afterwards (O(depth) per visit, O(depth^2) across the walk),
    /// this pushes ONLY this directory's destination merge rules and undoes
    /// exactly that delta on drop:
    ///
    /// - the single dynamic frame, the single ephemeral frame, and every
    ///   `layers[index].push` / `marker_layers[index].extend` are balanced by
    ///   [`DirectoryFilterGuard`]'s normal per-index pop (identical to the
    ///   source-side guard);
    /// - the ONLY unbalanced mutation is a `clear` directive
    ///   (`clear_inherited`) calling `layers[index].clear()` /
    ///   `marker_layers[index].clear()`, which discards inherited entries the
    ///   per-index pop cannot restore (exclude.c:801 keeps inherited rules in
    ///   `lp->head` an arbitrary number of indices deep). Those wiped vectors
    ///   are captured before the clear and restored after the pop, so the
    ///   source-visible state is left byte-identical to before the load.
    ///
    /// This mirrors upstream's persistent `delete_filt` chain (`change_local_
    /// filter_dir` pops filters at depth > current, then pushes this dir's
    /// local filters) without ever cloning the source-side chain.
    pub(crate) fn enter_destination_for_deletion(
        &self,
        destination: &Path,
        relative_dir: Option<&Path>,
    ) -> Result<DirectoryFilterGuard, LocalCopyError> {
        self.enter_directory_for_path(destination, relative_dir, false, true, &self.delete_dir_merge)
    }

    /// Advances the persistent destination delete-filter chain to `destination`.
    ///
    /// Mirrors upstream `change_local_filter_dir` (exclude.c:875): pops every
    /// held guard whose depth is `>= destination`'s depth, then loads this
    /// destination directory's per-dir merge files onto the isolated
    /// [`Self::delete_dir_merge`] chain, inheriting the surviving ancestor
    /// frame's rules. The pushed guard is retained (not dropped at scope exit)
    /// so a parent's `.rsync-filter` rules keep protecting a subdirectory's
    /// entries from deletion. Called once per destination directory the delete
    /// pass visits, in depth-first (parent-before-child) order.
    ///
    /// During a `--delete-during`/`--delete-before` sweep the destination merge
    /// files are not present yet, so the loaded frames are empty and the chain
    /// protects nothing - matching upstream, where the receiver has not merged
    /// the directory's rules before the during-delete pass runs.
    pub(crate) fn sync_delete_filter_chain(
        &self,
        destination: &Path,
        relative_dir: Option<&Path>,
    ) -> Result<(), LocalCopyError> {
        let depth = relative_dir.map_or(0, |rel| rel.components().count());
        loop {
            let pop = self
                .delete_filter_chain
                .borrow()
                .last()
                .is_some_and(|(held_depth, _)| *held_depth >= depth);
            if !pop {
                break;
            }
            // Dropping the guard pops exactly this directory's frame off the
            // isolated delete chain (deepest-first, LIFO).
            self.delete_filter_chain.borrow_mut().pop();
        }
        let guard = self.enter_destination_for_deletion(destination, relative_dir)?;
        self.delete_filter_chain.borrow_mut().push((depth, guard));
        Ok(())
    }

    fn enter_directory_for_path(
        &self,
        directory: &Path,
        relative_dir: Option<&Path>,
        check_directory_excluded: bool,
        capture_cleared_layers: bool,
        stacks: &DirectoryFilterHandles,
    ) -> Result<DirectoryFilterGuard, LocalCopyError> {
        let source = directory;
        let Some(program) = &self.filter_program else {
            return Ok(DirectoryFilterGuard::new(
                stacks.clone(),
                Vec::new(),
                Vec::new(),
                false,
                false,
                false,
            ));
        };

        let mut added_indices = Vec::new();
        let mut marker_counts = Vec::new();
        // PEX (#333): when the caller is the destination-deletion scan, record
        // the pre-clear contents of any `layers[index]` / `marker_layers[index]`
        // a `clear` directive is about to wipe, so the guard can restore the
        // inherited entries the per-index pop cannot rebuild. Empty (and never
        // populated) on the source-side path, whose nested guards restore state
        // by unwinding in child-before-parent order.
        let mut cleared_layers: Vec<ClearedLayerRestore> = Vec::new();
        let mut layers = stacks.layers.borrow_mut();
        let mut marker_layers = stacks.marker_layers.borrow_mut();
        let mut ephemeral_stack = stacks.ephemeral.borrow_mut();
        let mut marker_ephemeral_stack = stacks.marker_ephemeral.borrow_mut();
        ephemeral_stack.push(Vec::new());
        marker_ephemeral_stack.push(Vec::new());

        // upstream: exclude.c:801 `push_local_filters` sets `lp->tail = NULL`,
        // keeping `lp->head` so rules loaded at an ancestor depth keep matching
        // descendants. Seed the new frame's active rules AND inheritable loaded
        // segments from the parent frame, then look each active rule up in this
        // directory. Non-inheritable (`n`-modifier) segments are dropped.
        let (inherited_active, inherited_segments): (Vec<NestedDirMerge>, Vec<LoadedDynamicSegment>) =
            stacks
                .dynamic
                .borrow()
                .last()
                .map(|frame| {
                    let segments = frame
                        .loaded_segments
                        .iter()
                        .filter(|loaded| loaded.inherit)
                        .cloned()
                        .collect();
                    (frame.active_rules.clone(), segments)
                })
                .unwrap_or_default();
        let mut new_frame = DynamicDirMergeFrame {
            active_rules: inherited_active,
            loaded_segments: inherited_segments,
            loaded_markers: Vec::new(),
        };

        for (index, rule) in program.dir_merge_rules().iter().enumerate() {
            let candidate = resolve_dir_merge_path(source, rule.pattern());

            let metadata = match fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(LocalCopyError::io(
                        "inspect filter file",
                        candidate,
                        error,
                    ));
                }
            };

            if !metadata.is_file() {
                continue;
            }

            record_filter_file_load();
            let mut visited = Vec::new();
            let mut entries = match load_dir_merge_rules_recursive(
                candidate.as_path(),
                rule.options(),
                self.options.delete_excluded_enabled(),
                &mut visited,
            ) {
                Ok(entries) => entries,
                Err(error) => {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(error);
                }
            };

            let mut segment = FilterSegment::default();
            for compiled in entries.rules.drain(..) {
                // upstream: exclude.c:200-207 add_rule - an anchored pattern in a
                // per-dir merge file is rooted at the merge file's directory, not
                // the transfer root: `pre_len = dirbuf_len - module_dirlen - 1`
                // prepends the dir prefix. Mirror that so `- /file1` in `foo/.filt`
                // matches `foo/file1`, not a top-level `file1`.
                let compiled = anchor_dir_merge_rule(compiled, relative_dir);
                if let Err(error) = segment.push_rule(compiled) {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, &error));
                }
            }

            if rule.options().excludes_self() {
                let pattern = rule.pattern().to_string_lossy().into_owned();
                if let Err(error) = segment.push_rule(FilterRule::exclude(pattern)) {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, &error));
                }
            }

            let has_segment = !segment.is_empty();
            let markers = entries.exclude_if_present;
            let clear_inherited = entries.clear_inherited;

            // upstream: exclude.c:787-789 - dir-merge directives found in a
            // top-level merge file register per-directory rules that the growing
            // push loop then loads against this same directory (and descendants).
            // Append them to the dynamic frame's active set; the growing while
            // loop below picks them up so `dir-merge .filt2` in `bar/.filt` loads
            // `bar/.filt2` here as well as in descendants.
            new_frame
                .active_rules
                .append(&mut entries.nested_dir_merges);

            // If the filter file had a clear directive, we should clear inherited rules
            // from parent directories before adding any new rules from this directory.
            if clear_inherited && rule.options().inherit_rules() {
                // PEX (#333): capture the inherited entries this clear discards
                // BEFORE wiping them, but only once per index and only for the
                // destination-deletion scan. The captured vectors are restored
                // after the guard's per-index pop so source-visible state is
                // left byte-identical (the per-index pop alone cannot rebuild
                // cleared inherited entries - exclude.c:801).
                if capture_cleared_layers
                    && !cleared_layers.iter().any(|restore| restore.index == index)
                {
                    cleared_layers.push(ClearedLayerRestore {
                        index,
                        layer: layers[index].clone(),
                        markers: marker_layers[index].clone(),
                    });
                }
                layers[index].clear();
                marker_layers[index].clear();
                // Remove any indices we may have added for parent directories
                // in this same traversal (shouldn't happen normally, but be safe)
                added_indices.retain(|&i| i != index);
                marker_counts.retain(|(i, _)| *i != index);
            }

            if !has_segment && markers.is_empty() && !clear_inherited {
                continue;
            }

            if rule.options().inherit_rules() {
                if has_segment {
                    layers[index].push(segment);
                    added_indices.push(index);
                }
                if !markers.is_empty() {
                    let count = markers.len();
                    marker_layers[index].extend(markers);
                    marker_counts.push((index, count));
                }
            } else {
                if has_segment
                    && let Some(current) = ephemeral_stack.last_mut()
                {
                    current.push((index, segment));
                }
                if !markers.is_empty()
                    && let Some(current) = marker_ephemeral_stack.last_mut()
                {
                    current.push((index, markers));
                }
            }
        }

        drop(layers);
        drop(marker_layers);

        // upstream: exclude.c:787-789 - walk the GROWING active-rule list so a
        // dir-merge directive registered while loading an inherited file (or a
        // top-level merge file above) is itself loaded against THIS directory.
        // Loading a rule may append further nested rules; the re-read bound then
        // picks them up. This makes `:C` load `.cvsignore` for the current dir
        // and a same-directory nested `dir-merge` apply to the current dir.
        let mut next_index = 0usize;
        while next_index < new_frame.active_rules.len() {
            let rule = new_frame.active_rules[next_index].clone();
            next_index += 1;
            let loaded = match self.load_nested_dir_merge(source, relative_dir, &rule) {
                Ok(loaded) => loaded,
                Err(error) => {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(error);
                }
            };
            let Some(mut loaded) = loaded else { continue };
            if let Some(segment) = loaded.segment.take() {
                new_frame.loaded_segments.push(LoadedDynamicSegment {
                    segment,
                    inherit: rule.options.inherit_rules(),
                });
            }
            new_frame.loaded_markers.append(&mut loaded.markers);
            new_frame.active_rules.append(&mut loaded.nested);
        }

        drop(ephemeral_stack);
        drop(marker_ephemeral_stack);

        stacks.dynamic.borrow_mut().push(new_frame);

        let excluded = if check_directory_excluded {
            self.directory_excluded(source, program)?
        } else {
            false
        };

        let handles = stacks.clone();
        Ok(
            DirectoryFilterGuard::new(handles, added_indices, marker_counts, true, true, excluded)
                .with_cleared_restores(cleared_layers),
        )
    }

    /// Resolves a single nested `dir-merge` rule against `source` and loads its
    /// filter file (if present), returning the compiled segment, any
    /// `exclude-if-present` markers, and any further nested dir-merge rules the
    /// file registered.
    ///
    /// Anchored patterns are rewritten relative to `relative_dir` to mirror
    /// upstream `exclude.c:200-207 add_rule`.
    fn load_nested_dir_merge(
        &self,
        source: &Path,
        relative_dir: Option<&Path>,
        rule: &NestedDirMerge,
    ) -> Result<Option<LoadedNestedDirMerge>, LocalCopyError> {
        let candidate = resolve_dir_merge_path(source, &rule.pattern);
        let metadata = match fs::metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(LocalCopyError::io("inspect filter file", candidate, error));
            }
        };
        if !metadata.is_file() {
            return Ok(None);
        }

        record_filter_file_load();
        let mut visited = Vec::new();
        let mut entries = load_dir_merge_rules_recursive(
            candidate.as_path(),
            &rule.options,
            self.options.delete_excluded_enabled(),
            &mut visited,
        )?;

        let mut segment = FilterSegment::default();
        for compiled in entries.rules.drain(..) {
            let compiled = anchor_dir_merge_rule(compiled, relative_dir);
            segment
                .push_rule(compiled)
                .map_err(|error| filter_program_local_error(&candidate, &error))?;
        }

        if rule.options.excludes_self() {
            let pattern = rule.pattern.to_string_lossy().into_owned();
            segment
                .push_rule(FilterRule::exclude(pattern))
                .map_err(|error| filter_program_local_error(&candidate, &error))?;
        }

        Ok(Some(LoadedNestedDirMerge {
            segment: (!segment.is_empty()).then_some(segment),
            markers: entries.exclude_if_present,
            nested: entries.nested_dir_merges,
        }))
    }

    /// Returns whether `directory` is excluded by an active per-dir filter
    /// program, either directly or via an `exclude-if-present` marker file at
    /// any layer (persistent, ephemeral, or dynamic dir-merge).
    pub(super) fn directory_excluded(
        &self,
        directory: &Path,
        program: &FilterProgram,
    ) -> Result<bool, LocalCopyError> {
        if program.should_exclude_directory(directory)? {
            return Ok(true);
        }

        {
            let layers = self.dir_merge.marker_layers.borrow();
            for rules in layers.iter() {
                if directory_has_marker(rules, directory)? {
                    return Ok(true);
                }
            }
        }

        {
            let stack = self.dir_merge.marker_ephemeral.borrow();
            if let Some(entries) = stack.last() {
                for (_, rules) in entries {
                    if directory_has_marker(rules, directory)? {
                        return Ok(true);
                    }
                }
            }
        }

        {
            let stack = self.dir_merge.dynamic.borrow();
            if let Some(frame) = stack.last()
                && directory_has_marker(&frame.loaded_markers, directory)?
            {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Returns a mutable reference to the running transfer summary.
    pub(super) const fn summary_mut(&mut self) -> &mut LocalCopySummary {
        &mut self.summary
    }

    /// Returns a reference to the running transfer summary.
    pub(super) const fn summary(&self) -> &LocalCopySummary {
        &self.summary
    }

    /// Records a completed copy action: updates the `--stats` created-entry
    /// counters, forwards the record to the observer, and appends it to the
    /// event log when event collection is enabled.
    pub(super) fn record(&mut self, record: LocalCopyRecord) {
        // upstream: receiver.c:733-746 / sender.c:295-308 - every ITEM_IS_NEW
        // entry bumps `stats.created_*` for its type, whether or not file data
        // moved. `was_created()` is the ITEM_IS_NEW mirror; classify by action
        // to hit the right per-type counter. Directories are counted separately
        // via `record_directory`/`mark_destination_root_created` (which also
        // covers the synthesized root that has no record here), so they are
        // skipped. Runs for both real and dry-run records, matching upstream's
        // dry-run `--stats` accounting.
        if record.was_created() {
            match record.action() {
                LocalCopyAction::DataCopied | LocalCopyAction::HardLink => {
                    self.summary.record_created_regular_file();
                }
                LocalCopyAction::SymlinkCopied => self.summary.record_created_symlink(),
                LocalCopyAction::DeviceCopied => self.summary.record_created_device(),
                LocalCopyAction::FifoCopied => self.summary.record_created_special(),
                _ => {}
            }
        }
        if let Some(observer) = &mut self.observer {
            observer.handle(record.clone());
        }
        if let Some(events) = &mut self.events {
            events.push(record);
        }
    }

    /// Records progress and, if an observer is registered, reports the
    /// current transfer position for `relative`.
    pub(super) fn notify_progress(
        &mut self,
        relative: &Path,
        total_bytes: Option<u64>,
        transferred: u64,
        elapsed: Duration,
    ) {
        self.register_progress();
        if self.observer.is_none() {
            return;
        }

        if let Some(observer) = &mut self.observer {
            observer.handle_progress(LocalCopyProgress::new(
                relative,
                transferred,
                total_bytes,
                elapsed,
            ));
        }
    }

    /// Copies file contents from `reader` to `writer`, dispatching to a delta,
    /// sparse, or plain streaming path depending on the arguments. Reports
    /// progress to the observer and enforces the bandwidth limiter and
    /// inactivity timeout along the way.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn copy_file_contents(
        &mut self,
        reader: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        sparse: bool,
        compress: bool,
        source: &Path,
        destination: &Path,
        relative: &Path,
        delta: Option<&DeltaSignatureIndex>,
        total_size: u64,
        initial_bytes: u64,
        preallocated_len: u64,
        start: Instant,
        basis_separate_from_writer: bool,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        if let Some(index) = delta {
            return self.copy_file_contents_with_delta(
                reader,
                writer,
                buffer,
                sparse,
                compress,
                source,
                destination,
                relative,
                index,
                total_size,
                initial_bytes,
                preallocated_len,
                start,
                basis_separate_from_writer,
            );
        }

        let expected_remaining = total_size.saturating_sub(initial_bytes);

        // Fast path: use copy_file_range for simple whole-file copies.
        // Requires no sparse detection, no compression, no bandwidth limiter.
        // Disabled for append mode (initial_bytes > 0) because copy_file_range
        // and io_uring on Linux do not reliably respect the seeked file position
        // when both source and destination have been seeked to non-zero offsets.
        // upstream: receiver.c - append path uses standard read/write loop.
        if !sparse && !compress && self.limiter.is_none() && initial_bytes == 0 {
            let copied = fast_io::copy_file_range::copy_file_contents_buffered(
                reader,
                writer,
                expected_remaining,
                buffer,
            )
            .map_err(|error| LocalCopyError::io("copy file", source, error))?;
            if self.observer.is_some() {
                let progressed = initial_bytes.saturating_add(copied);
                self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
            }
            return Ok(FileCopyOutcome::new(copied, None));
        }

        if sparse {
            return self.copy_file_contents_sparse(
                reader, writer, buffer, compress, source, destination, relative,
                total_size, initial_bytes, expected_remaining, preallocated_len, start,
            );
        }

        let mut total_bytes: u64 = 0;
        let mut literal_bytes: u64 = 0;
        let mut compressor = self.start_compressor(compress, source)?;
        let mut compressed_progress: u64 = 0;
        // 1MB interval amortizes clock_gettime syscalls across the copy loop.
        const TIMEOUT_CHECK_INTERVAL: u64 = 1024 * 1024;
        let mut bytes_since_timeout_check: u64 = 0;

        loop {
            if total_bytes >= expected_remaining {
                break;
            }
            self.check_shutdown()?;
            if bytes_since_timeout_check >= TIMEOUT_CHECK_INTERVAL {
                self.enforce_timeout()?;
                bytes_since_timeout_check = 0;
            }
            let chunk_len = if let Some(limiter) = self.limiter.as_ref() {
                limiter.recommended_read_size(buffer.len())
            } else {
                buffer.len()
            };

            let read = reader
                .read(&mut buffer[..chunk_len])
                .map_err(|error| LocalCopyError::io("copy file", source, error))?;
            if read == 0 {
                break;
            }

            writer.write_all(&buffer[..read]).map_err(|error| {
                LocalCopyError::io("copy file", destination, error)
            })?;

            self.register_progress();

            let mut compressed_delta = None;
            if let Some(encoder) = compressor.as_mut() {
                encoder.write(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("compress file", source, error)
                })?;
                let total = encoder.bytes_written();
                let delta = total.saturating_sub(compressed_progress);
                compressed_progress = total;
                compressed_delta = Some(delta);
            }

            if let Some(delta) = compressed_delta {
                self.register_limiter_bytes(delta);
            } else {
                self.register_limiter_bytes(read as u64);
            }

            total_bytes = total_bytes.saturating_add(read as u64);
            bytes_since_timeout_check = bytes_since_timeout_check.saturating_add(read as u64);
            literal_bytes = literal_bytes.saturating_add(read as u64);
            // Only compute elapsed time if we have an observer to report to
            if self.observer.is_some() {
                let progressed = initial_bytes.saturating_add(total_bytes);
                self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
            }
        }

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source, error)
            })?;
            self.register_progress();
            let delta = compressed_total.saturating_sub(compressed_progress);
            self.register_limiter_bytes(delta);
            self.record_adaptive_compression(literal_bytes, compressed_total);
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

    /// Sparse variant of `copy_file_contents`.
    ///
    /// Streams the source through the preallocation-aware [`SparseWriteState`]:
    /// zero runs beyond any preallocated extent become seeks (natural holes),
    /// while zero runs inside a preallocated extent are punched out so the
    /// reserved blocks are actually deallocated.
    // upstream: fileio.c:write_sparse()
    #[allow(clippy::too_many_arguments)]
    fn copy_file_contents_sparse(
        &mut self,
        reader: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        compress: bool,
        source: &Path,
        destination: &Path,
        relative: &Path,
        total_size: u64,
        initial_bytes: u64,
        expected_remaining: u64,
        preallocated_len: u64,
        start: Instant,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        let mut total_bytes: u64 = 0;
        let mut literal_bytes: u64 = 0;
        // On NTFS, seek-past-zero + set_len only yields a sparse file when the
        // handle is first flagged sparse via FSCTL_SET_SPARSE. Elsewhere this
        // is a no-op (holes are implicit). Best-effort: a non-NTFS volume or a
        // refused control code falls back to a dense write, never an error.
        let _ = fast_io::mark_file_sparse(writer);
        let mut sparse_state = SparseWriteState::default();
        sparse_state.set_preallocated_len(preallocated_len);
        let mut compressor = self.start_compressor(compress, source)?;
        let mut compressed_progress: u64 = 0;
        const TIMEOUT_CHECK_INTERVAL: u64 = 1024 * 1024;
        let mut bytes_since_timeout_check: u64 = 0;

        loop {
            if total_bytes >= expected_remaining {
                break;
            }
            self.check_shutdown()?;
            if bytes_since_timeout_check >= TIMEOUT_CHECK_INTERVAL {
                self.enforce_timeout()?;
                bytes_since_timeout_check = 0;
            }
            let chunk_len = if let Some(limiter) = self.limiter.as_ref() {
                limiter.recommended_read_size(buffer.len())
            } else {
                buffer.len()
            };

            let read = reader
                .read(&mut buffer[..chunk_len])
                .map_err(|error| LocalCopyError::io("copy file", source, error))?;
            if read == 0 {
                break;
            }

            write_sparse_chunk(writer, &mut sparse_state, &buffer[..read], destination)?;

            self.register_progress();

            let mut compressed_delta = None;
            if let Some(encoder) = compressor.as_mut() {
                encoder.write(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("compress file", source, error)
                })?;
                let total = encoder.bytes_written();
                let delta = total.saturating_sub(compressed_progress);
                compressed_progress = total;
                compressed_delta = Some(delta);
            }

            if let Some(delta) = compressed_delta {
                self.register_limiter_bytes(delta);
            } else {
                self.register_limiter_bytes(read as u64);
            }

            total_bytes = total_bytes.saturating_add(read as u64);
            bytes_since_timeout_check = bytes_since_timeout_check.saturating_add(read as u64);
            literal_bytes = literal_bytes.saturating_add(read as u64);
            if self.observer.is_some() {
                let progressed = initial_bytes.saturating_add(total_bytes);
                self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
            }
        }

        let final_position = sparse_state.finish(writer, destination)?;
        writer.set_len(final_position).map_err(|error| {
            LocalCopyError::io(
                "truncate destination file",
                destination.to_path_buf(),
                error,
            )
        })?;
        self.register_progress();

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source, error)
            })?;
            self.register_progress();
            let delta = compressed_total.saturating_sub(compressed_progress);
            self.register_limiter_bytes(delta);
            self.record_adaptive_compression(literal_bytes, compressed_total);
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

}
