//! Synchronous (non-pipelined) receiver transfer loop.
//!
//! Mirrors upstream `recv_files()` in its simplest, single-file-at-a-time form.
//! Kept for compatibility and testing; production callers should prefer
//! `run_pipelined` / `run_pipelined_incremental` which decouple network reads
//! from disk writes via the pipeline machinery.

use std::fs;
use std::io::{self, Read, Write};

use logging::{PhaseTimer, debug_log, info_log};
use protocol::codec::{MonotonicNdxWriter, NdxCodec, create_ndx_codec};
use protocol::stats::DeleteStats;

use metadata::apply_metadata_with_cached_stat;

use engine::CleanupManager;

use crate::delta_apply::ChecksumVerifier;
use crate::receiver::basis::find_basis_file_with_config;
use crate::receiver::quick_check::is_hardlink_follower;
use crate::receiver::stats::TransferStats;
use crate::receiver::wire::{SenderAttrs, SumHead, write_signature_blocks};
use crate::receiver::{PipelineSetup, ReceiverContext, apply_acls_from_receiver_cache};
#[cfg(not(unix))]
use crate::temp_guard::open_tmpfile;
#[cfg(unix)]
use crate::temp_guard::open_tmpfile_sandboxed;
use crate::token_reader::TokenReader;

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
            acl_id_map,
            #[cfg(unix)]
            sandbox,
        } = setup;

        // upstream: receiver.c:653-654 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // First pass: create directories and symlinks from file list.
        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&dest_dir);
        let mut metadata_errors = self.create_directories(
            &dest_dir,
            &metadata_opts,
            acl_cache.as_deref(),
            acl_id_map.as_deref(),
            writer,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;
        #[cfg(unix)]
        self.create_symlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&dest_dir, writer)?;
        #[cfg(unix)]
        self.create_specials(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_specials(&dest_dir, writer)?;

        // upstream: generator.c:1348-1354 - missing_args == 2 && file->mode == 0
        // deletes the destination path and skips any creation for the sentinel.
        self.process_missing_args_sentinels(
            &dest_dir,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;

        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // upstream: token.c uses a single compression context across all files.
        // For zstd the DCtx must persist across file boundaries (continuous
        // stream), so create the reader once and reuse it across the session.
        let compression = self.negotiated_algorithms.map(|n| n.compression);
        let mut token_reader = TokenReader::new(compression)?;

        let deadline = crate::shared::TransferDeadline::from_system_time(self.config.stop_at);

        // upstream: generator.c:1249 - list-only renders the flist without
        // requesting any file data. Capture the entries and skip the per-file
        // NDX loop entirely so no per-file request crosses the wire.
        let list_only_entries = if self.config.flags.list_only {
            self.collect_list_only_entries()
        } else {
            Vec::new()
        };

        // upstream: generator.c:2300-2305 - pre-read INC_RECURSE sub-lists so a
        // hardlink follower's leader (which may live in a later segment) is
        // resolved before the per-file loop. No-op without INC_RECURSE.
        let mut flist_ndx_codec = create_ndx_codec(self.protocol.as_u8());
        if self.config.flags.hard_links {
            self.prefetch_for_hardlinks(reader, &mut flist_ndx_codec)?;
        }

        // upstream: generator.c:2299-2368 - walk the flist by a flat cursor,
        // pulling the next INC_RECURSE segment on demand. Without INC_RECURSE the
        // list is already complete, so `ensure_flat_idx` never reads the wire and
        // this is a plain 0..len walk.
        let mut flat_idx = 0usize;
        while self.ensure_flat_idx(flat_idx, reader, &mut flist_ndx_codec)? {
            let file_idx = flat_idx;
            flat_idx += 1;
            if self.config.flags.list_only {
                break;
            }
            if let Some(ref dl) = deadline {
                if dl.is_reached() {
                    break;
                }
            }

            let file_entry = &self.file_list[file_idx];
            let relative_path = file_entry.path();
            // upstream: receiver.c:708-709 DEBUG_GTE(RECV, 1)
            debug_log!(Recv, 1, "recv_files({})", relative_path.display());

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

            // upstream: generator.c:1704-1718 - skip files outside the
            // min-size/max-size window with a SKIP-gated notice on FINFO.
            if self.emit_size_bound_skip(
                writer,
                file_entry,
                self.config.file_selection.min_file_size,
                self.config.file_selection.max_file_size,
            ) {
                continue;
            }

            let ndx = self.flat_to_wire_ndx(file_idx);
            ndx_write_codec.write_ndx(&mut *writer, ndx)?;

            // The basis search must precede the iflags write so a
            // --partial-dir resume basis can set ITEM_BASIS_TYPE_FOLLOWS.
            let basis_config = self.build_basis_file_config(
                &file_path,
                &dest_dir,
                relative_path,
                file_entry.size(),
                file_entry.mtime(),
                checksum_length,
                checksum_algorithm,
            );
            let basis_result = find_basis_file_with_config(&basis_config);
            let signature_opt = basis_result.signature;
            let basis_path_opt = basis_result.basis_path;
            let fnamecmp_type = basis_result.fnamecmp_type;

            // upstream: protocol >= 29 sender expects iflags after NDX.
            // upstream: generator.c:1942-1943 - a non-FNAME basis sets
            // ITEM_BASIS_TYPE_FOLLOWS followed by the fnamecmp_type byte.
            if self.protocol.supports_iflags() {
                let mut iflags = SenderAttrs::ITEM_TRANSFER;
                let emit_basis_type = fnamecmp_type != protocol::FnameCmpType::Fname;
                if emit_basis_type {
                    iflags |= SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS;
                }
                writer.write_all(&iflags.to_le_bytes())?;
                if emit_basis_type {
                    writer.write_all(&[u8::from(fnamecmp_type)])?;
                }
            }

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
            let open_result = open_tmpfile_sandboxed(
                &file_path,
                self.config.temp_dir.as_deref(),
                sandbox.as_ref(),
                Some(dest_dir.as_path()),
            );
            #[cfg(not(unix))]
            let open_result = open_tmpfile(&file_path, self.config.temp_dir.as_deref());

            // upstream: receiver.c:999-1006 - when open_tmpfile() returns fd == -1
            // (e.g. EACCES from a read-only destination directory) the receiver
            // does NOT abort the receive loop. It logs the error, calls
            // discard_receive_data() to drain this file's delta off the wire, and
            // continues to the next NDX. Propagating the error here instead would
            // leave the delta stream undrained -> the next NDX read parses delta
            // bytes as a frame header -> protocol desync (exit 12 / crash). We
            // mirror the discard path: drain the tokens + trailing checksum, mark
            // the transfer partial (IOERR_GENERAL -> RERR_PARTIAL, exit 23, same
            // as upstream's FERROR_XFER -> got_xfer_error -> _exit(RERR_PARTIAL)),
            // and move on.
            let (file, mut temp_guard) = match open_result {
                Ok(pair) => pair,
                Err(open_err) => {
                    // The checksum length matches what receive_data() would read
                    // for this file's trailing whole-file sum (receiver.c:515).
                    let checksum_len = ChecksumVerifier::new(
                        self.negotiated_algorithms.as_ref(),
                        self.protocol,
                        self.checksum_seed,
                        self.compat_flags.as_ref(),
                    )
                    .digest_len();

                    token_reader.reset();
                    crate::delta_apply::discard_delta_stream(
                        reader,
                        &mut token_reader,
                        checksum_len,
                    )?;

                    metadata_errors
                        .push((file_path.clone(), format!("mkstemp failed: {open_err}")));
                    // upstream: FERROR_XFER on the open failure sets
                    // got_xfer_error, which main.c maps to _exit(RERR_PARTIAL).
                    self.flist_io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
                    continue;
                }
            };
            CleanupManager::global().register_temp_file(temp_guard.path().to_path_buf());
            temp_guard.mark_registered();

            let use_sparse = self.config.flags.sparse;

            // Drive the per-file reconstruction through `DeltaApplicator`. It
            // owns its basis handle (opened from the path), its own sparse
            // state, token buffer, and per-file checksum verifier - the same
            // mechanism the pipelined receiver uses. Behaviour is identical to
            // the previous in-module `apply_delta_tokens` loop:
            //   - literal/block-ref dispatch with `see_token` after every
            //     block match keeps the `-z` inflate dictionary synced
            //     (token.c:631);
            //   - sparse mode runs the same `SparseWriteState`; the post-write
            //     size check against `file_entry.size()` is performed below on
            //     `result.final_pos`, byte-identical to the old standalone loop;
            //   - `finish` reads + verifies the wire checksum exactly like the
            //     old `End` branch, raising `InvalidData` on mismatch.
            // The verifier MUST be a fresh per-file instance built the same way
            // the session-shared one is (and the same way the old `End` branch
            // reset it via `for_algorithm_seeded`): seed prepended only for
            // legacy MD4, no seed for MD5/XXH*.
            let file_verifier = ChecksumVerifier::new(
                self.negotiated_algorithms.as_ref(),
                self.protocol,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            );

            // REFLINK: the receiver's `ServerConfig` does not carry a CoW
            // policy - `--cow`/`--no-cow` is plumbed only through the
            // local-copy client config (core::client), never to this
            // wire-receive scope. Default to `Auto`; threading the flag here
            // would require new ServerConfig plumbing across the daemon/server
            // boundary, out of scope for this cutover.
            let config = crate::delta_apply::DeltaApplyConfig {
                sparse: use_sparse,
                writer_kind: crate::delta_apply::BasisWriterKind::Standard,
                cow_policy: fast_io::CowPolicy::Auto,
            };

            token_reader.reset();

            let mut applicator = crate::delta_apply::DeltaApplicator::new(
                file,
                &config,
                file_verifier,
                signature_opt.as_ref(),
                basis_path_opt.as_deref(),
            )?;

            crate::delta_apply::apply_delta_stream(reader, &mut applicator, &mut token_reader)?;

            // Pass `None` so `finish` records the sparse `final_pos` without
            // emitting its own generic size-mismatch error; the receiver
            // re-checks below to preserve the exact pre-change message text
            // (file path + role trailer). `finish` still reads and verifies
            // the wire checksum identically to the old `End` branch.
            let (file, result) = applicator.finish(reader, None)?;

            // Sparse mode: verify the materialized file length against the
            // file-list entry size. Mirrors the original sync.rs:276-289 check
            // byte-for-byte (same ErrorKind, same message). `final_pos` is the
            // position `SparseWriteState::finish` returned inside `finish`.
            if let Some(final_pos) = result.final_pos {
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

            // upstream: io.c:820 - only literal bytes traverse the read fd;
            // matched-from-basis bytes never do. Preserve the exact stat
            // mapping the old loop used (`literal_bytes` -> `bytes_received`).
            let literal_bytes = result.literal_bytes;

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
                acl_id_map.as_deref(),
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

        // upstream: generator.c:2169 finish_hard_link() itemizes every follower
        // before the phase's NDX_DONE. Emit through the request-phase NDX
        // diff-state so a pushing client's sender renders each `hf...` /
        // `=> leader` row (a no-op in client-mode pull).
        self.emit_server_hardlink_follower_itemize(writer, ndx_write_codec.inner_mut())?;

        #[cfg(unix)]
        self.create_hardlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(&dest_dir, writer)?;

        // upstream: generator.c:2080-2133 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&dest_dir);

        self.finalize_transfer(reader, writer)?;

        // upstream: io.c:1547 - io_error |= val on MSG_IO_ERROR from the sender.
        // The sender emits MSG_IO_ERROR (sender.c:485-486) for source files that
        // vanished or could not be opened during its send loop. Fold those bits
        // into the exit-code io_error so the receiver reports 24/23; MSG_NO_SEND
        // alone only skips the file and carries no exit-code bits.
        let sender_io_error = reader.take_io_error();

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
            bytes_sent: 0,
            total_source_bytes,
            metadata_errors,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error
                | sender_io_error,
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
            list_only_entries,
        })
    }
}
