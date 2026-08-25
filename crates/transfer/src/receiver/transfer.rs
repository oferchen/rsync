//! Transfer orchestration for the receiver role.
//!
//! Provides the `run`, `run_sync`, `run_pipelined`, and `run_pipelined_incremental`
//! entry points plus the common `setup_transfer` initialization. The driving
//! loops live in their own submodules:
//!
//! - `sync` - sequential per-file transfer used by `run_sync`.
//! - `pipelined` - decoupled two-phase pipeline used by `run_pipelined`.
//! - `pipelined_incremental` - same as `pipelined` plus incremental directory
//!   creation and failed-dir tracking.
//! - `setup` - common multiplex/filter/file-list setup.
//! - `phases` - protocol phase exchange and goodbye handshake.
//! - `candidates` - candidate-file selection for the pipelined paths.
//! - `pipeline` - the inner `run_pipeline_loop_decoupled` plus dry-run loop.
//! - `mode` - the single drive-mode decision both pipelined drivers share, plus
//!   the one implementation of every mode that moves no file data.

mod candidates;
pub(in crate::receiver) mod mode;
mod phases;
mod pipeline;
mod pipelined;
mod pipelined_incremental;
mod setup;
mod sync;

pub(in crate::receiver) use setup::parse_wire_filters_for_receiver;

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};

use crate::receiver::ReceiverContext;
use crate::receiver::stats::TransferStats;

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
            self.run_pipelined_incremental(
                reader,
                writer,
                crate::pipeline::PipelineConfig::default(),
                progress,
            )
        }
        #[cfg(not(feature = "incremental-flist"))]
        {
            self.run_pipelined(
                reader,
                writer,
                crate::pipeline::PipelineConfig::default(),
                progress,
            )
        }
    }

    /// Drives the receiver off a pre-recorded batch stream instead of a live
    /// peer, mirroring upstream's `--read-batch` receive.
    ///
    /// Upstream opens the batch file and hands its descriptor to the ordinary
    /// receiving client as `f_in`, pointing the generator's `f_out` at one end
    /// of a self-pipe whose read end is never drained (`main.c:635-651`). The
    /// receiver then reads the recorded file list and delta stream exactly as
    /// it would off a socket - `do_recv()` is unchanged - while its outbound
    /// requests and signatures fall into the dead-end pipe. Because the batch
    /// file was never framed, upstream also skips `io_start_multiplex_in()` for
    /// it (`main.c:1359-1366`, gated on `!read_batch`).
    ///
    /// This is the enabling mechanism for routing `--read-batch` through the
    /// real receiver rather than the native replay fork. It reproduces both
    /// halves of that model:
    ///
    /// - `batch_input` is wrapped as an undemultiplexed (`Plain`)
    ///   `ServerReader` `f_in`, and the
    ///   `local_replay` flag it sets keeps it Plain by
    ///   suppressing input-multiplex activation.
    /// - a [`DiscardSink`](crate::writer::DiscardSink) stands in for the
    ///   generator's consumer-less `f_out`, swallowing every request, signature
    ///   block, and `MSG_*` frame.
    ///
    /// The caller supplies a `ReceiverContext` already primed from the batch
    /// header - protocol, compat flags, and checksum seed are fed separately -
    /// so this method owns only the drive. It never touches the network path,
    /// which leaves `local_replay` at its `false` default and stays
    /// byte-identical.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:635-651` - `read_batch` sets `f_in = batch_fd` and points
    ///   `f_out` at a self-pipe with no live consumer.
    /// - `main.c:1359-1366` - the `!read_batch` gate that leaves the batch
    ///   `f_in` unmultiplexed.
    /// - `main.c:1387` - `do_recv(f_in, f_out, local_name)` drives the real
    ///   receiver over that batch `f_in`.
    pub fn run_local_replay<R: Read>(
        &mut self,
        batch_input: R,
        progress: Option<&mut dyn crate::TransferProgressCallback>,
    ) -> io::Result<TransferStats> {
        // upstream: main.c:1359 `!read_batch` keeps the batch f_in unmultiplexed.
        self.local_replay = true;
        let reader = crate::reader::ServerReader::new_plain(batch_input);
        let mut sink = crate::writer::DiscardSink::new();
        self.run(reader, &mut sink, progress)
    }

    /// True when this receiver is a *client* that was handed an empty file
    /// list, i.e. every requested source path failed to be listed (missing
    /// source, unreadable directory, everything filtered out).
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1383-1392` - `client_run()` gates the entire receive on
    ///   `if (flist && flist->used > 0)`. With no entries it takes the `else`
    ///   arm (`handle_stats(-1); output_summary();`) and returns without ever
    ///   calling `do_recv()`.
    /// - `main.c:968-974` - the peer sender bailed out at
    ///   `if (!flist || flist->used == 0) exit_cleanup(0)` (mirrored in
    ///   `generator::transfer::orchestrator`), so it never reads an ndx, never
    ///   writes its stats trailer, and never joins the goodbye handshake.
    ///
    /// Client-side only, exactly as upstream: `do_server_recv()` has no such
    /// gate and calls `do_recv()` unconditionally (`main.c:1201-1245`).
    pub(in crate::receiver) const fn is_empty_client_flist(&self, file_count: usize) -> bool {
        file_count == 0 && self.config.connection.client_mode
    }

    /// Ends a client receive that was handed an empty file list, without
    /// entering the transfer loop or the finalization exchange.
    ///
    /// Mirrors the `else` arm of `main.c:1389-1392`: no ndx is written, no
    /// stats trailer is read, and the goodbye handshake is skipped, because the
    /// peer sender already exited. Reading for any of them would block until
    /// the connection died, which is how this surfaced - the client reported
    /// `failed to fill whole buffer` (exit 12) instead of the summary.
    ///
    /// The returned stats carry every `io_error` bit the sender managed to
    /// deliver before it exited, from both wire encodings upstream uses:
    ///
    /// - the file-list end marker, when the peer negotiated a safe incremental
    ///   file list (`flist.c:2508-2517` -> `write_end_of_flist(f, 1)`), and
    /// - a `MSG_IO_ERROR` frame otherwise (`flist.c:2553-2555`), which arrives
    ///   interleaved with the list itself and has therefore already been folded
    ///   into the reader by the time the list ends - exactly how upstream's
    ///   global `io_error` picks it up before `client_run()` tests the list.
    ///
    /// `cleanup.c:217-218` turns `IOERR_GENERAL` into exit 23 (`RERR_PARTIAL`),
    /// while a genuinely empty-but-clean list still exits 0. Byte counters stay
    /// at whatever this process itself observed, matching `handle_stats(-1)`,
    /// which skips the wire read for `f < 0 && !am_sender` (`main.c:362-363`).
    pub(in crate::receiver) fn finish_empty_client_flist<R: Read, W: Write + ?Sized>(
        &mut self,
        reader: &mut crate::reader::ServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats> {
        // upstream: exit_cleanup() -> io_flush(FULL_FLUSH); nothing more is
        // written to the peer after this point.
        writer.flush()?;

        self.pipeline
            .advance_to(crate::transfer_state::TransferPhase::Finalization)
            .map_err(crate::fsm_error)?;
        self.pipeline
            .advance_to(crate::transfer_state::TransferPhase::Complete)
            .map_err(crate::fsm_error)?;

        Ok(TransferStats {
            io_error: self.flist_reader_io_error() | self.flist_io_error | reader.take_io_error(),
            // upstream: log.c:310-311 - an empty list is exactly what a missing
            // source argument produces, and `flist.c:2431` leaves io_error clear
            // for it, so the MSG_ERROR_XFER frames read while draining the list
            // are the only evidence the run must exit 23.
            got_xfer_error: reader.xfer_error_count() > 0,
            ..TransferStats::default()
        })
    }

    /// Finalizes a completed transfer's delayed updates and hardlink followers,
    /// in the order upstream mandates.
    ///
    /// `handle_delayed_updates` runs first, renaming every `--delay-updates`
    /// leader out of the `.~tmp~` partial-dir to its final path; only then does
    /// `create_hardlinks` link followers to those leaders. Running the hardlink
    /// pass first would target a leader still staged under `.~tmp~` - `ENOENT`
    /// (a fatal transfer error) or a stale pre-existing inode with the wrong
    /// content.
    ///
    /// This is the single ordering site shared by both pipelined drivers
    /// (`run_pipelined` and `run_pipelined_incremental`) so they cannot drift
    /// apart.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:694-695` - `handle_delayed_updates()` at `phase == 2`.
    /// - `receiver.c:551-552` - only after the delayed rename does
    ///   `send_msg_success()` drive `finish_hard_link()` (`hlink.c:475`,
    ///   `generator.c:2169`) to link the leader's followers.
    pub(in crate::receiver) fn finalize_delayed_updates_and_hardlinks<W>(
        &mut self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
        all_delayed_updates: &[(PathBuf, PathBuf)],
        writer: &mut W,
    ) -> io::Result<()>
    where
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    {
        if !all_delayed_updates.is_empty() {
            let backup_cfg = if self.config.flags.backup {
                Some(crate::disk_commit::BackupConfig {
                    dest_dir: dest_dir.to_path_buf(),
                    backup_dir: self.config.backup_dir.as_ref().map(PathBuf::from),
                    suffix: self.config.effective_backup_suffix().into(),
                })
            } else {
                None
            };
            // A delayed rename that fails (e.g. EACCES under a Landlock
            // sandbox without ACCESS_FS_REFER on kernels 5.13-5.18) sets
            // IOERR_GENERAL so the transfer exits 23 (RERR_PARTIAL) instead
            // of reporting success while the file was never updated.
            self.flist_io_error |= handle_delayed_updates(
                all_delayed_updates,
                backup_cfg,
                self.config.partial_dir.as_deref(),
            );
        }

        #[cfg(unix)]
        self.create_hardlinks(dest_dir, sandbox, writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(dest_dir, writer)?;

        Ok(())
    }

    /// True when the delete pass has work to do at the EARLY site, before the
    /// per-file transfer loop.
    ///
    /// Covers `--delete-before` and `--delete-during` (each an immediate sweep
    /// here) and `--delete-delay` (which *decides* its victim set here, deferring
    /// the unlink to the late site - upstream generator.c:2315-2327 calls
    /// `delete_in_dir()` during the walk, and `delete_during == 2` records the
    /// victim via `remember_delete()` rather than unlinking). `--delete-after`
    /// alone does nothing early. The phase dispatch in
    /// [`run_receiver_delete_pass`](Self::run_receiver_delete_pass) picks
    /// immediate-vs-collect per mode.
    ///
    /// upstream: generator.c:2280-2281 (`delete_before` -> `do_delete_pass()` up
    /// front), generator.c:2315-2327 (`delete_during` / `delete_during == 2`
    /// decide as each directory is entered during the loop).
    pub(in crate::receiver) fn delete_pass_is_early(&self) -> bool {
        self.config.flags.delete && !self.config.deletion.delete_after
    }

    /// True when the delete pass has work to do at the LATE site, after the
    /// per-file transfer loop.
    ///
    /// Covers `--delete-after` (an immediate sweep here, once every destination
    /// `.rsync-filter` merge file has landed so its protect rules apply) AND
    /// `--delete-delay` (which *executes* the victims it decided early). Both are
    /// the upstream `late_delete` modes (`delete_during == 2 || delete_after`).
    ///
    /// `--delete-delay`'s split is load-bearing: upstream decides during the walk
    /// (deferring only the unlink) so it DELETES a per-dir-merge-protected entry
    /// exactly as `--delete-during` does (verified vs upstream 3.4.4 over SSH),
    /// yet the unlink and `*deleting` output happen after the whole transfer -
    /// so a mid-transfer abort leaves the stale file in place.
    ///
    /// upstream: generator.c:2425-2428 - `do_delayed_deletions()` (delay) and
    /// `do_delete_pass()` (after) both run after `generate_files()` finishes.
    pub(in crate::receiver) fn delete_pass_is_late(&self) -> bool {
        self.config.flags.delete && self.config.deletion.late_delete
    }

    /// Runs the destination delete pass for `phase` and folds its results into
    /// `stats`. Called once at the early site (before the per-file loop) and once
    /// at the late site (after it, before finalize); the mode decides what each
    /// phase actually does:
    ///
    /// | mode | early | late |
    /// |------|-------|------|
    /// | `--delete-before` / `--delete-during` | immediate sweep | - |
    /// | `--delete-after` | - | immediate sweep |
    /// | `--delete-delay` (parallel path) | collect victims | execute victims |
    /// | `--delete-delay` (+`--max-delete`/`-x`) | immediate sweep | - |
    ///
    /// A `--delete-delay` run that would route through the serial, leaf-granular
    /// executor (`--max-delete` cap or `--one-file-system` boundary) cannot defer
    /// through the collect/execute split, so it stays on the immediate early pass,
    /// mirroring the engine crate's `delay_decides_during` gate.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:358` - `do_delete_pass()` tree sweep (before/after)
    /// - `generator.c:279` - `delete_in_dir()` per-directory candidate check
    /// - `generator.c:157` - `remember_delete()` records a delay victim
    /// - `generator.c:2419` - `do_delayed_deletions()` unlinks delay victims
    /// - `exclude.c:875` - `change_local_filter_dir()` reloads dest `.rsync-filter`
    pub(in crate::receiver) fn run_receiver_delete_pass<W>(
        &mut self,
        phase: DeletePassPhase,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        writer: &mut W,
        stats: &mut TransferStats,
    ) -> io::Result<()>
    where
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    {
        use crate::generator::io_error_flags::IOERR_GENERAL;

        // upstream: generator.c:304-311 delete_in_dir() - if the sender hit a
        // general I/O error while scanning the source, its file list may be
        // incomplete, so deleting dest files that merely never got listed would
        // lose data. Skip the entire delete pass (both phases) and print the
        // notice once, unless `--ignore-errors` was given.
        if stats.io_error & IOERR_GENERAL != 0 && !self.config.deletion.ignore_errors {
            if !self.io_error_delete_warning_emitted {
                self.io_error_delete_warning_emitted = true;
                info_log!(Nonreg, 1, "IO error encountered -- skipping file deletion");
            }
            return Ok(());
        }

        // `--delete-delay` (`delete_during == 2`): late_delete without the
        // `delete_after` decision-deferral. Deferrable only on the parallel path.
        let delay = self.config.deletion.late_delete && !self.config.deletion.delete_after;
        let deferrable_delay = delay && !self.delete_pass_uses_serial_executor();

        match phase {
            DeletePassPhase::Early => {
                if deferrable_delay {
                    // upstream: generator.c:345 remember_delete() records each
                    // victim during the walk; the unlink is deferred.
                    let (victims, io_bits) = self.collect_delayed_deletions(
                        dest_dir,
                        #[cfg(unix)]
                        sandbox,
                        writer,
                    )?;
                    stats.io_error |= io_bits;
                    self.delayed_delete_victims = victims;
                } else {
                    self.run_immediate_delete_pass(
                        dest_dir,
                        #[cfg(unix)]
                        sandbox,
                        writer,
                        stats,
                    )?;
                }
            }
            DeletePassPhase::Late => {
                if self.config.deletion.delete_after {
                    self.run_immediate_delete_pass(
                        dest_dir,
                        #[cfg(unix)]
                        sandbox,
                        writer,
                        stats,
                    )?;
                } else if deferrable_delay {
                    // upstream: generator.c:2419 do_delayed_deletions() unlinks the
                    // remembered victims after the whole transfer has completed.
                    let victims = std::mem::take(&mut self.delayed_delete_victims);
                    let (delete_stats, io_bits) = self.execute_delayed_deletions(
                        dest_dir,
                        #[cfg(unix)]
                        sandbox,
                        &victims,
                        writer,
                    )?;
                    stats.io_error |= io_bits;
                    stats.delete_stats = delete_stats;
                    // Carry the per-type counters into the receiver context so the
                    // goodbye handshake can emit NDX_DEL_STATS to the peer sender.
                    // upstream: generator.c:2437-2440 - late write_del_stats().
                    self.pending_del_stats = delete_stats;
                }
            }
        }
        Ok(())
    }

    /// Immediate delete sweep: scan, unlink, emit, and fold the stats into
    /// `stats`. Used by `--delete-before` / `--delete-during` / `--delete-after`
    /// and a capped/`--one-file-system` `--delete-delay`.
    fn run_immediate_delete_pass<W>(
        &mut self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        writer: &mut W,
        stats: &mut TransferStats,
    ) -> io::Result<()>
    where
        W: Write + crate::writer::MsgInfoSender + ?Sized,
    {
        let (delete_stats, limit_exceeded, io_bits) = self.delete_extraneous_files(
            dest_dir,
            #[cfg(unix)]
            sandbox,
            writer,
        )?;
        stats.io_error |= io_bits;
        stats.delete_stats = delete_stats;
        stats.delete_limit_exceeded = limit_exceeded;
        // Carry the per-type counters into the receiver context so the goodbye
        // handshake can emit NDX_DEL_STATS to the peer sender.
        // upstream: generator.c:2393-2398 - write_del_stats() emission.
        self.pending_del_stats = delete_stats;
        Ok(())
    }
}

/// Which side of the per-file transfer loop a
/// [`run_receiver_delete_pass`](ReceiverContext::run_receiver_delete_pass) call
/// sits on. The mode decides what work each phase performs (see that method).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::receiver) enum DeletePassPhase {
    /// Before the per-file transfer loop (upstream `do_delete_pass()` /
    /// `delete_in_dir()` during the walk).
    Early,
    /// After the per-file transfer loop (upstream `do_delayed_deletions()` /
    /// late `do_delete_pass()`).
    Late,
}

/// Renames all delayed-update files from their staging paths to their final
/// destinations, removing each emptied staging directory as it goes.
///
/// Mirrors upstream `receiver.c:688-717 handle_delayed_updates()` which
/// iterates `delayed_bits`, renames each file from its `partial_dir_fname()`
/// path to the final destination, and calls `handle_partial_dir(PDIR_DELETE)`.
///
/// `partial_dir` is the configured `--partial-dir` and decides whether that
/// last step does anything at all: upstream skips the rmdir entirely for an
/// absolute value. [`engine::remove_partial_dir`] owns the rule.
///
/// When `backup_config` is `Some`, backs up the existing destination file
/// before the rename (upstream: `receiver.c:538 make_backup(fname, False)`).
///
/// A failed rename (or backup) is logged and does not abort the sweep -
/// remaining files are still renamed, matching upstream which calls
/// `rsyserr(FERROR_XFER, ...)` and continues. The failure is NOT silently
/// swallowed, though: this returns the accumulated `io_error` bitfield
/// (`IOERR_GENERAL` when any rename/backup failed, else 0) so the caller can
/// fold it into the transfer's `io_error` and surface exit code 23
/// (`RERR_PARTIAL`). Upstream achieves the same via the
/// `FERROR_XFER -> got_xfer_error -> RERR_PARTIAL` side channel
/// (log.c:309-316, cleanup.c:210-218, main.c:1630-1631).
///
/// This matters on Linux kernels 5.13-5.18: those have Landlock but lack
/// `LANDLOCK_ACCESS_FS_REFER` (added in 5.19), so the cross-directory rename
/// out of the `.~tmp~` staging dir is denied with `EACCES` under a sandbox.
/// Without the returned io_error bit the file would silently not be updated
/// while the process still exited 0 - silent data loss.
pub(in crate::receiver) fn handle_delayed_updates(
    delayed: &[(PathBuf, PathBuf)],
    backup_config: Option<crate::disk_commit::BackupConfig>,
    partial_dir: Option<&Path>,
) -> i32 {
    use std::fs;

    use crate::generator::io_error_flags::IOERR_GENERAL;

    // Mirrors upstream's got_xfer_error: any rename/backup failure here is a
    // transfer error that must yield exit 23 (RERR_PARTIAL), not a silent 0.
    let mut io_error = 0;

    for (staging_path, final_path) in delayed {
        // upstream: receiver.c:538-539 - make_backup(fname, False)
        if let Some(ref bc) = backup_config {
            if final_path.exists() {
                let backup_path = engine::compute_backup_path(
                    &bc.dest_dir,
                    final_path,
                    None,
                    bc.backup_dir.as_deref(),
                    &bc.suffix,
                );
                if let Some(parent) = backup_path.parent() {
                    if !parent.exists() {
                        let _ = fs::create_dir_all(parent);
                    }
                }
                match fs::rename(final_path, &backup_path) {
                    Ok(()) => {
                        // upstream: backup.c:216-217 - DEBUG_GTE(BACKUP, 1)
                        // RENAME success notice. The delayed-updates sweep is
                        // the third backup site (alongside disk-commit and
                        // local-copy); upstream emits this from
                        // backup.c:make_backup() regardless of caller.
                        engine::trace_make_backup_rename(&final_path.display().to_string());
                        // upstream: backup.c:352-353 - INFO_GTE(BACKUP, 1)
                        // rprintf(FINFO, "backed up %s to %s\n", fname, buf)
                        // fires on success label of make_backup() after the
                        // rename completes. Paths are displayed relative to
                        // the destination root to match the upstream test
                        // assertions (testsuite/backup.test:29,43,56).
                        let final_rel = final_path.strip_prefix(&bc.dest_dir).unwrap_or(final_path);
                        let backup_rel = backup_path
                            .strip_prefix(&bc.dest_dir)
                            .unwrap_or(&backup_path);
                        info_log!(
                            Backup,
                            1,
                            "backed up {} to {}",
                            final_rel.display(),
                            backup_rel.display()
                        );
                    }
                    Err(e) => {
                        // upstream: make_backup() -> rsyserr(FERROR_XFER, ...)
                        // sets got_xfer_error -> RERR_PARTIAL.
                        eprintln!("rsync: backup failed for {}: {e}", final_path.display());
                        io_error |= IOERR_GENERAL;
                    }
                }
            }
        }

        // upstream: receiver.c:540-542 - DEBUG_GTE(RECV, 1) rename notice
        debug_log!(
            Recv,
            1,
            "renaming {} to {}",
            staging_path.display(),
            final_path.display()
        );

        // upstream: receiver.c:546 - do_rename(partialptr, fname)
        if let Err(e) = fs::rename(staging_path, final_path) {
            // upstream: rsyserr(FERROR_XFER, ...) sets got_xfer_error ->
            // RERR_PARTIAL (exit 23). On kernels 5.13-5.18 the Landlock
            // sandbox lacks ACCESS_FS_REFER, so this cross-dir rename is
            // denied with EACCES; flag it so the file's absence is visible
            // via a non-zero exit rather than silently skipped.
            eprintln!(
                "rsync: rename failed for {} (from {}): {e}",
                final_path.display(),
                staging_path.display()
            );
            io_error |= IOERR_GENERAL;
            continue;
        }

        // upstream: receiver.c:716 - handle_partial_dir(partialptr, PDIR_DELETE)
        // fires per file on the rename's success branch rather than as a
        // deferred sweep over the distinct parents. That ordering is what makes
        // the unchecked rmdir correct when two entries stage in one directory:
        // the first call fails harmlessly on the still-occupied directory and
        // the second succeeds.
        engine::remove_partial_dir(partial_dir, staging_path);
    }

    io_error
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
    use std::path::PathBuf;

    use protocol::ProtocolVersion;
    use protocol::flist::FileListWriter;

    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;

    /// Builds a protocol-32 client-mode receiver context, the shape a
    /// `--read-batch` client presents to `run_local_replay`.
    fn replay_client_ctx() -> ReceiverContext {
        let handshake = HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let mut config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        // upstream: the batch is applied by the receiving *client*, so the
        // empty-list short-circuit (main.c:1389-1392) is reachable.
        config.connection.client_mode = true;
        ReceiverContext::new_for_test(&handshake, config)
    }

    /// `run_local_replay` drives the real receiver to completion off a plain,
    /// pre-recorded (Cursor) `f_in` paired with the discard sink - no live peer,
    /// no multiplex framing - mirroring upstream's `do_recv()` over `batch_fd`.
    ///
    /// WHY: this is the whole point of approach A. A recorded batch is a
    /// one-way stream with nothing to answer the generator's requests; feeding
    /// it as a Plain `ServerReader` and swallowing the outbound frames in a
    /// [`DiscardSink`](crate::writer::DiscardSink) must let the ordinary
    /// receiver run to a clean finish. A trivial empty file list exercises the
    /// full setup path (Plain f_in, no filter read, flist decode) and returns
    /// through `finish_empty_client_flist` without touching the disk or the
    /// wire - so a green result proves the input mode is wired, not that a
    /// stub returned early.
    #[test]
    fn run_local_replay_drives_empty_recorded_stream_to_completion() {
        let mut ctx = replay_client_ctx();

        // A recorded stream that carries only an (empty) file list - the
        // minimal complete batch body an upstream `--write-batch` of nothing
        // would produce.
        let mut recorded = Vec::new();
        let writer = FileListWriter::new(ctx.protocol());
        writer.write_end(&mut recorded, None).unwrap();

        let stats = ctx
            .run_local_replay(Cursor::new(recorded), None)
            .expect("empty recorded batch must replay to completion");

        assert_eq!(
            stats.files_listed, 0,
            "an empty recorded file list yields no listed files"
        );
        assert_eq!(
            stats.files_transferred, 0,
            "no delta data is applied for an empty recorded batch"
        );
        // The mechanism must have kept the batch f_in unmultiplexed the whole
        // way through (upstream main.c:1359 `!read_batch`).
        assert!(!ctx.should_activate_input_multiplex());
    }

    /// The `local_replay` flag suppresses input-multiplex activation even at a
    /// protocol that would otherwise demux, and leaves the network default
    /// untouched.
    ///
    /// WHY: upstream gates every `io_start_multiplex_in(f_in)` on `!read_batch`
    /// (main.c:1359-1366) because the batch file was never framed. Without the
    /// gate the receiver would try to demux raw batch bytes as `MSG_DATA`
    /// frames and desync immediately.
    #[test]
    fn local_replay_flag_keeps_f_in_plain_at_protocol_32() {
        let mut ctx = replay_client_ctx();
        // Default (network) path: proto-32 client activates multiplex.
        assert!(ctx.should_activate_input_multiplex());
        ctx.local_replay = true;
        assert!(
            !ctx.should_activate_input_multiplex(),
            "a recorded batch f_in must stay Plain (upstream !read_batch)"
        );
    }

    /// Builds a client-mode receiver `ServerConfig` at `proto`, the shape a
    /// `--read-batch` client hands to [`ReceiverContext::for_batch_replay`].
    fn replay_config(proto: u8) -> ServerConfig {
        let mut config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(proto).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        };
        config.connection.client_mode = true;
        config
    }

    /// [`for_batch_replay`](ReceiverContext::for_batch_replay) pins the
    /// receiver's negotiated state from the batch header's protocol, checksum
    /// seed, and compat varint - not from a live handshake.
    ///
    /// WHY: this is the A2 seam. Upstream's `--read-batch` skips negotiation and
    /// reads the recorded protocol/compat/seed back from the batch fd
    /// (`compat.c` `setup_protocol()` under `read_batch`; the values
    /// `io.c:2521-2524` teed at capture). The replay receiver must run against
    /// the SAME protocol/seed/compat the batch was recorded under or its
    /// file-list decode and basis checksums desync. The header is round-tripped
    /// through the batch reader's own [`BatchHeader::read_from`] so the parse is
    /// the single source of truth, then asserted on the built context: the seed
    /// lands in the field every basis/signature site reads
    /// (context.rs `build_flist_reader` / `build_basis_file_config`), the
    /// protocol is pinned, and each compat bit is applied.
    #[test]
    fn for_batch_replay_pins_protocol_seed_and_compat_from_header() {
        use engine::batch::BatchHeader;
        use protocol::CompatibilityFlags;

        // A proto-32 batch recorded with two compat bits and a distinctive seed.
        let bits =
            CompatibilityFlags::INC_RECURSE.bits() | CompatibilityFlags::SYMLINK_TIMES.bits();
        let seed = 0x0BAD_F00D_u32 as i32;
        let mut written = BatchHeader::new(32, seed);
        written.compat_flags = Some(bits as i32);

        // Recorded, then parsed by the batch reader (the one source of truth).
        let mut recorded = Vec::new();
        written.write_to(&mut recorded).unwrap();
        let header = BatchHeader::read_from(&mut Cursor::new(recorded)).unwrap();

        let ctx = ReceiverContext::for_batch_replay(&header, replay_config(32))
            .expect("a supported batch header must build a replay context");

        assert_eq!(
            ctx.protocol(),
            ProtocolVersion::try_from(32u8).unwrap(),
            "the header's protocol version must pin the receiver's protocol"
        );
        assert_eq!(
            ctx.checksum_seed, seed,
            "the header's checksum seed must seed the receiver's basis checksums"
        );
        let compat = ctx
            .compat_flags()
            .expect("proto >= 30 records a compat varint");
        assert!(
            compat.contains(CompatibilityFlags::INC_RECURSE)
                && compat.contains(CompatibilityFlags::SYMLINK_TIMES),
            "every recorded compat bit must be applied to the replay receiver"
        );
    }

    /// A batch recorded below protocol 30 carries no compat varint, so the
    /// replay receiver's compat state is absent - never a phantom zero.
    ///
    /// WHY: upstream writes the compat varint only for protocol >= 30
    /// (`io.c:2522-2523`), and [`BatchHeader::read_from`] mirrors that by
    /// reading it only then. `for_batch_replay` must carry that `None` through
    /// (leaving `compat_flags()` `None`, exactly as a legacy live negotiation
    /// would) while still pinning the recorded protocol and seed.
    #[test]
    fn for_batch_replay_below_protocol_30_has_no_compat() {
        use engine::batch::BatchHeader;

        let seed = 0x0051_5EED_i32;
        let written = BatchHeader::new(29, seed);
        assert!(
            written.compat_flags.is_none(),
            "a proto-29 header records no compat varint"
        );

        let mut recorded = Vec::new();
        written.write_to(&mut recorded).unwrap();
        let header = BatchHeader::read_from(&mut Cursor::new(recorded)).unwrap();

        let ctx = ReceiverContext::for_batch_replay(&header, replay_config(29))
            .expect("a proto-29 batch header must build a replay context");

        assert_eq!(ctx.protocol(), ProtocolVersion::try_from(29u8).unwrap());
        assert_eq!(ctx.checksum_seed, seed);
        assert!(
            ctx.compat_flags().is_none(),
            "no compat varint below proto 30 must leave compat state absent"
        );
    }

    /// A context built by `for_batch_replay` drives the real receiver to a clean
    /// finish off the recorded stream, proving the header-seeded protocol
    /// actually feeds a live receive - not just the accessors.
    ///
    /// WHY: A2 is only wired if the seeded state survives all the way through a
    /// real drive. The header is parsed off the front of the recorded stream and
    /// the remaining bytes (an empty file list, the minimal complete batch body)
    /// stream straight into [`run_local_replay`](ReceiverContext::run_local_replay)
    /// as `f_in` - exactly the A3 shape, one parse feeding both the setup and the
    /// drive. A green empty-list finish through `finish_empty_client_flist`
    /// confirms the pinned protocol decoded the recorded flist, unmultiplexed.
    #[test]
    fn for_batch_replay_drives_recorded_stream_to_completion() {
        use engine::batch::BatchHeader;

        let seed = 0x1234_5678_i32;
        let written = BatchHeader::new(32, seed);

        // Header followed by an empty recorded file list, in one stream.
        let mut recorded = Vec::new();
        written.write_to(&mut recorded).unwrap();
        FileListWriter::new(ProtocolVersion::try_from(32u8).unwrap())
            .write_end(&mut recorded, None)
            .unwrap();

        // The batch reader consumes the header; the same cursor then feeds the
        // flist body to the receiver.
        let mut cursor = Cursor::new(recorded);
        let header = BatchHeader::read_from(&mut cursor).unwrap();
        let mut ctx = ReceiverContext::for_batch_replay(&header, replay_config(32))
            .expect("a supported batch header must build a replay context");

        assert_eq!(ctx.checksum_seed, seed, "the seed must be pinned pre-drive");

        let stats = ctx
            .run_local_replay(cursor, None)
            .expect("the header-seeded receiver must replay to completion");

        assert_eq!(stats.files_listed, 0);
        assert_eq!(stats.files_transferred, 0);
        assert_eq!(
            ctx.protocol(),
            ProtocolVersion::try_from(32u8).unwrap(),
            "the pinned protocol must survive the drive"
        );
        assert!(
            !ctx.should_activate_input_multiplex(),
            "a header-seeded batch f_in must stay Plain (upstream !read_batch)"
        );
    }

    /// Verifies the delayed rename sweep moves files from staging paths to
    /// final destinations, matching upstream `receiver.c:422-450`.
    #[test]
    fn handle_delayed_updates_renames_staged_files() {
        let dir = test_support::create_tempdir();
        let staging_dir = dir.path().join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        // Create two staged files.
        let staged_a = staging_dir.join("a.txt");
        let staged_b = staging_dir.join("b.txt");
        fs::write(&staged_a, b"content-a").unwrap();
        fs::write(&staged_b, b"content-b").unwrap();

        let final_a = dir.path().join("a.txt");
        let final_b = dir.path().join("b.txt");

        let delayed = vec![
            (staged_a.clone(), final_a.clone()),
            (staged_b.clone(), final_b.clone()),
        ];

        handle_delayed_updates(&delayed, None, None);

        // Files should be at final paths.
        assert_eq!(fs::read_to_string(&final_a).unwrap(), "content-a");
        assert_eq!(fs::read_to_string(&final_b).unwrap(), "content-b");

        // Staging paths should no longer exist.
        assert!(!staged_a.exists());
        assert!(!staged_b.exists());

        // The empty .~tmp~ directory should have been cleaned up.
        assert!(
            !staging_dir.exists(),
            "empty .~tmp~ dir should be removed after sweep"
        );
    }

    /// Verifies that the sweep cleans up the `.~tmp~` directory even when
    /// it contained multiple files across different parent directories.
    #[test]
    fn handle_delayed_updates_cleans_multiple_staging_dirs() {
        let dir = test_support::create_tempdir();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();

        let tmp1 = dir.path().join(".~tmp~");
        let tmp2 = sub.join(".~tmp~");
        fs::create_dir(&tmp1).unwrap();
        fs::create_dir(&tmp2).unwrap();

        let staged1 = tmp1.join("f1.txt");
        let staged2 = tmp2.join("f2.txt");
        fs::write(&staged1, b"one").unwrap();
        fs::write(&staged2, b"two").unwrap();

        let final1 = dir.path().join("f1.txt");
        let final2 = sub.join("f2.txt");

        let delayed = vec![
            (staged1.clone(), final1.clone()),
            (staged2.clone(), final2.clone()),
        ];

        handle_delayed_updates(&delayed, None, None);

        assert_eq!(fs::read_to_string(&final1).unwrap(), "one");
        assert_eq!(fs::read_to_string(&final2).unwrap(), "two");
        assert!(!tmp1.exists());
        assert!(!tmp2.exists());
    }

    /// An ABSOLUTE `--partial-dir` must survive the sweep.
    ///
    /// upstream: `util1.c:1506-1507` - `handle_partial_dir(fname, PDIR_DELETE)`
    /// returns immediately when `*partial_dir == '/'`, so `receiver.c:716` never
    /// rmdir's an operator-named absolute staging directory. Measured against
    /// real rsync 3.5.0 over a daemon push with `-a --delay-updates
    /// --partial-dir=/pdir`: upstream leaves a pre-existing `/pdir` in place,
    /// while oc removed it. The directory is reserved across runs and generally
    /// exists before the transfer starts, so removing it destroys operator
    /// state that is not ours to touch.
    #[test]
    fn absolute_partial_dir_survives_the_delayed_update_sweep() {
        let dir = test_support::create_tempdir();
        // An absolute --partial-dir is one path for the whole transfer, not one
        // per destination directory - so the configured value and the staging
        // directory are the same thing.
        let partial_dir = dir.path().join("pdir");
        fs::create_dir(&partial_dir).unwrap();
        let staged = partial_dir.join("f.txt");
        fs::write(&staged, b"payload").unwrap();
        let final_path = dir.path().join("f.txt");
        handle_delayed_updates(
            &[(staged, final_path.clone())],
            None,
            Some(partial_dir.as_path()),
        );
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "payload");
        assert!(
            partial_dir.is_dir(),
            "an absolute --partial-dir must outlive the transfer (util1.c:1507)"
        );
    }
    /// Non-vacuity companion for
    /// [`absolute_partial_dir_survives_the_delayed_update_sweep`]: the same
    /// fixture with a RELATIVE `--partial-dir` must still be swept away.
    ///
    /// Without this the pin would also hold if the sweep had simply stopped
    /// removing anything at all, which is a different bug in the other
    /// direction - upstream does rmdir a relative partial-dir, because it is
    /// created beside each destination file and belongs to that file's
    /// transfer.
    #[test]
    fn relative_partial_dir_is_removed_by_the_delayed_update_sweep() {
        let dir = test_support::create_tempdir();
        let partial_dir = dir.path().join("pdir");
        fs::create_dir(&partial_dir).unwrap();
        let staged = partial_dir.join("f.txt");
        fs::write(&staged, b"payload").unwrap();
        let final_path = dir.path().join("f.txt");
        handle_delayed_updates(
            &[(staged, final_path.clone())],
            None,
            Some(Path::new("pdir")),
        );
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "payload");
        assert!(
            !partial_dir.exists(),
            "an emptied relative --partial-dir is rmdir'd (util1.c:1531)"
        );
    }
    /// Verifies the sweep continues past a rename failure (matching upstream
    /// which logs the error but does not abort) AND flags the failure so the
    /// transfer exits 23 rather than silently reporting success.
    ///
    /// WHY: on kernels 5.13-5.18 the Landlock sandbox lacks
    /// `ACCESS_FS_REFER`, so the cross-directory rename out of `.~tmp~` is
    /// denied with `EACCES`. A rename returning any error models that case;
    /// swallowing it (no io_error, exit 0) would be silent data loss - the
    /// file the user asked to update would simply not be updated.
    #[test]
    fn handle_delayed_updates_flags_error_on_rename_failure() {
        use crate::generator::io_error_flags::IOERR_GENERAL;

        let dir = test_support::create_tempdir();
        let staging_dir = dir.path().join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        // Create one valid staged file and one that points to a missing source.
        let staged_good = staging_dir.join("good.txt");
        fs::write(&staged_good, b"good").unwrap();
        let staged_bad = PathBuf::from("/nonexistent/path/.~tmp~/bad.txt");

        let final_good = dir.path().join("good.txt");
        let final_bad = PathBuf::from("/nonexistent/path/bad.txt");

        let delayed = vec![
            (staged_bad, final_bad),
            (staged_good.clone(), final_good.clone()),
        ];

        // Should not panic or abort, but must report the failed rename.
        let io_error = handle_delayed_updates(&delayed, None, None);

        assert_eq!(
            io_error & IOERR_GENERAL,
            IOERR_GENERAL,
            "a failed delayed rename must set IOERR_GENERAL so the transfer \
             exits 23 (RERR_PARTIAL) instead of silently reporting success"
        );

        // The good file should still be renamed successfully (sweep continues).
        assert_eq!(fs::read_to_string(&final_good).unwrap(), "good");
        assert!(!staged_good.exists());
    }

    /// Verifies a fully successful sweep returns 0, so a clean
    /// `--delay-updates` finalization leaves the exit code untouched.
    #[test]
    fn handle_delayed_updates_success_returns_zero() {
        let dir = test_support::create_tempdir();
        let staging_dir = dir.path().join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        let staged = staging_dir.join("a.txt");
        fs::write(&staged, b"content").unwrap();
        let final_path = dir.path().join("a.txt");

        let io_error = handle_delayed_updates(&[(staged, final_path)], None, None);

        assert_eq!(
            io_error, 0,
            "a successful sweep must not flag any I/O error"
        );
    }

    /// Verifies the sweep handles an empty delayed list gracefully and
    /// reports no error.
    #[test]
    fn handle_delayed_updates_empty_is_noop() {
        assert_eq!(handle_delayed_updates(&[], None, None), 0);
    }

    /// Verifies that `handle_delayed_updates` backs up a pre-existing
    /// destination file before renaming the staged file into place when a
    /// `BackupConfig` is supplied.
    ///
    /// This is the receiver-side equivalent of upstream
    /// `receiver.c:538-539 make_backup(fname, False)` -> `backup.c:make_backup`
    /// which renames the existing file out of the way and emits the
    /// `backed up X to Y` info_log via `INFO_GTE(BACKUP, 1)` at
    /// `backup.c:352-353`. Upstream `testsuite/backup.test:43,56` greps for
    /// that exact line under `--info=BACKUP --delay-updates` so the rename
    /// must fire before the staged file replaces the destination.
    #[test]
    fn handle_delayed_updates_backs_up_existing_destination() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let final_path = dest_root.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(
            &[(staged.clone(), final_path.clone())],
            Some(backup_config),
            None,
        );

        assert_eq!(
            fs::read_to_string(&final_path).unwrap(),
            "new-content",
            "staged file must replace destination after backup"
        );
        // upstream: backup.c::get_backup_name() appends the configured
        // suffix even when backup_dir is set. `compute_backup_path` mirrors
        // that semantic (see `compute_backup_path_with_backup_dir` in
        // engine::local_copy::tests::executor_file_operations).
        let backup_path = backup_root.join("name1~");
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "pre-existing destination must be renamed into backup-dir before \
             the staged file is renamed into place"
        );
        assert!(!staged.exists(), "staging file should be moved out");
        assert!(
            !staging_dir.exists(),
            "empty .~tmp~ dir should be removed after sweep"
        );
    }

    /// Verifies the backup step is skipped when no existing destination is
    /// present, matching upstream `backup.c:make_backup()` which returns
    /// early when `lstat(fname)` reports `ENOENT`.
    #[test]
    fn handle_delayed_updates_no_backup_when_dest_missing() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"only-content").unwrap();

        let final_path = dest_root.join("name1");

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(
            &[(staged.clone(), final_path.clone())],
            Some(backup_config),
            None,
        );

        assert_eq!(fs::read_to_string(&final_path).unwrap(), "only-content");
        assert!(
            !backup_root.join("name1").exists(),
            "no backup file should be created when destination did not exist"
        );
    }

    /// Mirrors upstream `testsuite/backup.test:27-33` (`--no-whole-file
    /// --backup` without `--backup-dir`). With `backup_dir = None` and a
    /// `~` suffix, the existing destination must be renamed alongside the
    /// original (`name1` -> `name1~`), and the staged update must land at
    /// the original path. Upstream emits `backed up name1 to name1~`.
    #[test]
    fn handle_delayed_updates_backs_up_in_place_with_suffix_only() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let final_path = dest_root.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: None,
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(
            &[(staged.clone(), final_path.clone())],
            Some(backup_config),
            None,
        );

        assert_eq!(
            fs::read_to_string(&final_path).unwrap(),
            "new-content",
            "staged file must replace destination after in-place backup"
        );
        let backup_path = dest_root.join("name1~");
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "pre-existing destination must be renamed to <name><suffix> in \
             the same directory when no --backup-dir is set"
        );
    }

    /// Mirrors upstream `testsuite/backup.test:38-45` (`--backup-dir=bakdir`
    /// with nested source path `deep/name1`). The backup hierarchy must
    /// mirror the source layout: `deep/name1` -> `bakdir/deep/name1~`.
    /// Upstream's `copy_valid_path()` creates missing parents; oc-rsync's
    /// `handle_delayed_updates` relies on `create_dir_all(parent)` for the
    /// same effect.
    #[test]
    fn handle_delayed_updates_creates_intermediate_backup_dirs() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let deep_dest = dest_root.join("deep");
        fs::create_dir(&deep_dest).unwrap();
        let final_path = deep_dest.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(
            &[(staged.clone(), final_path.clone())],
            Some(backup_config),
            None,
        );

        let backup_path = backup_root.join("deep").join("name1~");
        assert!(
            backup_path.exists(),
            "backup_dir must mirror the source hierarchy: {} should exist",
            backup_path.display()
        );
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "nested backup must carry the pre-existing destination content"
        );
        assert_eq!(
            fs::read_to_string(&final_path).unwrap(),
            "new-content",
            "staged file must reach the nested destination path"
        );
    }

    /// Mirrors upstream `testsuite/backup.test:43` regex `backed up $fn
    /// to .*/$fn$` - when `--backup-dir` is set and `--suffix` is left at
    /// its `--backup-dir` default (empty string), the backup path has NO
    /// suffix appended. Upstream's `stringjoin(rel, remainder, fname,
    /// backup_suffix, NULL)` collapses to just `bakdir/path/name` when
    /// `backup_suffix == ""`.
    #[test]
    fn handle_delayed_updates_backup_dir_with_empty_suffix() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();
        let staged = staging_dir.join("name1");
        fs::write(&staged, b"new-content").unwrap();

        let final_path = dest_root.join("name1");
        fs::write(&final_path, b"old-content").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from(""),
        };

        handle_delayed_updates(
            &[(staged.clone(), final_path.clone())],
            Some(backup_config),
            None,
        );

        let suffixed = backup_root.join("name1~");
        assert!(
            !suffixed.exists(),
            "empty suffix must NOT append `~` (would diverge from upstream \
             default when --backup-dir is set without explicit --suffix)"
        );
        let backup_path = backup_root.join("name1");
        assert_eq!(
            fs::read_to_string(&backup_path).unwrap(),
            "old-content",
            "with empty suffix, backup path is bakdir/<name> verbatim"
        );
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "new-content");
    }

    /// Mirrors upstream `testsuite/backup.test:28,42,55` which iterate
    /// `for fn in deep/name1 deep/name2; do ...` - a single `--backup`
    /// invocation must back up every modified file in one delayed-updates
    /// sweep, with each backup honoring its own source path.
    #[test]
    fn handle_delayed_updates_backs_up_multiple_files_in_one_sweep() {
        use crate::disk_commit::BackupConfig;
        use std::ffi::OsString;

        let dir = test_support::create_tempdir();
        let dest_root = dir.path();
        let backup_root = dest_root.join("bak");
        fs::create_dir(&backup_root).unwrap();

        let staging_dir = dest_root.join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        let final_a = dest_root.join("name1");
        let final_b = dest_root.join("name2");
        fs::write(&final_a, b"old-a").unwrap();
        fs::write(&final_b, b"old-b").unwrap();

        let staged_a = staging_dir.join("name1");
        let staged_b = staging_dir.join("name2");
        fs::write(&staged_a, b"new-a").unwrap();
        fs::write(&staged_b, b"new-b").unwrap();

        let backup_config = BackupConfig {
            dest_dir: dest_root.to_path_buf(),
            backup_dir: Some(backup_root.clone()),
            suffix: OsString::from("~"),
        };

        handle_delayed_updates(
            &[
                (staged_a.clone(), final_a.clone()),
                (staged_b.clone(), final_b.clone()),
            ],
            Some(backup_config),
            None,
        );

        assert_eq!(fs::read_to_string(&final_a).unwrap(), "new-a");
        assert_eq!(fs::read_to_string(&final_b).unwrap(), "new-b");
        assert_eq!(
            fs::read_to_string(backup_root.join("name1~")).unwrap(),
            "old-a",
            "every file in the sweep must be backed up independently"
        );
        assert_eq!(
            fs::read_to_string(backup_root.join("name2~")).unwrap(),
            "old-b",
            "every file in the sweep must be backed up independently"
        );
    }

    /// Verifies that staged files in `.~tmp~/` persist as valid partials when
    /// the sweep is never called (simulating an interrupted transfer).
    ///
    /// This is the core invariant for `--delay-updates` interrupt safety:
    /// on interrupt, `handle_delayed_updates()` is skipped (the `?` operator
    /// propagates the error before reaching the sweep call in `pipelined.rs`),
    /// leaving staged files intact for the next resume attempt.
    ///
    /// upstream: receiver.c:694-695 - handle_delayed_updates() only after
    /// successful completion of both transfer phases.
    #[test]
    fn interrupt_skips_sweep_files_persist_in_staging() {
        let dir = test_support::create_tempdir();
        let staging_dir = dir.path().join(".~tmp~");
        fs::create_dir(&staging_dir).unwrap();

        // Create staged files as if commit_file() placed them there.
        let staged_a = staging_dir.join("a.txt");
        let staged_b = staging_dir.join("b.txt");
        fs::write(&staged_a, b"staged-a").unwrap();
        fs::write(&staged_b, b"staged-b").unwrap();

        let final_a = dir.path().join("a.txt");
        let final_b = dir.path().join("b.txt");

        // Do NOT call handle_delayed_updates - simulating interrupt.
        // Verify files remain in staging.
        assert!(staged_a.exists(), "staged file a must persist");
        assert!(staged_b.exists(), "staged file b must persist");
        assert!(!final_a.exists(), "final path a must not exist");
        assert!(!final_b.exists(), "final path b must not exist");

        // Verify the staged content is valid (usable for resume).
        assert_eq!(fs::read(&staged_a).unwrap(), b"staged-a");
        assert_eq!(fs::read(&staged_b).unwrap(), b"staged-b");

        // Now verify that a subsequent resume (calling the sweep) works.
        let delayed = vec![
            (staged_a.clone(), final_a.clone()),
            (staged_b.clone(), final_b.clone()),
        ];
        handle_delayed_updates(&delayed, None, None);

        assert!(final_a.exists());
        assert!(final_b.exists());
        assert_eq!(fs::read_to_string(&final_a).unwrap(), "staged-a");
        assert_eq!(fs::read_to_string(&final_b).unwrap(), "staged-b");
    }
}
