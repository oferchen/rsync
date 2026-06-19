//! Synchronous (non-pipelined) receiver transfer loop.
//!
//! Mirrors upstream `recv_files()` in its simplest, single-file-at-a-time form.
//! Kept for compatibility and testing; production callers should prefer
//! `run_pipelined` / `run_pipelined_incremental` which decouple network reads
//! from disk writes via the pipeline machinery.

use std::fs;
use std::io::{self, Read, Write};

use logging::{PhaseTimer, info_log};
use protocol::codec::{MonotonicNdxWriter, NdxCodec, create_ndx_codec};
use protocol::stats::DeleteStats;

use metadata::apply_metadata_with_cached_stat;

use engine::CleanupManager;

use crate::adaptive_buffer::adaptive_writer_capacity;
use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::map_file::MapFile;
use crate::receiver::basis::find_basis_file_with_config;
use crate::receiver::quick_check::is_hardlink_follower;
use crate::receiver::stats::TransferStats;
use crate::receiver::wire::{SenderAttrs, SumHead, write_signature_blocks};
use crate::receiver::{PipelineSetup, ReceiverContext, apply_acls_from_receiver_cache};
#[cfg(not(unix))]
use crate::temp_guard::open_tmpfile;
#[cfg(unix)]
use crate::temp_guard::open_tmpfile_sandboxed;
use crate::token_buffer::TokenBuffer;
use crate::token_reader::{DeltaToken as TokenReaderDeltaToken, LiteralData, TokenReader};

impl ReceiverContext {
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
        let (mut reader, file_count, setup) = self.setup_transfer(reader, writer)?;
        let reader = &mut reader;

        let PipelineSetup {
            dest_dir,
            metadata_opts,
            checksum_length,
            checksum_algorithm,
            acl_cache,
            #[cfg(unix)]
            sandbox,
        } = setup;

        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // First pass: create directories and symlinks from file list.
        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&dest_dir);
        let mut metadata_errors = self.create_directories(
            &dest_dir,
            &metadata_opts,
            acl_cache.as_deref(),
            writer,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;
        #[cfg(unix)]
        self.create_symlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&dest_dir, writer)?;

        // upstream: generator.c:1348-1354 - missing_args == 2 && file->mode == 0
        // deletes the destination path and skips any creation for the sentinel.
        self.process_missing_args_sentinels(
            &dest_dir,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;

        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        let mut checksum_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let mut token_buffer = TokenBuffer::with_default_capacity();

        // upstream: token.c uses a single compression context across all files.
        // For zstd the DCtx must persist across file boundaries (continuous
        // stream), so create the reader once and reuse it across the session.
        let compression = self.negotiated_algorithms.map(|n| n.compression);
        let mut token_reader = TokenReader::new(compression)?;

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

            let ndx = self.flat_to_wire_ndx(file_idx);
            ndx_write_codec.write_ndx(&mut *writer, ndx)?;

            // upstream: protocol >= 29 sender expects iflags after NDX.
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
                self.protocol.as_u8() >= 31
                    && self.compat_flags.is_some_and(|f| {
                        !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
                    }),
            )?;

            debug_assert_eq!(
                echoed_ndx, ndx,
                "sender echoed NDX {echoed_ndx} but we requested {ndx}"
            );

            let _echoed_sum_head = SumHead::read(reader)?;

            // SEC-1.r: route temp create + drop unlink through the sandbox
            // carrier so a TOCTOU swap on the temp parent cannot redirect
            // the create or the unlink-on-error.
            #[cfg(unix)]
            let (file, mut temp_guard) = open_tmpfile_sandboxed(
                &file_path,
                self.config.temp_dir.as_deref(),
                sandbox.as_ref(),
                Some(dest_dir.as_path()),
            )?;
            #[cfg(not(unix))]
            let (file, mut temp_guard) = open_tmpfile(&file_path, self.config.temp_dir.as_deref())?;
            CleanupManager::global().register_temp_file(temp_guard.path().to_path_buf());
            let target_size = file_entry.size();
            let writer_capacity = adaptive_writer_capacity(target_size);
            let mut output = std::io::BufWriter::with_capacity(writer_capacity, file);
            let mut total_bytes: u64 = 0;
            let mut literal_bytes: u64 = 0;

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

            apply_delta_tokens(
                reader,
                &mut output,
                &mut sparse_state,
                &mut basis_map,
                signature_opt.as_ref(),
                &mut token_reader,
                &mut token_buffer,
                &mut checksum_verifier,
                self.checksum_seed,
                &file_path,
                &mut total_bytes,
                &mut literal_bytes,
            )?;

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
                // SEC-1.j: route the backup rename through the sandbox dirfd
                // when both endpoints sit beneath the destination root as
                // single-component leaves, so a TOCTOU symlink swap on
                // either leaf cannot redirect the commit to an
                // attacker-chosen inode. Falls back to path-based
                // `std::fs::rename` for multi-component / cross-tree cases.
                #[cfg(unix)]
                {
                    let backup_rel = backup_path
                        .strip_prefix(&dest_dir)
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_else(|_| backup_path.clone());
                    fast_io::renameat_via_sandbox_or_fallback(
                        sandbox.as_deref(),
                        &dest_dir,
                        relative_path,
                        &file_path,
                        &dest_dir,
                        &backup_rel,
                        &backup_path,
                        true,
                    )?;
                }
                #[cfg(not(unix))]
                {
                    fs::rename(&file_path, &backup_path)?;
                }
                // upstream: backup.c:216-217 - DEBUG_GTE(BACKUP, 1) on RENAME
                // success branch of link_or_rename.
                engine::trace_make_backup_rename(&file_path.display().to_string());
                // upstream: backup.c:352 - INFO_GTE(BACKUP, 1) reports every
                // successful backup. Mirrors the local-copy executor emission
                // in engine::local_copy::context_impl::state::backup_existing_entry.
                // Paths are displayed relative to the destination root to
                // match upstream test assertions (testsuite/backup.test).
                let file_rel = file_path.strip_prefix(&dest_dir).unwrap_or(&file_path);
                let backup_rel_display =
                    backup_path.strip_prefix(&dest_dir).unwrap_or(&backup_path);
                info_log!(
                    Backup,
                    1,
                    "backed up {} to {}",
                    file_rel.display(),
                    backup_rel_display.display()
                );
            }

            // upstream: Linux 5.11+ io_uring submits IORING_OP_RENAMEAT; we
            // fall back to std::fs::rename on other platforms or older kernels.
            //
            // SEC-1.j: when the sandbox is plumbed and both temp leaf +
            // final leaf are single components beneath `dest_dir`, route
            // through `renameat(dirfd, leaf, dirfd, leaf)` so a TOCTOU
            // swap on either leaf cannot redirect the commit. The
            // io_uring fast path is preserved by trying it first; the
            // sandbox routing is the SEC-1.j hardening for the synchronous
            // fallback.
            if let Some(result) = fast_io::try_rename_via_io_uring(temp_guard.path(), &file_path) {
                result?;
            } else {
                #[cfg(unix)]
                {
                    let temp_path = temp_guard.path();
                    let temp_rel = temp_path
                        .strip_prefix(&dest_dir)
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or_else(|_| temp_path.to_path_buf());
                    fast_io::renameat_via_sandbox_or_fallback(
                        sandbox.as_deref(),
                        &dest_dir,
                        &temp_rel,
                        temp_path,
                        &dest_dir,
                        relative_path,
                        &file_path,
                        true,
                    )?;
                }
                #[cfg(not(unix))]
                {
                    fs::rename(temp_guard.path(), &file_path)?;
                }
            }
            CleanupManager::global().unregister_temp_file(temp_guard.path());
            temp_guard.keep();

            // Skip the stat inside apply_metadata_from_file_entry: the file
            // was just renamed from a temp file, so pass None to apply
            // ownership/permissions/timestamps unconditionally.
            if let Err(meta_err) =
                apply_metadata_with_cached_stat(&file_path, file_entry, &metadata_opts, None)
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

            // upstream: rsync.c:672-676 set_file_attrs emits the bare-name
            // notice AFTER the transfer/uptodate decision is known. Files
            // that take this branch are always `updated` (the receiver only
            // enters the delta loop when the generator selected the file
            // for transfer), so emit the upstream "updated" line. Uptodate
            // files are routed through the event-stream MetadataReused path
            // and never reach this point.
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(Name, 1, "{}", relative_path.display());
            }

            // upstream: io.c:820 stats.total_read only counts bytes read
            // off the wire. Matched-from-basis bytes never traverse the
            // read fd, so exclude them from bytes_received.
            bytes_received += literal_bytes;
            files_transferred += 1;
        }

        #[cfg(unix)]
        self.create_hardlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(&dest_dir, writer)?;

        // upstream: generator.c:2080-2133 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&dest_dir);

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
}

/// Applies the stream of delta tokens for a single file.
///
/// Drives the token reader until `End`, dispatching `Literal` writes through
/// the optional sparse-write path and resolving `BlockRef` tokens against the
/// (optional) basis map. On `End` the receiver-side checksum is compared
/// against the sender's; mismatch is a hard `InvalidData` error.
///
/// # Upstream Reference
///
/// - `match.c:hash_search()` - sender emits literal/block-ref tokens
/// - `receiver.c:recv_token()` - receiver-side token consumption
/// - `checksum.c:sum_init()` - per-file MD4 seed reset
#[allow(clippy::too_many_arguments)]
fn apply_delta_tokens<R: Read>(
    reader: &mut crate::reader::ServerReader<R>,
    output: &mut std::io::BufWriter<std::fs::File>,
    sparse_state: &mut Option<SparseWriteState>,
    basis_map: &mut Option<MapFile>,
    signature_opt: Option<&engine::signature::FileSignature>,
    token_reader: &mut TokenReader,
    token_buffer: &mut TokenBuffer,
    checksum_verifier: &mut ChecksumVerifier,
    checksum_seed: i32,
    file_path: &std::path::Path,
    total_bytes: &mut u64,
    literal_bytes: &mut u64,
) -> io::Result<()> {
    loop {
        match token_reader.read_token(reader)? {
            TokenReaderDeltaToken::End => {
                let checksum_len = checksum_verifier.digest_len();
                let mut expected_buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
                reader.read_exact(&mut expected_buf[..checksum_len])?;

                let algo = checksum_verifier.algorithm();
                // upstream: checksum.c:sum_init() prepends seed for legacy MD4.
                let old_verifier = std::mem::replace(
                    checksum_verifier,
                    ChecksumVerifier::for_algorithm_seeded(algo, checksum_seed),
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
                return Ok(());
            }
            TokenReaderDeltaToken::Literal(literal) => match literal {
                LiteralData::Ready(data) => {
                    let len = data.len();
                    write_chunk(output, sparse_state, &data)?;
                    checksum_verifier.update(&data);
                    *total_bytes += len as u64;
                    *literal_bytes += len as u64;
                }
                LiteralData::Pending(len) => {
                    if let Some(data) = reader.try_borrow_exact(len)? {
                        write_chunk(output, sparse_state, data)?;
                        checksum_verifier.update(data);
                    } else {
                        token_buffer.resize_for(len);
                        reader.read_exact(token_buffer.as_mut_slice())?;
                        let data = token_buffer.as_slice();
                        write_chunk(output, sparse_state, data)?;
                        checksum_verifier.update(data);
                    }
                    *total_bytes += len as u64;
                    *literal_bytes += len as u64;
                }
            },
            TokenReaderDeltaToken::BlockRef(block_idx) => {
                let (sig, map) = match (signature_opt, basis_map.as_mut()) {
                    (Some(sig), Some(map)) => (sig, map),
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "block reference {block_idx} without basis file {}{}",
                                crate::role_trailer::error_location!(),
                                crate::role_trailer::receiver()
                            ),
                        ));
                    }
                };

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

                let block_data = map.map_ptr(offset, bytes_to_copy)?;

                write_chunk(output, sparse_state, block_data)?;
                checksum_verifier.update(block_data);

                // upstream: token.c:631 - see_deflate_token()
                token_reader.see_token(block_data)?;

                *total_bytes += bytes_to_copy as u64;
            }
        }
    }
}

/// Writes a chunk either through the sparse-aware writer or straight to the
/// underlying buffered output.
fn write_chunk(
    output: &mut std::io::BufWriter<std::fs::File>,
    sparse_state: &mut Option<SparseWriteState>,
    data: &[u8],
) -> io::Result<()> {
    if let Some(sparse) = sparse_state.as_mut() {
        sparse.write(output, data)?;
        Ok(())
    } else {
        output.write_all(data)
    }
}
