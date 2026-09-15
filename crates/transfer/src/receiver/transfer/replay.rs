//! Local-replay (`--read-batch`) receiver drive.
//!
//! Drives the real receiver off a pre-recorded batch stream. Unlike the
//! network drivers, which plan requests and expect the sender to echo them
//! back in lockstep, a recorded stream is authoritative: upstream's
//! `recv_files()` reads each `NDX + iflags` row off `f_in` and obeys it -
//! non-transfer itemize rows are logged and skipped, transfer rows carry
//! their own recorded sum head and delta tokens (`receiver.c:828-1050`).
//! The record-time generator's choices (which files to update, which rows to
//! itemize) therefore rule the replay, exactly as they do upstream where the
//! replaying generator's requests fall into a consumer-less pipe.

use std::io::{self, Read, Write};
use std::num::{NonZeroU8, NonZeroU32};

use logging::{PhaseTimer, debug_log, info_log};
use protocol::codec::{MonotonicNdxWriter, create_ndx_codec};

use metadata::apply_metadata_with_cached_stat;

use engine::CleanupManager;
use signature::{FileSignature, SignatureLayout};

use crate::delta_apply::ChecksumVerifier;
use crate::receiver::stats::TransferStats;
use crate::receiver::wire::{SenderAttrs, SumHead};
use crate::receiver::{PipelineSetup, ReceiverContext, apply_acls_from_receiver_cache};
#[cfg(not(unix))]
use crate::temp_guard::open_tmpfile;
#[cfg(unix)]
use crate::temp_guard::open_tmpfile_sandboxed;
use crate::token_reader::TokenReader;
use crate::transfer_ops::usable_basis;
use crate::transfer_ops::wire_basis::WireBasis;

impl ReceiverContext {
    /// Drives the receiver over a recorded batch stream, sender-driven.
    ///
    /// The loop mirrors upstream `recv_files()` under `read_batch`
    /// (`receiver.c:828-1050`): each recorded `NDX + iflags` row is read off
    /// the stream and obeyed. `NDX_DONE` advances the phase
    /// (`receiver.c:836-862`, `max_phase = protocol >= 29 ? 2 : 1`); a row
    /// without `ITEM_TRANSFER` is a metadata-only itemize record and is
    /// skipped (`receiver.c:904-925`); a transfer row names the flist entry,
    /// and its recorded sum head describes the block layout the recorded
    /// `COPY` tokens reference (`receiver.c:282 read_sum_head` inside
    /// `receive_data`).
    ///
    /// The generator half still runs locally, exactly as upstream's forked
    /// generator does on `--read-batch` (`main.c:639-651`): directories,
    /// symlinks and specials are created, and metadata-only fixes for
    /// up-to-date entries are applied - only its outbound requests are
    /// discarded.
    pub(in crate::receiver) fn run_replay<
        R: Read,
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    >(
        &mut self,
        reader: crate::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        let _t = PhaseTimer::new("receiver-transfer-replay");
        let (mut reader, file_count, setup) = self.setup_transfer(reader, writer)?;
        let reader = &mut reader;

        // upstream: main.c:1383-1392 - a client handed an empty list skips
        // do_recv() entirely; same gate as the network drivers.
        if self.is_empty_client_flist(file_count) {
            return self.finish_empty_client_flist(reader, writer);
        }

        // Materialize any INC_RECURSE sub-list segments up front so the
        // generator-half passes below see the complete list. The recorded
        // stream carries the segments exactly where the tee captured them,
        // ahead of the delta rows. upstream: generator.c:2299-2368.
        let mut flist_ndx_codec = create_ndx_codec(self.protocol.as_u8());
        self.ensure_all_segments_loaded(reader, &mut flist_ndx_codec)?;

        let PipelineSetup {
            dest_dir,
            metadata_opts,
            checksum_length: _,
            checksum_algorithm: _,
            acl_cache,
            acl_id_map,
            #[cfg(unix)]
            sandbox,
        } = setup;

        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        // Generator half: create directories, symlinks and specials, then the
        // quick-check walk that applies metadata-only fixes for up-to-date
        // entries. upstream runs the real generator locally on --read-batch
        // (main.c:639-651); only its outbound requests go to the dead pipe.
        let mut metadata_errors = self.create_directories(
            &dest_dir,
            &metadata_opts,
            acl_cache.as_deref(),
            acl_id_map.as_deref(),
            writer,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;
        self.ensure_relative_parents(
            &dest_dir,
            #[cfg(unix)]
            sandbox.as_deref(),
        );
        #[cfg(unix)]
        self.create_symlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&dest_dir, writer)?;
        #[cfg(unix)]
        self.create_specials(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_specials(&dest_dir, writer)?;
        self.process_missing_args_sentinels(
            &dest_dir,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;

        let (num_dirs, num_symlinks, num_devices, num_specials) = self.file_type_counts();
        let mut stats = TransferStats {
            files_listed: file_count,
            num_dirs,
            num_symlinks,
            num_devices,
            num_specials,
            entries_received: file_count as u64,
            io_error: self.flist_reader_io_error() | self.flist_io_error,
            ..Default::default()
        };

        // Quick-check walk for its side effects only (metadata-only fixes,
        // upstream generator.c:1900 set_file_attrs for identical files). The
        // returned request list is discarded: the recorded stream, not the
        // local plan, decides what is transferred.
        drop(self.build_files_to_transfer(
            writer,
            &dest_dir,
            #[cfg(unix)]
            sandbox.as_deref(),
            &metadata_opts,
            None,
            &mut metadata_errors,
            &mut stats,
            acl_cache.as_deref(),
            acl_id_map.as_deref(),
        ));
        self.server_no_transfer_itemize.borrow_mut().clear();

        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // upstream: compat.c:414 getenv_nstr() pins every batch to the zlib
        // codec, carried on the compact `-z` flag; there is no negotiated
        // vstring on a replay (compat.c:797 do_negotiated_strings = 0).
        let compression = if self.config.flags.compress {
            Some(protocol::CompressionAlgorithm::Zlib)
        } else {
            self.negotiated_algorithms.map(|n| n.compression)
        };
        let mut token_reader = TokenReader::new(compression, u32::from(self.protocol.as_u8()))?;

        let preserve_xattrs = self.config.flags.xattrs;
        let want_xattr_optim = self.protocol.as_u8() >= 31
            && self.compat_flags.is_some_and(|f| {
                !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
            });

        let mut files_transferred = 0usize;
        let mut transferred_file_size = 0u64;
        let mut bytes_received = 0u64;
        let mut literal_data = 0u64;
        let mut matched_data = 0u64;

        // upstream: receiver.c:809 - max_phase = protocol >= 29 ? 2 : 1.
        let max_phase: i32 = if self.protocol.as_u8() >= 29 { 2 } else { 1 };
        let mut phase: i32 = 0;

        loop {
            let row = crate::receiver::ndx_stream::read_ndx_and_attrs(
                reader,
                &mut ndx_read_codec,
                self,
                preserve_xattrs,
                want_xattr_optim,
            )?;

            let Some((ndx, attrs)) = row else {
                // upstream: receiver.c:854-862 - NDX_DONE advances the phase;
                // the loop ends once phase > max_phase.
                phase += 1;
                if phase > max_phase {
                    break;
                }
                debug_log!(Recv, 1, "recv_files phase={}", phase);
                continue;
            };

            if attrs.iflags & SenderAttrs::ITEM_TRANSFER == 0 {
                // upstream: receiver.c:904-925 - a metadata-only itemize row
                // carries no data payload; the local generator half already
                // applied the attribute fixes, so the row is only consumed.
                // Such a row may also name a segment's parent directory via
                // an index below the segment's ndx_start (receiver.c:864-871
                // resolves it from dir_flist), so no flist lookup is needed.
                continue;
            }

            let Some(flat_idx) = self.wire_to_flat_ndx(ndx) else {
                return Err(protocol::protocol_violation(format!(
                    "recorded batch names file index {ndx} outside the file list {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                )));
            };
            let file_entry = self.file_list[flat_idx].clone();
            let relative_path = file_entry.path();

            // upstream: receiver.c:926-930 - phase 2 carries no transfers.
            if phase == 2 {
                return Err(protocol::protocol_violation(format!(
                    "got transfer request in phase 2 {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                )));
            }

            // upstream: receiver.c:1044-1050 - a transfer row must name a
            // regular file.
            if !file_entry.is_file() {
                return Err(protocol::protocol_violation(format!(
                    "recorded transfer for non-regular file {} {}{}",
                    relative_path.display(),
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                )));
            }

            let file_path = if relative_path.as_os_str() == "." {
                dest_dir.clone()
            } else {
                dest_dir.join(relative_path)
            };
            debug_log!(Recv, 1, "recv_files({})", relative_path.display());

            // upstream: receiver.c:282 receive_data() begins with
            // read_sum_head(f_in) - the recorded head describes the block
            // layout the recorded COPY tokens reference.
            let sum_head = SumHead::read(reader)?;

            // upstream: receiver.c:995-1046 - the recorded fnamecmp_type/xname
            // select the basis; FNAMECMP_FNAME (the default) is the
            // destination file itself.
            let wire_basis = WireBasis {
                entry_relative_path: relative_path,
                basis_dirs: &self.config.reference_directories,
                fuzzy_level: self.config.flags.fuzzy_level,
            };
            let basis_path = match wire_basis.resolve(
                attrs.fnamecmp_type,
                attrs.xname.as_deref(),
                &file_path,
            )? {
                Some(named) => usable_basis(&named),
                None => usable_basis(&file_path),
            };

            // The recorded sum head is the applicator's block layout; the
            // strong sums themselves are not needed to resolve COPY offsets.
            let layout_signature = (sum_head.count > 0 && sum_head.blength > 0)
                .then(|| {
                    NonZeroU32::new(sum_head.blength).map(|blength| {
                        FileSignature::from_raw_parts(
                            SignatureLayout::from_raw_parts(
                                blength,
                                sum_head.remainder,
                                u64::from(sum_head.count),
                                NonZeroU8::new(u8::try_from(sum_head.s2length).unwrap_or(16))
                                    .unwrap_or(NonZeroU8::MIN),
                            ),
                            Vec::new(),
                            sum_head.flength(),
                        )
                    })
                })
                .flatten();

            #[cfg(unix)]
            let open_result = open_tmpfile_sandboxed(
                &file_path,
                self.config.temp_dir.as_deref(),
                sandbox.as_ref(),
                Some(dest_dir.as_path()),
            );
            #[cfg(not(unix))]
            let open_result = open_tmpfile(&file_path, self.config.temp_dir.as_deref());

            // upstream: receiver.c:999-1006 - a failed temp open drains this
            // file's delta off the stream and continues (see sync.rs for the
            // full rationale).
            let (file, mut temp_guard) = match open_result {
                Ok(pair) => pair,
                Err(open_err) => {
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
                    self.flist_io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
                    continue;
                }
            };
            CleanupManager::global().register_temp_file(temp_guard.path().to_path_buf());
            temp_guard.mark_registered();

            let file_verifier = ChecksumVerifier::new(
                self.negotiated_algorithms.as_ref(),
                self.protocol,
                self.checksum_seed,
                self.compat_flags.as_ref(),
            );

            let config = crate::delta_apply::DeltaApplyConfig {
                sparse: self.config.flags.sparse,
                writer_kind: crate::delta_apply::BasisWriterKind::Standard,
                cow_policy: fast_io::CowPolicy::Auto,
            };

            token_reader.reset();

            let mut applicator = crate::delta_apply::DeltaApplicator::new(
                file,
                &config,
                file_verifier,
                layout_signature.as_ref(),
                basis_path.as_deref(),
            )?;

            crate::delta_apply::apply_delta_stream(reader, &mut applicator, &mut token_reader)?;
            let (file, result) = applicator.finish(reader, None)?;

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

            // Commit: rename the temp file over the destination. Mirrors the
            // SEC-1.j routing of sync.rs (the sandbox-anchored renameat with
            // the io_uring fast path first).
            if let Some(rename_result) =
                fast_io::try_rename_via_io_uring(temp_guard.path(), &file_path)
            {
                rename_result?;
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
                #[cfg(windows)]
                {
                    crate::temp_guard::commit_rename_no_follow(temp_guard.path(), &file_path)?;
                }
                #[cfg(all(not(unix), not(windows)))]
                {
                    std::fs::rename(temp_guard.path(), &file_path)?;
                }
            }
            CleanupManager::global().unregister_temp_file(temp_guard.path());
            temp_guard.keep();

            if let Err(meta_err) =
                apply_metadata_with_cached_stat(&file_path, &file_entry, &metadata_opts, None)
            {
                metadata_errors.push((file_path.clone(), meta_err.to_string()));
            } else if let Some(ref xattr_list) = self.resolve_xattr_list(&file_entry) {
                let filter = self.xattr_name_filter().map(|set| {
                    move |name: &str| set.xattr_name_allowed(name, filters::XattrSide::Receiver)
                });
                let filter_ref = filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool);
                if let Err(e) = metadata::apply_xattrs_from_list(
                    &file_path,
                    xattr_list,
                    true,
                    Some(&file_path),
                    filter_ref,
                    None,
                ) {
                    metadata_errors.push((file_path.clone(), e.to_string()));
                }
            }

            if let Err(acl_err) = apply_acls_from_receiver_cache(
                &file_path,
                &file_entry,
                acl_cache.as_deref(),
                acl_id_map.as_deref(),
                !file_entry.is_symlink(),
            ) {
                metadata_errors.push((file_path.clone(), acl_err.to_string()));
            }

            // upstream: rsync.c:672-676 - name the updated file under -v.
            if self.config.flags.verbose
                && self.config.connection.client_mode
                && !self.should_emit_itemize()
            {
                info_log!(Name, 1, "{}", relative_path.display());
            }

            bytes_received += literal_bytes;
            literal_data += literal_bytes;
            matched_data += result.bytes_written.saturating_sub(literal_bytes);
            files_transferred += 1;
            transferred_file_size += file_entry.size();
        }

        // upstream: generator.c:2169 finish_hard_link() itemizes each follower
        // before the phase's NDX_DONE, then the followers are linked.
        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        #[cfg(unix)]
        self.emit_server_hardlink_follower_itemize(
            writer,
            ndx_write_codec.inner_mut(),
            &dest_dir,
            sandbox.as_deref(),
        )?;
        #[cfg(not(unix))]
        self.emit_server_hardlink_follower_itemize(writer, ndx_write_codec.inner_mut(), &dest_dir)?;

        #[cfg(unix)]
        self.create_hardlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(&dest_dir, writer)?;

        // upstream: generator.c:2093-2146 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&dest_dir, writer);

        self.finalize_replay(reader, writer)?;

        stats.io_error |= reader.take_io_error();
        stats.got_xfer_error = reader.xfer_error_count() > 0 || self.got_xfer_error.get();
        stats.files_transferred = files_transferred;
        stats.transferred_file_size = transferred_file_size;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.total_source_bytes = self.total_source_size();
        if !metadata_errors.is_empty() {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        if self.dest_root_created {
            self.record_created(protocol::flist::FileType::Directory.to_mode_bits());
        }
        stats.created_stats = self.created_stats.get();
        stats.delete_stats = self.effective_del_stats();

        Ok(stats)
    }

    /// Finalizes a replay after the phase `NDX_DONE`s have been consumed by
    /// the row loop: reads the recorded stats trailer, then completes the
    /// pipeline FSM.
    ///
    /// The network `finalize_transfer` cannot be reused here: its
    /// `exchange_phase_done` writes and reads the phase boundary markers
    /// itself, but a sender-driven loop has already consumed them, exactly as
    /// upstream's `recv_files()` does before `handle_stats()` runs
    /// (`main.c:1085-1096`, `main.c:362-373`).
    ///
    /// No goodbye read follows the stats: upstream's replay receiver ends at
    /// `handle_stats(f_in)` - `read_final_goodbye()` is sender-only
    /// (`main.c:908`) - so any recorded goodbye bytes (written only when the
    /// recording sender teed them, protocol >= 31) are left unread exactly as
    /// upstream leaves them. A protocol-29/30 recording carries none at all.
    fn finalize_replay<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<()> {
        self.pipeline
            .advance_to(crate::transfer_state::TransferPhase::Finalization)
            .map_err(crate::fsm_error)?;

        // upstream: main.c:362-373 - the client receiver reads the sender's
        // recorded byte counters from the trailer.
        if self.config.connection.client_mode {
            self.sender_stats = Some(self.receive_stats(reader)?);
        }

        writer.flush()?;
        debug_log!(Recv, 1, "recv_files finished");

        self.pipeline
            .advance_to(crate::transfer_state::TransferPhase::Complete)
            .map_err(crate::fsm_error)?;
        Ok(())
    }
}
