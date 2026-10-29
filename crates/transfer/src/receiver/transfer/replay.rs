//! Local-replay (`--read-batch`) receiver drive.
//!
//! Drives the real receiver off a pre-recorded batch stream. Unlike the
//! network drivers, which plan requests and expect the sender to echo them
//! back in lockstep, a recorded stream is authoritative: upstream's
//! `recv_files()` reads each `NDX + iflags` row off `f_in` and obeys it -
//! non-transfer itemize rows are logged and skipped, transfer rows carry
//! their own recorded sum head and delta tokens (`receiver.c:844-1066`).
//! The record-time generator's choices (which files to update, which rows to
//! itemize) therefore rule the replay, exactly as they do upstream where the
//! replaying generator's requests fall into a consumer-less pipe.

use std::io::{self, Read, Write};

use logging::{PhaseTimer, debug_log, info_log};
use protocol::codec::{MonotonicNdxWriter, create_ndx_codec};

use metadata::apply_metadata_with_cached_stat;

use engine::CleanupManager;

use crate::delta_apply::ChecksumVerifier;
use crate::receiver::stats::TransferStats;
use crate::receiver::wire::{SenderAttrs, SumHead};
use crate::receiver::{PipelineSetup, ReceiverContext, apply_acls_from_receiver_cache};
use crate::temp_guard::TempFileGuard;
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
    /// (`receiver.c:844-1066`): each recorded `NDX + iflags` row is read off
    /// the stream and obeyed. `NDX_DONE` advances the phase
    /// (`receiver.c:852-878`, `max_phase = protocol >= 29 ? 2 : 1`); a row
    /// without `ITEM_TRANSFER` is a metadata-only itemize record and is
    /// skipped (`receiver.c:920-941`); a transfer row names the flist entry,
    /// and its recorded sum head describes the block layout the recorded
    /// `COPY` tokens reference (`receiver.c:510 read_sum_head` inside
    /// `receive_data`).
    ///
    /// The generator half still runs locally, exactly as upstream's forked
    /// generator does on `--read-batch` (`main.c:652-664`): directories,
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
        // Buffer itemize rows so the generator-half's speculative local rows can
        // be dropped below and only the recorded stream's authoritative rows are
        // flushed, in flist-index order (like run_pipelined). Under a custom
        // `--out-format` the buffered rows are metadata events drained by the
        // dispatch after this returns.
        self.defer_itemize = true;
        let (mut reader, file_count, setup) = self.setup_transfer(reader, writer)?;
        let reader = &mut reader;

        // upstream: main.c:1438-1446 - a client handed an empty list skips
        // do_recv() entirely; same gate as the network drivers.
        if self.is_empty_client_flist(file_count) {
            return self.finish_empty_client_flist(reader, writer);
        }

        let inc_recurse = self
            .compat_flags
            .is_some_and(|f| f.contains(protocol::CompatibilityFlags::INC_RECURSE));
        let PipelineSetup {
            dest_dir,
            metadata_opts,
            acl_cache,
            acl_id_map,
            #[cfg(unix)]
            sandbox,
            ..
        } = &setup;
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);
        let mut failed_dirs = crate::receiver::directory::FailedDirectories::new();
        let mut generated_segments = 0usize;
        let (mut metadata_errors, mut stats) = if inc_recurse {
            // upstream: under INC_RECURSE the replaying generator takes each
            // file list as it arrives (generator.c:2770-2841), and the recorded
            // stream carries the sub-lists among the data rows wherever the
            // sender's lookahead put them (sender.c:515-547). The generator
            // half therefore runs per list: the initial list now, each later
            // one as the row loop below receives it.
            let mut metadata_errors = Vec::new();
            let mut stats = TransferStats {
                files_listed: file_count,
                entries_received: file_count as u64,
                ..Default::default()
            };
            self.generate_replay_segments(
                &mut generated_segments,
                &setup,
                &mut failed_dirs,
                writer,
                &mut metadata_errors,
                &mut stats,
            )?;
            (metadata_errors, stats)
        } else {
            // Generator half: create directories, symlinks and specials, then the
            // quick-check walk that applies metadata-only fixes for up-to-date
            // entries. upstream runs the real generator locally on --read-batch
            // (main.c:652-664); only its outbound requests go to the dead pipe.
            let mut metadata_errors = self.create_directories(
                dest_dir,
                metadata_opts,
                acl_cache.as_deref(),
                acl_id_map.as_deref(),
                writer,
                #[cfg(unix)]
                sandbox.as_deref(),
            )?;
            self.ensure_relative_parents(
                dest_dir,
                #[cfg(unix)]
                sandbox.as_deref(),
            );
            #[cfg(unix)]
            self.create_symlinks(dest_dir, sandbox.as_deref(), writer)?;
            #[cfg(not(unix))]
            self.create_symlinks(&dest_dir, writer)?;
            #[cfg(unix)]
            self.create_specials(
                dest_dir,
                acl_cache.as_deref(),
                acl_id_map.as_deref(),
                sandbox.as_deref(),
                writer,
            )?;
            #[cfg(not(unix))]
            self.create_specials(&dest_dir, writer)?;
            self.process_missing_args_sentinels(
                dest_dir,
                #[cfg(unix)]
                sandbox.as_deref(),
                writer,
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

            // upstream: generator.c:2753-2754 - `do_delete_pass()` runs regardless
            // of read_batch (the replaying generator forks and runs locally,
            // main.c:652-664). --delete-before / --delete-during sweep here, before
            // the row loop, exactly as the network drivers do (pipelined.rs Early);
            // --delete-after / --delete-delay defer to the late site below.
            if self.delete_pass_is_early() {
                self.run_receiver_delete_pass(
                    super::DeletePassPhase::Early,
                    dest_dir,
                    #[cfg(unix)]
                    sandbox.as_ref(),
                    writer,
                    &mut stats,
                )?;
            }

            // Quick-check walk for its side effects only (metadata-only fixes,
            // upstream generator.c:2246 set_file_attrs for identical files). The
            // returned request list is discarded: the recorded stream, not the
            // local plan, decides what is transferred.
            drop(self.build_files_to_transfer(
                writer,
                dest_dir,
                #[cfg(unix)]
                sandbox.as_deref(),
                metadata_opts,
                None,
                &mut metadata_errors,
                &mut stats,
                acl_cache.as_deref(),
                acl_id_map.as_deref(),
            ));
            self.server_no_transfer_itemize.borrow_mut().clear();
            // The generator half above itemizes against the LOCAL quick-check (it
            // classifies which files it would transfer), but on a replay the
            // recorded stream - not the local plan - decides what is itemized and
            // transferred (upstream drives itemize off the stream iflags:
            // receiver.c:919 maybe_log_item for non-transfer rows, receiver.c:1290
            // log_item for transfer rows). Drop the speculative rows so only the
            // recorded rows emitted in the loop below survive; this is the itemize
            // counterpart of the server_no_transfer_itemize clear above.
            self.itemize_rows.borrow_mut().clear();
            self.event_rows.borrow_mut().clear();
            (metadata_errors, stats)
        };
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // upstream: compat.c:413-415 getenv_nstr() pins every batch to the zlib
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

        // upstream: receiver.c:825 - max_phase = protocol >= 29 ? 2 : 1.
        let max_phase: i32 = if self.protocol.as_u8() >= 29 { 2 } else { 1 };
        let mut phase: i32 = 0;

        // upstream: receiver.c:875-886 - with INC_RECURSE each NDX_DONE frees
        // the oldest list still held and, while another remains, does not
        // advance the phase. The lists counted are those received so far: the
        // recording sender echoes a DONE as each list is freed, which can be
        // before the later sub-lists arrive (sender.c:525-545).
        let mut freed_lists = 0usize;
        let mut gap_heads = self.gap_ndx_heads();

        loop {
            let row = crate::receiver::ndx_stream::read_ndx_and_attrs(
                reader,
                &mut ndx_read_codec,
                self,
                preserve_xattrs,
                want_xattr_optim,
            )?;
            // A sub-list read with this row is generated before the row is
            // obeyed, as upstream's generator handles a list ahead of its data.
            if inc_recurse && generated_segments < self.ndx_segments.len() {
                self.generate_replay_segments(
                    &mut generated_segments,
                    &setup,
                    &mut failed_dirs,
                    writer,
                    &mut metadata_errors,
                    &mut stats,
                )?;
                gap_heads = self.gap_ndx_heads();
            }
            let Some((ndx, attrs)) = row else {
                if inc_recurse && freed_lists < self.ndx_segments.len() {
                    freed_lists += 1;
                    if freed_lists < self.ndx_segments.len() {
                        continue;
                    }
                }
                // upstream: receiver.c:870-875 - NDX_DONE advances the phase;
                // the loop ends once phase > max_phase.
                phase += 1;
                if phase > max_phase {
                    break;
                }
                debug_log!(Recv, 1, "recv_files phase={}", phase);
                continue;
            };

            if attrs.iflags & SenderAttrs::ITEM_TRANSFER == 0 {
                // upstream: receiver.c:919 maybe_log_item(file, iflags, ...) - a
                // metadata-only itemize row carries no data payload; the local
                // generator half already applied the attribute fixes, so the row
                // is only itemized and consumed. The recorded iflags are
                // authoritative, so re-emit the row here from them. A row may
                // name a segment's parent directory on the segment's gap NDX
                // `ndx_start - 1` (generator.c:2787), which receiver.c:880-887
                // resolves from dir_flist; when the index resolves to neither
                // there is nothing to itemize, so it is only consumed.
                if let Some(flat_idx) = self
                    .wire_to_flat_ndx(ndx)
                    .or_else(|| gap_heads.get(&ndx).copied())
                {
                    let entry = self.file_list[flat_idx].clone();
                    let iflags = crate::generator::ItemFlags::from_raw(u32::from(attrs.iflags));
                    self.emit_or_record_itemize(writer, flat_idx, &iflags, &entry)?;
                }
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

            // upstream: receiver.c:942-946 - phase 2 carries no transfers.
            if phase == 2 {
                return Err(protocol::protocol_violation(format!(
                    "got transfer request in phase 2 {}{}",
                    crate::role_trailer::error_location!(),
                    crate::role_trailer::receiver()
                )));
            }

            // upstream: receiver.c:1060-1066 - a transfer row must name a
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

            // upstream: receiver.c:474 receive_data() begins with
            // read_sum_head(f_in) - the recorded head describes the block
            // layout the recorded COPY tokens reference.
            let sum_head = SumHead::read(reader)?;

            // upstream: receiver.c:1011-1062 - the recorded fnamecmp_type/xname
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
            let layout_signature = sum_head.block_signature();

            // upstream: receiver.c:1212-1241 - under --inplace the destination
            // itself is opened (never truncated: it is the delta basis) and the
            // pre-image is backed up by copy first (generator.c:2281-2301);
            // otherwise a temp file is staged and renamed over it.
            let inplace = self.config.write.inplace;
            if inplace {
                self.backup_existing_dest_by_copy(
                    dest_dir,
                    &file_path,
                    metadata_opts,
                    #[cfg(unix)]
                    sandbox.as_deref(),
                )?;
            }
            let open_result = if inplace {
                open_inplace_dest(
                    &file_path,
                    relative_path,
                    dest_dir,
                    #[cfg(unix)]
                    sandbox.as_deref(),
                )
                .map(|file| (file, TempFileGuard::keep_dest(file_path.clone())))
            } else {
                #[cfg(unix)]
                let staged = open_tmpfile_sandboxed(
                    &file_path,
                    self.config.temp_dir.as_deref(),
                    sandbox.as_ref(),
                    Some(dest_dir.as_path()),
                );
                #[cfg(not(unix))]
                let staged = open_tmpfile(&file_path, self.config.temp_dir.as_deref());
                staged
            };

            // upstream: receiver.c:1015-1022 - a failed open drains this
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
                        u64::from(sum_head.count),
                        checksum_len,
                    )?;
                    // upstream: receiver.c:1243 "open %s failed" (inplace) vs
                    // receiver.c:466 "mkstemp %s failed" (temp file).
                    let what = if inplace { "open" } else { "mkstemp" };
                    metadata_errors.push((file_path.clone(), format!("{what} failed: {open_err}")));
                    self.flist_io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
                    continue;
                }
            };
            if !inplace {
                CleanupManager::global().register_temp_file(temp_guard.path().to_path_buf());
                temp_guard.mark_registered();
            }

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

            // upstream: receiver.c:1156-1157 - `updating_basis_or_equiv = ... ||
            // (inplace && fnamecmp == fname ...)`: a block already at its offset
            // in the file being rewritten is skipped, as skip_matched() does.
            let updating_basis =
                self.config.write.inplace && basis_path.as_deref() == Some(file_path.as_path());
            let mut applicator = crate::delta_apply::DeltaApplicator::new(
                file,
                &config,
                file_verifier,
                layout_signature.as_ref(),
                basis_path.as_deref(),
            )?
            .with_updating_basis(updating_basis);

            crate::delta_apply::apply_delta_stream(reader, &mut applicator, &mut token_reader)?;
            let (file, result) = applicator.finish(reader, None)?;
            self.record_touched_blocks(result.touched_blocks_4k);

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

            // upstream: receiver.c:664-671 receive_data() - an in-place write
            // truncates the destination to the length it reconstructed; a
            // failure is reported, not fatal.
            if inplace
                && result.final_pos.is_none()
                && let Err(e) = file.set_len(file_entry.size())
            {
                metadata_errors.push((file_path.clone(), format!("ftruncate failed: {e}")));
            }

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
            // upstream: receiver.c:1296-1300 - a failed close is fatal,
            // exit_cleanup(RERR_FILEIO).
            crate::disk_commit::close_received_file(Some(file), temp_guard.path())?;

            if !inplace {
                // upstream: generator.c:2280-2288 - read_batch with make_backups>0
                // still resolves the backup name and preserves the pre-image before
                // the destination is overwritten. Routed through the same
                // commit-tier owner the network commit uses (see
                // `ReceiverContext::backup_existing_dest`).
                self.backup_existing_dest(
                    dest_dir,
                    &file_path,
                    metadata_opts,
                    #[cfg(unix)]
                    sandbox.as_deref(),
                )?;

                // Commit: rename the temp file over the destination. Mirrors the
                // SEC-1.j routing of sync.rs (the sandbox-anchored renameat with
                // the io_uring fast path first). A `--temp-dir` on another
                // filesystem takes robust_rename's copy fallback (util1.c:599).
                #[cfg(unix)]
                crate::robust_rename::robust_rename(
                    crate::robust_rename::CommitAnchor {
                        sandbox: sandbox.as_deref(),
                        root: dest_dir,
                        fsync: self.config.write.fsync,
                    },
                    temp_guard.path(),
                    &file_path,
                    None,
                    |old, new| {
                        if let Some(result) = fast_io::try_rename_via_io_uring(old, new) {
                            return result;
                        }
                        let old_rel = old.strip_prefix(dest_dir).unwrap_or(old);
                        fast_io::renameat_via_sandbox_or_fallback(
                            sandbox.as_deref(),
                            dest_dir,
                            old_rel,
                            old,
                            dest_dir,
                            relative_path,
                            new,
                            true,
                        )
                    },
                )?;
                #[cfg(not(unix))]
                if let Some(rename_result) =
                    fast_io::try_rename_via_io_uring(temp_guard.path(), &file_path)
                {
                    rename_result?;
                } else {
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
            }

            if let Err(meta_err) =
                apply_metadata_with_cached_stat(&file_path, &file_entry, metadata_opts, None)
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
                    Some(dest_dir.as_path()),
                    self.config.fake_super,
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
                Some(dest_dir.as_path()),
            ) {
                metadata_errors.push((file_path.clone(), acl_err.to_string()));
            }

            // upstream: receiver.c:1290 - `log_item(log_code, file, iflags,
            // NULL)` itemizes every transferred row locally, regardless of
            // read_batch (generator.c:602's `!read_batch` guards only the wire
            // itemize header, not the per-file local log). The recorded iflags
            // carry the generator's itemize decision (ITEM_IS_NEW /
            // ITEM_REPORT_* / ITEM_TRANSFER), so reuse them through the same
            // owner the network receiver uses. A no-op unless `-i` /
            // `--out-format` is active (emit_itemize gates on
            // should_emit_itemize()).
            let iflags = crate::generator::ItemFlags::from_raw(u32::from(attrs.iflags));
            self.emit_or_record_itemize(writer, flat_idx, &iflags, &file_entry)?;

            // upstream: rsync.c:824-829 - name the updated file under -v. Under
            // `-i`/`--out-format` the itemize row above carries the name, so the
            // bare name is suppressed (should_emit_itemize()).
            if self.config.flags.verbose
                && self.config.connection.client_mode
                && !self.should_emit_itemize()
            {
                info_log!(Name, 1, "{}", relative_path.display());
            }

            // upstream: hlink.c:496-565 finish_hard_link() links a cluster to
            // the member that completed its transfer. Record the just-committed
            // member as this group's data-holder so create_hardlinks() links the
            // rest to the fresh payload. Which member the batch transferred is
            // stream-dictated - upstream (and oc, post the sorted-last fix) ships
            // it under the sorted-last FLAG_HLINK_LAST member, not necessarily
            // the sorted-first hlink_first one - so the source cannot be inferred
            // from disk presence (a stale pre-existing member would win).
            if file_entry.hlinked()
                && let Some(gnum) = file_entry.hardlink_idx()
                && let Some(tracker) = self.hardlink_tracker.as_mut()
            {
                let _ = tracker.record_leader(gnum, file_path.clone());
            }

            bytes_received += literal_bytes;
            literal_data += literal_bytes;
            matched_data += result.bytes_written.saturating_sub(literal_bytes);
            files_transferred += 1;
            transferred_file_size += file_entry.size();
        }

        // upstream: generator.c:2641 finish_hard_link() itemizes each follower
        // before the phase's NDX_DONE, then the followers are linked.
        let mut ndx_write_codec = MonotonicNdxWriter::new(self.protocol.as_u8());
        #[cfg(unix)]
        self.emit_server_hardlink_follower_itemize(
            writer,
            ndx_write_codec.inner_mut(),
            0..self.file_list.len(),
            dest_dir,
            sandbox.as_deref(),
        )?;
        #[cfg(not(unix))]
        self.emit_server_hardlink_follower_itemize(
            writer,
            ndx_write_codec.inner_mut(),
            0..self.file_list.len(),
            &dest_dir,
        )?;

        #[cfg(unix)]
        self.create_hardlinks(dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(&dest_dir, writer)?;

        // Drain the recorded stream's accumulated io_error before the late
        // sweep consults `stats.io_error`, mirroring the network drivers
        // (pipelined.rs, upstream generator.c:304-311 gates delete_in_dir on the
        // global io_error). `take_io_error` is destructive and ORs, so the
        // second drain below folds in anything read during finalization without
        // double-counting.
        stats.io_error |= reader.take_io_error();

        // upstream: generator.c:2899-2902 - --delete-after / --delete-delay run
        // the sweep only after every file has landed. Runs before touch_up_dirs
        // so deletion-induced parent mtime changes are re-tidied, matching the
        // network drivers and upstream's touch_up_dirs-after-late-delete order.
        if self.delete_pass_is_late() {
            self.run_receiver_delete_pass(
                super::DeletePassPhase::Late,
                dest_dir,
                #[cfg(unix)]
                sandbox.as_ref(),
                writer,
                &mut stats,
            )?;
        }

        // upstream: generator.c:2565-2618 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(
            dest_dir,
            #[cfg(unix)]
            sandbox.as_deref(),
            writer,
        );

        // Flush any buffered `-v` names then the deferred itemize rows in
        // flist-index order before the goodbye handshake, matching run_pipelined.
        // Under a custom `--out-format` the rows are metadata events left in
        // `event_rows` for the dispatch to drain and render (batch.rs).
        self.flush_names_all()?;
        self.flush_itemize_rows(writer)?;

        self.finalize_replay(reader, writer)?;

        stats.io_error |= reader.take_io_error();
        if inc_recurse {
            // The list is complete only now that every sub-list has arrived.
            let (num_dirs, num_symlinks, num_devices, num_specials) = self.file_type_counts();
            stats.num_dirs = num_dirs;
            stats.num_symlinks = num_symlinks;
            stats.num_devices = num_devices;
            stats.num_specials = num_specials;
            stats.io_error |= self.flist_reader_io_error() | self.flist_io_error;
        }
        stats.got_xfer_error = reader.xfer_error_count() > 0 || self.got_xfer_error.get();
        stats.files_transferred = files_transferred;
        stats.transferred_file_size = transferred_file_size;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.touched_blocks_4k = self.touched_blocks_4k;
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
    /// (`main.c:1102-1109`, `main.c:366-373`).
    ///
    /// No goodbye read follows the stats: upstream's replay receiver ends at
    /// `handle_stats(f_in)` - `read_final_goodbye()` is sender-only
    /// (`main.c:921`) - so any recorded goodbye bytes (written only when the
    /// recording sender teed them, protocol >= 31) are left unread exactly as
    /// upstream leaves them. A protocol-29/30 recording carries none at all.
    /// Runs the replay's generator half over every file list received since
    /// the last call, one list at a time, in the order the streaming network
    /// driver walks a segment.
    ///
    /// The generator half's itemize rows are speculative - the recorded rows
    /// rule a replay - so each list's are dropped again here, leaving the
    /// recorded rows already buffered by the row loop untouched.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2770-2841` - each list's directory, `delete_in_dir()`,
    ///   then `recv_generator()` over its entries, as the list arrives.
    /// - `main.c:652-664` - on `--read-batch` that generator runs locally and
    ///   its requests go to a pipe nothing reads.
    #[allow(clippy::too_many_arguments)]
    fn generate_replay_segments<W: Write + crate::writer::MsgInfoSender + ?Sized>(
        &mut self,
        generated: &mut usize,
        setup: &PipelineSetup,
        failed_dirs: &mut crate::receiver::directory::FailedDirectories,
        writer: &mut W,
        metadata_errors: &mut Vec<(std::path::PathBuf, String)>,
        stats: &mut TransferStats,
    ) -> io::Result<()> {
        let walk_dirs = !self.config.flags.dry_run;
        while *generated < self.ndx_segments.len() {
            let segment_idx = *generated;
            *generated += 1;
            let range = self.ndx_segments[segment_idx].0..self.segment_end(segment_idx);
            // The rows already buffered are recorded ones; set them aside so
            // this list's speculative rows land in empty buffers and are
            // dropped, then put them back.
            let recorded_rows = std::mem::take(&mut *self.itemize_rows.borrow_mut());
            let recorded_events = std::mem::take(&mut *self.event_rows.borrow_mut());
            let recorded_no_transfer =
                std::mem::take(&mut *self.server_no_transfer_itemize.borrow_mut());
            stats.io_error |= self.flist_reader_io_error() | self.flist_io_error;
            let head = if walk_dirs {
                self.segment_root_head(segment_idx)
            } else {
                None
            };
            if let Some(flist_idx) = head {
                self.walk_streamed_directory(flist_idx, setup, &mut *writer, failed_dirs, stats)?;
            }
            self.delete_in_segment(
                segment_idx,
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_ref(),
                failed_dirs,
                writer,
                stats,
            )?;
            if walk_dirs {
                for flist_idx in range.clone() {
                    if head != Some(flist_idx) && self.file_list[flist_idx].is_dir() {
                        self.walk_streamed_directory(
                            flist_idx,
                            setup,
                            &mut *writer,
                            failed_dirs,
                            stats,
                        )?;
                    }
                }
            }
            self.run_segment_post_passes(range.clone(), setup, writer)?;
            drop(self.build_files_to_transfer_in_range(
                range,
                writer,
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_deref(),
                &setup.metadata_opts,
                Some(failed_dirs),
                metadata_errors,
                stats,
                setup.acl_cache.as_deref(),
                setup.acl_id_map.as_deref(),
            ));
            *self.itemize_rows.borrow_mut() = recorded_rows;
            *self.event_rows.borrow_mut() = recorded_events;
            *self.server_no_transfer_itemize.borrow_mut() = recorded_no_transfer;
        }
        Ok(())
    }

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

/// Opens the replay destination for an in-place write, anchored beneath the
/// destination sandbox when one is held.
///
/// upstream: receiver.c:1212-1241 - the three-arm in-place open; never
/// truncates, since the destination is the delta basis.
fn open_inplace_dest(
    file_path: &std::path::Path,
    relative_path: &std::path::Path,
    dest_dir: &std::path::Path,
    #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        fast_io::open_inplace_output_via_sandbox_or_fallback(
            sandbox,
            dest_dir,
            relative_path,
            file_path,
            false,
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (relative_path, dest_dir);
        fast_io::open_inplace_output(file_path, false, fast_io::InplaceResolution::Direct)
    }
}
