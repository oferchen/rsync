pub(super) struct SparseCopy<'state> {
    enabled: bool,
    state: &'state mut SparseWriteState,
}

impl<'a> CopyContext<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn copy_file_contents_with_delta(
        &mut self,
        reader: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        sparse: bool,
        compress: bool,
        source: &Path,
        destination: &Path,
        relative: &Path,
        index: &DeltaSignatureIndex,
        total_size: u64,
        initial_bytes: u64,
        start: Instant,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        let mut destination_reader = fs::File::open(destination).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?;
        let mut compressor = self.start_compressor(compress, source)?;
        let mut compressed_progress = 0u64;
        let mut total_bytes = 0u64;
        let mut literal_bytes = 0u64;
        let mut sparse_state = SparseWriteState::default();
        let mut window: VecDeque<u8> = VecDeque::with_capacity(index.block_length());
        let mut pending_literals = Vec::with_capacity(index.block_length());
        let mut scratch = Vec::with_capacity(index.block_length());
        let mut rolling = RollingChecksum::new();
        let mut outgoing: Option<u8> = None;
        let mut read_buffer = vec![0u8; buffer.len().max(index.block_length())];
        let mut buffer_len = 0usize;
        let mut buffer_pos = 0usize;

        loop {
            self.enforce_timeout()?;
            if buffer_pos == buffer_len {
                buffer_len = reader.read(&mut read_buffer).map_err(|error| {
                    LocalCopyError::io("copy file", source.to_path_buf(), error)
                })?;
                buffer_pos = 0;
                if buffer_len == 0 {
                    break;
                }
            }

            let byte = read_buffer[buffer_pos];
            buffer_pos += 1;

            window.push_back(byte);
            if let Some(outgoing_byte) = outgoing.take() {
                debug_assert!(window.len() <= index.block_length());
                rolling
                    .roll_many(&[outgoing_byte], &[byte])
                    .map_err(|_| {
                        LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::UnsupportedFileType,
                        )
                    })?;
            } else {
                rolling.update(&[byte]);
            }

            if window.len() < index.block_length() {
                continue;
            }

            let digest = rolling.digest();
            if let Some(block_index) = index.find_match_window(digest, &window, &mut scratch) {
                if !pending_literals.is_empty() {
                    let flushed_len = pending_literals.len();
                    let flushed = self.flush_literal_chunk(
                        writer,
                        pending_literals.as_slice(),
                        sparse,
                        &mut sparse_state,
                        compressor.as_mut(),
                        &mut compressed_progress,
                        source,
                        destination,
                    )?;
                    let literal_written = if sparse {
                        flushed_len as u64
                    } else {
                        flushed as u64
                    };
                    literal_bytes = literal_bytes.saturating_add(literal_written);
                    total_bytes = total_bytes.saturating_add(flushed_len as u64);
                    let progressed = initial_bytes.saturating_add(total_bytes);
                    self.notify_progress(
                        relative,
                        Some(total_size),
                        progressed,
                        start.elapsed(),
                    );
                    pending_literals.clear();
                }

                let block = index.block(block_index);
                let block_len = block.len();
                let matched = MatchedBlock::new(block, index.block_length());
                self.copy_matched_block(
                    &mut destination_reader,
                    writer,
                    buffer,
                    destination,
                    matched,
                    SparseCopy {
                        enabled: sparse,
                        state: &mut sparse_state,
                    },
                )?;
                total_bytes = total_bytes.saturating_add(block_len as u64);
                let progressed = initial_bytes.saturating_add(total_bytes);
                self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
                window.clear();
                rolling.reset();
                outgoing = None;
                continue;
            }

            if let Some(front) = window.pop_front() {
                pending_literals.push(front);
                outgoing = Some(front);
            }
        }

        while let Some(byte) = window.pop_front() {
            pending_literals.push(byte);
        }

        if !pending_literals.is_empty() {
            let flushed_len = pending_literals.len();
            let flushed = self.flush_literal_chunk(
                writer,
                pending_literals.as_slice(),
                sparse,
                &mut sparse_state,
                compressor.as_mut(),
                &mut compressed_progress,
                source,
                destination,
            )?;
            total_bytes = total_bytes.saturating_add(flushed_len as u64);
            let literal_written = if sparse {
                flushed_len as u64
            } else {
                flushed as u64
            };
            literal_bytes = literal_bytes.saturating_add(literal_written);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
        }

        if sparse {
            let final_position = sparse_state.finish(writer, destination)?;
            writer.set_len(final_position).map_err(|error| {
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
            let delta = compressed_total.saturating_sub(compressed_progress);
            self.register_limiter_bytes(delta);
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn flush_literal_chunk(
        &mut self,
        writer: &mut fs::File,
        chunk: &[u8],
        sparse: bool,
        state: &mut SparseWriteState,
        compressor: Option<&mut ActiveCompressor>,
        compressed_progress: &mut u64,
        source: &Path,
        destination: &Path,
    ) -> Result<usize, LocalCopyError> {
        if chunk.is_empty() {
            return Ok(0);
        }
        self.enforce_timeout()?;

        // Capture LITERAL operation to batch file if batch mode is active
        if let Some(batch_writer_arc) = self.batch_writer() {
            // Encode the literal operation in wire format
            let delta_op = protocol::wire::delta::DeltaOp::Literal(chunk.to_vec());
            let mut encoded = Vec::new();
            protocol::wire::delta::write_delta_op(&mut encoded, &delta_op).map_err(|e| {
                LocalCopyError::io(
                    "encode batch literal",
                    destination.to_path_buf(),
                    e,
                )
            })?;

            // Write encoded operation to batch file
            let mut writer_guard = batch_writer_arc.lock().unwrap();
            writer_guard.write_data(&encoded).map_err(|e| {
                LocalCopyError::io(
                    "write batch literal",
                    destination.to_path_buf(),
                    std::io::Error::new(std::io::ErrorKind::Other, e),
                )
            })?;
        }

        let written = if sparse {
            write_sparse_chunk(writer, state, chunk, destination)?
        } else {
            writer.write_all(chunk).map_err(|error| {
                LocalCopyError::io("copy file", destination.to_path_buf(), error)
            })?;
            chunk.len()
        };

        if let Some(encoder) = compressor {
            encoder.write(chunk).map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            let total = encoder.bytes_written();
            let delta = total.saturating_sub(*compressed_progress);
            *compressed_progress = total;
            self.register_limiter_bytes(delta);
        } else {
            self.register_limiter_bytes(chunk.len() as u64);
        }

        Ok(written)
    }

    pub(super) fn copy_matched_block(
        &mut self,
        existing: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        destination: &Path,
        matched: MatchedBlock<'_>,
        sparse: SparseCopy<'_>,
    ) -> Result<(), LocalCopyError> {
        let offset = matched.offset();
        let block_length = matched.descriptor().len();

        // Capture COPY operation to batch file if batch mode is active
        if let Some(batch_writer_arc) = self.batch_writer() {
            // Get the block index from the matched block
            let block_index = matched.descriptor().index();

            // Encode the copy operation in wire format
            // Note: block_index is u64 but DeltaOp::Copy takes u32
            let delta_op = protocol::wire::delta::DeltaOp::Copy {
                block_index: block_index as u32,
                length: block_length as u32,
            };

            let mut encoded = Vec::new();
            protocol::wire::delta::write_delta_op(&mut encoded, &delta_op).map_err(|e| {
                LocalCopyError::io(
                    "encode batch copy",
                    destination.to_path_buf(),
                    e,
                )
            })?;

            // Write encoded operation to batch file
            let mut writer_guard = batch_writer_arc.lock().unwrap();
            writer_guard.write_data(&encoded).map_err(|e| {
                LocalCopyError::io(
                    "write batch copy",
                    destination.to_path_buf(),
                    std::io::Error::new(std::io::ErrorKind::Other, e),
                )
            })?;
        }

        existing.seek(SeekFrom::Start(offset)).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?;

        let mut remaining = block_length;
        while remaining > 0 {
            self.enforce_timeout()?;
            let chunk_len = remaining.min(buffer.len());
            let read = existing.read(&mut buffer[..chunk_len]).map_err(|error| {
                LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            if read == 0 {
                let eof = io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading existing block",
                );
                return Err(LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    eof,
                ));
            }

            if sparse.enabled {
                let _ =
                    write_sparse_chunk(writer, sparse.state, &buffer[..read], destination)?;
            } else {
                writer.write_all(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("copy file", destination.to_path_buf(), error)
                })?;
            }

            remaining -= read;
        }

        Ok(())
    }
}
