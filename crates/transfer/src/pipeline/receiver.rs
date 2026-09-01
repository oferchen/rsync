//! `PipelinedReceiver` - mediator between network and disk threads.
//!
//! Owns the channels and the disk commit thread, coordinating lifecycle,
//! error collection, and graceful shutdown. Supports upstream rsync's
//! redo mechanism: when a file's whole-file checksum fails verification,
//! it is queued for retransmission in phase 2 instead of aborting the
//! entire transfer.
//!
//! # Upstream Reference
//!
//! - `receiver.c:1093-1099` - `send_msg_int(MSG_REDO, ndx)` on checksum failure
//! - `generator.c:2160-2199` - `check_for_finished_files()` processes redo queue
//! - `receiver.c:580-587` - phase transition on `NDX_DONE`

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::thread::JoinHandle;

use protocol::MessageCode;

use crate::pipeline::spsc::{self, TryRecvError};

use crate::delta_apply::ChecksumVerifier;
use crate::disk_commit::{DiskCommitConfig, PartialMode, spawn_disk_thread};
use crate::full_fname::full_fname_path;
use crate::pipeline::messages::{CommitResult, FileMessage};

/// Expected checksum for a pending file, used for deferred verification.
///
/// Stored in a FIFO queue by `PipelinedReceiver`. When the disk thread
/// returns a `CommitResult` with a computed checksum, it is compared
/// against the next `PendingChecksum` in the queue.
struct PendingChecksum {
    expected: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
    len: usize,
    file_path: PathBuf,
    /// The file's name as it appears in the file list, i.e. upstream's
    /// `f_name(file, ..)`.
    ///
    /// Kept beside the joined destination path in [`Self::file_path`] because
    /// the two diagnostics this queue feeds want different renderings: the
    /// `mkstemp` failure names the temp file it tried to create, while the
    /// verification failure names the transferred file the way upstream's
    /// receiver does - relative to the destination root it `change_dir()`ed
    /// into, never as an absolute path.
    ///
    /// upstream: receiver.c:882 - `fname = local_name ? local_name : f_name(file, fbuf)`
    /// upstream: receiver.c:1352 - `local_name ? f_name(file, NULL) : fname`, so
    /// the flist name is printed in both the `local_name` and the plain case.
    flist_name: PathBuf,
    /// File list index for this file, used to identify which file to redo.
    file_index: usize,
    /// Whether this file was written in place (`--inplace`/`--append`).
    ///
    /// Selects the upstream `keptstr` wording on a verification failure: an
    /// in-place update is "retained" rather than "discarded", because the
    /// destination inode was overwritten and cannot be rolled back.
    ///
    /// upstream: receiver.c:1073-1078 - `!inplace` gates the "discarded" word.
    is_inplace: bool,
}

/// Mediator that coordinates the network ingest thread with the disk
/// commit thread via bounded channels, including deferred checksum
/// verification.
///
/// When `redo_enabled` is `true`, checksum mismatches in phase 1 are
/// collected in `redo_indices` instead of returning errors. The caller
/// retrieves the list via [`Self::take_redo_indices`] and retransmits
/// those files in phase 2 with empty basis (whole-file transfer).
pub struct PipelinedReceiver {
    file_tx: spsc::Sender<FileMessage>,
    result_rx: spsc::Receiver<io::Result<CommitResult>>,
    /// Return channel for buffer recycling from the disk thread.
    buf_return_rx: spsc::Receiver<Vec<u8>>,
    disk_thread: Option<JoinHandle<()>>,
    /// Number of commits sent but not yet collected.
    pending_commits: usize,
    /// Queue of expected checksums for deferred verification.
    /// Consumed FIFO when collecting `CommitResult`s, since the disk thread
    /// processes files in the same order they are submitted.
    expected_checksums: VecDeque<PendingChecksum>,
    /// File indices that failed checksum verification and should be retried.
    /// Mirrors upstream `redo_list` in `io.c:158`.
    redo_indices: Vec<usize>,
    /// Whether the redo mechanism is active (phase 1). When false (phase 2),
    /// checksum mismatches are hard errors.
    redo_enabled: bool,
    /// Count of files skipped due to permission-denied errors during disk commit.
    /// Used to accumulate `IOERR_GENERAL` for exit code 23.
    permission_error_count: u32,
    /// Accumulated warning/error messages from checksum verification and
    /// permission failures. Collected here instead of using `eprintln!` to
    /// avoid deadlocking on the global stderr mutex in daemon handler threads.
    /// The caller retrieves these via [`Self::drain_warnings`] and routes
    /// them through the multiplexed protocol writer. Each entry carries the
    /// upstream `rwrite()` message code so a fatal transfer error
    /// (`MSG_ERROR_XFER`) is not downgraded to an informational message: the
    /// peer sets `got_xfer_error` on `MSG_ERROR_XFER` receipt, yielding exit
    /// 23 (`RERR_PARTIAL`).
    warnings: Vec<(MessageCode, String)>,
    /// Files staged to `.~tmp~` partial dir when `--delay-updates` is active.
    /// Each entry is `(staging_path, final_path)`. The receiver performs a
    /// bulk rename sweep at the phase 2 boundary.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:546-547`: `delayed_bits = bitbag_create()`
    /// - `receiver.c:694-695`: `handle_delayed_updates()` sweep
    delayed_updates: Vec<(PathBuf, PathBuf)>,
    /// Flat file indices whose commit was confirmed (finish_transfer succeeded,
    /// checksum verified). The receiver drains these and, when
    /// `--remove-source-files` is active, sends `MSG_SUCCESS(ndx)` to the sender
    /// so it can unlink the confirmed source. Mirrors upstream's
    /// `send_msg_success()` at `recv_ok == 1`.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:1063-1069`: `send_msg_success(fname, ndx)` on `recv_ok == 1`.
    success_indices: Vec<usize>,
    /// Partial-retention mode for the session, captured from the disk-commit
    /// config before it is moved into the disk thread. Selects the upstream
    /// `keptstr` wording on a verification failure.
    ///
    /// upstream: receiver.c:1073-1078 - `keep_partial`/`partial_dir` gating.
    partial_mode: PartialMode,
    /// Daemon module served by this process, captured from the disk-commit
    /// config. Gates the ` (in MODULE)` suffix that `full_fname()` appends to
    /// the quoted path in the `mkstemp` failure line.
    ///
    /// upstream: util1.c:1453 - `if (module_id >= 0)` in `full_fname()`.
    daemon_module: Option<String>,
    /// Module root and destination directory, i.e. upstream's `module_dir` and
    /// the `curr_dir` the receiver `chdir()`ed into (`main.c:815`
    /// `change_dir(dest_path, ..)`). Together they make the `mkstemp` failure
    /// name its temp file relative to the module root.
    ///
    /// upstream: util1.c:1448 - `p1 = curr_dir + module_dirlen`.
    daemon_module_root: Option<PathBuf>,
    dest_dir: Option<PathBuf>,
    /// Session state behind the verification-failure report, seeded by
    /// [`Self::with_verify_report`].
    verify_report: VerifyReport,
}

/// The session state upstream's receiver consults when a whole-file
/// verification fails, beyond what the per-file `PendingChecksum` carries.
///
/// Both fields are process-wide globals upstream and are `false` by default
/// there, so [`Default`] reproduces a plain, non-batch run with no `%i` in the
/// per-file format.
///
/// upstream: receiver.c:1072,1085 - the two globals read by `case 0:`.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct VerifyReport {
    /// The resolved per-file output format carries `%i`, i.e. upstream's
    /// `stdout_format_has_i`. Together with `INFO_GTE(NAME, 1)` it is what
    /// lets the `FWARNING` form print at all.
    ///
    /// upstream: receiver.c:1072 - `msgtype == FERROR_XFER || INFO_GTE(NAME, 1)
    /// || stdout_format_has_i`.
    pub out_format_forwards_i: bool,
    /// This receiver is replaying a recorded batch (`--read-batch`), i.e.
    /// upstream's `read_batch`. Selects the retry wording: a batch replay may
    /// only *try* the redo, because the recorded stream may not carry it.
    ///
    /// upstream: receiver.c:1347 - `redostr = read_batch ? " (may try again)"
    /// : " (will try again)"`.
    pub read_batch: bool,
}

/// Projects this receiver's partial-retention mode onto the three upstream
/// variables `receiver.c:1074-1076` reads.
///
/// Upstream keeps `keep_partial`, `partialptr` and `partial_dir` separately;
/// oc collapses them into one session-wide [`PartialMode`], so a mode other
/// than [`PartialMode::None`] always has a partial path to retain.
fn partial_state(partial_mode: &PartialMode) -> (bool, bool, bool) {
    match partial_mode {
        PartialMode::None => (false, false, false),
        PartialMode::Partial => (true, true, false),
        PartialMode::PartialDir(_) => (true, true, true),
    }
}

/// Builds the diagnostic upstream's receiver emits for a file that failed its
/// whole-file verification, or `None` when upstream stays silent.
///
/// The rule itself is [`logging::verification_failure`]; this maps the
/// receiver's own state onto it and converts upstream's `msgtype` into the
/// multiplexed code the warning queue carries. The local-copy executor
/// supplies its own state to the same function, so neither path can drift the
/// wording, the severity or the emission gate away from the other.
///
/// `name` is the file's *file list* name. Upstream's receiver has already
/// `change_dir()`ed into the destination root (`main.c:815`), so `fname` - and
/// `f_name(file, NULL)` in the `local_name` case - renders relative to it; the
/// joined absolute destination path is never what a user sees here.
///
/// `redoing` is upstream's global of the same name: false in phase 1, where a
/// failure is a retryable `FWARNING`, and true in the phase-2 redo, where the
/// same failure is a fatal `FERROR_XFER` with no retry left to promise.
///
/// upstream: receiver.c:1071-1091.
fn verification_failure_report(
    name: &std::path::Path,
    partial_mode: &PartialMode,
    is_inplace: bool,
    redoing: bool,
    report: VerifyReport,
) -> Option<(MessageCode, String)> {
    let (keep_partial, has_partial_path, partial_dir) = partial_state(partial_mode);
    let (code, message) = logging::verification_failure(
        name,
        logging::VerifyFailure {
            redoing,
            read_batch: report.read_batch,
            stdout_format_has_i: report.out_format_forwards_i,
            keep_partial,
            has_partial_path,
            partial_dir,
            inplace: is_inplace,
        },
    )?;
    // upstream: log.c:332-337 - the receiver's rprintf(msgtype, ..) travels to
    // the client as the multiplexed message for that log code.
    let code = MessageCode::from_log_code(code)
        .expect("FERROR_XFER and FWARNING both have multiplexed equivalents");
    Some((code, message))
}

impl PipelinedReceiver {
    /// Spawns the disk commit thread and returns a new mediator.
    ///
    /// # Errors
    ///
    /// Propagates the `io::Error` from [`spawn_disk_thread`] when the disk
    /// commit thread cannot be spawned (typically `EAGAIN`/`RLIMIT_NPROC`).
    pub fn new(config: DiskCommitConfig) -> io::Result<Self> {
        let partial_mode = config.partial_mode.clone();
        let daemon_module = config.daemon_module.clone();
        let daemon_module_root = config.daemon_module_root.clone();
        let dest_dir = config.dest_dir.clone();
        let h = spawn_disk_thread(config)?;
        Ok(Self {
            file_tx: h.file_tx,
            result_rx: h.result_rx,
            buf_return_rx: h.buf_return_rx,
            disk_thread: Some(h.join_handle),
            pending_commits: 0,
            expected_checksums: VecDeque::new(),
            redo_indices: Vec::new(),
            redo_enabled: true,
            permission_error_count: 0,
            warnings: Vec::new(),
            delayed_updates: Vec::new(),
            success_indices: Vec::new(),
            partial_mode,
            daemon_module,
            daemon_module_root,
            dest_dir,
            verify_report: VerifyReport::default(),
        })
    }

    /// Seeds the session state the verification-failure report reads.
    ///
    /// Separate from [`Self::new`] because it carries no disk-commit meaning:
    /// the disk thread neither knows nor cares about the client's output format
    /// or a batch replay. The default is upstream's default global state, so a
    /// receiver that never calls this reports exactly as a plain run does.
    #[must_use]
    pub fn with_verify_report(mut self, report: VerifyReport) -> Self {
        self.verify_report = report;
        self
    }

    /// Returns the daemon path context `full_fname()` renders against, or
    /// `None` outside a daemon server process (upstream `module_id < 0`).
    fn daemon_paths(&self) -> Option<crate::full_fname::DaemonPaths<'_>> {
        let module = self.daemon_module.as_deref()?;
        let module_root = self.daemon_module_root.as_deref()?;
        Some(crate::full_fname::DaemonPaths {
            module,
            module_root,
            curr_dir: self.dest_dir.as_deref().unwrap_or(module_root),
        })
    }

    /// Formats the receiver's temp-file creation failure the way upstream does.
    ///
    /// Upstream names the temp file it tried to create, not the destination it
    /// would eventually have been renamed to, so a denied write into a module
    /// reads `mkstemp ".new.txt.a2dBeL" (in romod) failed` - never the
    /// server's absolute path. The attempted name rides along on the error
    /// from [`crate::temp_guard::open_tmpfile`]; `dest` is the fallback for an
    /// error raised anywhere else in the commit.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:452-453` - `rsyserr(FERROR_XFER, errno, "mkstemp %s failed",
    ///   full_fname(fnametmp))`
    fn commit_failure(&self, dest: &std::path::Path, error: &io::Error) -> String {
        let (op, named) = match crate::temp_guard::commit_op_failure(error) {
            Some((op, path)) => (Some(op), path),
            None => (None, dest),
        };
        let name = full_fname_path(named, self.daemon_paths());
        let reason = upstream_errno_text(error);
        match op {
            // upstream: rsync-3.5.0/receiver.c:452-453
            Some(crate::temp_guard::CommitOp::Mkstemp) | None => {
                format!("rsync: [receiver] mkstemp {name} failed: {reason}")
            }
            // upstream: rsync-3.5.0/backup.c:401-402 `keep_backup failed: %s -> "%s"`.
            // oc names the pre-image only; the backup destination is chosen
            // inside make_backup and is not carried back on the error.
            Some(crate::temp_guard::CommitOp::Backup) => {
                format!("rsync: [receiver] keep_backup failed: {name}: {reason}")
            }
            // upstream: rsync-3.5.0/receiver.c:710-712 `rename failed for %s
            // (from %s)`. oc names the destination only; the `from` operand is
            // the internal temp name, which upstream prints and oc omits.
            Some(crate::temp_guard::CommitOp::Rename) => {
                format!("rsync: [receiver] rename failed for {name}: {reason}")
            }
        }
    }

    /// Formats a failed metadata apply the way upstream reports it.
    ///
    /// Upstream's `set_file_attrs()` does not swallow a failed chmod/chown/
    /// utimes: each arm calls `rsyserr(FERROR_XFER, errno, "failed to set ...
    /// on %s", full_fname(fname))` and then `goto cleanup`. The diagnostic is
    /// not verbosity-gated, so an operator sees it at the default `-a`.
    ///
    /// `message` is [`metadata::MetadataError`]'s own rendering, which already
    /// carries upstream's `failed to <op> <path>: <errno text>` shape and names
    /// the failing attribute at the site that knows it. Re-deriving upstream's
    /// per-attribute wording here would mean inventing a mapping the apply
    /// chain does not report.
    ///
    /// Emitting this as `FERROR_XFER` (not `FINFO`) is what makes the peer's
    /// `rwrite()` set `got_xfer_error`, matching the exit 23 the collected
    /// error already forces through `IOERR_GENERAL` - so the code and the
    /// diagnostic now come from one decision instead of the code alone.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:814-816` - `rsyserr(FERROR_XFER, errno, "failed to set
    ///   permissions on %s", full_fname(fname))` in `set_file_attrs()`.
    /// - `rsync.c:784` - the sibling `"failed to set times on %s"` arm.
    /// - `log.c:311` - receipt of `FERROR_XFER` sets `got_xfer_error = 1`.
    fn metadata_failure(message: &str) -> String {
        format!("rsync: [receiver] {message}")
    }

    /// Returns a reference to the channel sender for `FileMessage` items.
    ///
    /// Pass this to [`crate::transfer_ops::process_file_response_streaming`].
    #[inline]
    pub fn file_sender(&self) -> &spsc::Sender<FileMessage> {
        &self.file_tx
    }

    /// Returns a reference to the buffer return receiver.
    ///
    /// Pass this to [`crate::transfer_ops::process_file_response_streaming`]
    /// so it can reuse buffers returned by the disk thread.
    #[inline]
    pub fn buf_return_rx(&self) -> &spsc::Receiver<Vec<u8>> {
        &self.buf_return_rx
    }

    /// Increments the pending-commit counter and records the expected checksum
    /// for deferred verification.
    ///
    /// Call this after `process_file_response_streaming` successfully returns
    /// (meaning it sent `Commit` through the channel).
    pub fn note_commit_sent(
        &mut self,
        expected_checksum: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
        checksum_len: usize,
        file_path: PathBuf,
        flist_name: PathBuf,
        file_index: usize,
        is_inplace: bool,
    ) {
        self.pending_commits += 1;
        self.expected_checksums.push_back(PendingChecksum {
            expected: expected_checksum,
            len: checksum_len,
            file_path,
            flist_name,
            file_index,
            is_inplace,
        });
    }

    /// Decides whether a failed commit skips one file or stops the whole run,
    /// and records the per-file diagnostic when it skips.
    ///
    /// Returns `None` when the file was skipped and the transfer may continue,
    /// or `Some(error)` for the caller to propagate.
    ///
    /// Upstream answers the two commit failures differently, and the
    /// discriminator is the operation, not the errno:
    ///
    /// - a denied `mkstemp()` is per-file. `receiver.c:452-455` reports it and
    ///   `recv_files()` moves to the next file, so the run ends at
    ///   `RERR_PARTIAL` (23) through `io_error`.
    /// - a failed backup is fatal. `finish_transfer()` answers
    ///   `!make_backup(fname, False)` with `exit_cleanup(RERR_FILEIO)`, which
    ///   does not return. Continuing would let every later file overwrite the
    ///   pre-image its own backup was asked to preserve, so the abort - not the
    ///   exit code - is the data guarantee. The `FERROR` diagnostic
    ///   `make_backup()` already printed is kept, matching upstream's two-line
    ///   output (the per-file reason, then the fatal exit line).
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.0/rsync.c:897-900` - `finish_transfer()`:
    ///   `if (!ok) exit_cleanup(RERR_FILEIO);`
    /// - `rsync-3.5.0/cleanup.c:103` - `_exit_cleanup()` is `NORETURN`.
    /// - `rsync-3.5.0/cleanup.c:113-117` - the first code in wins, and
    ///   `cleanup.c:210-218` only reaches for `RERR_PARTIAL` when no code has
    ///   been claimed, so `RERR_FILEIO` (11) outranks the 23 the accumulated
    ///   `got_xfer_error` would otherwise produce.
    /// - `rsync-3.5.0/receiver.c:452-455` - the contrasting per-file `mkstemp`
    ///   failure that does continue.
    fn absorb_commit_error(
        &mut self,
        error: io::Error,
        meta_errors: &mut Vec<(PathBuf, String)>,
    ) -> Option<io::Error> {
        let pending = self.expected_checksums.pop_front();
        let path = pending.map(|p| p.file_path).unwrap_or_default();
        let fatal_backup = matches!(
            crate::temp_guard::commit_op_failure(&error),
            Some((crate::temp_guard::CommitOp::Backup, _))
        );

        if fatal_backup {
            self.warnings
                .push((MessageCode::ErrorXfer, self.commit_failure(&path, &error)));
            return Some(error);
        }

        if !is_permission_error(&error) {
            return Some(error);
        }

        // upstream: receiver.c:452-453 - rsyserr(FERROR_XFER, errno,
        // "mkstemp %s failed", full_fname(fnametmp)). Emitting this as
        // FERROR_XFER (not FINFO) makes the peer's rwrite() set
        // got_xfer_error, so the run exits 23 (RERR_PARTIAL) instead of 0
        // when mkstemp() was denied.
        let message = self.commit_failure(&path, &error);
        self.warnings.push((MessageCode::ErrorXfer, message));
        meta_errors.push((path, error.to_string()));
        self.permission_error_count += 1;
        None
    }

    /// Non-blockingly drains all available commit results.
    ///
    /// Returns accumulated (bytes_written, metadata_errors).
    /// Permission-denied errors from the disk thread are treated as recoverable
    /// per-file errors - logged and added to `meta_errors` rather than aborting
    /// the transfer. A failed backup and every other disk error are propagated;
    /// see [`Self::absorb_commit_error`] for the split.
    /// Verifies per-file checksums when the disk thread returns a computed digest.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` continues on EACCES/EPERM, sets io_error
    pub fn drain_ready_results(&mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let mut bytes = 0u64;
        let mut meta_errors = Vec::new();

        loop {
            match self.result_rx.try_recv() {
                Ok(Ok(result)) => {
                    self.collect_delayed_update(&result);
                    Self::emit_backup_notice(&result);
                    self.verify_checksum(&result)?;
                    bytes += result.bytes_written;
                    if let Some(err) = result.metadata_error {
                        self.warnings
                            .push((MessageCode::ErrorXfer, Self::metadata_failure(&err.1)));
                        meta_errors.push(err);
                    }
                    self.pending_commits = self.pending_commits.saturating_sub(1);
                }
                Ok(Err(e)) => {
                    self.pending_commits = self.pending_commits.saturating_sub(1);
                    if let Some(err) = self.absorb_commit_error(e, &mut meta_errors) {
                        return Err(err);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.pending_commits > 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "disk commit thread disconnected with pending commits",
                        ));
                    }
                    break;
                }
            }
        }

        Ok((bytes, meta_errors))
    }

    /// Blocks until all pending commits have been collected.
    ///
    /// Returns accumulated (bytes_written, metadata_errors).
    /// Permission-denied errors from the disk thread are treated as recoverable
    /// per-file errors - logged and added to `meta_errors` rather than aborting
    /// the transfer. A failed backup and every other disk error are propagated;
    /// see [`Self::absorb_commit_error`] for the split.
    /// Verifies per-file checksums when the disk thread returns a computed digest.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` continues on EACCES/EPERM, sets io_error
    pub fn drain_all_results(&mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let mut bytes = 0u64;
        let mut meta_errors = Vec::new();

        while self.pending_commits > 0 {
            match self.result_rx.recv() {
                Ok(Ok(result)) => {
                    self.collect_delayed_update(&result);
                    Self::emit_backup_notice(&result);
                    self.verify_checksum(&result)?;
                    bytes += result.bytes_written;
                    if let Some(err) = result.metadata_error {
                        self.warnings
                            .push((MessageCode::ErrorXfer, Self::metadata_failure(&err.1)));
                        meta_errors.push(err);
                    }
                    self.pending_commits -= 1;
                }
                Ok(Err(e)) => {
                    self.pending_commits -= 1;
                    if let Some(err) = self.absorb_commit_error(e, &mut meta_errors) {
                        return Err(err);
                    }
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "disk commit thread disconnected with pending commits",
                    ));
                }
            }
        }

        Ok((bytes, meta_errors))
    }

    /// Returns the number of files skipped due to permission-denied errors.
    ///
    /// Used by the receiver to accumulate `IOERR_GENERAL` in `TransferStats.io_error`,
    /// which maps to exit code 23 (`RERR_PARTIAL`).
    pub fn permission_error_count(&self) -> u32 {
        self.permission_error_count
    }

    /// Verifies a commit result's computed checksum against the expected value.
    ///
    /// Pops the next expected checksum from the FIFO queue (files are processed
    /// in submission order).
    ///
    /// When `redo_enabled` is true (phase 1), checksum mismatches queue the file
    /// index into `redo_indices` and log a warning - mirroring upstream
    /// `receiver.c:1083-1096` which sends `MSG_REDO` and continues.
    ///
    /// When `redo_enabled` is false (phase 2), checksum mismatches are logged
    /// as errors but do not abort the transfer - mirroring upstream
    /// `receiver.c:1071-1080` where `redoing=1` uses `FERROR_XFER`.
    fn verify_checksum(&mut self, result: &CommitResult) -> io::Result<()> {
        let pending = match self.expected_checksums.pop_front() {
            Some(p) => p,
            None => return Ok(()),
        };

        if let Some(ref computed) = result.computed_checksum {
            if computed.len != pending.len
                || computed.bytes[..computed.len] != pending.expected[..pending.len]
            {
                self.warnings.extend(verification_failure_report(
                    &pending.flist_name,
                    &self.partial_mode,
                    pending.is_inplace,
                    !self.redo_enabled,
                    self.verify_report,
                ));
                if self.redo_enabled {
                    // upstream: receiver.c:1093-1096 - `send_msg_int(MSG_REDO,
                    // ndx)` sits OUTSIDE the emit `if`, so a suppressed
                    // diagnostic never costs the retry that corrects the file.
                    self.redo_indices.push(pending.file_index);
                }
                // In phase 2, upstream logs the error but continues the transfer.
                return Ok(());
            }
        }

        // upstream: receiver.c:1063-1069 - the file committed cleanly
        // (finish_transfer succeeded and any wire checksum verified), i.e.
        // `recv_ok == 1`. Record it as a confirmed success so the receiver can
        // emit MSG_SUCCESS(ndx) to the sender, which drives the deferred
        // --remove-source-files unlink of the confirmed source.
        self.success_indices.push(pending.file_index);
        Ok(())
    }

    /// Collects a delayed update entry from a commit result, if present.
    ///
    /// Uses the pending checksum queue to recover the final destination path
    /// (which is stored there as `file_path`). The staging path comes from
    /// `CommitResult::delayed_path`.
    fn collect_delayed_update(&mut self, result: &CommitResult) {
        if let Some(ref staged) = result.delayed_path {
            // The expected_checksums queue is FIFO and hasn't been popped
            // yet (verify_checksum pops it). Peek at the front entry.
            if let Some(pending) = self.expected_checksums.front() {
                self.delayed_updates
                    .push((staged.clone(), pending.file_path.clone()));
            }
        }
    }

    /// Emit the upstream `INFO_GTE(BACKUP, 1)` line for a successful
    /// `--backup` rename produced by the disk thread.
    ///
    /// The disk thread cannot emit this line itself because its thread-local
    /// [`logging::VerbosityConfig`] is never seeded with the user's
    /// `--info=backup` selection. Instead, `crate::disk_commit::make_backup`
    /// records destination-relative paths on the [`CommitResult`] and the
    /// main thread surfaces them here, so the `info_log!` guard runs against
    /// the correct verbosity configuration.
    ///
    /// upstream: backup.c:433 - `rprintf(FINFO, "backed up %s to %s\n",
    /// fname, buf)` on the `success:` label of `make_backup()`.
    fn emit_backup_notice(result: &CommitResult) {
        if let Some(ref notice) = result.backup_notice {
            logging::info_log!(
                Backup,
                1,
                "backed up {} to {}",
                notice.original.display(),
                notice.backup.display()
            );
        }
    }

    /// Returns and clears the accumulated delayed update entries.
    ///
    /// Each entry is `(staging_path, final_path)`. The receiver calls this
    /// at the phase 2 boundary and renames each file from its staging
    /// location to the final destination.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:694-695`: `handle_delayed_updates()` at phase 2
    pub fn take_delayed_updates(&mut self) -> Vec<(PathBuf, PathBuf)> {
        std::mem::take(&mut self.delayed_updates)
    }

    /// Drains newly detected redo indices without disabling redo mode.
    ///
    /// Call this after `drain_ready_results` or `drain_all_results` to
    /// retrieve file indices that failed checksum verification since the
    /// last drain. The caller should send `MSG_REDO` for each index over
    /// the multiplexed writer to signal the generator.
    ///
    /// Unlike [`Self::take_redo_indices`], this does NOT disable redo mode.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:1093-1097`: `send_msg_int(MSG_REDO, ndx)` sent immediately
    ///   when a checksum mismatch is detected during phase 1.
    pub fn drain_new_redo_indices(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.redo_indices)
    }

    /// Drains the flat file indices whose commit has been confirmed since the
    /// last call.
    ///
    /// Call this after `drain_ready_results` / `drain_all_results`. When
    /// `--remove-source-files` is active the caller maps each index to its wire
    /// NDX and sends `MSG_SUCCESS(ndx)` to the sender so it unlinks the
    /// confirmed source; otherwise the drained indices are simply discarded, so
    /// this stays bounded even when the flag is off.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:1063-1069`: `send_msg_success(fname, ndx)` on `recv_ok == 1`.
    /// - `io.c:1623-1637`: sender-side `MSG_SUCCESS` handler -> `successful_send`.
    pub fn drain_new_success_indices(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.success_indices)
    }

    /// Returns the list of file indices that need redo (failed checksum in phase 1).
    ///
    /// After calling this, the redo list is empty and `redo_enabled` is set to
    /// `false` so that subsequent checksum failures in phase 2 are hard-logged.
    pub fn take_redo_indices(&mut self) -> Vec<usize> {
        self.redo_enabled = false;
        std::mem::take(&mut self.redo_indices)
    }

    /// Returns the number of files queued for redo.
    pub fn redo_count(&self) -> usize {
        self.redo_indices.len()
    }

    /// Drains accumulated warning/error messages from checksum verification and
    /// permission failures. Each is paired with the upstream `rwrite()` message
    /// code so the caller routes it through the matching multiplex frame:
    /// `MSG_ERROR_XFER` for fatal transfer errors (setting the peer's
    /// `got_xfer_error`), `MSG_INFO`/`MSG_WARNING` otherwise.
    pub fn drain_warnings(&mut self) -> Vec<(MessageCode, String)> {
        std::mem::take(&mut self.warnings)
    }

    /// Sends `Shutdown` and joins the disk thread.
    ///
    /// Implicitly drains remaining results. Returns the final accumulated
    /// (bytes_written, metadata_errors).
    pub fn shutdown(mut self) -> io::Result<(u64, Vec<(PathBuf, String)>)> {
        let result = self.drain_all_results();

        // Send shutdown - ignore error (thread may have already exited).
        let _ = self.file_tx.send(FileMessage::Shutdown);

        if let Some(handle) = self.disk_thread.take() {
            let _ = handle.join();
        }

        result
    }
}

/// Returns `true` if the I/O error represents a permission-denied condition
/// (EACCES or EPERM) that should be treated as a recoverable per-file error.
///
/// Upstream rsync continues the transfer when individual files cannot be
/// opened due to insufficient privileges, setting `io_error |= IOERR_GENERAL`
/// and returning exit code 23 (`RERR_PARTIAL`) at the end.
///
/// # Upstream Reference
///
/// - `receiver.c:825-832` - `do_open()` failure handling logs and continues
fn is_permission_error(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::PermissionDenied
}
/// Renders an error the way upstream's `rsyserr` does: `strerror(errno)`
/// followed by the numeric errno in parentheses.
///
/// `io::Error`'s own `Display` appends `" (os error N)"`, so the suffix is
/// stripped and re-emitted in upstream's shape. A non-OS error renders
/// verbatim, with no parenthesised number to invent.
///
/// upstream: `rsync-3.5.0/log.c` `rsyserr()` - `"%s: %s (%d)"`.
fn upstream_errno_text(error: &io::Error) -> String {
    let display = error.to_string();
    // A tagged failure is an `io::Error::new(kind, payload)`, i.e. the `Custom`
    // variant, whose `raw_os_error()` is `None` even though the payload wraps a
    // real OS error. Recover the errno from the source chain so tagging an
    // error does not silently drop the number from its own message.
    let errno = error.raw_os_error().or_else(|| {
        std::error::Error::source(error.get_ref()?)?
            .downcast_ref::<io::Error>()?
            .raw_os_error()
    });
    match errno {
        Some(errno) => {
            let suffix = format!(" (os error {errno})");
            let text = display.strip_suffix(&suffix).unwrap_or(&display);
            format!("{text} ({errno})")
        }
        None => display,
    }
}

impl Drop for PipelinedReceiver {
    fn drop(&mut self) {
        // Best-effort shutdown: send Shutdown and join.
        let _ = self.file_tx.send(FileMessage::Shutdown);
        if let Some(handle) = self.disk_thread.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the config a `--delay-updates` receiver actually runs with.
    ///
    /// `--delay-updates` always stages through a partial directory: upstream
    /// substitutes the implicit `.~tmp~` when the operator named none
    /// (options.c:2563-2564), and oc mirrors that where the receiver derives
    /// its `PartialMode`. A `delay_updates` config carrying no partial mode is
    /// a state the production path cannot produce.
    fn delay_updates_config() -> DiskCommitConfig {
        DiskCommitConfig {
            delay_updates: true,
            partial_mode: PartialMode::PartialDir(std::path::PathBuf::from(
                crate::disk_commit::DELAY_UPDATES_PARTIAL_DIR,
            )),
            ..DiskCommitConfig::default()
        }
    }
    use crate::pipeline::messages::BeginMessage;

    #[test]
    fn spawn_and_shutdown() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        let (bytes, errors) = pr.shutdown().unwrap();
        assert_eq!(bytes, 0);
        assert!(errors.is_empty());
    }

    #[test]
    fn write_file_through_mediator() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("test.dat");

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        pr.file_sender()
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 100,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
                xattr_basis: None,
                file_entry: None,
            })))
            .unwrap();

        pr.file_sender()
            .send(FileMessage::Chunk(b"test data".to_vec()))
            .unwrap();

        pr.file_sender()
            .send(FileMessage::Commit {
                expected_checksum: Default::default(),
            })
            .unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        let (bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(bytes, 9);
        assert!(errors.is_empty());

        assert_eq!(std::fs::read(&file_path).unwrap(), b"test data");

        let (extra_bytes, _) = pr.shutdown().unwrap();
        assert_eq!(extra_bytes, 0);
    }

    #[test]
    fn drain_ready_returns_empty_when_nothing_pending() {
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        let (bytes, errors) = pr.drain_ready_results().unwrap();
        assert_eq!(bytes, 0);
        assert!(errors.is_empty());
        drop(pr);
    }

    #[test]
    fn drop_cleans_up_thread() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        drop(pr); // Should not hang or panic.
    }

    #[test]
    fn redo_initially_empty() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        assert_eq!(pr.redo_count(), 0);
        drop(pr);
    }

    #[test]
    fn take_redo_indices_disables_redo() {
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        assert!(pr.redo_enabled);
        let indices = pr.take_redo_indices();
        assert!(indices.is_empty());
        assert!(!pr.redo_enabled);
        drop(pr);
    }

    #[test]
    fn checksum_mismatch_queues_redo_in_phase1() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        // Simulate a committed file with a mismatching checksum.
        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/file.txt"),
            flist_name: PathBuf::from("file.txt"),
            file_index: 7,
            is_inplace: false,
        });

        // Create a CommitResult with a different computed checksum.
        let mut computed_bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        computed_bytes[0] = 0xBB;
        let result = CommitResult {
            bytes_written: 100,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: computed_bytes,
                len: 4,
            }),
            delayed_path: None,
            backup_notice: None,
        };

        // In phase 1 (redo_enabled=true), this should NOT return an error.
        pr.verify_checksum(&result).unwrap();

        // The file should be queued for redo.
        assert_eq!(pr.redo_count(), 1);
        let indices = pr.take_redo_indices();
        assert_eq!(indices, vec![7]);

        // After take_redo_indices, redo is disabled (phase 2).
        assert!(!pr.redo_enabled);
        assert_eq!(pr.redo_count(), 0);

        drop(pr);
    }

    #[test]
    fn checksum_mismatch_logs_error_in_phase2() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        // Move to phase 2.
        let _ = pr.take_redo_indices();
        assert!(!pr.redo_enabled);

        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/file2.txt"),
            flist_name: PathBuf::from("file2.txt"),
            file_index: 3,
            is_inplace: false,
        });

        let mut computed_bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        computed_bytes[0] = 0xCC;
        let result = CommitResult {
            bytes_written: 200,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: computed_bytes,
                len: 4,
            }),
            delayed_path: None,
            backup_notice: None,
        };

        // In phase 2, mismatch should still return Ok (error is logged, not fatal).
        pr.verify_checksum(&result).unwrap();

        // No redo queued in phase 2.
        assert_eq!(pr.redo_count(), 0);

        drop(pr);
    }

    #[test]
    fn checksum_match_does_not_queue_redo() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;
        expected[1] = 0xBB;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/ok.txt"),
            flist_name: PathBuf::from("ok.txt"),
            file_index: 5,
            is_inplace: false,
        });

        // Same checksum - should succeed without redo.
        let result = CommitResult {
            bytes_written: 50,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: expected,
                len: 4,
            }),
            delayed_path: None,
            backup_notice: None,
        };

        pr.verify_checksum(&result).unwrap();
        assert_eq!(pr.redo_count(), 0);

        drop(pr);
    }

    /// A cleanly committed file is recorded for `MSG_SUCCESS` emission, and only
    /// once, so the sender is told to unlink the source exactly one time.
    ///
    /// This is the receiver half of the `--remove-source-files` crash-safety
    /// contract: `success_indices` is populated only inside `verify_checksum`
    /// while draining a post-rename `CommitResult`, so recording an index here
    /// means the file has durably landed at the destination. A refactor that
    /// confirmed a source before the commit completed - the data-loss bug this
    /// mechanism prevents - would have to add a second producer, which this test
    /// would expose as an out-of-band success.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:1063-1069` - `send_msg_success(fname, ndx)` on `recv_ok == 1`.
    #[test]
    fn clean_commit_records_msg_success_once() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;
        expected[1] = 0xBB;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/ok.txt"),
            flist_name: PathBuf::from("ok.txt"),
            file_index: 9,
            is_inplace: false,
        });

        // A matching checksum on a committed (post-rename) result confirms the file.
        let result = CommitResult {
            bytes_written: 64,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: expected,
                len: 4,
            }),
            delayed_path: None,
            backup_notice: None,
        };

        pr.verify_checksum(&result).unwrap();

        // The confirmed index is queued so the caller emits MSG_SUCCESS(ndx).
        assert_eq!(
            pr.drain_new_success_indices(),
            vec![9],
            "a durably committed file must be confirmed for MSG_SUCCESS"
        );
        // Draining clears it, so the sender is never told to unlink twice.
        assert!(
            pr.drain_new_success_indices().is_empty(),
            "a confirmation must not be replayed on a second drain"
        );

        drop(pr);
    }

    /// A file that fails checksum verification (a would-be-corrupt commit that is
    /// discarded and queued for redo) must NOT be confirmed, so no `MSG_SUCCESS`
    /// reaches the sender and it keeps the source for the retry.
    ///
    /// This is the receiver-side guard against data loss on a failed commit: the
    /// mismatch branch of `verify_checksum` returns before the
    /// `success_indices.push`, so an interrupted or corrupt transfer never
    /// tells the sender its source landed safely. Without this assertion a
    /// refactor could push the success before the checksum check and silently
    /// re-introduce the inline-unlink data-loss bug.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:965-968` - checksum failure discards the update and redoes it.
    /// - `receiver.c:1063-1069` - `send_msg_success` runs only on `recv_ok == 1`.
    #[test]
    fn checksum_mismatch_withholds_msg_success() {
        use crate::pipeline::messages::ComputedChecksum;

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;

        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/dest/corrupt.txt"),
            flist_name: PathBuf::from("corrupt.txt"),
            file_index: 4,
            is_inplace: false,
        });

        // A different computed checksum: the update is discarded and queued for redo.
        let mut computed_bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        computed_bytes[0] = 0xBB;
        let result = CommitResult {
            bytes_written: 100,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: computed_bytes,
                len: 4,
            }),
            delayed_path: None,
            backup_notice: None,
        };

        pr.verify_checksum(&result).unwrap();

        // The file is redone, not confirmed: the sender receives no MSG_SUCCESS
        // and therefore never unlinks the source of a discarded update.
        assert_eq!(pr.redo_count(), 1, "a failed verification queues a redo");
        assert!(
            pr.drain_new_success_indices().is_empty(),
            "a discarded update must never confirm a source removal"
        );

        drop(pr);
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_on_output_is_recoverable() {
        use std::os::unix::fs::PermissionsExt;

        // Check if running as root via /proc or whoami fallback.
        // Root bypasses permission checks so the test would be meaningless.
        if std::env::var("USER").is_ok_and(|u| u == "root") {
            return;
        }

        let dir = test_support::create_tempdir();
        // Create a read-only directory so file creation inside fails
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir(&readonly_dir).unwrap();
        std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o555)).unwrap();

        let file_path = readonly_dir.join("test.dat");
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        // Send a file destined for the read-only directory
        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 9,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"test data".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();

        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        // Should NOT return an error - permission denied is recoverable
        let (bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(bytes, 0, "no bytes written for failed file");
        assert_eq!(errors.len(), 1, "one error recorded");
        assert!(
            errors[0].1.contains("ermission denied") || errors[0].1.contains("EACCES"),
            "error should mention permission denied: {}",
            errors[0].1
        );
        assert_eq!(pr.permission_error_count(), 1);

        // Restore permissions for cleanup
        let _ = std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o755));
        drop(pr);
    }

    /// Pipelined-path mirror of #6249's synchronous discard test: a receiver
    /// whose temp-file create fails mid-transfer on a MULTI-CHUNK delta must not
    /// desync the disk-thread result stream. The network thread has already
    /// lifted the whole delta off the wire and enqueued it as `Begin` + several
    /// `Chunk`s + `Commit`; when `open_tmpfile()` fails, the disk thread must
    /// drain those queued messages (the channel analog of upstream
    /// `discard_receive_data`, receiver.c:999-1006) rather than returning
    /// immediately and mis-parsing the next `Chunk` as a "message without
    /// Begin". The file is marked failed and folded to a per-file partial
    /// (IOERR_GENERAL -> RERR_PARTIAL, exit 23), never a fatal desync (exit 12).
    ///
    /// A MATCH-token delta on the wire becomes basis-copied `Chunk`s on this
    /// channel, so multiple chunks reproduce the exact desync a MATCH delta
    /// would trigger. Before the fix the orphaned `Chunk`/`Commit` messages
    /// produced a spurious second error; a single clean recoverable error here
    /// proves the drain works.
    #[cfg(unix)]
    #[test]
    fn multi_chunk_temp_create_failure_drains_without_desync_exit_23() {
        use crate::generator::io_error_flags::{IOERR_GENERAL, to_exit_code};
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses the read-only dir bit, so the create would succeed.
        if std::env::var("USER").is_ok_and(|u| u == "root") {
            return;
        }

        let dir = test_support::create_tempdir();
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir(&readonly_dir).unwrap();
        std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o555)).unwrap();

        let file_path = readonly_dir.join("multi.dat");
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        // Drive the non-coalesced path: an explicit Begin followed by several
        // Chunks and a Commit - exactly what the network thread emits for a
        // delta that reduces to multiple (basis-copied) chunks.
        pr.file_sender()
            .send(FileMessage::Begin(Box::new(BeginMessage {
                file_path: file_path.clone(),
                target_size: 9,
                file_entry_index: 0,
                checksum_verifier: None,
                is_device_target: false,
                is_inplace: false,
                append_offset: 0,
                xattr_list: None,
                xattr_basis: None,
                file_entry: None,
            })))
            .unwrap();
        pr.file_sender()
            .send(FileMessage::Chunk(b"aaa".to_vec()))
            .unwrap();
        pr.file_sender()
            .send(FileMessage::Chunk(b"bbb".to_vec()))
            .unwrap();
        pr.file_sender()
            .send(FileMessage::Chunk(b"ccc".to_vec()))
            .unwrap();
        pr.file_sender()
            .send(FileMessage::Commit {
                expected_checksum: Default::default(),
            })
            .unwrap();

        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        // Exactly ONE recoverable error - no spurious second error from
        // orphaned Chunk/Commit messages, no propagated fatal Err (exit 12).
        let (bytes, errors) = pr
            .drain_all_results()
            .expect("temp-create failure must be recoverable, not a fatal desync");
        assert_eq!(bytes, 0, "no bytes written for the failed file");
        assert_eq!(
            errors.len(),
            1,
            "the drained multi-chunk file yields exactly one per-file error"
        );
        assert_eq!(pr.permission_error_count(), 1);

        // The failed file sets IOERR_GENERAL, which maps to RERR_PARTIAL (23),
        // matching upstream's FERROR_XFER -> got_xfer_error -> _exit(RERR_PARTIAL).
        assert_eq!(
            to_exit_code(IOERR_GENERAL),
            23,
            "temp-create failure must yield RERR_PARTIAL (exit 23), not fatal (12)"
        );

        // The disk thread survived the drain and processes the next file
        // cleanly - proof the result stream is still aligned.
        let ok_path = dir.path().join("after.dat");
        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: ok_path.clone(),
                    target_size: 4,
                    file_entry_index: 1,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"next".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            ok_path.clone(),
            PathBuf::from(ok_path.file_name().unwrap()),
            1,
            false,
        );
        let (bytes2, errors2) = pr.drain_all_results().unwrap();
        assert_eq!(bytes2, 4, "the following file transfers normally");
        assert!(errors2.is_empty());
        assert_eq!(std::fs::read(&ok_path).unwrap(), b"next");

        let _ = std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o755));
        drop(pr);
    }

    #[test]
    fn permission_error_count_starts_at_zero() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        assert_eq!(pr.permission_error_count(), 0);
        drop(pr);
    }

    /// A failed output `mkstemp()` must surface as a `MSG_ERROR_XFER` warning,
    /// not `MSG_INFO`. The distinction is load-bearing: the peer's `rwrite()`
    /// The NON-BLOCKING drain must report a failed metadata apply too.
    ///
    /// `drain_ready_results` and `drain_all_results` are two separate copies of
    /// the collection loop, and the transfer driver calls BOTH: the pipelined
    /// receiver polls the non-blocking one every iteration
    /// (`receiver/transfer/pipeline.rs`) and blocks on the other only at the
    /// end of a phase. A diagnostic present in one and absent from the other is
    /// invisible for every file that completes mid-transfer - which is nearly
    /// all of them.
    ///
    /// Its sibling
    /// [`Self::metadata_apply_failure_drains_as_error_xfer_warning`] exercises
    /// only the blocking arm, so the non-blocking emission was deletable with
    /// the suite still green. Two drains, two cells, deliberately not one
    /// parameterised cell: a shared cell that happens to reach one arm is how
    /// that gap arose.
    ///
    /// The loop terminates on `pending_commits`, NOT on having collected a
    /// warning. That is load-bearing: `pending_commits` is decremented by the
    /// drain regardless of whether the emission is present, so removing the
    /// push makes this cell FAIL rather than hang.
    ///
    /// upstream: rsync.c:814-816 `set_file_attrs()`.
    #[cfg(unix)]
    #[test]
    fn metadata_apply_failure_drains_as_error_xfer_warning_when_polled() {
        use std::time::{Duration, Instant};

        let dir = test_support::create_tempdir();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let file_path = link.join("payload.dat");

        let mut entry =
            protocol::flist::FileEntry::new_file(PathBuf::from("payload.dat"), 9, 0o640);
        entry.set_mtime(1_000_000_000, 0);

        let config = DiskCommitConfig {
            metadata_opts: Some(
                metadata::MetadataOptions::new()
                    .preserve_permissions(true)
                    .preserve_times(true),
            ),
            ..DiskCommitConfig::default()
        };
        let mut pr = PipelinedReceiver::new(config).unwrap();

        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 9,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: Some(entry),
                }),
                data: b"test data".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from("payload.dat"),
            0,
            false,
        );

        // Poll the way the transfer loop does: take whatever the disk thread has
        // finished so far, until it has finished everything.
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut errors = Vec::new();
        while pr.pending_commits > 0 {
            assert!(
                Instant::now() < deadline,
                "the disk thread never returned the commit; the non-blocking \
                 drain could not be exercised"
            );
            let (_bytes, drained) = pr.drain_ready_results().unwrap();
            errors.extend(drained);
            std::thread::yield_now();
        }

        assert_eq!(
            errors.len(),
            1,
            "the fixture must actually fail the metadata apply on this arm too"
        );

        let warnings = pr.drain_warnings();
        assert_eq!(
            warnings.len(),
            1,
            "the non-blocking drain must queue the diagnostic, not just count it"
        );
        assert_eq!(
            warnings[0].0,
            MessageCode::ErrorXfer,
            "same log code as the blocking arm - one rule, two drains"
        );
        assert!(
            warnings[0].1.starts_with("rsync: [receiver] failed to "),
            "must carry upstream's `failed to ...` wording: {}",
            warnings[0].1
        );
        assert!(
            warnings[0].1.contains("payload.dat"),
            "must name the file it could not apply to: {}",
            warnings[0].1
        );
    }

    /// A metadata apply that fails must be REPORTED, not merely counted.
    ///
    /// Upstream's `set_file_attrs()` never swallows one: every arm calls
    /// `rsyserr(FERROR_XFER, errno, "failed to set ... on %s", ...)` before
    /// giving up (`rsync.c:784` for times, `rsync.c:814-816` for permissions).
    /// The diagnostic is not verbosity-gated, so it prints at a plain `-a`.
    ///
    /// oc collected the failure into `metadata_errors`, which sets
    /// `IOERR_GENERAL` and forces exit 23, and then rendered nothing anywhere -
    /// `stats.metadata_errors` has no consumer outside this crate. The result
    /// was a transfer that exits 23 with an unapplied mode and an empty stderr
    /// even at `-vvv --debug=all`, which is unreportable in the field.
    ///
    /// The fixture is the live shape measured against real rsync 3.5.0: a
    /// destination whose parent is a symlink. `fast_io::secure_chmod_at` walks
    /// the parent with `openat2(RESOLVE_NO_SYMLINKS)` on Linux and a leaf-only
    /// `openat(O_NOFOLLOW|O_DIRECTORY)` elsewhere; both refuse a symlinked
    /// parent (`ELOOP` / `ENOTDIR`), so the apply fails on every Unix without
    /// needing root or a contrived permission bit.
    ///
    /// The data still lands - only the metadata apply fails - which is exactly
    /// the case that used to be invisible.
    ///
    /// upstream: rsync.c:814-816 `set_file_attrs()`; log.c:311 - `FERROR_XFER`
    /// receipt sets `got_xfer_error = 1`, the exit-23 half this pairs with.
    #[cfg(unix)]
    #[test]
    fn metadata_apply_failure_drains_as_error_xfer_warning() {
        let dir = test_support::create_tempdir();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // The parent component is the symlink, so the confined walk refuses it
        // on both the openat2 and the O_NOFOLLOW platform arm.
        let file_path = link.join("payload.dat");

        let mut entry =
            protocol::flist::FileEntry::new_file(PathBuf::from("payload.dat"), 9, 0o640);
        entry.set_mtime(1_000_000_000, 0);

        let config = DiskCommitConfig {
            metadata_opts: Some(
                metadata::MetadataOptions::new()
                    .preserve_permissions(true)
                    .preserve_times(true),
            ),
            ..DiskCommitConfig::default()
        };
        let mut pr = PipelinedReceiver::new(config).unwrap();

        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 9,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: Some(entry),
                }),
                data: b"test data".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from("payload.dat"),
            0,
            false,
        );

        let (_bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(
            errors.len(),
            1,
            "the fixture must actually fail the metadata apply, or this test \
             proves nothing"
        );

        let warnings = pr.drain_warnings();
        assert_eq!(
            warnings.len(),
            1,
            "the failed metadata apply must queue exactly one diagnostic"
        );
        assert_eq!(
            warnings[0].0,
            MessageCode::ErrorXfer,
            "upstream reports it on FERROR_XFER, which is what sets \
             got_xfer_error and yields exit 23"
        );
        assert!(
            warnings[0].1.starts_with("rsync: [receiver] failed to "),
            "must carry upstream's `failed to ...` wording: {}",
            warnings[0].1
        );
        assert!(
            warnings[0].1.contains("payload.dat"),
            "must name the file it could not apply to: {}",
            warnings[0].1
        );
    }

    /// The commit path runs mkstemp, backup, rename and metadata behind one
    /// `Result`, so a `PermissionDenied` from any of them reaches the same
    /// reporting arm. Upstream gives each site its own text - `mkstemp %s
    /// failed` (`rsync-3.5.0/receiver.c:452-453`), `rename failed for %s`
    /// (`:710-712`), `keep_backup failed` (`rsync-3.5.0/backup.c:401-402`) -
    /// and oc must not label all three `mkstemp`.
    ///
    /// The untagged arm is the one that matters for regressions: an error
    /// raised outside the tagged stages still renders under `mkstemp` with the
    /// DESTINATION name, and that fallback is what made a non-mkstemp failure
    /// read as a temp-create failure in CI.
    #[cfg(unix)]
    #[test]
    fn commit_failure_names_the_operation_that_failed() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        let dest = std::path::Path::new("/srv/mod/payload");
        let temp = std::path::Path::new("/srv/mod/.payload.a2dBeL");
        let denied = || io::Error::from_raw_os_error(libc::EACCES);

        let mkstemp = pr.commit_failure(
            dest,
            &crate::temp_guard::attach_commit_op(
                crate::temp_guard::CommitOp::Mkstemp,
                temp,
                denied(),
            ),
        );
        let backup = pr.commit_failure(
            dest,
            &crate::temp_guard::attach_commit_op(
                crate::temp_guard::CommitOp::Backup,
                dest,
                denied(),
            ),
        );
        let rename = pr.commit_failure(
            dest,
            &crate::temp_guard::attach_commit_op(
                crate::temp_guard::CommitOp::Rename,
                dest,
                denied(),
            ),
        );
        let untagged = pr.commit_failure(dest, &denied());

        assert!(
            mkstemp.contains("mkstemp") && mkstemp.contains(".payload.a2dBeL"),
            "mkstemp keeps upstream's text and names the TEMP file: {mkstemp}"
        );
        assert!(
            backup.contains("keep_backup failed") && !backup.contains("mkstemp"),
            "a backup failure must not be labelled mkstemp: {backup}"
        );
        assert!(
            rename.contains("rename failed for") && !rename.contains("mkstemp"),
            "a rename failure must not be labelled mkstemp: {rename}"
        );
        assert!(
            untagged.contains("mkstemp") && untagged.contains("payload"),
            "an untagged error keeps the historical fallback: {untagged}"
        );
        for rendered in [&mkstemp, &backup, &rename, &untagged] {
            assert!(
                rendered.ends_with("Permission denied (13)"),
                "every arm renders strerror + errno like rsyserr: {rendered}"
            );
        }
    }

    /// The errno must come from the error, not from a literal. A hardcoded
    /// `(13)` renders every failure as `Permission denied` regardless of what
    /// actually happened, which is why the CI line could not be trusted to
    /// describe its own cause.
    #[cfg(unix)]
    #[test]
    fn commit_failure_reports_the_real_errno() {
        let pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        let dest = std::path::Path::new("/srv/mod/payload");
        let rendered = pr.commit_failure(dest, &io::Error::from_raw_os_error(libc::EPERM));
        assert!(
            rendered.ends_with("(1)"),
            "EPERM must render as (1), not the EACCES literal: {rendered}"
        );
        assert!(
            !rendered.contains("(13)"),
            "the errno must not be hardcoded: {rendered}"
        );
    }

    /// Wiring proof for the rename stage: the tag has to be attached by the
    /// real commit path, not merely spellable in a unit test.
    ///
    /// A writable `--temp-dir` with a read-only destination directory lets
    /// mkstemp SUCCEED and the rename onto the destination fail `EACCES`,
    /// which is the one shape that separates the two stages end to end.
    #[cfg(unix)]
    #[test]
    fn a_failed_rename_is_reported_as_a_rename_not_a_mkstemp() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses the read-only dir bit, so the rename would succeed.
        if std::env::var("USER").is_ok_and(|u| u == "root") {
            return;
        }
        let dir = test_support::create_tempdir();
        let temp_dir = dir.path().join("tmp");
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir(&temp_dir).unwrap();
        std::fs::create_dir(&readonly_dir).unwrap();
        std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o555)).unwrap();
        let file_path = readonly_dir.join("payload.dat");

        let config = DiskCommitConfig {
            temp_dir: Some(temp_dir),
            ..DiskCommitConfig::default()
        };
        let mut pr = PipelinedReceiver::new(config).unwrap();
        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 9,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"test data".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );
        let (_bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(errors.len(), 1, "one recoverable per-file error");
        let warnings = pr.drain_warnings();
        assert_eq!(
            warnings.len(),
            1,
            "one warning queued for the failed rename"
        );
        assert!(
            warnings[0].1.contains("rename failed for"),
            "the live commit path must tag the rename stage: {}",
            warnings[0].1
        );
        assert!(
            !warnings[0].1.contains("mkstemp"),
            "mkstemp SUCCEEDED here - labelling this mkstemp is the defect: {}",
            warnings[0].1
        );
    }

    /// only sets `got_xfer_error` (yielding exit 23, `RERR_PARTIAL`) on
    /// `FERROR_XFER` receipt (upstream log.c:311). Routing this as `MSG_INFO`
    /// would let a client-sender push against a read-only destination exit 0,
    /// silently masking that no file was transferred.
    #[cfg(unix)]
    #[test]
    fn output_open_failure_drains_as_error_xfer_warning() {
        use std::os::unix::fs::PermissionsExt;

        // Root bypasses the read-only dir bit, so mkstemp would succeed.
        if std::env::var("USER").is_ok_and(|u| u == "root") {
            return;
        }

        let dir = test_support::create_tempdir();
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir(&readonly_dir).unwrap();
        std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o555)).unwrap();

        let file_path = readonly_dir.join("denied.dat");
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 9,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"test data".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        let (_bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(errors.len(), 1, "one recoverable per-file error");

        let warnings = pr.drain_warnings();
        assert_eq!(warnings.len(), 1, "one warning queued for the failed open");
        assert_eq!(
            warnings[0].0,
            MessageCode::ErrorXfer,
            "output-open failure must be MSG_ERROR_XFER so the peer exits 23"
        );
        // Outside a daemon module upstream's module_id is -1 and module_dirlen
        // is 0, so full_fname() neither strips a prefix nor appends a suffix:
        // the absolute temp path stays exactly as a local or SSH run prints it
        // (util1.c:1285-1290).
        let expected_prefix = format!(
            "rsync: [receiver] mkstemp \"{}/.denied.dat.",
            readonly_dir.display()
        );
        assert!(
            warnings[0].1.starts_with(&expected_prefix)
                && warnings[0].1.ends_with("\" failed: Permission denied (13)"),
            "message should mirror upstream receiver.c:452-453 \"mkstemp %s failed\" \
             with the absolute temp name: {}",
            warnings[0].1
        );
        assert!(
            !warnings[0].1.contains(" (in "),
            "non-daemon run must not carry a module suffix: {}",
            warnings[0].1
        );

        let _ = std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o755));
        drop(pr);
    }

    /// Drives one denied `mkstemp` through the disk thread and returns the
    /// single queued warning, with the module context the daemon would supply.
    #[cfg(unix)]
    fn denied_mkstemp_warning(
        readonly_dir: &std::path::Path,
        daemon_module_root: Option<PathBuf>,
        dest_dir: Option<PathBuf>,
    ) -> String {
        let file_path = readonly_dir.join("denied.dat");
        let mut pr = PipelinedReceiver::new(DiskCommitConfig {
            daemon_module: Some("mymod".to_string()),
            daemon_module_root,
            dest_dir,
            ..DiskCommitConfig::default()
        })
        .unwrap();

        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 9,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"test data".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();
        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        let (_bytes, _errors) = pr.drain_all_results().unwrap();
        let mut warnings = pr.drain_warnings();
        assert_eq!(warnings.len(), 1);
        drop(pr);
        warnings.remove(0).1
    }

    /// Serving a daemon module, upstream's `full_fname()` appends
    /// ` (in MODULE)` after the closing quote and renders the path relative to
    /// the module root, so the receiver's `mkstemp` failure names the module
    /// the client asked for and never the server's real filesystem layout.
    /// It also names the *temp* file, not the destination it would have been
    /// renamed to.
    ///
    /// Ground truth, rsync 3.4.4 daemon, module `romod` at `/tmp/modrel/romod`,
    /// `rsync -r src/new.txt rsync://h:p/romod/`:
    ///
    /// ```text
    /// rsync: [receiver] mkstemp ".new.txt.a2dBeL" (in romod) failed: Permission denied (13)
    /// ```
    ///
    /// upstream: util1.c:1285-1290 inside `full_fname()`, reached from
    /// receiver.c:452-453 `rsyserr(..., "mkstemp %s failed", full_fname(fnametmp))`.
    #[cfg(unix)]
    #[test]
    fn output_open_failure_names_daemon_module() {
        use std::os::unix::fs::PermissionsExt;

        if std::env::var("USER").is_ok_and(|u| u == "root") {
            return;
        }

        let dir = test_support::create_tempdir();
        let readonly_dir = dir.path().join("readonly");
        std::fs::create_dir(&readonly_dir).unwrap();
        std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o555)).unwrap();

        // The client pushed into the module root itself: upstream's curr_dir
        // equals module_dir, so p1 is empty and the temp name is bare.
        let at_root = denied_mkstemp_warning(
            &readonly_dir,
            Some(readonly_dir.clone()),
            Some(readonly_dir.clone()),
        );
        assert!(
            at_root.starts_with("rsync: [receiver] mkstemp \".denied.dat.")
                && at_root.ends_with("\" (in mymod) failed: Permission denied (13)"),
            "module-root push must name the bare temp file: {at_root}"
        );

        // The client pushed into `readonly/` under the module root: upstream's
        // p1 is `/readonly` and p2 collapses to `/`, so the render is anchored
        // at the module root with a leading slash.
        let in_subdir = denied_mkstemp_warning(
            &readonly_dir,
            Some(dir.path().to_path_buf()),
            Some(readonly_dir.clone()),
        );
        assert!(
            in_subdir.starts_with("rsync: [receiver] mkstemp \"/readonly/.denied.dat.")
                && in_subdir.ends_with("\" (in mymod) failed: Permission denied (13)"),
            "sub-directory push must anchor at the module root: {in_subdir}"
        );
        assert!(
            !in_subdir.contains(&dir.path().display().to_string()),
            "the server's absolute path must never reach the client: {in_subdir}"
        );

        let _ = std::fs::set_permissions(&readonly_dir, PermissionsExt::from_mode(0o755));
    }

    /// Drives one mismatching commit through `verify_checksum` and returns the
    /// queued diagnostics, so every wording and gating assertion below exercises
    /// the production branch rather than restating a format string.
    ///
    /// `verbose` seeds the thread-local `NAME` info level the way `-v` does, and
    /// `report` supplies the two session globals upstream reads alongside it.
    fn queued_verification_messages(
        verbose: u8,
        report: VerifyReport,
        redo_enabled: bool,
    ) -> Vec<(MessageCode, String)> {
        use crate::pipeline::messages::ComputedChecksum;
        use logging::VerbosityConfig;

        logging::init(VerbosityConfig::from_verbose_level(verbose));

        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default())
            .unwrap()
            .with_verify_report(report);
        pr.redo_enabled = redo_enabled;

        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;
        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            // A deep absolute destination path: if it ever reaches the message
            // the assertions below fail loudly instead of matching a substring.
            file_path: PathBuf::from("/abs/dest/root/sub/payload.bin"),
            flist_name: PathBuf::from("sub/payload.bin"),
            file_index: 0,
            is_inplace: false,
        });

        let mut computed_bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        computed_bytes[0] = 0xBB;
        let result = CommitResult {
            bytes_written: 100,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: computed_bytes,
                len: 4,
            }),
            delayed_path: None,
            backup_notice: None,
        };
        pr.verify_checksum(&result).unwrap();
        pr.drain_warnings()
    }

    /// Under `-v` the phase-1 line must be upstream's verbatim, naming the file
    /// as the file list does.
    ///
    /// The flist name is what upstream renders: its receiver has already
    /// `change_dir()`ed into the destination root (`main.c:815`), so `fname` -
    /// and `f_name(file, NULL)` in the `local_name` case - is destination
    /// relative. An absolute path here is a real divergence a user sees.
    ///
    /// upstream: receiver.c:1088-1091.
    #[test]
    fn verbose_phase1_warning_matches_upstream_verbatim() {
        let msgs = queued_verification_messages(1, VerifyReport::default(), true);
        assert_eq!(
            msgs,
            vec![(
                MessageCode::Warning,
                "WARNING: sub/payload.bin failed verification -- update discarded (will try again)."
                    .to_string()
            )]
        );
    }

    /// Without `-v`, `-i`, or a `%i` out-format, upstream stays silent about a
    /// phase-1 failure it is about to retry - but it still queues the redo.
    ///
    /// Both halves matter: suppressing the line must not suppress the retry, or
    /// the destination is left wrong without any diagnostic at all.
    ///
    /// upstream: receiver.c:1072 gates only the `rprintf`; the
    /// `send_msg_int(MSG_REDO, ndx)` at receiver.c:1093-1096 sits outside it.
    #[test]
    fn default_verbosity_suppresses_the_phase1_warning_but_not_the_redo() {
        use crate::pipeline::messages::ComputedChecksum;
        use logging::VerbosityConfig;

        logging::init(VerbosityConfig::from_verbose_level(0));
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        let mut expected = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        expected[0] = 0xAA;
        pr.expected_checksums.push_back(PendingChecksum {
            expected,
            len: 4,
            file_path: PathBuf::from("/abs/dest/root/payload.bin"),
            flist_name: PathBuf::from("payload.bin"),
            file_index: 9,
            is_inplace: false,
        });
        let mut computed_bytes = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        computed_bytes[0] = 0xBB;
        pr.verify_checksum(&CommitResult {
            bytes_written: 100,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: Some(ComputedChecksum {
                bytes: computed_bytes,
                len: 4,
            }),
            delayed_path: None,
            backup_notice: None,
        })
        .unwrap();

        assert!(
            pr.drain_warnings().is_empty(),
            "-a alone must print nothing"
        );
        assert_eq!(
            pr.take_redo_indices(),
            vec![9],
            "the retry must survive the suppressed diagnostic"
        );
    }

    /// `-i` (or any `--out-format` carrying `%i`) satisfies the gate on its own,
    /// with no `-v`: upstream ORs `stdout_format_has_i` with `INFO_GTE(NAME, 1)`.
    ///
    /// upstream: receiver.c:1072.
    #[test]
    fn out_format_i_alone_reports_the_phase1_warning() {
        let report = VerifyReport {
            out_format_forwards_i: true,
            read_batch: false,
        };
        let msgs = queued_verification_messages(0, report, true);
        assert_eq!(msgs.len(), 1, "%i alone must satisfy the gate: {msgs:?}");
        assert_eq!(msgs[0].0, MessageCode::Warning);
    }

    /// A `--read-batch` replay says "may try again": the recorded stream is not
    /// guaranteed to carry the redo the way a live sender does.
    ///
    /// upstream: receiver.c:1347 - `redostr = read_batch ? " (may try again)"
    /// : " (will try again)"`.
    #[test]
    fn read_batch_replay_says_may_try_again() {
        let report = VerifyReport {
            out_format_forwards_i: false,
            read_batch: true,
        };
        let msgs = queued_verification_messages(1, report, true);
        assert_eq!(
            msgs,
            vec![(
                MessageCode::Warning,
                "WARNING: sub/payload.bin failed verification -- update discarded (may try again)."
                    .to_string()
            )]
        );
    }

    /// The phase-2 form is a `FERROR_XFER`, which short-circuits the emit gate,
    /// so it prints at default verbosity and carries no retry suffix.
    ///
    /// upstream: receiver.c:1334-1335,1083-1084 - `msgtype == FERROR_XFER` is the
    /// first disjunct, and `redostr = ""` on that branch.
    #[test]
    fn phase2_error_is_ungated_and_has_no_retry_suffix() {
        let msgs = queued_verification_messages(0, VerifyReport::default(), false);
        assert_eq!(
            msgs,
            vec![(
                MessageCode::ErrorXfer,
                "ERROR: sub/payload.bin failed verification -- update discarded.".to_string()
            )]
        );
    }

    /// Each [`PartialMode`] must project onto the three upstream variables
    /// `receiver.c:1074-1076` reads. The `keptstr` chain those variables drive
    /// is pinned by the table in `logging::verify_failure`; what this test owns
    /// is the projection, the only part of the rule that is oc-specific.
    ///
    /// The end-to-end wordings below (`queued_verification_messages`) then
    /// confirm the projection and the shared rule agree on the live path.
    #[test]
    fn partial_state_projects_onto_the_upstream_variables() {
        assert_eq!(
            partial_state(&PartialMode::None),
            (false, false, false),
            "no retention: keep_partial and partialptr are both unset"
        );
        assert_eq!(
            partial_state(&PartialMode::Partial),
            (true, true, false),
            "--partial retains at the destination name, with no partial-dir"
        );
        assert_eq!(
            partial_state(&PartialMode::PartialDir(PathBuf::from("/pd"))),
            (true, true, true),
            "--partial-dir also sets partial_dir, which selects its own keptstr"
        );
    }

    /// The projection and the shared rule together must still produce every
    /// upstream `keptstr`, reached through this receiver's own types.
    ///
    /// upstream: receiver.c:1073-1079.
    #[test]
    fn every_kept_str_is_reachable_through_partial_mode() {
        for (partial_mode, is_inplace, expected) in [
            (PartialMode::None, false, "discarded"),
            (PartialMode::None, true, "retained"),
            (PartialMode::Partial, false, "retained"),
            (
                PartialMode::PartialDir(PathBuf::from("/pd")),
                false,
                "put into partial-dir",
            ),
        ] {
            let (_, message) = verification_failure_report(
                std::path::Path::new("sub/payload.bin"),
                &partial_mode,
                is_inplace,
                true,
                VerifyReport::default(),
            )
            .expect("the FERROR_XFER form is unconditional");
            assert_eq!(
                message,
                format!("ERROR: sub/payload.bin failed verification -- update {expected}.")
            );
        }
    }

    /// Verifies `collect_delayed_update` captures the staging path from
    /// `CommitResult::delayed_path` paired with the final destination from
    /// the pending checksum queue.
    #[test]
    fn collect_delayed_update_tracks_staging_and_final_path() {
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();

        // Simulate a pending checksum entry (which carries the final path).
        pr.expected_checksums.push_back(PendingChecksum {
            expected: [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            len: 0,
            file_path: PathBuf::from("/dest/file.txt"),
            flist_name: PathBuf::from("file.txt"),
            file_index: 0,
            is_inplace: false,
        });

        let result = CommitResult {
            bytes_written: 42,
            file_entry_index: 0,
            metadata_error: None,
            computed_checksum: None,
            delayed_path: Some(PathBuf::from("/dest/.~tmp~/file.txt")),
            backup_notice: None,
        };

        pr.collect_delayed_update(&result);

        let updates = pr.take_delayed_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, PathBuf::from("/dest/.~tmp~/file.txt"));
        assert_eq!(updates[0].1, PathBuf::from("/dest/file.txt"));

        drop(pr);
    }

    /// Verifies that `take_delayed_updates` returns empty when no files
    /// were staged.
    #[test]
    fn take_delayed_updates_empty_when_no_delays() {
        let mut pr = PipelinedReceiver::new(DiskCommitConfig::default()).unwrap();
        assert!(pr.take_delayed_updates().is_empty());
        drop(pr);
    }

    /// Verifies that when `PipelinedReceiver` is dropped (simulating an
    /// interrupted transfer), staged files remain in `.~tmp~/` and are not
    /// renamed. The caller never calls `handle_delayed_updates()`, so files
    /// persist as valid partials for resume.
    ///
    /// upstream: receiver.c:694-695 - handle_delayed_updates() only after
    /// successful transfer; interruption leaves staged files intact.
    #[test]
    fn delay_updates_drop_without_sweep_preserves_staged_files() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("drop_staged.dat");

        let mut pr = PipelinedReceiver::new(delay_updates_config()).unwrap();

        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 11,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"drop staged".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();

        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        let (bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(bytes, 11);
        assert!(errors.is_empty());

        // Collect delayed updates but do NOT call handle_delayed_updates().
        let updates = pr.take_delayed_updates();
        assert_eq!(updates.len(), 1);

        // Drop the mediator (simulating interrupted transfer).
        drop(pr);

        // File must remain in staging, not at final destination.
        assert!(
            !file_path.exists(),
            "final path must not exist when sweep is skipped"
        );
        let staging = dir.path().join(".~tmp~").join("drop_staged.dat");
        assert!(
            staging.exists(),
            "staged file must persist after mediator drop"
        );
        assert_eq!(std::fs::read(&staging).unwrap(), b"drop staged");
    }

    /// Verifies that `PipelinedReceiver::shutdown()` with delay_updates does
    /// not perform the rename sweep. The sweep is the caller's responsibility,
    /// not the mediator's. When the caller does not sweep (interrupt path),
    /// staged files persist.
    #[test]
    fn delay_updates_shutdown_without_sweep_preserves_staged_files() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("shutdown_staged.dat");

        let mut pr = PipelinedReceiver::new(delay_updates_config()).unwrap();

        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 14,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"shutdown stage".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();

        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        // Explicit shutdown (not drop) - still no sweep.
        let (bytes, errors) = pr.shutdown().unwrap();
        assert_eq!(bytes, 14);
        assert!(errors.is_empty());

        // File must remain staged.
        assert!(
            !file_path.exists(),
            "final path must not exist after shutdown without sweep"
        );
        let staging = dir.path().join(".~tmp~").join("shutdown_staged.dat");
        assert!(staging.exists(), "staged file must persist after shutdown");
        assert_eq!(std::fs::read(&staging).unwrap(), b"shutdown stage");
    }

    /// End-to-end: disk thread with `delay_updates` stages a file, and
    /// `PipelinedReceiver` captures the delayed path through the result
    /// drain.
    #[test]
    fn delay_updates_end_to_end_through_mediator() {
        let dir = test_support::create_tempdir();
        let file_path = dir.path().join("e2e.dat");

        let mut pr = PipelinedReceiver::new(delay_updates_config()).unwrap();

        pr.file_sender()
            .send(FileMessage::WholeFile {
                begin: Box::new(BeginMessage {
                    file_path: file_path.clone(),
                    target_size: 10,
                    file_entry_index: 0,
                    checksum_verifier: None,
                    is_device_target: false,
                    is_inplace: false,
                    append_offset: 0,
                    xattr_list: None,
                    xattr_basis: None,
                    file_entry: None,
                }),
                data: b"e2e staged".to_vec(),
                expected_checksum: Default::default(),
            })
            .unwrap();

        pr.note_commit_sent(
            [0u8; ChecksumVerifier::MAX_DIGEST_LEN],
            0,
            file_path.clone(),
            PathBuf::from(file_path.file_name().unwrap()),
            0,
            false,
        );

        let (bytes, errors) = pr.drain_all_results().unwrap();
        assert_eq!(bytes, 10);
        assert!(errors.is_empty());

        // Final path should NOT exist - file is staged.
        assert!(!file_path.exists());

        // Staging path should exist.
        let staging = dir.path().join(".~tmp~").join("e2e.dat");
        assert!(staging.exists());
        assert_eq!(std::fs::read(&staging).unwrap(), b"e2e staged");

        // PipelinedReceiver tracked the delayed update.
        let updates = pr.take_delayed_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].0, staging);
        assert_eq!(updates[0].1, file_path);

        drop(pr);
    }
}
