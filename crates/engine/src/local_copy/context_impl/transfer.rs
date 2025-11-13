impl<'a> CopyContext<'a> {
    fn start_compressor(
        &self,
        compress: bool,
        source: &Path,
    ) -> Result<Option<ActiveCompressor>, LocalCopyError> {
        if !compress {
            return Ok(None);
        }

        ActiveCompressor::new(self.compression_algorithm(), self.compression_level())
            .map(Some)
            .map_err(|error| {
                LocalCopyError::io("initialise compression", source.to_path_buf(), error)
            })
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

    pub(super) fn enter_directory(
        &self,
        source: &Path,
    ) -> Result<DirectoryFilterGuard, LocalCopyError> {
        let Some(program) = &self.filter_program else {
            let handles = DirectoryFilterHandles {
                layers: Rc::clone(&self.dir_merge_layers),
                marker_layers: Rc::clone(&self.dir_merge_marker_layers),
                ephemeral: Rc::clone(&self.dir_merge_ephemeral),
                marker_ephemeral: Rc::clone(&self.dir_merge_marker_ephemeral),
            };
            return Ok(DirectoryFilterGuard::new(
                handles,
                Vec::new(),
                Vec::new(),
                false,
                false,
            ));
        };

        let mut added_indices = Vec::new();
        let mut marker_counts = Vec::new();
        let mut layers = self.dir_merge_layers.borrow_mut();
        let mut marker_layers = self.dir_merge_marker_layers.borrow_mut();
        let mut ephemeral_stack = self.dir_merge_ephemeral.borrow_mut();
        let mut marker_ephemeral_stack = self.dir_merge_marker_ephemeral.borrow_mut();
        ephemeral_stack.push(Vec::new());
        marker_ephemeral_stack.push(Vec::new());

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
                        candidate.clone(),
                        error,
                    ));
                }
            };

            if !metadata.is_file() {
                continue;
            }

            let mut visited = Vec::new();
            let mut entries = match load_dir_merge_rules_recursive(
                candidate.as_path(),
                rule.options(),
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
                if let Err(error) = segment.push_rule(compiled) {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, error));
                }
            }

            if rule.options().excludes_self() {
                let pattern = rule.pattern().to_string_lossy().into_owned();
                if let Err(error) = segment.push_rule(FilterRule::exclude(pattern)) {
                    ephemeral_stack.pop();
                    marker_ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, error));
                }
            }

            let has_segment = !segment.is_empty();
            let markers = entries.exclude_if_present;
            if !has_segment && markers.is_empty() {
                continue;
            }

            if rule.options().inherit_rules() {
                if has_segment {
                    layers[index].push(segment);
                    added_indices.push(index);
                }
                if !markers.is_empty() {
                    let count = markers.len();
                    marker_layers[index].extend(markers.into_iter());
                    marker_counts.push((index, count));
                }
            } else {
                if has_segment {
                    if let Some(current) = ephemeral_stack.last_mut() {
                        current.push((index, segment));
                    }
                }
                if !markers.is_empty() {
                    if let Some(current) = marker_ephemeral_stack.last_mut() {
                        current.push((index, markers));
                    }
                }
            }
        }

        drop(layers);
        drop(marker_layers);
        drop(ephemeral_stack);
        drop(marker_ephemeral_stack);

        let excluded = self.directory_excluded(source, program)?;

        let handles = DirectoryFilterHandles {
            layers: Rc::clone(&self.dir_merge_layers),
            marker_layers: Rc::clone(&self.dir_merge_marker_layers),
            ephemeral: Rc::clone(&self.dir_merge_ephemeral),
            marker_ephemeral: Rc::clone(&self.dir_merge_marker_ephemeral),
        };
        Ok(DirectoryFilterGuard::new(
            handles,
            added_indices,
            marker_counts,
            true,
            excluded,
        ))
    }

    pub(super) fn directory_excluded(
        &self,
        directory: &Path,
        program: &FilterProgram,
    ) -> Result<bool, LocalCopyError> {
        if program.should_exclude_directory(directory)? {
            return Ok(true);
        }

        {
            let layers = self.dir_merge_marker_layers.borrow();
            for rules in layers.iter() {
                if directory_has_marker(rules, directory)? {
                    return Ok(true);
                }
            }
        }

        {
            let stack = self.dir_merge_marker_ephemeral.borrow();
            if let Some(entries) = stack.last() {
                for (_, rules) in entries.iter() {
                    if directory_has_marker(rules, directory)? {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    pub(super) fn summary_mut(&mut self) -> &mut LocalCopySummary {
        &mut self.summary
    }

    pub(super) fn summary(&self) -> &LocalCopySummary {
        &self.summary
    }

    pub(super) fn record(&mut self, record: LocalCopyRecord) {
        if let Some(observer) = &mut self.observer {
            observer.handle(record.clone());
        }
        if let Some(events) = &mut self.events {
            events.push(record);
        }
    }

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
        start: Instant,
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
                start,
            );
        }

        let mut total_bytes: u64 = 0;
        let mut literal_bytes: u64 = 0;
        let mut sparse_state = SparseWriteState::default();
        let mut compressor = self.start_compressor(compress, source)?;
        let mut compressed_progress: u64 = 0;
        let expected_remaining = total_size.saturating_sub(initial_bytes);

        loop {
            if total_bytes >= expected_remaining {
                break;
            }
            self.enforce_timeout()?;
            let chunk_len = if let Some(limiter) = self.limiter.as_ref() {
                limiter.recommended_read_size(buffer.len())
            } else {
                buffer.len()
            };

            let read = reader
                .read(&mut buffer[..chunk_len])
                .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
            if read == 0 {
                break;
            }

            let written = if sparse {
                write_sparse_chunk(writer, &mut sparse_state, &buffer[..read], destination)?
            } else {
                writer.write_all(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("copy file", destination.to_path_buf(), error)
                })?;
                read
            };

            self.register_progress();

            let mut compressed_delta = None;
            if let Some(encoder) = compressor.as_mut() {
                encoder.write(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("compress file", source.to_path_buf(), error)
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
            literal_bytes = literal_bytes.saturating_add(written as u64);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
        }

        if sparse {
            sparse_state.finish(writer, destination)?;
            let final_len = initial_bytes.saturating_add(total_bytes);
            writer.set_len(final_len).map_err(|error| {
                LocalCopyError::io(
                    "truncate destination file",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            self.register_progress();
        }

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            self.register_progress();
            let delta = compressed_total.saturating_sub(compressed_progress);
            self.register_limiter_bytes(delta);
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

}
