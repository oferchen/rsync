//! Transfer orchestration for the receiver role.
//!
//! Provides the `run`, `run_sync`, `run_pipelined`, and `run_pipelined_incremental`
//! entry points plus the common `setup_transfer` initialization. Protocol phase
//! exchange lives in [`phases`], file candidate selection in [`candidates`],
//! and the pipelined transfer loop in [`pipeline`].

mod candidates;
mod phases;
mod pipeline;

use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use logging::{PhaseTimer, info_log};
use protocol::codec::{MonotonicNdxWriter, NdxCodec, create_ndx_codec};
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;

use metadata::{MetadataOptions, apply_metadata_from_file_entry};

use super::basis::find_basis_file_with_config;
use super::quick_check::is_hardlink_follower;
use super::stats::TransferStats;
use super::wire::{SenderAttrs, SumHead, write_signature_blocks};
use super::{
    PHASE1_CHECKSUM_LENGTH, PipelineSetup, REDO_CHECKSUM_LENGTH, ReceiverContext,
    apply_acls_from_receiver_cache,
};

use crate::adaptive_buffer::adaptive_writer_capacity;
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::map_file::MapFile;
use crate::pipeline::PipelineConfig;
use crate::shared::ChecksumFactory;
use crate::temp_guard::open_tmpfile;
use crate::token_buffer::TokenBuffer;
use crate::token_reader::{DeltaToken as TokenReaderDeltaToken, LiteralData, TokenReader};
use filters::{DirMergeConfig, FilterChain, FilterSet};
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};

impl ReceiverContext {
    /// Runs the receiver role to completion.
    ///
    /// Orchestrates the full receive operation: file list reception, signature
    /// generation, delta application, and metadata finalization. Delegates to
    /// `run_pipelined_incremental` (with `incremental-flist`) or `run_pipelined`.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` main reception loop
    /// - `main.c:1160-1200` - `do_recv()` orchestration
    pub fn run<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
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
    /// Processes files sequentially: send NDX + signature, receive delta, apply,
    /// verify checksum. Kept for compatibility and testing. For production use,
    /// prefer [`run`](Self::run) which uses pipelining for improved throughput.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` processes one file at a time
    pub fn run_sync<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        let _t = PhaseTimer::new("receiver-transfer");
        let (mut reader, file_count, setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        let PipelineSetup {
            dest_dir,
            metadata_opts,
            checksum_length,
            checksum_algorithm,
            acl_cache,
        } = setup;

        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // First pass: create directories and symlinks from file list.
        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&dest_dir);
        let mut metadata_errors =
            self.create_directories(&dest_dir, &metadata_opts, acl_cache.as_deref())?;
        self.create_symlinks(&dest_dir, writer);

        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let mut checksum_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let mut token_buffer = TokenBuffer::with_default_capacity();

        // Create token reader once for the entire transfer session.
        // upstream: token.c uses a single compression context across all files.
        // For zstd, the DCtx must persist across file boundaries (continuous stream).
        let compression = self.negotiated_algorithms.map(|n| n.compression);
        let mut token_reader = TokenReader::new(compression);

        let deadline = crate::shared::TransferDeadline::from_system_time(self.config.stop_at);

        for (file_idx, file_entry) in self.file_list.iter().enumerate() {
            if let Some(ref dl) = deadline {
                if dl.is_reached() {
                    break;
                }
            }

            let relative_path = file_entry.path();

            let file_path = if relative_path.as_os_str() == "." {
                dest_dir.clone()
            } else {
                dest_dir.join(relative_path)
            };

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

            if is_hardlink_follower(file_entry) {
                continue;
            }

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

            // upstream: write_sum_head()
            let sum_head = match signature_opt {
                Some(ref signature) => SumHead::from_signature(signature),
                None => SumHead::empty(),
            };
            sum_head.write(&mut *writer)?;

            // upstream: generator.c:775-776 - skip signature blocks in append mode
            if !self.config.flags.append {
                if let Some(ref signature) = signature_opt {
                    write_signature_blocks(&mut *writer, signature, sum_head.s2length)?;
                }
            }
            writer.flush()?;

            let (echoed_ndx, _sender_attrs) = SenderAttrs::read_with_codec_xattr(
                reader,
                &mut ndx_read_codec,
                self.config.flags.xattrs,
                self.compat_flags.is_some_and(|f| {
                    f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
                }),
            )?;

            debug_assert_eq!(
                echoed_ndx, ndx,
                "sender echoed NDX {echoed_ndx} but we requested {ndx}"
            );

            let _echoed_sum_head = SumHead::read(reader)?;

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

            let mut basis_map = if let Some(ref path) = basis_path_opt {
                Some(MapFile::open(path).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!(
                            "failed to open basis file {path:?}: {e} {}{}",
                            crate::role_trailer::error_location!(),
                            crate::role_trailer::receiver()
                        ),
                    )
                })?)
            } else {
                None
            };

            token_reader.reset();

            loop {
                match token_reader.read_token(reader)? {
                    TokenReaderDeltaToken::End => {
                        let checksum_len = checksum_verifier.digest_len();
                        let mut expected_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                        reader.read_exact(&mut expected_buf[..checksum_len])?;

                        let algo = checksum_verifier.algorithm();
                        // upstream: checksum.c:sum_init() prepends seed for legacy MD4.
                        let old_verifier = std::mem::replace(
                            &mut checksum_verifier,
                            ChecksumVerifier::for_algorithm_seeded(algo, self.checksum_seed),
                        );
                        let mut computed = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                        let computed_len = old_verifier.finalize_into(&mut computed);
                        if computed_len != checksum_len {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "checksum length mismatch for {file_path:?}: expected {checksum_len} bytes, got {computed_len} bytes {}{}",
                                    crate::role_trailer::error_location!(),
                                    crate::role_trailer::receiver(),
                                ),
                            ));
                        }
                        if computed[..computed_len] != expected_buf[..checksum_len] {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "checksum verification failed for {file_path:?}: expected {:02x?}, got {:02x?} {}{}",
                                    &expected_buf[..checksum_len],
                                    &computed[..computed_len],
                                    crate::role_trailer::error_location!(),
                                    crate::role_trailer::receiver(),
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
                                        "block index {block_idx} out of bounds (file has {block_count} blocks) {}{}",
                                        crate::role_trailer::error_location!(),
                                        crate::role_trailer::receiver(),
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
                                format!(
                                    "block reference {block_idx} without basis file {}{}",
                                    crate::role_trailer::error_location!(),
                                    crate::role_trailer::receiver()
                                ),
                            ));
                        }
                    }
                }
            }

            if let Some(ref mut sparse) = sparse_state {
                let final_pos = sparse.finish(&mut output)?;
                let expected_size = file_entry.size();
                if final_pos != expected_size {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "sparse file size mismatch for {file_path:?}: \
                             expected {expected_size} bytes, got {final_pos} bytes {}{}",
                            crate::role_trailer::error_location!(),
                            crate::role_trailer::receiver(),
                        ),
                    ));
                }
            }

            let file = output.into_inner().map_err(|e| {
                io::Error::other(format!(
                    "failed to flush output buffer for {file_path:?}: {e} {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver(),
                ))
            })?;
            if self.config.write.fsync {
                file.sync_all().map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!(
                            "fsync failed for {file_path:?}: {e} {}{}",
                            crate::role_trailer::error_location!(),
                            crate::role_trailer::receiver()
                        ),
                    )
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
                    std::ffi::OsStr::new(self.config.effective_backup_suffix()),
                );
                if let Some(parent) = backup_path.parent() {
                    if !parent.exists() {
                        fs::create_dir_all(parent)?;
                    }
                }
                fs::rename(&file_path, &backup_path)?;
            }

            fs::rename(temp_guard.path(), &file_path)?;
            temp_guard.keep();

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
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error,
            error_count: 0,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
            delete_stats: DeleteStats::new(),
            delete_limit_exceeded: false,
            literal_data: 0,
            matched_data: 0,
            redo_count: 0,
        })
    }

    /// Runs the pipelined receiver transfer loop.
    ///
    /// Creates directories and symlinks, optionally deletes extraneous files,
    /// builds the candidate list, runs the pipeline loop, handles redo pass,
    /// and finalizes the transfer.
    ///
    /// Phase 1 uses `SHORT_SUM_LENGTH` (2 bytes) for reduced signature overhead.
    /// Phase 2 redo uses `SUM_LENGTH` (16 bytes) for full collision resistance.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` main loop
    /// - `generator.c:2157-2163` - phase 1 vs phase 2 checksum length
    pub fn run_pipelined<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats> {
        let _t = PhaseTimer::new("receiver-transfer-pipelined");
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&setup.dest_dir);
        let mut metadata_errors = self.create_directories(
            &setup.dest_dir,
            &setup.metadata_opts,
            setup.acl_cache.as_deref(),
        )?;
        self.create_symlinks(&setup.dest_dir, writer);

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
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error,
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

        let mut files_transferred: usize = 0;
        let mut bytes_received: u64 = 0;
        let mut literal_data: u64 = 0;
        let mut matched_data: u64 = 0;
        let mut redo_count: usize = 0;

        // upstream: generator.c:1845 - dry_run sends NDX requests without data
        if self.config.flags.dry_run {
            self.run_dry_run_loop(reader, writer, &files_to_transfer)?;
        } else {
            let total_files = files_to_transfer.len();
            let redo_config = pipeline_config.clone();
            let mut no_progress: Option<&mut dyn crate::TransferProgressCallback> = None;
            let redo_indices;
            (
                files_transferred,
                bytes_received,
                literal_data,
                matched_data,
                redo_indices,
            ) = self.run_pipeline_loop_decoupled(
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
            redo_count = redo_indices.len();
            if !redo_indices.is_empty() {
                setup.checksum_length = REDO_CHECKSUM_LENGTH;

                let redo_files: Vec<(usize, &FileEntry, PathBuf)> = redo_indices
                    .iter()
                    .filter_map(|&idx| {
                        self.file_list.get(idx).map(|entry| {
                            let p = entry.path();
                            let file_path = if p.as_os_str() == "." {
                                setup.dest_dir.clone()
                            } else {
                                setup.dest_dir.join(p)
                            };
                            (idx, entry, file_path)
                        })
                    })
                    .collect();

                let (redo_transferred, redo_bytes, redo_literal, redo_matched, _) = self
                    .run_pipeline_loop_decoupled(
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
                literal_data += redo_literal;
                matched_data += redo_matched;
            }
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            for file_entry in &self.file_list {
                if file_entry.is_dir() {
                    let relative_path = file_entry.path();
                    if relative_path.as_os_str() == "." {
                        info_log!(Name, 1, "./");
                    } else {
                        info_log!(Name, 1, "{}/", relative_path.display());
                    }
                }
            }
        }

        self.create_hardlinks(&setup.dest_dir, writer);

        self.finalize_transfer(reader, writer)?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
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
    ///
    /// Unlike [`run_pipelined`](Self::run_pipelined), tracks directory creation
    /// failures and skips files whose parent directories could not be created.
    /// Emits per-directory itemize output for both new and existing directories.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1432` - itemize new directory
    /// - `generator.c:2260` - itemize existing directory (metadata only)
    pub fn run_pipelined_incremental<R: Read, W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
        mut progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        let _t = PhaseTimer::new("receiver-transfer-incremental");
        let (mut reader, file_count, mut setup) = self.setup_transfer(reader)?;
        let reader = &mut reader;

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error,
            ..Default::default()
        };
        let mut failed_dirs = super::directory::FailedDirectories::new();
        let mut metadata_errors: Vec<(PathBuf, String)> = Vec::new();

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

        let mut files_transferred: usize = 0;
        let mut bytes_received: u64 = 0;
        let mut literal_data: u64 = 0;
        let mut matched_data: u64 = 0;
        let mut redo_count: usize = 0;

        // upstream: generator.c:1845 - dry_run sends NDX requests without data
        if self.config.flags.dry_run {
            self.run_dry_run_loop(reader, writer, &files_to_transfer)?;
        } else {
            let total_files = files_to_transfer.len();
            let redo_config = pipeline_config.clone();
            let redo_indices;
            (
                files_transferred,
                bytes_received,
                literal_data,
                matched_data,
                redo_indices,
            ) = self.run_pipeline_loop_decoupled(
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
            redo_count = redo_indices.len();
            if !redo_indices.is_empty() {
                setup.checksum_length = REDO_CHECKSUM_LENGTH;

                let redo_files: Vec<(usize, &FileEntry, PathBuf)> = redo_indices
                    .iter()
                    .filter_map(|&idx| {
                        self.file_list.get(idx).map(|entry| {
                            let p = entry.path();
                            let file_path = if p.as_os_str() == "." {
                                setup.dest_dir.clone()
                            } else {
                                setup.dest_dir.join(p)
                            };
                            (idx, entry, file_path)
                        })
                    })
                    .collect();

                let (redo_transferred, redo_bytes, redo_literal, redo_matched, _) = self
                    .run_pipeline_loop_decoupled(
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
                literal_data += redo_literal;
                matched_data += redo_matched;
            }
        }

        self.create_hardlinks(&setup.dest_dir, writer);

        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.total_source_bytes = self.file_list.iter().map(|e| e.size()).sum();
        if !metadata_errors.is_empty() || stats.directories_failed > 0 || stats.files_skipped > 0 {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.redo_count = redo_count;

        self.finalize_transfer(reader, writer)?;

        Ok(stats)
    }

    /// Common setup for all transfer modes.
    ///
    /// Activates input multiplex, reads filter list if needed, receives the file
    /// list (including INC_RECURSE extra segments), sanitizes paths, and builds
    /// the `PipelineSetup` with checksum and metadata configuration.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1342-1343` - client receiver activates multiplex at protocol >= 23
    /// - `main.c:1167-1168` - server receiver activates multiplex at protocol >= 30
    pub(super) fn setup_transfer<R: Read>(
        &mut self,
        reader: crate::reader::ServerReader<R>,
    ) -> io::Result<(crate::reader::ServerReader<R>, usize, PipelineSetup)> {
        let mut reader = if self.should_activate_input_multiplex() {
            reader.activate_multiplex().map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to activate INPUT multiplex: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?
        } else {
            reader
        };

        if self.should_read_filter_list() {
            let wire_rules = read_filter_list(&mut reader, self.protocol).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to read filter list: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?;

            // upstream: clientserver.c:rsync_module() - daemon_filter_list is applied
            // on top of client filters. Daemon rules take precedence (prepended).
            let daemon_rules = &self.config.daemon_filter_rules;
            let combined = if daemon_rules.is_empty() {
                wire_rules
            } else if wire_rules.is_empty() {
                daemon_rules.clone()
            } else {
                let mut combined = daemon_rules.clone();
                combined.extend(wire_rules);
                combined
            };

            // Build a FilterChain from the combined rules for deletion filtering.
            // upstream: generator.c:delete_in_dir() - is_excluded() before deletion
            if !combined.is_empty() {
                let (filter_set, merge_configs) = parse_wire_filters_for_receiver(&combined)
                    .map_err(|e| {
                        io::Error::new(
                            e.kind(),
                            format!(
                                "filter error: {e} {}{}",
                                crate::role_trailer::error_location!(),
                                crate::role_trailer::receiver()
                            ),
                        )
                    })?;
                let mut chain = FilterChain::new(filter_set);
                for config in merge_configs {
                    chain.add_merge_config(config);
                }
                self.filter_chain = chain;
            }
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            info_log!(Flist, 1, "receiving incremental file list");
        }

        let file_count = self.receive_file_list(&mut reader)?;

        let extra_count = self.receive_extra_file_lists(&mut reader)?;
        let file_count = file_count + extra_count;

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
            .numeric_ids(self.config.flags.numeric_ids)
            // upstream: clientserver.c:1106-1107 - `fake super = yes` on the
            // daemon module forces fake-super metadata storage on the receiver
            // (ownership and special-file metadata go to user.rsync.%stat
            // xattrs instead of being applied to inodes).
            .fake_super(self.config.fake_super);

        let dest_dir = self
            .config
            .args
            .first()
            .map_or_else(|| PathBuf::from("."), PathBuf::from);

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
}

/// Parses wire-format filter rules into a `FilterSet` and `DirMergeConfig` list for the receiver.
///
/// Separates DirMerge rules (for per-directory merge file scanning) from regular
/// filter rules. The returned `FilterSet` contains compiled include/exclude/protect/risk
/// rules. The `DirMergeConfig` list configures per-directory merge file scanning
/// used during deletion filtering.
///
/// # Upstream Reference
///
/// - `exclude.c:recv_filter_list()` - receiver-side filter list reception
/// - `generator.c:delete_in_dir()` - deletion pass uses filter evaluation
fn parse_wire_filters_for_receiver(
    wire_rules: &[FilterRuleWireFormat],
) -> io::Result<(FilterSet, Vec<DirMergeConfig>)> {
    use ::filters::FilterRule;

    let mut rules = Vec::with_capacity(wire_rules.len());
    let mut merge_configs = Vec::new();

    for wire_rule in wire_rules {
        let mut rule = match wire_rule.rule_type {
            RuleType::Include => FilterRule::include(&wire_rule.pattern),
            RuleType::Exclude => FilterRule::exclude(&wire_rule.pattern),
            RuleType::Protect => FilterRule::protect(&wire_rule.pattern),
            RuleType::Risk => FilterRule::risk(&wire_rule.pattern),
            RuleType::Clear => {
                rules.push(
                    FilterRule::clear().with_sides(wire_rule.sender_side, wire_rule.receiver_side),
                );
                continue;
            }
            RuleType::DirMerge => {
                let mut config = DirMergeConfig::new(&wire_rule.pattern);
                if wire_rule.no_inherit {
                    config = config.with_inherit(false);
                }
                if wire_rule.exclude_from_merge {
                    config = config.with_exclude_self(true);
                }
                if wire_rule.sender_side {
                    config = config.with_sender_only(true);
                }
                if wire_rule.receiver_side {
                    config = config.with_receiver_only(true);
                }
                if wire_rule.perishable {
                    config = config.with_perishable(true);
                }
                merge_configs.push(config);
                continue;
            }
            RuleType::Merge => continue,
        };

        if wire_rule.sender_side || wire_rule.receiver_side {
            rule = rule.with_sides(wire_rule.sender_side, wire_rule.receiver_side);
        }
        if wire_rule.perishable {
            rule = rule.with_perishable(true);
        }
        if wire_rule.xattr_only {
            rule = rule.with_xattr_only(true);
        }
        if wire_rule.negate {
            rule = rule.with_negate(true);
        }
        if wire_rule.anchored {
            rule = rule.anchor_to_root();
        }

        rules.push(rule);
    }

    let filter_set = FilterSet::from_rules(rules)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("filter error: {e}")))?;

    Ok((filter_set, merge_configs))
}
