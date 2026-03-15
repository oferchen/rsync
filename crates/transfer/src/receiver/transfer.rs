//! Transfer orchestration for the receiver role.
//!
//! Contains the `run`, `run_sync`, `run_pipelined`, and `run_pipelined_incremental`
//! entry points, the pipelined transfer loop, phase-done exchange, goodbye
//! handshake, stats reception, and file-transfer candidate building.

use std::collections::VecDeque;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use logging::info_log;
use protocol::CompatibilityFlags;
use protocol::codec::{NdxCodec, ProtocolCodec, create_ndx_codec, create_protocol_codec};
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;

use metadata::{MetadataOptions, apply_metadata_from_file_entry, apply_metadata_with_cached_stat};

use super::basis::find_basis_file_with_config;
use super::directory::FailedDirectories;
use super::quick_check::{
    dest_mtime_newer, is_hardlink_follower, quick_check_matches, try_reference_dest,
};
use super::stats::{SenderStats, TransferStats};
use super::wire::{SenderAttrs, SumHead, write_signature_blocks};
use super::{
    PARALLEL_STAT_THRESHOLD, PHASE1_CHECKSUM_LENGTH, PipelineSetup, REDO_CHECKSUM_LENGTH,
    ReceiverContext, apply_acls_from_receiver_cache,
};

use crate::adaptive_buffer::adaptive_writer_capacity;
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::map_file::MapFile;
use crate::pipeline::{PipelineConfig, PipelineState};
use crate::shared::ChecksumFactory;
use crate::temp_guard::open_tmpfile;
use crate::token_buffer::TokenBuffer;
use crate::token_reader::{DeltaToken as TokenReaderDeltaToken, LiteralData, TokenReader};
use crate::transfer_ops::{
    RequestConfig, ResponseContext, process_file_response_streaming, send_file_request,
};
use protocol::filters::read_filter_list;

impl ReceiverContext {
    /// Runs the receiver role to completion.
    ///
    /// This orchestrates the full receive operation:
    /// 1. Receive file list
    /// 2. For each file: generate signature, receive delta, apply
    /// 3. Set final metadata
    pub fn run<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        // Use pipelined transfer by default for improved performance.
        // When incremental-flist feature is enabled, use incremental mode
        // which provides failed directory tracking and better error recovery.
        #[cfg(feature = "incremental-flist")]
        {
            self.run_pipelined_incremental(reader, writer, PipelineConfig::default(), progress)
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            let _ = progress;
            self.run_pipelined(reader, writer, PipelineConfig::default())
        }
    }

    /// Runs the receiver with synchronous (non-pipelined) transfer.
    ///
    /// This method is kept for compatibility and testing purposes.
    /// For production use, prefer the default `run()` which uses pipelining.
    pub fn run_sync<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        let (mut reader, file_count, setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        let PipelineSetup {
            dest_dir,
            metadata_opts,
            checksum_length,
            checksum_algorithm,
            acl_cache,
        } = setup;

        // Transfer loop: for each file, generate signature, receive delta, apply
        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // First pass: create directories and symlinks from file list.
        // For --relative, ensure implied parent directories exist before
        // directory/symlink creation to handle --no-implied-dirs.
        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&dest_dir);
        let mut metadata_errors =
            self.create_directories(&dest_dir, &metadata_opts, acl_cache.as_deref())?;
        self.create_symlinks(&dest_dir, writer);

        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // Reusable per-file resources
        let mut checksum_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let mut token_buffer = TokenBuffer::with_default_capacity();

        let deadline = crate::shared::TransferDeadline::from_system_time(self.config.stop_at);

        for (file_idx, file_entry) in self.file_list.iter().enumerate() {
            // Check deadline at file boundary before starting next file.
            if let Some(ref dl) = deadline {
                if dl.is_reached() {
                    break;
                }
            }

            let relative_path = file_entry.path();

            // Compute actual file path
            let file_path = if relative_path.as_os_str() == "." {
                dest_dir.clone()
            } else {
                dest_dir.join(relative_path)
            };

            // Skip non-regular files (directories, symlinks, devices, etc.)
            if !file_entry.is_file() {
                if file_entry.is_dir()
                    && self.config.flags.verbose
                    && self.config.connection.client_mode
                {
                    if relative_path.as_os_str() == "." {
                        info_log!(Name, 1, "./");
                    } else {
                        info_log!(Name, 1, "{}/", relative_path.display());
                    }
                }
                continue;
            }

            // Skip hardlink followers
            if is_hardlink_follower(file_entry) {
                continue;
            }

            // Skip files outside the configured size range.
            let file_size = file_entry.size();
            if let Some(min_limit) = self.config.file_selection.min_file_size {
                if file_size < min_limit {
                    continue;
                }
            }
            if let Some(max_limit) = self.config.file_selection.max_file_size {
                if file_size > max_limit {
                    continue;
                }
            }

            // upstream: rsync.c:674
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(Name, 1, "{}", relative_path.display());
            }

            // Convert flat index to wire NDX using segment boundary table.
            let ndx = self.flat_to_wire_ndx(file_idx);
            ndx_write_codec.write_ndx(&mut *writer, ndx)?;

            // For protocol >= 29, sender expects iflags after NDX
            if self.protocol.supports_iflags() {
                writer.write_all(&SenderAttrs::ITEM_TRANSFER.to_le_bytes())?;
            }

            // Generate signature if basis file exists
            let basis_config = self.build_basis_file_config(
                &file_path,
                &dest_dir,
                relative_path,
                file_entry.size(),
                checksum_length,
                checksum_algorithm,
            );
            let basis_result = find_basis_file_with_config(&basis_config);
            let signature_opt = basis_result.signature;
            let basis_path_opt = basis_result.basis_path;

            // Send sum_head (signature header) - upstream write_sum_head()
            let sum_head = match signature_opt {
                Some(ref signature) => SumHead::from_signature(signature),
                None => SumHead::empty(),
            };
            sum_head.write(&mut *writer)?;

            if let Some(ref signature) = signature_opt {
                write_signature_blocks(&mut *writer, signature, sum_head.s2length)?;
            }
            writer.flush()?;

            // Read sender attributes (echoed NDX + iflags)
            let (echoed_ndx, _sender_attrs) =
                SenderAttrs::read_with_codec(reader, &mut ndx_read_codec)?;

            debug_assert_eq!(
                echoed_ndx, ndx,
                "sender echoed NDX {echoed_ndx} but we requested {ndx}"
            );

            // Read sum_head echoed by sender (we don't use it, but must consume it)
            let _echoed_sum_head = SumHead::read(reader)?;

            // Apply delta to reconstruct file
            let (file, mut temp_guard) = open_tmpfile(&file_path, self.config.temp_dir.as_deref())?;
            let target_size = file_entry.size();
            let writer_capacity = adaptive_writer_capacity(target_size);
            let mut output = std::io::BufWriter::with_capacity(writer_capacity, file);
            let mut total_bytes: u64 = 0;

            let use_sparse = self.config.flags.sparse;
            let mut sparse_state = if use_sparse {
                Some(SparseWriteState::default())
            } else {
                None
            };

            // MapFile: Cache basis file with 256KB sliding window
            let mut basis_map = if let Some(ref path) = basis_path_opt {
                Some(MapFile::open(path).map_err(|e| {
                    io::Error::new(e.kind(), format!("failed to open basis file {path:?}: {e}"))
                })?)
            } else {
                None
            };

            // Read and apply delta tokens.
            let compression = self.negotiated_algorithms.map(|n| n.compression);
            let mut token_reader = TokenReader::new(compression);

            loop {
                match token_reader.read_token(reader)? {
                    TokenReaderDeltaToken::End => {
                        // End of file - verify checksum using stack buffers.
                        let checksum_len = checksum_verifier.digest_len();
                        let mut expected_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                        reader.read_exact(&mut expected_buf[..checksum_len])?;

                        let algo = checksum_verifier.algorithm();
                        let old_verifier = std::mem::replace(
                            &mut checksum_verifier,
                            ChecksumVerifier::for_algorithm(algo),
                        );
                        let mut computed = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                        let computed_len = old_verifier.finalize_into(&mut computed);
                        if computed_len != checksum_len {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "checksum length mismatch for {file_path:?}: expected {checksum_len} bytes, got {computed_len} bytes",
                                ),
                            ));
                        }
                        if computed[..computed_len] != expected_buf[..checksum_len] {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "checksum verification failed for {file_path:?}: expected {:02x?}, got {:02x?}",
                                    &expected_buf[..checksum_len],
                                    &computed[..computed_len]
                                ),
                            ));
                        }
                        break;
                    }
                    TokenReaderDeltaToken::Literal(literal) => match literal {
                        LiteralData::Ready(data) => {
                            let len = data.len();
                            if let Some(ref mut sparse) = sparse_state {
                                sparse.write(&mut output, &data)?;
                            } else {
                                output.write_all(&data)?;
                            }
                            checksum_verifier.update(&data);
                            total_bytes += len as u64;
                        }
                        LiteralData::Pending(len) => {
                            if let Some(data) = reader.try_borrow_exact(len)? {
                                if let Some(ref mut sparse) = sparse_state {
                                    sparse.write(&mut output, data)?;
                                } else {
                                    output.write_all(data)?;
                                }
                                checksum_verifier.update(data);
                            } else {
                                token_buffer.resize_for(len);
                                reader.read_exact(token_buffer.as_mut_slice())?;
                                let data = token_buffer.as_slice();
                                if let Some(ref mut sparse) = sparse_state {
                                    sparse.write(&mut output, data)?;
                                } else {
                                    output.write_all(data)?;
                                }
                                checksum_verifier.update(data);
                            }
                            total_bytes += len as u64;
                        }
                    },
                    TokenReaderDeltaToken::BlockRef(block_idx) => {
                        if let (Some(sig), Some(basis_map)) = (&signature_opt, basis_map.as_mut()) {
                            let layout = sig.layout();
                            let block_count = layout.block_count() as usize;

                            if block_idx >= block_count {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    format!(
                                        "block index {block_idx} out of bounds (file has {block_count} blocks)"
                                    ),
                                ));
                            }

                            let block_len = layout.block_length().get() as u64;
                            let offset = block_idx as u64 * block_len;

                            let bytes_to_copy = if block_idx == block_count.saturating_sub(1) {
                                let remainder = layout.remainder();
                                if remainder > 0 {
                                    remainder as usize
                                } else {
                                    block_len as usize
                                }
                            } else {
                                block_len as usize
                            };

                            let block_data = basis_map.map_ptr(offset, bytes_to_copy)?;

                            if let Some(ref mut sparse) = sparse_state {
                                sparse.write(&mut output, block_data)?;
                            } else {
                                output.write_all(block_data)?;
                            }
                            checksum_verifier.update(block_data);

                            // upstream: token.c:631 - see_deflate_token()
                            token_reader.see_token(block_data)?;

                            total_bytes += bytes_to_copy as u64;
                        } else {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("block reference {block_idx} without basis file"),
                            ));
                        }
                    }
                }
            }

            // Finalize sparse writing if active
            if let Some(ref mut sparse) = sparse_state {
                let final_pos = sparse.finish(&mut output)?;
                let expected_size = file_entry.size();
                if final_pos != expected_size {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "sparse file size mismatch for {file_path:?}: \
                             expected {expected_size} bytes, got {final_pos} bytes"
                        ),
                    ));
                }
            }

            // Flush BufWriter and get inner file for sync_all
            let file = output.into_inner().map_err(|e| {
                io::Error::other(format!(
                    "failed to flush output buffer for {file_path:?}: {e}"
                ))
            })?;
            if self.config.write.fsync {
                file.sync_all().map_err(|e| {
                    io::Error::new(e.kind(), format!("fsync failed for {file_path:?}: {e}"))
                })?;
            }
            drop(file);

            // upstream: backup.c:make_backup() - rename existing file before overwrite
            if self.config.flags.backup && file_path.exists() {
                let backup_path = engine::compute_backup_path(
                    &dest_dir,
                    &file_path,
                    None,
                    self.config.backup_dir.as_ref().map(std::path::Path::new),
                    std::ffi::OsStr::new(self.config.backup_suffix.as_deref().unwrap_or("~")),
                );
                if let Some(parent) = backup_path.parent() {
                    if !parent.exists() {
                        fs::create_dir_all(parent)?;
                    }
                }
                fs::rename(&file_path, &backup_path)?;
            }

            // Atomic rename (crash-safe)
            fs::rename(temp_guard.path(), &file_path)?;
            temp_guard.keep();

            // Apply metadata from FileEntry (best-effort)
            if let Err(meta_err) =
                apply_metadata_from_file_entry(&file_path, file_entry, &metadata_opts)
            {
                metadata_errors.push((file_path.clone(), meta_err.to_string()));
            } else if let Some(ref xattr_list) = self.resolve_xattr_list(file_entry) {
                // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
                if let Err(e) = metadata::apply_xattrs_from_list(&file_path, xattr_list, true) {
                    metadata_errors.push((file_path.clone(), e.to_string()));
                }
            }

            // Apply cached ACLs after metadata (best-effort)
            if let Err(acl_err) = apply_acls_from_receiver_cache(
                &file_path,
                file_entry,
                acl_cache.as_deref(),
                !file_entry.is_symlink(),
            ) {
                metadata_errors.push((file_path.clone(), acl_err.to_string()));
            }

            bytes_received += total_bytes;
            files_transferred += 1;
        }

        // Create hard links for follower entries now that leaders are transferred.
        self.create_hardlinks(&dest_dir, writer);

        self.finalize_transfer(reader, writer)?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
            bytes_sent: 0,
            total_source_bytes,
            metadata_errors,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error()),
            error_count: 0,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
            delete_stats: DeleteStats::new(),
            delete_limit_exceeded: false,
            redo_count: 0,
        })
    }

    /// Runs the pipelined receiver transfer loop.
    pub fn run_pipelined<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats> {
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        // Batch directory and symlink creation.
        // For --relative, ensure implied parent directories exist before
        // directory/symlink creation to handle --no-implied-dirs.
        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&setup.dest_dir);
        let mut metadata_errors = self.create_directories(
            &setup.dest_dir,
            &setup.metadata_opts,
            setup.acl_cache.as_deref(),
        )?;
        self.create_symlinks(&setup.dest_dir, writer);

        // Delete extraneous files at destination (--delete-before pass).
        let mut delete_stats = DeleteStats::new();
        let mut delete_limit_exceeded = false;
        if self.config.flags.delete {
            let (ds, exceeded) = self.delete_extraneous_files(&setup.dest_dir, writer)?;
            delete_stats = ds;
            delete_limit_exceeded = exceeded;
        }

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error()),
            ..Default::default()
        };
        let files_to_transfer = self.build_files_to_transfer(
            writer,
            &setup.dest_dir,
            &setup.metadata_opts,
            None,
            &mut metadata_errors,
            &mut stats,
            setup.acl_cache.as_deref(),
        );

        // Run pipelined transfer with decoupled network/disk I/O (phase 1)
        let total_files = files_to_transfer.len();
        let redo_config = pipeline_config.clone();
        let mut no_progress: Option<&mut dyn crate::TransferProgressCallback> = None;
        let (mut files_transferred, mut bytes_received, redo_indices) = self
            .run_pipeline_loop_decoupled(
                reader,
                writer,
                pipeline_config,
                &setup,
                files_to_transfer,
                &mut metadata_errors,
                false,
                total_files,
                &mut no_progress,
            )?;

        // Phase 2: redo pass for files that failed checksum verification.
        let redo_count = redo_indices.len();
        if !redo_indices.is_empty() {
            setup.checksum_length = REDO_CHECKSUM_LENGTH;

            let redo_files: Vec<(usize, &FileEntry)> = redo_indices
                .iter()
                .filter_map(|&idx| self.file_list.get(idx).map(|entry| (idx, entry)))
                .collect();

            let (redo_transferred, redo_bytes, _) = self.run_pipeline_loop_decoupled(
                reader,
                writer,
                redo_config,
                &setup,
                redo_files,
                &mut metadata_errors,
                true,
                total_files,
                &mut no_progress,
            )?;

            files_transferred += redo_transferred;
            bytes_received += redo_bytes;
        }

        // Print verbose directories
        for file_entry in &self.file_list {
            if file_entry.is_dir()
                && self.config.flags.verbose
                && self.config.connection.client_mode
            {
                let relative_path = file_entry.path();
                if relative_path.as_os_str() == "." {
                    info_log!(Name, 1, "./");
                } else {
                    info_log!(Name, 1, "{}/", relative_path.display());
                }
            }
        }

        // Create hard links for follower entries now that leaders are transferred.
        self.create_hardlinks(&setup.dest_dir, writer);

        // Finalize handshake
        self.finalize_transfer(reader, writer)?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.total_source_bytes = total_source_bytes;
        if !metadata_errors.is_empty() {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.delete_stats = delete_stats;
        stats.delete_limit_exceeded = delete_limit_exceeded;
        stats.redo_count = redo_count;

        Ok(stats)
    }

    /// Runs the receiver with incremental directory creation and failed-dir tracking.
    pub fn run_pipelined_incremental<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        mut progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        // Incremental directory creation with failure tracking
        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error()),
            ..Default::default()
        };
        let mut failed_dirs = FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

        // For --relative, ensure implied parent directories exist before
        // incremental directory creation to handle --no-implied-dirs.
        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&setup.dest_dir);

        for file_entry in &self.file_list {
            if file_entry.is_dir() {
                let result = self.create_directory_incremental(
                    &setup.dest_dir,
                    file_entry,
                    &setup.metadata_opts,
                    &mut failed_dirs,
                    setup.acl_cache.as_deref(),
                )?;
                match result {
                    Some(true) => {
                        stats.directories_created += 1;
                        // upstream: generator.c:1432 - itemize new directory
                        let iflags = crate::generator::ItemFlags::from_raw(
                            crate::generator::ItemFlags::ITEM_LOCAL_CHANGE
                                | crate::generator::ItemFlags::ITEM_IS_NEW,
                        );
                        let _ = self.emit_itemize(writer, &iflags, file_entry);
                    }
                    Some(false) => {
                        // upstream: generator.c:2260 - existing dir, metadata only
                        let iflags = crate::generator::ItemFlags::from_raw(0);
                        let _ = self.emit_itemize(writer, &iflags, file_entry);
                    }
                    None => {
                        stats.directories_failed += 1;
                    }
                }
            }
        }

        // Create symlinks after directories are in place
        self.create_symlinks(&setup.dest_dir, writer);

        let files_to_transfer = self.build_files_to_transfer(
            writer,
            &setup.dest_dir,
            &setup.metadata_opts,
            Some(&failed_dirs),
            &mut metadata_errors,
            &mut stats,
            setup.acl_cache.as_deref(),
        );

        // Run pipelined transfer with decoupled network/disk I/O (phase 1)
        let total_files = files_to_transfer.len();
        let redo_config = pipeline_config.clone();
        let (mut files_transferred, mut bytes_received, redo_indices) = self
            .run_pipeline_loop_decoupled(
                reader,
                writer,
                pipeline_config,
                &setup,
                files_to_transfer,
                &mut metadata_errors,
                false,
                total_files,
                &mut progress,
            )?;

        // Phase 2: redo pass for files that failed checksum verification.
        let redo_count = redo_indices.len();
        if !redo_indices.is_empty() {
            setup.checksum_length = REDO_CHECKSUM_LENGTH;

            let redo_files: Vec<(usize, &FileEntry)> = redo_indices
                .iter()
                .filter_map(|&idx| self.file_list.get(idx).map(|entry| (idx, entry)))
                .collect();

            let (redo_transferred, redo_bytes, _) = self.run_pipeline_loop_decoupled(
                reader,
                writer,
                redo_config,
                &setup,
                redo_files,
                &mut metadata_errors,
                true,
                total_files,
                &mut progress,
            )?;

            files_transferred += redo_transferred;
            bytes_received += redo_bytes;
        }

        // Create hard links for follower entries now that leaders are transferred.
        self.create_hardlinks(&setup.dest_dir, writer);

        // Finalize
        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.total_source_bytes = self.file_list.iter().map(|e| e.size()).sum();
        if !metadata_errors.is_empty() || stats.directories_failed > 0 || stats.files_skipped > 0 {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.redo_count = redo_count;

        self.finalize_transfer(reader, writer)?;

        Ok(stats)
    }

    /// Common setup for both pipelined transfer modes.
    pub(super) fn setup_transfer<R: Read>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
    ) -> io::Result<(crate::reader::ServerReader<R>, usize, PipelineSetup)> {
        let mut reader = if self.protocol.uses_binary_negotiation() {
            reader.activate_multiplex().map_err(|e| {
                io::Error::new(e.kind(), format!("failed to activate INPUT multiplex: {e}"))
            })?
        } else {
            reader
        };

        if self.should_read_filter_list() {
            let _wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(e.kind(), format!("failed to read filter list: {e}"))
            })?;
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            info_log!(Flist, 1, "receiving incremental file list");
        }

        let file_count = self.receive_file_list(&mut reader)?;

        // Receive incremental file list segments (INC_RECURSE).
        let extra_count = self.receive_extra_file_lists(&mut reader)?;
        let file_count = file_count + extra_count;

        // Validate received file list for path safety.
        let removed = self.sanitize_file_list();
        let file_count = file_count - removed;

        let checksum_factory = ChecksumFactory::from_negotiation(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let checksum_algorithm = checksum_factory.signature_algorithm();
        let checksum_length = PHASE1_CHECKSUM_LENGTH;

        let metadata_opts = MetadataOptions::new()
            .preserve_permissions(self.config.flags.perms)
            .preserve_times(self.config.flags.times)
            .preserve_atimes(self.config.flags.atimes)
            .preserve_crtimes(self.config.flags.crtimes)
            .preserve_owner(self.config.flags.owner)
            .preserve_group(self.config.flags.group)
            .numeric_ids(self.config.flags.numeric_ids);

        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

        // Extract ACL cache from the flist reader when --acls is active.
        // The cache is fully populated during flist reception and only read
        // during transfer, so sharing via Arc is safe.
        let acl_cache = if self.config.flags.acls {
            self.flist_reader_cache
                .as_ref()
                .map(|r| Arc::new(r.acl_cache().clone()))
        } else {
            None
        };

        Ok((
            reader,
            file_count,
            PipelineSetup {
                dest_dir,
                metadata_opts,
                checksum_length,
                checksum_algorithm,
                acl_cache,
            },
        ))
    }

    /// Pipelined transfer loop with decoupled network/disk I/O.
    ///
    /// Returns (files_transferred, bytes, redo_indices).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_pipeline_loop_decoupled<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        setup: &PipelineSetup,
        files_to_transfer: Vec<(usize, &FileEntry)>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        is_redo_pass: bool,
        total_files: usize,
        progress: &mut Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<(usize, u64, Vec<usize>)> {
        use crate::disk_commit::{BackupConfig, DiskCommitConfig};
        use crate::pipeline::receiver::PipelinedReceiver;
        use crate::shared::TransferDeadline;

        let deadline = TransferDeadline::from_system_time(self.config.stop_at);

        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let request_config = RequestConfig {
            protocol: self.protocol,
            write_iflags: self.protocol.supports_iflags(),
            checksum_length: setup.checksum_length,
            checksum_algorithm: setup.checksum_algorithm,
            negotiated_algorithms: self.negotiated_algorithms.as_ref(),
            compat_flags: self.compat_flags.as_ref(),
            checksum_seed: self.checksum_seed,
            use_sparse: self.config.flags.sparse,
            do_fsync: self.config.write.fsync,
            temp_dir: self.config.temp_dir.as_deref(),
            write_devices: self.config.write.write_devices,
            inplace: self.config.write.inplace,
            inplace_partial: self.config.write.inplace_partial,
            io_uring_policy: self.config.write.io_uring_policy,
        };

        let mut pipeline = PipelineState::new(pipeline_config);
        let mut file_iter = files_to_transfer.into_iter();
        // (file_idx, file_path, file_entry, is_new_file) - is_new_file tracks whether
        // the destination file existed for itemize direction indicator
        let mut pending_files_info: VecDeque<(usize, PathBuf, &FileEntry, bool)> =
            VecDeque::with_capacity(pipeline.window_size());
        let mut files_transferred = 0usize;
        let mut bytes_received = 0u64;

        // Reusable per-file resources
        let mut checksum_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let backup = if self.config.flags.backup {
            Some(BackupConfig {
                dest_dir: setup.dest_dir.clone(),
                backup_dir: self.config.backup_dir.as_ref().map(PathBuf::from),
                suffix: self.config.backup_suffix.as_deref().unwrap_or("~").into(),
            })
        } else {
            None
        };
        let disk_config = DiskCommitConfig {
            do_fsync: self.config.write.fsync,
            metadata_opts: Some(setup.metadata_opts.clone()),
            backup,
            acl_cache: setup.acl_cache.clone(),
            ..DiskCommitConfig::default()
        };
        let mut pipelined_receiver = PipelinedReceiver::new(disk_config);
        if is_redo_pass {
            let _ = pipelined_receiver.take_redo_indices();
        }

        let result = (|| -> io::Result<(usize, u64, Vec<usize>)> {
            loop {
                if let Some(ref dl) = deadline {
                    if dl.is_reached() {
                        break;
                    }
                }

                // Fill the pipeline with requests
                while pipeline.can_send() {
                    if let Some((file_idx, file_entry)) = file_iter.next() {
                        let relative_path = file_entry.path();
                        let file_path = if relative_path.as_os_str() == "." {
                            setup.dest_dir.clone()
                        } else {
                            setup.dest_dir.join(relative_path)
                        };

                        if self.config.flags.verbose && self.config.connection.client_mode {
                            info_log!(Name, 1, "{}", relative_path.display());
                        }

                        let (sig, basis) = if is_redo_pass {
                            (None, None)
                        } else {
                            let basis_config = self.build_basis_file_config(
                                &file_path,
                                &setup.dest_dir,
                                relative_path,
                                file_entry.size(),
                                setup.checksum_length,
                                setup.checksum_algorithm,
                            );
                            let basis_result = find_basis_file_with_config(&basis_config);
                            (basis_result.signature, basis_result.basis_path)
                        };

                        let is_new_file = basis.is_none();

                        let pending = send_file_request(
                            writer,
                            &mut ndx_write_codec,
                            self.flat_to_wire_ndx(file_idx),
                            file_path.clone(),
                            sig,
                            basis,
                            file_entry.size(),
                            &request_config,
                        )?;

                        pipeline.push(pending);
                        pending_files_info.push_back((
                            file_idx,
                            file_path,
                            file_entry,
                            is_new_file,
                        ));
                    } else {
                        break;
                    }
                }

                if pipeline.is_empty() {
                    break;
                }

                // upstream: io.c perform_io() flushes buffered output while
                // waiting for input via select(). Flush pending file requests
                // so the generator can see them before we block reading its response.
                writer.flush()?;

                // Process one response
                let pending = pipeline.pop().expect("pipeline not empty");
                let (file_idx, file_path, file_entry, is_new_file) =
                    pending_files_info.pop_front().expect("pipeline not empty");

                let response_ctx = ResponseContext {
                    config: &request_config,
                };

                let xattr_list = self.resolve_xattr_list(file_entry);
                let result = process_file_response_streaming(
                    reader,
                    &mut ndx_read_codec,
                    pending,
                    &response_ctx,
                    &mut checksum_verifier,
                    pipelined_receiver.file_sender(),
                    pipelined_receiver.buf_return_rx(),
                    0,
                    Some(file_entry.clone()),
                    xattr_list,
                )?;

                pipelined_receiver.note_commit_sent(
                    result.expected_checksum,
                    result.checksum_len,
                    file_path.clone(),
                    file_idx,
                );

                // Non-blocking: collect any ready disk results
                let (disk_bytes, disk_meta_errors) = pipelined_receiver.drain_ready_results()?;
                bytes_received += disk_bytes;
                metadata_errors.extend(disk_meta_errors);

                bytes_received += result.total_bytes;
                files_transferred += 1;

                // upstream: receiver.c:950 - log_item() after successful file transfer
                {
                    use crate::generator::ItemFlags;
                    let raw = ItemFlags::ITEM_TRANSFER
                        | if is_new_file {
                            ItemFlags::ITEM_IS_NEW
                        } else {
                            0
                        };
                    let iflags = ItemFlags::from_raw(raw);
                    let _ = self.emit_itemize(writer, &iflags, file_entry);
                }

                if let Some(cb) = progress.as_mut() {
                    let event = crate::TransferProgressEvent {
                        path: file_entry.path(),
                        file_bytes: result.total_bytes,
                        total_file_bytes: Some(file_entry.size()),
                        files_done: files_transferred,
                        total_files,
                    };
                    cb.on_file_transferred(&event);
                }
            }

            // Drain all remaining disk results
            let (disk_bytes, disk_meta_errors) = pipelined_receiver.drain_all_results()?;
            bytes_received += disk_bytes;
            metadata_errors.extend(disk_meta_errors);

            let redo_indices = pipelined_receiver.take_redo_indices();

            Ok((files_transferred, bytes_received, redo_indices))
        })();

        // Graceful shutdown regardless of success or failure.
        let _ = pipelined_receiver.shutdown();

        result
    }

    /// Builds the list of files that need transfer, applying quick-check to skip
    /// unchanged files and respecting size bounds and failed directory tracking.
    ///
    /// For files that are up-to-date (quick-check match), emits a metadata-only
    /// itemize line via MSG_INFO when the daemon has itemize output enabled.
    pub(super) fn build_files_to_transfer<'a, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &'a self,
        writer: &mut W,
        dest_dir: &Path,
        metadata_opts: &MetadataOptions,
        failed_dirs: Option<&FailedDirectories>,
        metadata_errors: &mut Vec<(PathBuf, String)>,
        stats: &mut TransferStats,
        acl_cache: Option<&protocol::acl::AclCache>,
    ) -> Vec<(usize, &'a FileEntry)> {
        // upstream: receiver.c:693 - dry_run (!do_xfers) skips all file transfers
        if self.config.flags.dry_run {
            return Vec::new();
        }

        // Phase A: Filter candidates (cheap, in-memory checks only).
        let candidates: Vec<(usize, &FileEntry)> = self
            .file_list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_file())
            .filter(|(_, e)| !is_hardlink_follower(e))
            .filter(|(_, e)| {
                let sz = e.size();
                self.config
                    .file_selection
                    .min_file_size
                    .is_none_or(|m| sz >= m)
                    && self
                        .config
                        .file_selection
                        .max_file_size
                        .is_none_or(|m| sz <= m)
            })
            .filter(|(_, e)| {
                if let Some(fd) = failed_dirs {
                    if let Some(failed_parent) = fd.failed_ancestor(e.name()) {
                        if self.config.flags.verbose && self.config.connection.client_mode {
                            info_log!(
                                Skip,
                                1,
                                "skipping {} (parent {} failed)",
                                e.name(),
                                failed_parent
                            );
                        }
                        stats.files_skipped += 1;
                        return false;
                    }
                }
                true
            })
            .collect();

        let preserve_times = self.config.flags.times && !self.config.flags.ignore_times;
        let size_only = self.config.file_selection.size_only;
        let ignore_existing = self.config.file_selection.ignore_existing;
        let existing_only = self.config.file_selection.existing_only;
        let update_only = self.config.flags.update;
        let always_checksum = if self.config.flags.checksum {
            Some(self.get_checksum_algorithm())
        } else {
            None
        };

        // Phase B: Parallel stat
        let stat_paths: Vec<(usize, PathBuf)> = candidates
            .iter()
            .map(|&(idx, entry)| (idx, dest_dir.join(entry.path())))
            .collect();

        let stat_results: Vec<(usize, Option<fs::Metadata>)> = crate::parallel_io::map_blocking(
            stat_paths,
            PARALLEL_STAT_THRESHOLD,
            move |(idx, file_path)| {
                let meta = fs::metadata(&file_path).ok();
                (idx, meta)
            },
        );

        // Phase C: Sequential post-processing with stat results.
        let mut files_to_transfer = Vec::with_capacity(stat_results.len());
        for (idx, dest_meta) in stat_results {
            let entry = &self.file_list[idx];
            if let Some(ref meta) = dest_meta {
                if ignore_existing {
                    continue;
                }
                if update_only && dest_mtime_newer(meta, entry) {
                    continue;
                }
                let file_path = dest_dir.join(entry.path());
                if quick_check_matches(
                    entry,
                    &file_path,
                    meta,
                    preserve_times,
                    size_only,
                    always_checksum,
                ) {
                    // upstream: generator.c:2260 - itemize() for up-to-date files
                    // No ITEM_TRANSFER flag - file content unchanged, metadata only.
                    let iflags = crate::generator::ItemFlags::from_raw(0);
                    let _ = self.emit_itemize(writer, &iflags, entry);
                    if let Err(e) =
                        apply_metadata_with_cached_stat(&file_path, entry, metadata_opts, dest_meta)
                    {
                        metadata_errors.push((file_path.clone(), e.to_string()));
                    }
                    if let Err(e) = apply_acls_from_receiver_cache(
                        &file_path,
                        entry,
                        acl_cache,
                        !entry.is_symlink(),
                    ) {
                        metadata_errors.push((file_path, e.to_string()));
                    } else if let Some(ref xattr_list) = self.resolve_xattr_list(entry) {
                        // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
                        if let Err(e) =
                            metadata::apply_xattrs_from_list(&file_path, xattr_list, true)
                        {
                            metadata_errors.push((file_path, e.to_string()));
                        }
                    }
                    continue;
                }
            } else {
                if existing_only {
                    continue;
                }
                if try_reference_dest(
                    entry,
                    dest_dir,
                    &self.config.reference_directories,
                    preserve_times,
                    size_only,
                    always_checksum,
                    metadata_opts,
                    metadata_errors,
                    acl_cache,
                ) {
                    continue;
                }
            }
            files_to_transfer.push((idx, entry));
        }
        files_to_transfer
    }

    /// Exchanges NDX_DONE messages for phase transitions.
    pub(super) fn exchange_phase_done<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<()> {
        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::INC_RECURSE));

        let max_phase: i32 = if self.protocol.supports_multi_phase() {
            2
        } else {
            1
        };

        if inc_recurse {
            let num_segments = self.ndx_segments.len();
            for _ in 0..num_segments {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "segment completion")?;
            }

            for _phase in 2..=max_phase {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
            }

            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;
        } else {
            let mut phase: i32 = 0;
            loop {
                ndx_write_codec.write_ndx_done(&mut *writer)?;
                writer.flush()?;
                phase += 1;
                if phase > max_phase {
                    break;
                }
                self.read_expected_ndx_done(ndx_read_codec, reader, "phase transition")?;
            }
        }

        self.read_expected_ndx_done(ndx_read_codec, reader, "sender final")?;

        Ok(())
    }

    /// Reads an NDX and validates it is NDX_DONE (-1).
    pub(super) fn read_expected_ndx_done<R: Read>(
        &self,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
        reader: &mut R,
        context: &str,
    ) -> io::Result<()> {
        let ndx = ndx_read_codec.read_ndx(reader)?;
        if ndx != -1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected NDX_DONE (-1) from sender during {context}, got {ndx}"),
            ));
        }
        Ok(())
    }

    /// Handles the goodbye handshake at end of transfer.
    pub(super) fn handle_goodbye<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut R,
        writer: &mut W,
        ndx_write_codec: &mut protocol::codec::NdxCodecEnum,
        ndx_read_codec: &mut protocol::codec::NdxCodecEnum,
    ) -> io::Result<()> {
        if !self.protocol.supports_goodbye_exchange() {
            return Ok(());
        }

        ndx_write_codec.write_ndx_done(&mut *writer)?;
        writer.flush()?;

        if self.protocol.supports_extended_goodbye() {
            let goodbye_echo = ndx_read_codec.read_ndx(reader)?;
            if goodbye_echo != -1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected goodbye NDX_DONE echo (-1) from sender, got {goodbye_echo}"),
                ));
            }

            ndx_write_codec.write_ndx_done(&mut *writer)?;
            writer.flush()?;
        }

        Ok(())
    }

    /// Receives transfer statistics from the sender.
    pub(super) fn receive_stats<R: Read + ?Sized>(
        &self,
        reader: &mut R,
    ) -> io::Result<SenderStats> {
        let stats_codec = create_protocol_codec(self.protocol.as_u8());

        let total_read = stats_codec.read_stat(reader)? as u64;
        let total_written = stats_codec.read_stat(reader)? as u64;
        let total_size = stats_codec.read_stat(reader)? as u64;

        let (flist_buildtime_ms, flist_xfertime_ms) = if self.protocol.supports_flist_times() {
            let buildtime = stats_codec.read_stat(reader)? as u64;
            let xfertime = stats_codec.read_stat(reader)? as u64;
            (Some(buildtime), Some(xfertime))
        } else {
            (None, None)
        };

        Ok(SenderStats {
            total_read,
            total_written,
            total_size,
            flist_buildtime_ms,
            flist_xfertime_ms,
        })
    }

    /// Exchange phase transitions, receive stats, and handle goodbye handshake.
    pub(super) fn finalize_transfer<R: Read, W: Write + ?Sized>(
        &self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<()> {
        let mut ndx_write_codec = create_ndx_codec(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        self.exchange_phase_done(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        if self.config.connection.client_mode {
            let _sender_stats = self.receive_stats(reader)?;
        }

        self.handle_goodbye(reader, writer, &mut ndx_write_codec, &mut ndx_read_codec)?;

        Ok(())
    }
}
