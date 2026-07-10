//! Per-file processing for the disk commit thread.
//!
//! Drives chunked file writes, whole-file coalesced writes, output file
//! opening (device, inplace, temp+rename), and writer backend selection
//! (io_uring, IOCP, macOS `F_NOCACHE`, dontcache, vmsplice, buffered).

use std::fs;
use std::io;

use engine::CleanupManager;

use crate::delta_apply::SparseWriteState;
use crate::pipeline::messages::{BeginMessage, CommitResult, FileMessage};
use crate::pipeline::spsc;
use crate::temp_guard::TempFileGuard;
#[cfg(not(unix))]
use crate::temp_guard::open_tmpfile;
#[cfg(unix)]
use crate::temp_guard::open_tmpfile_sandboxed;

use super::super::config::DiskCommitConfig;
use super::super::writer::{ReusableBufWriter, Writer};
use super::commit::{SparseFinalize, commit_file, retain_partial_file};
use super::metadata::{apply_file_metadata, finalize_checksum};

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

    let mut output = make_writer(
        file,
        write_buf,
        disk_batch,
        iocp_batch,
        config.use_sparse,
        begin.append_offset,
        begin.target_size,
    )?;

    let mut sparse_state = if config.use_sparse {
        let mut state = SparseWriteState::default();
        state.set_preallocated_len(basis_len);
        Some(state)
    } else {
        None
    };

    // Per-file checksum verifier, moved from the network thread.
    // Computing the checksum here overlaps hashing with disk I/O and
    // removes ~42% of instructions from the network-critical path.
    let mut checksum_verifier = begin.checksum_verifier.take();

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
                let _ = buf_return_tx.send(data);
            }
            FileMessage::Commit => {
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

                let computed_checksum = finalize_checksum(checksum_verifier);

                return Ok(CommitResult {
                    bytes_written,
                    file_entry_index: begin.file_entry_index,
                    metadata_error,
                    computed_checksum,
                    delayed_path: outcome.delayed_path,
                    backup_notice: outcome.backup_notice,
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
    write_buf: &mut Vec<u8>,
    disk_batch: Option<&mut fast_io::IoUringDiskBatch>,
    iocp_batch: Option<&mut fast_io::IocpDiskBatch>,
) -> io::Result<CommitResult> {
    // upstream: receiver.c:999-1006 - open failure is a benign per-file partial,
    // not a fatal abort. The coalesced WholeFile carries its data inline, so
    // there are no queued channel messages to drain (unlike process_file); the
    // open error surfaces directly and drain_all_results maps a permission
    // failure to RERR_PARTIAL (exit 23).
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

    let mut output = make_writer(
        file,
        write_buf,
        disk_batch,
        iocp_batch,
        config.use_sparse,
        begin.append_offset,
        begin.target_size,
    )?;
    let bytes_written = data.len() as u64;

    let mut checksum_verifier = begin.checksum_verifier.take();
    if let Some(ref mut verifier) = checksum_verifier {
        verifier.update(&data);
    }

    let sparse_final = if config.use_sparse {
        let mut sparse = SparseWriteState::default();
        sparse.set_preallocated_len(basis_len);
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

    let _ = buf_return_tx.send(data);

    output.flush_and_sync(config.do_fsync, &begin.file_path)?;
    output.finish(config.do_fsync, &begin.file_path)?;

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

    let computed_checksum = finalize_checksum(checksum_verifier);

    Ok(CommitResult {
        bytes_written,
        file_entry_index: begin.file_entry_index,
        metadata_error,
        computed_checksum,
        delayed_path: outcome.delayed_path,
        backup_notice: outcome.backup_notice,
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
            Ok(FileMessage::Chunk(data)) => {
                // Recycle the buffer for the network thread; drop the bytes.
                let _ = buf_return_tx.send(data);
            }
            Ok(FileMessage::Commit) => {
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
    size_hint: u64,
) -> io::Result<Writer<'a>> {
    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    {
        if !use_sparse && append_offset == 0 {
            if let Some(batch) = disk_batch {
                batch.begin_file(file)?;
                return Ok(Writer::IoUring { batch });
            }
        }
    }
    #[cfg(all(target_os = "windows", feature = "iocp"))]
    {
        if !use_sparse && append_offset == 0 {
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
        if !use_sparse && append_offset == 0 {
            return Ok(Writer::MacosGcd(fast_io::GcdWriter::from_file(file)?));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if !use_sparse && append_offset == 0 {
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
        if !use_sparse && append_offset == 0 && fast_io::dontcache_supported() {
            return Ok(Writer::Dontcache(fast_io::DontcacheFileWriter::new(file)?));
        }
    }
    // vmsplice path: only when neither io_uring nor IOCP claimed the file. It
    // gates per-chunk inside VmspliceFileWriter, falling back to plain write
    // for chunks below 64 KiB or with unaligned pointers - the design doc's
    // shape B. Sparse and append require Seek, so they keep using Buffered.
    #[cfg(all(target_os = "linux", feature = "vmsplice"))]
    {
        if !use_sparse && append_offset == 0 {
            return Ok(Writer::Vmsplice(fast_io::VmspliceFileWriter::new(file)?));
        }
    }
    Ok(Writer::Buffered(ReusableBufWriter::new(file, write_buf)))
}
