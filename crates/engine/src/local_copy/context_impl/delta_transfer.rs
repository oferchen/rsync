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
        preallocated_len: u64,
        start: Instant,
        basis_separate_from_writer: bool,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        // upstream: receiver.c:receive_data() - the matched-block path mirrors
        // upstream's two-condition optimization. When `updating_basis_or_equiv
        // && offset == offset2` (inplace + basis at the same offset as where
        // we are writing), upstream calls skip_matched(), avoiding the copy.
        // Otherwise upstream calls write_file(fd, offset, map, len) to copy
        // the basis bytes to the writer at the current offset. We open a
        // read fd on the basis even in inplace mode so the copy fallback has
        // a source; the fast skip-path still fires for the common case.
        //
        // When `basis_separate_from_writer` is true (e.g. `--inplace
        // --backup-dir` moved the original destination to the backup
        // location), the skip-path is never safe because the writer's file
        // is freshly opened and contains nothing. Force every matched block
        // through the copy path by treating the run as non-inplace.
        let inplace_mode = self.inplace_enabled() && !basis_separate_from_writer;

        // On NTFS the sparse zero-run seeks below only deallocate blocks once
        // the handle is flagged sparse via FSCTL_SET_SPARSE. Elsewhere this is a
        // no-op (holes are implicit). Best-effort: a non-NTFS volume or a
        // refused control code falls back to a dense write, never an error.
        if sparse {
            let _ = fast_io::mark_file_sparse(writer);
        }

        let mut destination_reader =
            Some(fs::File::open(destination).map_err(|error| {
                LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?);
        let mut compressor = self.start_compressor(compress, source)?;
        let mut compressed_progress = 0u64;
        let mut total_bytes = 0u64;
        let mut literal_bytes = 0u64;
        let mut sparse_state = SparseWriteState::default();
        sparse_state.set_preallocated_len(preallocated_len);
        let mut window: VecDeque<u8> = VecDeque::with_capacity(index.block_length());
        let mut pending_literals = Vec::with_capacity(index.block_length());
        let mut scratch = Vec::with_capacity(index.block_length());
        let mut rolling = RollingChecksum::new();
        let mut outgoing: Option<u8> = None;
        let mut read_buffer = vec![0u8; buffer.len().max(index.block_length())];
        let mut buffer_len = 0usize;
        let mut buffer_pos = 0usize;
        // Inplace mode seeks to this position before each literal write.
        let mut output_position = 0u64;
        // 256KB interval - smaller than regular copy since delta is more
        // CPU-intensive, but still amortizes clock_gettime syscalls.
        const TIMEOUT_CHECK_INTERVAL: usize = 256 * 1024;
        let mut bytes_since_timeout_check: usize = 0;

        loop {
            self.check_shutdown(destination)?;
            if bytes_since_timeout_check >= TIMEOUT_CHECK_INTERVAL {
                self.enforce_timeout()?;
                bytes_since_timeout_check = 0;
            }
            if buffer_pos == buffer_len {
                buffer_len = reader.read(&mut read_buffer).map_err(|error| {
                    LocalCopyError::io("copy file", source, error)
                })?;
                buffer_pos = 0;
                if buffer_len == 0 {
                    break;
                }
            }

            let byte = read_buffer[buffer_pos];
            buffer_pos += 1;
            bytes_since_timeout_check += 1;

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
                    if inplace_mode {
                        writer.seek(SeekFrom::Start(output_position)).map_err(|error| {
                            LocalCopyError::io("seek destination file", destination.to_path_buf(), error)
                        })?;
                    }
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
                    output_position = output_position.saturating_add(flushed_len as u64);
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
                let basis_offset = matched.offset();

                // upstream: receiver.c:468-477. The skip-fast-path only fires
                // when basis offset == output position. For any other matched
                // block (re-ordered content, inserted prefix, ...) copy the
                // basis bytes to the current write offset just like upstream
                // does when `offset != offset2`. Without this fallback the
                // writer keeps stale basis bytes at the new position and the
                // final ftruncate clips the file off at the wrong length.
                if inplace_mode && basis_offset == output_position {
                    output_position = output_position.saturating_add(block_len as u64);
                } else if inplace_mode {
                    // Inplace + basis == destination: reading basis at a
                    // back-references offset can race with prior writes that
                    // already overwrote that region. The matched window is
                    // bit-equivalent to the basis block (both rolling and
                    // strong checksums confirmed), so write the verified
                    // source bytes directly rather than re-reading from the
                    // (potentially overwritten) destination basis.
                    writer
                        .seek(SeekFrom::Start(output_position))
                        .map_err(|error| {
                            LocalCopyError::io(
                                "seek destination file",
                                destination.to_path_buf(),
                                error,
                            )
                        })?;
                    let matched_bytes = &scratch[..block_len];
                    if sparse {
                        let _ = write_sparse_chunk(
                            writer,
                            &mut sparse_state,
                            matched_bytes,
                            destination,
                        )?;
                    } else {
                        writer.write_all(matched_bytes).map_err(|error| {
                            LocalCopyError::io("copy file", destination, error)
                        })?;
                    }
                    self.capture_batch_block_match(&matched, destination)?;
                    output_position = output_position.saturating_add(block_len as u64);
                } else {
                    self.copy_matched_block(
                        destination_reader
                            .as_mut()
                            .expect("destination reader is always open"),
                        writer,
                        buffer,
                        destination,
                        matched,
                        SparseCopy {
                            enabled: sparse,
                            state: &mut sparse_state,
                        },
                    )?;
                }

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

        // EOF tail match: the window now holds the file's short trailing bytes
        // (fewer than block_length). Upstream rsync matches this short tail
        // against the basis's final partial block via `l = MIN(blength,
        // len-offset)` (`match.c:222-224`). Mirror that: probe the same-length
        // basis block and, on a hit, flush preceding literals then emit the
        // short matched block. Without this the trailing partial block is
        // always sent as literal data, diverging from upstream deltas.
        let tail_matched_block = {
            let digest = rolling.digest();
            index.find_tail_match_window(digest, &window, &mut scratch)
        };
        if let Some(block_index) = tail_matched_block {
            if !pending_literals.is_empty() {
                let flushed_len = pending_literals.len();
                if inplace_mode {
                    writer.seek(SeekFrom::Start(output_position)).map_err(|error| {
                        LocalCopyError::io(
                            "seek destination file",
                            destination.to_path_buf(),
                            error,
                        )
                    })?;
                }
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
                output_position = output_position.saturating_add(flushed_len as u64);
                pending_literals.clear();
            }

            let block = index.block(block_index);
            let block_len = block.len();
            let matched = MatchedBlock::new(block, index.block_length());
            let basis_offset = matched.offset();

            if inplace_mode && basis_offset == output_position {
                output_position = output_position.saturating_add(block_len as u64);
            } else if inplace_mode {
                writer.seek(SeekFrom::Start(output_position)).map_err(|error| {
                    LocalCopyError::io(
                        "seek destination file",
                        destination.to_path_buf(),
                        error,
                    )
                })?;
                let matched_bytes = &scratch[..block_len];
                if sparse {
                    let _ = write_sparse_chunk(
                        writer,
                        &mut sparse_state,
                        matched_bytes,
                        destination,
                    )?;
                } else {
                    writer.write_all(matched_bytes).map_err(|error| {
                        LocalCopyError::io("copy file", destination, error)
                    })?;
                }
                self.capture_batch_block_match(&matched, destination)?;
                output_position = output_position.saturating_add(block_len as u64);
            } else {
                self.copy_matched_block(
                    destination_reader
                        .as_mut()
                        .expect("destination reader is always open"),
                    writer,
                    buffer,
                    destination,
                    matched,
                    SparseCopy {
                        enabled: sparse,
                        state: &mut sparse_state,
                    },
                )?;
            }

            total_bytes = total_bytes.saturating_add(block_len as u64);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
            window.clear();
        } else {
            while let Some(byte) = window.pop_front() {
                pending_literals.push(byte);
            }
        }

        if !pending_literals.is_empty() {
            let flushed_len = pending_literals.len();
            if inplace_mode {
                writer.seek(SeekFrom::Start(output_position)).map_err(|error| {
                    LocalCopyError::io("seek destination file", destination.to_path_buf(), error)
                })?;
            }
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
            output_position = output_position.saturating_add(flushed_len as u64);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
        }

        if sparse {
            // upstream: fileio.c:sparse_end() flushes the trailing zero run then
            // ftruncates to the file's true size. In inplace mode matched blocks
            // are skipped without advancing the writer, so the final length is
            // the delta loop's output_position rather than the sparse writer's
            // stream position; a fresh sparse copy uses the flushed logical end.
            let sparse_end = sparse_state.finish(writer, destination)?;
            let final_position = if inplace_mode {
                output_position
            } else {
                sparse_end
            };
            writer.set_len(final_position).map_err(|error| {
                LocalCopyError::io(
                    "truncate destination file",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            self.register_progress();
        } else if inplace_mode {
            // Truncate to the final output size in case the new file is
            // smaller than the old one.
            writer.set_len(output_position).map_err(|error| {
                LocalCopyError::io(
                    "truncate destination file",
                    destination.to_path_buf(),
                    error,
                )
            })?;
        }

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source, error)
            })?;
            let delta = compressed_total.saturating_sub(compressed_progress);
            self.register_limiter_bytes(delta);
            self.record_adaptive_compression(literal_bytes, compressed_total);
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

        // Capture LITERAL operation to the batch delta buffer if active.
        // upstream: token.c:simple_send_token() - literals are written as
        // write_int(length) + raw bytes, chunked to CHUNK_SIZE (32KB).
        if let Some(delta_writer) = self.batch_delta_writer() {
            let mut encoded = Vec::new();
            protocol::wire::delta::write_token_literal(&mut encoded, chunk).map_err(|e| {
                LocalCopyError::io(
                    "encode batch literal token",
                    destination.to_path_buf(),
                    e,
                )
            })?;

            delta_writer.write_all(&encoded).map_err(|e| {
                LocalCopyError::io("write batch literal token", destination.to_path_buf(), e)
            })?;
        }

        let written = if sparse {
            write_sparse_chunk(writer, state, chunk, destination)?
        } else {
            writer.write_all(chunk).map_err(|error| {
                LocalCopyError::io("copy file", destination, error)
            })?;
            chunk.len()
        };

        if let Some(encoder) = compressor {
            encoder.write(chunk).map_err(|error| {
                LocalCopyError::io("compress file", source, error)
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

    /// Records a block-match token to the batch delta stream, if active.
    ///
    /// Mirrors upstream `token.c:simple_send_token()` which encodes a block
    /// reference as `write_int(-(token+1))`.
    fn capture_batch_block_match(
        &mut self,
        matched: &MatchedBlock<'_>,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        if let Some(delta_writer) = self.batch_delta_writer() {
            let block_index = matched.descriptor().index();

            let mut encoded = Vec::new();
            protocol::wire::delta::write_token_block_match(&mut encoded, block_index as u32)
                .map_err(|e| {
                    LocalCopyError::io(
                        "encode batch block match token",
                        destination.to_path_buf(),
                        e,
                    )
                })?;

            delta_writer.write_all(&encoded).map_err(|e| {
                LocalCopyError::io(
                    "write batch block match token",
                    destination.to_path_buf(),
                    e,
                )
            })?;
        }
        Ok(())
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

        self.capture_batch_block_match(&matched, destination)?;

        // REFLINK-3/4: attempt a same-filesystem FICLONERANGE reflink before
        // falling back to the read+write copy loop. On a CoW filesystem this
        // shares the basis extents into the destination with zero data copied.
        // Skipped for sparse output (the reflink cannot reproduce the sparse
        // writer's hole layout) and when `--no-cow` disables reflink. When the
        // clone is unavailable (cross-fs, unaligned, unsupported fs) the helper
        // reports `false` and we fall through to the byte copy. Byte-identical
        // output either way - this is a local-only acceleration with no wire
        // impact.
        if !sparse.enabled
            && self.try_reflink_matched_block(existing, writer, offset, block_length, destination)?
        {
            return Ok(());
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
                    LocalCopyError::io("copy file", destination, error)
                })?;
            }

            remaining -= read;
        }

        Ok(())
    }

    /// Attempts to reflink `existing[offset..offset+len]` into the writer at
    /// its current position via Linux `FICLONERANGE`.
    ///
    /// Returns `Ok(true)` when the range clone succeeded and the writer's file
    /// position has been advanced past the cloned range (so subsequent
    /// sequential writes land correctly). Returns `Ok(false)` when the clone
    /// was declined - `--no-cow` in effect, the operands are on different
    /// filesystems (REFLINK-3), the range is below the worthwhile threshold,
    /// or the ioctl reported that the filesystem / alignment cannot satisfy the
    /// clone - so the caller falls back to the read+write copy.
    ///
    /// # Correctness
    ///
    /// The destination position is `writer.stream_position()`. On success the
    /// clone copies no bytes but does not move the file offset, so we seek the
    /// writer forward by `len` to match what an equivalent `write_all` would
    /// have left behind. Cross-filesystem clones are never issued: `st_dev` of
    /// the basis and destination are compared up front, and `FICLONERANGE`'s
    /// own `EXDEV`/`EINVAL`/`EOPNOTSUPP` results are mapped to a graceful
    /// fallback by the `fast_io` wrapper. The transfer never fails on a clone
    /// error.
    fn try_reflink_matched_block(
        &self,
        existing: &fs::File,
        writer: &mut fs::File,
        offset: u64,
        len: usize,
        destination: &Path,
    ) -> Result<bool, LocalCopyError> {
        // REFLINK-4: honour the `--cow` / `--no-cow` policy that also gates the
        // whole-file FICLONE fast path.
        if !self.reflink_enabled() {
            return Ok(false);
        }

        let len = len as u64;
        if len < fast_io::CLONE_FILE_RANGE_MIN_BYTES {
            return Ok(false);
        }

        // REFLINK-3: never issue FICLONERANGE across filesystems. The ioctl
        // returns EXDEV cross-mount; comparing st_dev up front avoids the
        // wasted syscall. `None` (device id unavailable) falls through to let
        // the ioctl decide rather than forcing the slow path.
        if fast_io::same_fs::files_same_device(existing, writer) == Some(false) {
            return Ok(false);
        }

        let dst_offset = writer.stream_position().map_err(|error| {
            LocalCopyError::io("seek destination file", destination.to_path_buf(), error)
        })?;

        let cloned = fast_io::try_clone_file_range(existing, offset, writer, dst_offset, len)
            .map_err(|error| LocalCopyError::io("clone basis range", destination, error))?;

        if !cloned {
            return Ok(false);
        }

        // FICLONERANGE leaves the writer's file offset unchanged; advance it so
        // the next sequential write continues after the cloned range.
        writer
            .seek(SeekFrom::Start(dst_offset.saturating_add(len)))
            .map_err(|error| {
                LocalCopyError::io("seek destination file", destination.to_path_buf(), error)
            })?;

        Ok(true)
    }
}
