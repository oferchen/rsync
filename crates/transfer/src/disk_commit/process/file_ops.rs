//! Per-file processing for the disk commit thread.
//!
//! Drives chunked file writes, whole-file coalesced writes, output file
//! opening (device, inplace, temp+rename), and writer backend selection
//! (io_uring, IOCP, macOS `F_NOCACHE`, dontcache, vmsplice, buffered).

use std::fs;
use std::io;

use engine::CleanupManager;

use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::pipeline::messages::{
    BeginMessage, CommitResult, ComputedChecksum, ExpectedChecksum, FileMessage,
};
use crate::pipeline::spsc;
use crate::temp_guard::TempFileGuard;
#[cfg(not(unix))]
use crate::temp_guard::open_tmpfile;
#[cfg(unix)]
use crate::temp_guard::open_tmpfile_sandboxed;

use super::super::config::DiskCommitConfig;
use super::super::writer::{ReusableBufWriter, Writer};
use super::commit::{SparseFinalize, commit_file, make_backup_copy, retain_partial_file};
use super::metadata::{apply_file_metadata, finalize_checksum};

/// Folds the existing on-disk prefix into the whole-file checksum for
/// `--append-verify` (append_mode == 2).
///
/// Reads `[0, append_offset)` from the destination and feeds it to `verifier`
/// before the appended tokens are hashed, so a corrupted prefix fails
/// verification and triggers a re-transmit. A no-op unless append-verify is
/// active with a non-zero offset and a live verifier.
///
/// # Upstream Reference
///
/// - `receiver.c:357-373` - `if (append_mode == 2 && mapbuf)` sum_update loop
///   over `sum.flength` bytes in `CHUNK_SIZE` steps.
fn sum_append_prefix(
    config: &DiskCommitConfig,
    begin: &BeginMessage,
    verifier: &mut Option<ChecksumVerifier>,
) -> io::Result<()> {
    use std::io::Read;

    if !config.append_verify || begin.append_offset == 0 {
        return Ok(());
    }
    let Some(verifier) = verifier.as_mut() else {
        return Ok(());
    };

    // Append implies inplace (receiver.c:855), so the prefix lives at the final
    // destination and is untouched until the appended tail is written.
    let mut file = fs::File::open(&begin.file_path)?;
    let mut remaining = begin.append_offset;
    let mut buf = vec![0u8; 256 * 1024];
    while remaining > 0 {
        let to_read = buf.len().min(remaining as usize);
        file.read_exact(&mut buf[..to_read])?;
        verifier.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }
    Ok(())
}

/// Finalizes the per-file checksum and compares it against the sender's
/// trailing whole-file sum.
///
/// Returns the computed digest (so the receiver's redo bookkeeping still runs)
/// and whether verification passed. A missing verifier or a zero-length
/// expected sum means no checksum was supplied, so it always passes.
///
/// # Upstream Reference
///
/// - `receiver.c:505` - `sum_end(file_sum1)` computes the whole-file digest.
/// - `receiver.c:515-519` - `read_buf(f_in, sender_file_sum, ...)` then
///   `if (fd != -1 && memcmp(file_sum1, sender_file_sum, xfer_sum_len) != 0)
///   return 0;` - a mismatch yields `recv_ok = 0` and the file is not put into
///   place.
fn verify_whole_file_checksum(
    verifier: Option<ChecksumVerifier>,
    expected: &ExpectedChecksum,
) -> (Option<ComputedChecksum>, bool) {
    let computed = finalize_checksum(verifier);
    let verify_ok = match computed {
        Some(ref c) if expected.len > 0 => {
            c.len == expected.len && c.bytes[..c.len] == expected.bytes[..expected.len]
        }
        _ => true,
    };
    (computed, verify_ok)
}

/// Handles a whole-file checksum verification failure for a temp+rename file.
///
/// The temp file is retained in the partial dir (when `--partial`/`--partial-dir`
/// is active) or discarded via the guard's `Drop`; it is never renamed over the
/// destination. The computed digest is still reported so the receiver queues a
/// redo (phase 1) or logs the error (phase 2).
///
/// # Upstream Reference
///
/// - `receiver.c:1039-1056` - on `recv_ok == 0` the temp goes to the partial dir
///   (`handle_partial_dir(PDIR_CREATE)`) or is unlinked (`do_unlink_at`);
///   `finish_transfer()` - the destination rename - is skipped.
fn withhold_failed_commit(
    config: &DiskCommitConfig,
    mut cleanup_guard: TempFileGuard,
    begin: &BeginMessage,
    bytes_written: u64,
    computed_checksum: Option<ComputedChecksum>,
) -> CommitResult {
    retain_partial_file(config, &mut cleanup_guard, &begin.file_path);
    drop(cleanup_guard);
    CommitResult {
        bytes_written,
        file_entry_index: begin.file_entry_index,
        metadata_error: None,
        computed_checksum,
        delayed_path: None,
        backup_notice: None,
    }
}

/// Processes a single file: open, write chunks, commit or abort.
///
/// After writing each chunk, the owned `Vec<u8>` is returned through
/// `buf_return_tx` for reuse by the network thread.
///
/// When `disk_batch` is `Some` (Linux/io_uring) or `iocp_batch` is `Some`
/// (Windows/IOCP) and sparse mode is disabled, writes are submitted via the
/// shared batched writer. Sparse mode requires `Seek`, which neither batch
/// writer provides, so it always falls back to buffered writes. Only one of
/// the two batched writers can be active at a time.
pub(in crate::disk_commit) fn process_file(
    file_rx: &spsc::Receiver<FileMessage>,
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    mut begin: BeginMessage,
    write_buf: &mut Vec<u8>,
    disk_batch: Option<&mut fast_io::IoUringDiskBatch>,
    iocp_batch: Option<&mut fast_io::IocpDiskBatch>,
) -> io::Result<CommitResult> {
    // upstream: receiver.c:999-1006 - when open_tmpfile() fails (e.g. EACCES
    // from a read-only destination directory) the receiver does NOT abort the
    // receive loop. It logs the error, calls discard_receive_data() to drain
    // this file's delta, and continues to the next file. On the pipelined path
    // the network thread has already lifted this file's entire delta off the
    // wire into channel messages, so the wire itself cannot desync here - but
    // the leftover Chunk/Commit messages for this file are still queued.
    // Returning the open error immediately would leave those messages in the
    // channel; the disk loop would then read the next Chunk and mis-parse it as
    // a "message without Begin", corrupting the result stream. We instead drain
    // this file's queued messages (the channel analog of discard_receive_data)
    // and then surface the original open error. `drain_all_results` maps a
    // permission failure to a per-file partial (IOERR_GENERAL -> RERR_PARTIAL,
    // exit 23) with the upstream sender warning, exactly like the synchronous
    // receiver (receiver/transfer/sync.rs via delta_apply::discard_delta_stream).
    // upstream: generator.c:1862,1898 make_backup() inplace copy path - under
    // --inplace --backup the destination inode is rewritten in place, so the
    // backup must be a COPY of the pre-transfer contents taken BEFORE the first
    // write (a rename would move the very inode we are about to update). The
    // temp+rename path instead backs up at commit time (see commit_file).
    let inplace_backup_notice = make_inplace_backup(&begin, config)?;

    let (file, mut cleanup_guard, needs_rename) = match open_output_file(&begin, config) {
        Ok(triple) => triple,
        Err(open_err) => {
            return discard_file_on_open_failure(file_rx, buf_return_tx, open_err);
        }
    };
    // Register the temp file with the global cleanup manager so a SIGKILL
    // (which bypasses Drop) still removes orphaned temp files on restart.
    // Only temp+rename paths produce actual temp files; inplace and device
    // writes operate on the final destination. The guard records its
    // registration so its Drop unregisters the path on both success and error,
    // preventing a per-errored-file PathBuf leak into the global set.
    if needs_rename {
        CleanupManager::global().register_temp_file(cleanup_guard.path().to_path_buf());
        cleanup_guard.mark_registered();
    }

    // Pre-existing basis length for sparse hole-punching: only in-place writes
    // reuse existing bytes, so a zero run there must be punched rather than
    // merely seeked over. upstream: receiver.c:318-338 preallocated_len.
    let basis_len = if config.use_sparse && begin.is_inplace {
        file.metadata().map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    // upstream: receiver.c:319-336 - when --preallocate is set, fallocate the
    // destination to its eventual length before writing. do_fallocate()'s return
    // becomes preallocated_len, overriding the inplace basis for sparse hole
    // decisions; a failure warns and continues (never aborts).
    let preallocated_len = maybe_preallocate(&file, config, &begin, basis_len);

    let mut output = make_writer(
        file,
        write_buf,
        disk_batch,
        iocp_batch,
        config.use_sparse,
        begin.append_offset,
        begin.is_inplace,
        begin.target_size,
    )?;

    let mut sparse_state = if config.use_sparse {
        let mut state = SparseWriteState::default();
        state.set_preallocated_len(preallocated_len);
        Some(state)
    } else {
        None
    };

    // Per-file checksum verifier, moved from the network thread.
    // Computing the checksum here overlaps hashing with disk I/O and
    // removes ~42% of instructions from the network-critical path.
    let mut checksum_verifier = begin.checksum_verifier.take();

    // upstream: receiver.c:357-373 - fold the existing prefix into the
    // whole-file checksum under --append-verify before hashing the tail.
    sum_append_prefix(config, &begin, &mut checksum_verifier)?;

    let mut bytes_written: u64 = 0;

    loop {
        let msg = match file_rx.recv() {
            Ok(m) => m,
            Err(_) => {
                // Channel disconnected - treat as an interrupt.
                // Flush buffered data and commit any batched writes (io_uring/
                // IOCP) before considering partial retention. flush_and_sync
                // handles Buffered/Macos; finish handles IoUring/Iocp.
                let _ = output.flush_and_sync(false, &begin.file_path);
                // finish() takes `self`, closing the file handle before
                // rename+mtime stamp (Windows resets mtime on handle close).
                let _ = output.finish(false, &begin.file_path);
                // upstream: cleanup.c - retain partial on unexpected disconnect
                if bytes_written > 0 && needs_rename {
                    retain_partial_file(config, &mut cleanup_guard, &begin.file_path);
                }
                drop(cleanup_guard);
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "disk thread: channel disconnected while processing file",
                ));
            }
        };

        match msg {
            FileMessage::Chunk(data) => {
                // Update per-file checksum before writing (mirrors upstream
                // receiver.c:315 which hashes each token before writing).
                if let Some(ref mut verifier) = checksum_verifier {
                    verifier.update(&data);
                }

                if let Some(ref mut sparse) = sparse_state {
                    sparse.write(output.buffered_for_sparse(), &data)?;
                } else {
                    output.write_chunk(&data)?;
                }
                bytes_written += data.len() as u64;
                // Return the buffer for reuse. Ignore errors - the network
                // thread may have moved on (e.g. after an error).
                let _ = buf_return_tx.try_send(data);
            }
            FileMessage::SkipMatched(data) => {
                // In-place matched block already at its destination offset.
                // upstream: receiver.c:461-465 hashes the matched bytes via
                // sum_update BEFORE skip_matched(), so fold them into the
                // per-file checksum here regardless of whether we write them.
                if let Some(ref mut verifier) = checksum_verifier {
                    verifier.update(&data);
                }
                if let Some(ref mut sparse) = sparse_state {
                    // upstream: fileio.c:196-200 skip_matched() sparse branch
                    // hands the bytes to the sparse processor (with the seek
                    // flag). Writing them through SparseWriteState is
                    // byte-identical for an in-place basis==dest update.
                    sparse.write(output.buffered_for_sparse(), &data)?;
                } else {
                    // upstream: fileio.c:202-209 skip_matched() - flush then
                    // lseek past the already-in-place bytes instead of
                    // rewriting identical data.
                    output.skip_matched(data.len() as u64)?;
                }
                bytes_written += data.len() as u64;
                let _ = buf_return_tx.try_send(data);
            }
            FileMessage::Commit { expected_checksum } => {
                // upstream: fileio.c:43 sparse_end() - flush the trailing hole
                // and hand the logical length + in-basis hole ranges to the
                // commit step for ftruncate + punch (no materialized byte).
                let sparse_final = if let Some(ref mut sparse) = sparse_state {
                    let logical = sparse.finish(output.buffered_for_sparse())?;
                    Some(SparseFinalize {
                        logical_len: logical,
                        holes: sparse.take_holes(),
                    })
                } else {
                    None
                };

                output.flush_and_sync(config.do_fsync, &begin.file_path)?;
                output.finish(config.do_fsync, &begin.file_path)?;

                // upstream: receiver.c:505-519 - compute the whole-file checksum
                // and compare it against the sender's trailing sum BEFORE the
                // file is put into place. On a temp+rename mismatch (recv_ok == 0)
                // the temp is retained/discarded, never renamed over the
                // destination. Inplace/device targets cannot be withheld (the
                // bytes already landed), matching upstream's `|| inplace` branch
                // at receiver.c:1029; the receiver still queues the redo.
                let (computed_checksum, verify_ok) =
                    verify_whole_file_checksum(checksum_verifier.take(), &expected_checksum);
                if !verify_ok && needs_rename {
                    return Ok(withhold_failed_commit(
                        config,
                        cleanup_guard,
                        &begin,
                        bytes_written,
                        computed_checksum,
                    ));
                }

                // upstream: rsync.c:748 finish_transfer() - "Change
                // permissions before putting the file into place."
                // Apply metadata to the temp file before rename so the
                // file is already correct when it appears at the final
                // path. For inplace/device, metadata is applied after.
                let pre_meta_error = if needs_rename {
                    apply_file_metadata(cleanup_guard.path(), &begin, config)
                } else {
                    None
                };

                let outcome = commit_file(
                    &begin,
                    config,
                    &mut cleanup_guard,
                    needs_rename,
                    bytes_written,
                    sparse_final,
                )?;

                // Temp file has been renamed to its final destination (or
                // kept via guard.keep()), so remove it from the global
                // cleanup registry - it is no longer an orphan candidate.
                if needs_rename && outcome.delayed_path.is_none() {
                    CleanupManager::global().unregister_temp_file(cleanup_guard.path());
                }

                // After commit: apply metadata for inplace/device paths
                // (no pre-rename step), or re-apply after cross-device
                // copy since fs::copy does not preserve ownership,
                // timestamps, ACLs, or xattrs.
                // When delay_updates staged the file, apply metadata to
                // the staging path (it will be renamed to final later).
                let metadata_error = if outcome.delayed_path.is_some() {
                    // upstream: receiver.c:924 - finish_transfer(partialptr, ...)
                    // applies metadata to the staged partial file.
                    let staged = outcome.delayed_path.as_ref().unwrap();
                    apply_file_metadata(staged, &begin, config)
                } else if needs_rename && !outcome.was_copy {
                    pre_meta_error
                } else {
                    apply_file_metadata(&begin.file_path, &begin, config)
                };

                return Ok(CommitResult {
                    bytes_written,
                    file_entry_index: begin.file_entry_index,
                    metadata_error,
                    computed_checksum,
                    delayed_path: outcome.delayed_path,
                    // Inplace copy-backup (taken before the write) or the
                    // temp+rename backup (taken at commit); never both.
                    backup_notice: outcome.backup_notice.or(inplace_backup_notice),
                });
            }
            FileMessage::Abort { reason } => {
                // Flush buffered data and commit any batched writes (io_uring/
                // IOCP) so the temp file contains all received data.
                let _ = output.flush_and_sync(false, &begin.file_path);
                // finish() takes `self`, closing the file handle before
                // rename+mtime stamp (Windows resets mtime on handle close).
                let _ = output.finish(false, &begin.file_path);
                // upstream: cleanup.c:105-115 - on abort, retain temp file
                // if partial mode is enabled and literal data was received.
                // bytes_written > 0 is a proxy for upstream's got_literal:
                // if any data was written, the transfer made progress worth
                // retaining for later resume.
                if bytes_written > 0 && needs_rename {
                    retain_partial_file(config, &mut cleanup_guard, &begin.file_path);
                }
                drop(cleanup_guard);
                return Err(io::Error::other(reason));
            }
            FileMessage::Shutdown => {
                // Flush buffered data and commit any batched writes (io_uring/
                // IOCP) before considering partial retention.
                let _ = output.flush_and_sync(false, &begin.file_path);
                // finish() takes `self`, closing the file handle before
                // rename+mtime stamp (Windows resets mtime on handle close).
                let _ = output.finish(false, &begin.file_path);
                // upstream: cleanup.c - same partial retention on shutdown
                if bytes_written > 0 && needs_rename {
                    retain_partial_file(config, &mut cleanup_guard, &begin.file_path);
                }
                drop(cleanup_guard);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "disk thread: shutdown received while processing file",
                ));
            }
            FileMessage::Begin(_) | FileMessage::WholeFile { .. } => {
                drop(output);
                drop(cleanup_guard);
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "disk thread: received Begin while processing another file",
                ));
            }
        }
    }
}

/// Processes a single-chunk file in one shot (coalesced Begin+Chunk+Commit).
///
/// Avoids the per-message channel recv loop of [`process_file`], reducing
/// futex overhead from 3+ sends/recvs to 1 for small files. When
/// `disk_batch` (io_uring) or `iocp_batch` (IOCP) is `Some` and sparse mode
/// is disabled, the chunk is submitted via the shared batched writer.
pub(in crate::disk_commit) fn process_whole_file(
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    mut begin: BeginMessage,
    data: Vec<u8>,
    expected_checksum: ExpectedChecksum,
    write_buf: &mut Vec<u8>,
    disk_batch: Option<&mut fast_io::IoUringDiskBatch>,
    iocp_batch: Option<&mut fast_io::IocpDiskBatch>,
) -> io::Result<CommitResult> {
    // upstream: receiver.c:999-1006 - open failure is a benign per-file partial,
    // not a fatal abort. The coalesced WholeFile carries its data inline, so
    // there are no queued channel messages to drain (unlike process_file); the
    // open error surfaces directly and drain_all_results maps a permission
    // failure to RERR_PARTIAL (exit 23).
    // upstream: generator.c:1862,1898 - copy the pre-transfer contents aside
    // before rewriting the destination in place (see process_file).
    let inplace_backup_notice = make_inplace_backup(&begin, config)?;

    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin, config)?;
    if needs_rename {
        CleanupManager::global().register_temp_file(cleanup_guard.path().to_path_buf());
        cleanup_guard.mark_registered();
    }

    // Pre-existing basis length for sparse hole-punching (see process_file).
    let basis_len = if config.use_sparse && begin.is_inplace {
        file.metadata().map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    // upstream: receiver.c:319-336 - preallocate the destination before writing
    // when --preallocate is set (see process_file).
    let preallocated_len = maybe_preallocate(&file, config, &begin, basis_len);

    let mut output = make_writer(
        file,
        write_buf,
        disk_batch,
        iocp_batch,
        config.use_sparse,
        begin.append_offset,
        begin.is_inplace,
        begin.target_size,
    )?;
    let bytes_written = data.len() as u64;

    let mut checksum_verifier = begin.checksum_verifier.take();
    // upstream: receiver.c:357-373 - fold the existing prefix into the
    // whole-file checksum under --append-verify before hashing the tail.
    sum_append_prefix(config, &begin, &mut checksum_verifier)?;
    if let Some(ref mut verifier) = checksum_verifier {
        verifier.update(&data);
    }

    let sparse_final = if config.use_sparse {
        let mut sparse = SparseWriteState::default();
        sparse.set_preallocated_len(preallocated_len);
        sparse.write(output.buffered_for_sparse(), &data)?;
        let logical = sparse.finish(output.buffered_for_sparse())?;
        Some(SparseFinalize {
            logical_len: logical,
            holes: sparse.take_holes(),
        })
    } else {
        output.write_chunk(&data)?;
        None
    };

    let _ = buf_return_tx.try_send(data);

    output.flush_and_sync(config.do_fsync, &begin.file_path)?;
    output.finish(config.do_fsync, &begin.file_path)?;

    // upstream: receiver.c:505-519 - verify the whole-file checksum before the
    // file is put into place (see process_file for the full rationale). A
    // temp+rename mismatch is retained/discarded, never renamed over dest.
    let (computed_checksum, verify_ok) =
        verify_whole_file_checksum(checksum_verifier.take(), &expected_checksum);
    if !verify_ok && needs_rename {
        return Ok(withhold_failed_commit(
            config,
            cleanup_guard,
            &begin,
            bytes_written,
            computed_checksum,
        ));
    }

    // upstream: rsync.c:748 finish_transfer() - apply metadata to the
    // temp file before rename (see process_file for full rationale).
    let pre_meta_error = if needs_rename {
        apply_file_metadata(cleanup_guard.path(), &begin, config)
    } else {
        None
    };

    let outcome = commit_file(
        &begin,
        config,
        &mut cleanup_guard,
        needs_rename,
        bytes_written,
        sparse_final,
    )?;

    if needs_rename && outcome.delayed_path.is_none() {
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
    }

    let metadata_error = if outcome.delayed_path.is_some() {
        let staged = outcome.delayed_path.as_ref().unwrap();
        apply_file_metadata(staged, &begin, config)
    } else if needs_rename && !outcome.was_copy {
        pre_meta_error
    } else {
        apply_file_metadata(&begin.file_path, &begin, config)
    };

    Ok(CommitResult {
        bytes_written,
        file_entry_index: begin.file_entry_index,
        metadata_error,
        computed_checksum,
        delayed_path: outcome.delayed_path,
        // Inplace copy-backup (before the write) or temp+rename backup (at
        // commit); never both.
        backup_notice: outcome.backup_notice.or(inplace_backup_notice),
    })
}

/// Drains a chunked file's queued channel messages after its output could not
/// be opened, then surfaces the original open error.
///
/// The network thread has already lifted this file's entire delta off the wire
/// and enqueued it as `Chunk` messages terminated by `Commit`. With no open
/// output the disk thread cannot write them, so it recycles each chunk buffer
/// and drops the bytes until the terminating message - the channel analog of
/// upstream `discard_receive_data()`. This keeps the disk loop from reading the
/// next queued `Chunk` and mis-parsing it as a "message without Begin".
///
/// Reaching `Commit` yields the original `open_err`, which the main-thread
/// [`crate::pipeline::receiver::PipelinedReceiver`] maps to a per-file partial
/// (permission failure -> IOERR_GENERAL -> RERR_PARTIAL, exit 23) with the
/// upstream sender warning, matching the synchronous receiver
/// (`receiver/transfer/sync.rs` via `delta_apply::discard_delta_stream`). If the
/// channel instead delivers `Shutdown`/`Abort`/disconnect first, that terminal
/// signal is propagated so the disk loop exits, exactly as the write path does.
///
/// upstream: receiver.c:999-1006 - discard this file's data and continue.
fn discard_file_on_open_failure(
    file_rx: &spsc::Receiver<FileMessage>,
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    open_err: io::Error,
) -> io::Result<CommitResult> {
    loop {
        match file_rx.recv() {
            Ok(FileMessage::Chunk(data) | FileMessage::SkipMatched(data)) => {
                // Recycle the buffer for the network thread; drop the bytes.
                let _ = buf_return_tx.try_send(data);
            }
            Ok(FileMessage::Commit { .. }) => {
                return Err(open_err);
            }
            Ok(FileMessage::Abort { reason }) => {
                return Err(io::Error::other(reason));
            }
            Ok(FileMessage::Shutdown) => {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "disk thread: shutdown received while discarding file",
                ));
            }
            Ok(FileMessage::Begin(_) | FileMessage::WholeFile { .. }) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "disk thread: received Begin while discarding another file",
                ));
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "disk thread: channel disconnected while discarding file",
                ));
            }
        }
    }
}

/// Makes the `--inplace --backup` pre-image copy when required.
///
/// Under `--inplace` the destination is rewritten in place (same inode), so the
/// backup cannot be a rename of the destination - it must be a COPY of the
/// pre-transfer contents taken before the first write. Upstream does this in the
/// generator (`generator.c:1862,1898` `copy_file(fname, backupptr, ...)`) while
/// keeping `fnamecmp_type == FNAMECMP_FNAME`. Because oc refuses
/// `--inplace --partial-dir` (config validation), an inplace basis is always the
/// destination itself, so `begin.is_inplace` here matches upstream's
/// `inplace && fnamecmp_type == FNAMECMP_FNAME` condition exactly.
///
/// Returns `Ok(None)` (no backup) unless the target is inplace, backup is
/// configured, and `--delay-updates` is off (which stages its own backup during
/// the sweep). `make_backup_copy` further no-ops when the destination is absent.
fn make_inplace_backup(
    begin: &BeginMessage,
    config: &DiskCommitConfig,
) -> io::Result<Option<crate::pipeline::messages::BackupNotice>> {
    if !begin.is_inplace || config.delay_updates {
        return Ok(None);
    }
    let Some(ref backup_config) = config.backup else {
        return Ok(None);
    };
    make_backup_copy(&begin.file_path, backup_config, config)
}

/// Opens the output file using device write, inplace, or temp+rename strategy.
///
/// # Device targets
///
/// When `begin.is_device_target` is set, the device file is opened with `O_WRONLY`
/// (no create, no truncate). Device files cannot use temp+rename since you cannot
/// rename onto a device node.
///
/// # Inplace mode
///
/// When `begin.is_inplace` is set, the destination file is opened directly for
/// writing (created if absent). No temp file or rename.
///
/// # Upstream Reference
///
/// - `receiver.c`: `write_devices && IS_DEVICE(st.st_mode)` - inplace write to device
/// - `receiver.c:855-860`: opens destination directly when inplace
fn open_output_file(
    begin: &BeginMessage,
    config: &DiskCommitConfig,
) -> io::Result<(fs::File, TempFileGuard, bool)> {
    if begin.is_device_target {
        let file = fs::OpenOptions::new().write(true).open(&begin.file_path)?;
        // The guard wraps the real device node, not a temp file. A
        // mid-transfer error must NOT unlink it (upstream never unlinks an
        // inplace/device target - receiver.c:1054 gates on !one_inplace), so
        // seed it keep-on-drop.
        Ok((
            file,
            TempFileGuard::keep_dest(begin.file_path.clone()),
            false,
        ))
    } else if begin.is_inplace {
        // upstream: receiver.c:855 - do_open(fname, O_WRONLY|O_CREAT, 0600)
        let opened = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&begin.file_path);
        // upstream: receiver.c:978-986 - under the fs.protected_regular sysctl
        // the kernel returns EACCES for an O_CREAT open of an existing file we
        // do not own in a sticky, world-writable dir. The inplace target
        // already exists, so retry without O_CREAT.
        #[cfg(target_os = "linux")]
        let opened = match opened {
            Err(error) if error.raw_os_error() == Some(libc::EACCES) => fs::OpenOptions::new()
                .write(true)
                .truncate(false)
                .open(&begin.file_path),
            other => other,
        };
        let mut file = opened?;
        // upstream: receiver.c:307-308 - in append mode, seek past existing content
        if begin.append_offset > 0 {
            use std::io::Seek;
            file.seek(io::SeekFrom::Start(begin.append_offset))?;
        }
        // The guard wraps the real destination file, not a temp file. On a
        // mid-transfer error the guard must LEAVE the partial write in place
        // rather than delete the user's existing file. upstream: receiver.c:1054
        // gates the destination unlink on !one_inplace, so an inplace target is
        // never unlinked; a partial inplace write stays. keep_dest() seeds the
        // guard keep-on-drop so its Drop is a no-op on the error path.
        Ok((
            file,
            TempFileGuard::keep_dest(begin.file_path.clone()),
            false,
        ))
    } else {
        // SEC-1.r: when the dest_dir + sandbox carrier is plumbed, route the
        // temp-file create through `openat(dirfd, leaf, O_WRONLY|O_CREAT|
        // O_EXCL|O_NOFOLLOW, 0o600)` and seed the guard with a sandbox anchor
        // so the Drop unlink uses the same dirfd. The carrier only engages
        // for the default in-destination temp pattern (single component under
        // dest_dir); --temp-dir routing keeps the path-based fallback.
        #[cfg(unix)]
        let (file, guard) = open_tmpfile_sandboxed(
            &begin.file_path,
            config.temp_dir.as_deref(),
            config.sandbox.as_ref(),
            config.dest_dir.as_deref(),
        )?;
        #[cfg(not(unix))]
        let (file, guard) = open_tmpfile(&begin.file_path, config.temp_dir.as_deref())?;
        Ok((file, guard, true))
    }
}

/// Preallocates the destination file to its eventual length under
/// `config.preallocate`, returning the `preallocated_len` the sparse writer
/// must use (bytes reserved that a zero run should punch rather than seek).
///
/// Mirrors upstream `receiver.c:319-336`: the preallocation branch gates on
/// `preallocate_files && total_size > 0 && (!inplace_sizing || total_size >
/// size_r)`, and its `do_fallocate()` return value overrides the inplace basis
/// length. A `do_fallocate()` failure warns and continues (`receiver.c:324`), so
/// this never propagates an error - preallocation is a best-effort optimization
/// and its failure must not abort the receive.
// upstream: receiver.c:320 - preallocated_len = do_fallocate(fd, 0, total_size)
fn maybe_preallocate(
    file: &fs::File,
    config: &DiskCommitConfig,
    begin: &BeginMessage,
    fallback_preallocated_len: u64,
) -> u64 {
    if !config.preallocate {
        return fallback_preallocated_len;
    }
    // upstream: receiver.c:320 - size_r is the existing basis length; only an
    // in-place write reuses it, otherwise the temp file starts empty.
    let existing_len = if begin.is_inplace {
        file.metadata().map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    if begin.target_size == 0 || (begin.is_inplace && begin.target_size <= existing_len) {
        return fallback_preallocated_len;
    }
    match fast_io::preallocate(file, begin.target_size) {
        Ok(preallocated_len) => preallocated_len,
        // upstream: receiver.c:324 - rsyserr(FWARNING, ...) then continue.
        Err(err) => {
            logging::debug_log!(
                Io,
                1,
                "preallocate {}: {err}; continuing without preallocation",
                begin.file_path.display()
            );
            fallback_preallocated_len
        }
    }
}

/// Constructs the per-file [`Writer`] dispatching between batched async
/// submission (io_uring on Linux, IOCP on Windows), the macOS `F_NOCACHE` +
/// `writev` writer, and the buffered fallback.
///
/// Selects a non-buffered backend when (a) the disk thread holds an active
/// batch (Linux/Windows) or the target is macOS, (b) sparse mode is disabled,
/// and (c) the file does not start at a non-zero offset (append mode). Sparse
/// mode requires `Seek`, which none of these writers provide.
///
/// Append mode opens the file and seeks past existing content via
/// [`std::io::Seek::seek`]. The batch writers issue submissions with absolute
/// offsets starting at 0 and ignore the file position, so they would
/// overwrite the existing prefix with zeros. [`fast_io::MacosWriter`]
/// likewise issues writes from the current position without preserving the
/// seek. Append mode therefore always falls back to the buffered writer,
/// which honors the seek via `Write::write_all` on the underlying `File`.
///
/// On macOS, `MacosWriter::from_file` uses `size_hint` to decide whether to
/// set `F_NOCACHE` (only for files >= 1 MiB), preserving the page cache for
/// small files. `size_hint` is the receiver-known target size.
///
/// On the batched paths, `batch.begin_file(file)` registers the file with the
/// backend; the matching `commit_file` happens via [`Writer::finish`].
#[allow(unused_variables)] // batch params are unused on platforms without their backend
pub(super) fn make_writer<'a>(
    file: fs::File,
    write_buf: &'a mut Vec<u8>,
    disk_batch: Option<&'a mut fast_io::IoUringDiskBatch>,
    iocp_batch: Option<&'a mut fast_io::IocpDiskBatch>,
    use_sparse: bool,
    append_offset: u64,
    is_inplace: bool,
    size_hint: u64,
) -> io::Result<Writer<'a>> {
    // In-place updates must use the buffered writer: they seek past matched
    // basis bytes already in the destination (upstream skip_matched, fileio.c:
    // 202-209), and the batched backends submit at their own internally
    // tracked offset and cannot honor an intervening seek. This mirrors
    // upstream, which always drives in-place writes through buffered
    // write_file + lseek rather than any async submission path.
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    {
        if !use_sparse && append_offset == 0 && !is_inplace {
            if let Some(batch) = disk_batch {
                batch.begin_file(file)?;
                return Ok(Writer::IoUring { batch });
            }
        }
    }
    #[cfg(all(target_os = "windows", feature = "iocp"))]
    {
        if !use_sparse && append_offset == 0 && !is_inplace {
            if let Some(batch) = iocp_batch {
                batch.begin_file(file)?;
                return Ok(Writer::Iocp { batch });
            }
        }
    }
    // GCD (`dispatch_io`) writer, preferred over the F_NOCACHE + writev
    // `Macos` writer when the default-off `macos-gcd` feature is enabled.
    // Sparse and append need Seek, which the channel does not provide, so
    // they fall back to the buffered writer below.
    #[cfg(all(target_os = "macos", feature = "macos-gcd"))]
    {
        if !use_sparse && append_offset == 0 && !is_inplace {
            return Ok(Writer::MacosGcd(fast_io::GcdWriter::from_file(file)?));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !use_sparse && append_offset == 0 && !is_inplace {
            return Ok(Writer::Macos(fast_io::MacosWriter::from_file(
                file, size_hint,
            )));
        }
    }
    // dontcache path: preferred over vmsplice when both are enabled. Lands
    // chunks via pwritev2(RWF_DONTCACHE) so bulk transfers do not evict the
    // page-cache working set, falling back per-chunk to a buffered write when
    // the filesystem rejects the flag. `dontcache_supported()` version-gates
    // the selection up front (RWF_DONTCACHE needs Linux 6.14+) so older
    // kernels skip straight to vmsplice/Buffered instead of burning a failed
    // syscall per file. Sparse and append require Seek, so they keep using
    // Buffered.
    #[cfg(all(target_os = "linux", feature = "dontcache"))]
    {
        if !use_sparse && append_offset == 0 && !is_inplace && fast_io::dontcache_supported() {
            return Ok(Writer::Dontcache(fast_io::DontcacheFileWriter::new(file)?));
        }
    }
    // vmsplice path: only when neither io_uring nor IOCP claimed the file. It
    // gates per-chunk inside VmspliceFileWriter, falling back to plain write
    // for chunks below 64 KiB or with unaligned pointers - the design doc's
    // shape B. Sparse and append require Seek, so they keep using Buffered.
    #[cfg(all(target_os = "linux", feature = "vmsplice"))]
    {
        if !use_sparse && append_offset == 0 && !is_inplace {
            return Ok(Writer::Vmsplice(fast_io::VmspliceFileWriter::new(file)?));
        }
    }
    Ok(Writer::Buffered(ReusableBufWriter::new(file, write_buf)))
}
