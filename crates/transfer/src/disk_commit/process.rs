//! File processing logic for the disk commit thread.
//!
//! Handles chunked file writes, whole-file coalesced writes, output file
//! opening (device, inplace, temp+rename), and metadata application.
//!
//! Metadata is applied to the temp file before rename to match upstream
//! `rsync.c:finish_transfer()` line 748: "Change permissions before putting
//! the file into place." When rename crosses device boundaries (EXDEV), a
//! copy+remove fallback re-applies metadata to the final path since
//! `fs::copy` does not preserve ownership, timestamps, ACLs, or xattrs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use engine::{CleanupManager, compute_backup_path, trace_make_backup_rename};
use protocol::acl::AclCache;

use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::pipeline::messages::{
    BackupNotice, BeginMessage, CommitResult, ComputedChecksum, FileMessage,
};
use crate::pipeline::spsc;
use crate::temp_guard::TempFileGuard;
#[cfg(not(unix))]
use crate::temp_guard::open_tmpfile;
#[cfg(unix)]
use crate::temp_guard::open_tmpfile_sandboxed;

use super::config::{BackupConfig, DiskCommitConfig, PartialMode};
use super::writer::{ReusableBufWriter, Writer};

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
pub(super) fn process_file(
    file_rx: &spsc::Receiver<FileMessage>,
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    mut begin: BeginMessage,
    write_buf: &mut Vec<u8>,
    disk_batch: Option<&mut fast_io::IoUringDiskBatch>,
    iocp_batch: Option<&mut fast_io::IocpDiskBatch>,
) -> io::Result<CommitResult> {
    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin, config)?;
    if needs_rename {
        CleanupManager::global().register_temp_file(cleanup_guard.path().to_path_buf());
    }

    // Register the temp file with the global cleanup manager so a SIGKILL
    // (which bypasses Drop) still removes orphaned temp files on restart.
    // Only temp+rename paths produce actual temp files; inplace and device
    // writes operate on the final destination.
    if needs_rename {
        CleanupManager::global().register_temp_file(cleanup_guard.path().to_path_buf());
    }

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
        Some(SparseWriteState::default())
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
                    retain_partial_file(&config.partial_mode, &mut cleanup_guard, &begin.file_path);
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
                if let Some(ref mut sparse) = sparse_state {
                    let _final_pos = sparse.finish(output.buffered_for_sparse())?;
                }

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
                    retain_partial_file(&config.partial_mode, &mut cleanup_guard, &begin.file_path);
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
                    retain_partial_file(&config.partial_mode, &mut cleanup_guard, &begin.file_path);
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
pub(super) fn process_whole_file(
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    mut begin: BeginMessage,
    data: Vec<u8>,
    write_buf: &mut Vec<u8>,
    disk_batch: Option<&mut fast_io::IoUringDiskBatch>,
    iocp_batch: Option<&mut fast_io::IocpDiskBatch>,
) -> io::Result<CommitResult> {
    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin, config)?;
    if needs_rename {
        CleanupManager::global().register_temp_file(cleanup_guard.path().to_path_buf());
    }

    if needs_rename {
        CleanupManager::global().register_temp_file(cleanup_guard.path().to_path_buf());
    }

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

    if config.use_sparse {
        let mut sparse = SparseWriteState::default();
        sparse.write(output.buffered_for_sparse(), &data)?;
        let _final_pos = sparse.finish(output.buffered_for_sparse())?;
    } else {
        output.write_chunk(&data)?;
    }

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
        Ok((file, TempFileGuard::new(begin.file_path.clone()), false))
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
        Ok((file, TempFileGuard::new(begin.file_path.clone()), false))
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
fn make_writer<'a>(
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
    #[cfg(target_os = "macos")]
    {
        if !use_sparse && append_offset == 0 {
            return Ok(Writer::Macos(fast_io::MacosWriter::from_file(
                file, size_hint,
            )));
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

/// Commit result indicating whether a cross-device copy occurred and
/// whether the file was staged to the partial dir for delayed updates.
struct CommitOutcome {
    /// True when a cross-device copy was needed (EXDEV fallback).
    was_copy: bool,
    /// When `--delay-updates` staged the file to `.~tmp~`, holds the
    /// staging path. `None` for immediate commits and inplace writes.
    delayed_path: Option<PathBuf>,
    /// Destination-relative paths recorded when `--backup` renamed an
    /// existing file. Propagated to the main thread via [`CommitResult`]
    /// so the upstream `INFO_GTE(BACKUP, 1)` notice can be emitted by the
    /// thread whose `VerbosityConfig` carries the user's `--info=backup`.
    backup_notice: Option<BackupNotice>,
}

/// Performs backup, atomic rename, and inplace truncation after writing.
///
/// When `delay_updates` is true and the file uses temp+rename, stages the
/// file to `.~tmp~/<filename>` in the same parent directory instead of
/// renaming to the final destination. The caller reports the staging path
/// back so the receiver can perform a bulk rename sweep at phase 2.
///
/// When io_uring is available (Linux 5.11+ with `IORING_OP_RENAMEAT`), the
/// temp-file rename is submitted as an io_uring SQE instead of a synchronous
/// `rename(2)` syscall. Falls back to `std::fs::rename` on all other
/// platforms or when the kernel lacks the opcode.
///
/// # Upstream Reference
///
/// - `receiver.c:906-929`: delay_updates stages to partial dir
/// - `receiver.c:422-450`: `handle_delayed_updates()` bulk rename
fn commit_file(
    begin: &BeginMessage,
    config: &DiskCommitConfig,
    cleanup_guard: &mut TempFileGuard,
    needs_rename: bool,
    bytes_written: u64,
) -> io::Result<CommitOutcome> {
    // upstream: backup.c:make_backup() - rename existing file before overwrite
    // With delay_updates, backup happens during the sweep, not here.
    let backup_notice = if !config.delay_updates {
        if let Some(ref backup_config) = config.backup {
            make_backup(&begin.file_path, backup_config)?
        } else {
            None
        }
    } else {
        None
    };

    if needs_rename && config.delay_updates {
        // upstream: receiver.c:916-929 - stage to partial dir (.~tmp~)
        let staging_path = partial_dir_path(&begin.file_path);
        if let Some(parent) = staging_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let result = rename_with_io_uring_fallback(cleanup_guard.path(), &staging_path)?;
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
        cleanup_guard.keep();
        return Ok(CommitOutcome {
            was_copy: result,
            delayed_path: Some(staging_path),
            backup_notice,
        });
    }

    let was_copy = if needs_rename {
        let result = rename_with_io_uring_fallback(cleanup_guard.path(), &begin.file_path)?;
        CleanupManager::global().unregister_temp_file(cleanup_guard.path());
        result
    } else if begin.is_inplace {
        // upstream: receiver.c:340 - set_file_length(fd, F_LENGTH(file))
        // In append mode, bytes_written only counts newly received data -
        // the full file size includes the existing content we seeked past.
        let final_size = if begin.append_offset > 0 {
            begin.target_size
        } else {
            bytes_written
        };
        let file = fs::OpenOptions::new().write(true).open(&begin.file_path)?;
        file.set_len(final_size)?;
        false
    } else {
        false
    };
    cleanup_guard.keep();
    Ok(CommitOutcome {
        was_copy,
        delayed_path: None,
        backup_notice,
    })
}

/// Upstream partial dir name for `--delay-updates` staging.
///
/// upstream: options.c - `static char tmp_partialdir[] = ".~tmp~";`
const DELAY_UPDATES_PARTIAL_DIR: &str = ".~tmp~";

/// Computes the `.~tmp~/<filename>` staging path for a destination file.
///
/// upstream: receiver.c:430 - `partial_dir_fname(fname)` returns
/// `<parent>/.~tmp~/<basename>`.
fn partial_dir_path(file_path: &Path) -> PathBuf {
    let parent = file_path.parent().unwrap_or(Path::new("."));
    let basename = file_path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("unknown"));
    parent.join(DELAY_UPDATES_PARTIAL_DIR).join(basename)
}

/// Retains a partial temp file instead of deleting it on interrupt.
///
/// Depending on the `PartialMode`:
/// - `None`: does nothing (the guard's Drop will delete the temp file).
/// - `Partial`: renames the temp file to the final destination path, so
///   the incomplete file is available for resume on the next run.
/// - `PartialDir(dir)`: moves the temp file into the partial directory,
///   using the destination filename as the target name.
///
/// Errors are logged and silently ignored - partial retention is best-effort.
/// On failure, the guard's Drop will clean up the temp file.
///
/// # Upstream Reference
///
/// - `cleanup.c:105-115` - `handle_partial_dir()` moves temp to partial-dir
/// - `cleanup.c:130-135` - `keep_partial && got_literal` guard
/// - `receiver.c:340-345` - `do_rename(partialptr, fname)` for `--partial`
fn retain_partial_file(
    partial_mode: &PartialMode,
    cleanup_guard: &mut TempFileGuard,
    dest_path: &Path,
) {
    match partial_mode {
        PartialMode::None => {}
        PartialMode::Partial => {
            // upstream: cleanup.c:130-135 - rename temp file directly to
            // the destination. The incomplete content replaces any existing
            // file at the destination path.
            let temp_path = cleanup_guard.path().to_path_buf();
            match rename_with_io_uring_fallback(cleanup_guard.path(), dest_path) {
                Ok(_) => {
                    // upstream: cleanup.c:174-178 - stamp modtime=0 on
                    // retained partial files so --update does not skip them
                    // as "up to date" on the next run. Only for plain
                    // --partial, not --partial-dir (upstream uses
                    // handle_partial_dir() for --partial-dir which does not
                    // zero the mtime).
                    //
                    // Use from_unix_time(0, 0) rather than FileTime::zero()
                    // because on Windows, zero() maps to the Windows epoch
                    // (1601-01-01) which becomes an all-zero FILETIME -
                    // SetFileTime treats that as "do not change", silently
                    // skipping the stamp. from_unix_time(0, 0) maps to
                    // 1970-01-01 which is a non-zero FILETIME that Windows
                    // will actually apply.
                    let epoch = filetime::FileTime::from_unix_time(0, 0);
                    if let Err(e) = filetime::set_file_mtime(dest_path, epoch) {
                        logging::debug_log!(
                            Io,
                            1,
                            "failed to set mtime=0 on partial file {}: {}",
                            dest_path.display(),
                            e
                        );
                    }
                    logging::debug_log!(Io, 1, "retained partial file: {}", dest_path.display());
                    CleanupManager::global().unregister_temp_file(&temp_path);
                    cleanup_guard.keep();
                }
                Err(e) => {
                    logging::debug_log!(
                        Io,
                        1,
                        "failed to retain partial file {}: {}",
                        dest_path.display(),
                        e
                    );
                }
            }
        }
        PartialMode::PartialDir(dir) => {
            // upstream: cleanup.c:105-115 - move temp file into partial-dir
            let temp_path = cleanup_guard.path().to_path_buf();
            match cleanup_guard.rename_to_partial_dir(dest_path, dir) {
                Ok(partial_path) => {
                    CleanupManager::global().unregister_temp_file(&temp_path);
                    logging::debug_log!(
                        Io,
                        1,
                        "retained partial file in partial-dir: {}",
                        partial_path.display()
                    );
                }
                Err(e) => {
                    logging::debug_log!(
                        Io,
                        1,
                        "failed to retain partial file in {}: {}",
                        dir.display(),
                        e
                    );
                }
            }
        }
    }
}

/// Renames a temp file to its final destination, trying io_uring first.
///
/// Returns `Ok(false)` when the rename succeeded in-place, `Ok(true)` when
/// a cross-device copy+remove fallback was used (EXDEV). Callers use the
/// return value to decide whether metadata must be re-applied to the
/// destination.
///
/// On Linux 5.11+ with io_uring RENAMEAT2 support, submits the rename as an
/// `IORING_OP_RENAMEAT` SQE. Falls back to `std::fs::rename` when io_uring
/// is unavailable (non-Linux, old kernel, or feature not compiled in).
///
/// Cross-device fallback mirrors upstream `util1.c:robust_rename()` which
/// uses `copy_file()` + `do_unlink()` when `rename()` returns EXDEV. This
/// happens when `--temp-dir` points to a different filesystem than the
/// destination.
fn rename_with_io_uring_fallback(old_path: &Path, new_path: &Path) -> io::Result<bool> {
    if let Some(result) = fast_io::try_rename_via_io_uring(old_path, new_path) {
        return result.map(|()| false);
    }
    match fs::rename(old_path, new_path) {
        Ok(()) => Ok(false),
        Err(e) if is_cross_device(&e) => {
            // upstream: util1.c:robust_rename() - copy_file + do_unlink
            fs::copy(old_path, new_path)?;
            fs::remove_file(old_path)?;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

/// Returns `true` when an I/O error represents a cross-device link (EXDEV).
///
/// On Unix, `raw_os_error() == libc::EXDEV` (errno 18). On Windows,
/// `ERROR_NOT_SAME_DEVICE` (error 17) is the equivalent.
fn is_cross_device(e: &io::Error) -> bool {
    match e.raw_os_error() {
        #[cfg(unix)]
        Some(code) => code == libc::EXDEV,
        #[cfg(windows)]
        Some(code) => code == 17, // ERROR_NOT_SAME_DEVICE
        #[cfg(not(any(unix, windows)))]
        Some(_) => false,
        None => false,
    }
}

/// Applies metadata, ACLs, and xattrs to the given path.
///
/// Called with the temp file path before rename (upstream
/// `rsync.c:finish_transfer()` line 748), or with the final destination
/// path for inplace writes and after cross-device copy fallback.
///
/// Skips metadata for device targets: changing perms/ownership on a device
/// node after writing data is not appropriate.
fn apply_file_metadata(
    target_path: &Path,
    begin: &BeginMessage,
    config: &DiskCommitConfig,
) -> Option<(PathBuf, String)> {
    let file_entry = config
        .file_list
        .as_ref()
        .and_then(|fl| fl.get(begin.file_entry_index));

    if begin.is_device_target {
        None
    } else {
        apply_metadata_acls_and_xattrs(
            target_path,
            file_entry,
            config.metadata_opts.as_ref(),
            config.acl_cache.as_deref(),
            begin.xattr_list.as_ref(),
        )
    }
}

/// Applies file metadata, ACLs, and xattrs from the receiver's caches.
///
/// Combines `apply_metadata_from_file_entry` with `apply_acls_from_cache` and
/// `apply_xattrs_from_list` into a single call that mirrors upstream
/// `set_file_attrs()` in receiver.c. ACLs are applied after permissions so that
/// any ACL mask is set on the final mode. Xattrs are applied last.
///
/// Returns `Some((path, error_message))` on failure, `None` on success or when
/// no metadata/entry is available.
fn apply_metadata_acls_and_xattrs(
    file_path: &Path,
    file_entry: Option<&protocol::flist::FileEntry>,
    metadata_opts: Option<&metadata::MetadataOptions>,
    acl_cache: Option<&AclCache>,
    xattr_list: Option<&protocol::xattr::XattrList>,
) -> Option<(PathBuf, String)> {
    let (opts, entry) = match (metadata_opts, file_entry) {
        (Some(o), Some(e)) => (o, e),
        _ => return None,
    };

    // Skip the stat inside apply_metadata_from_file_entry: the file was
    // just renamed into place from a temp file, so its metadata will not
    // match the desired entry. Pass None to apply unconditionally.
    if let Err(e) = metadata::apply_metadata_with_cached_stat(file_path, entry, opts, None) {
        return Some((file_path.to_path_buf(), e.to_string()));
    }

    // upstream: set_file_attrs() calls set_acl() after perms/times/ownership
    if let Some(cache) = acl_cache {
        if let Some(access_ndx) = entry.acl_ndx() {
            let follow = !entry.is_symlink();
            if let Err(e) = metadata::apply_acls_from_cache(
                file_path,
                cache,
                access_ndx,
                entry.def_acl_ndx(),
                follow,
                Some(entry.mode()),
            ) {
                return Some((file_path.to_path_buf(), e.to_string()));
            }
        }
    }

    // upstream: xattrs.c:set_xattr() - apply xattrs after metadata and ACLs
    if let Some(xattr_list) = xattr_list {
        if let Err(e) = metadata::apply_xattrs_from_list(file_path, xattr_list, true) {
            return Some((file_path.to_path_buf(), e.to_string()));
        }
    }

    None
}

/// Creates a backup of the destination file before overwriting.
///
/// Mirrors upstream `backup.c:make_backup()` which renames the existing file
/// to the backup path. Parent directories are created if needed when using
/// `--backup-dir`. On success, emits the upstream `--debug=BACKUP` RENAME
/// notice (`backup.c:216-217`) and returns a [`BackupNotice`] carrying the
/// destination-relative paths so the main thread can emit upstream's
/// `INFO_GTE(BACKUP, 1)` line (`backup.c:352`). The disk thread cannot emit
/// the info line directly because its thread-local [`logging::VerbosityConfig`]
/// is never seeded with the user's `--info=backup` selection.
fn make_backup(file_path: &Path, config: &BackupConfig) -> io::Result<Option<BackupNotice>> {
    if !file_path.exists() {
        return Ok(None);
    }

    let backup_path = compute_backup_path(
        &config.dest_dir,
        file_path,
        None,
        config.backup_dir.as_deref(),
        &config.suffix,
    );

    if let Some(parent) = backup_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    fs::rename(file_path, &backup_path)?;
    // upstream: backup.c:216-217 - DEBUG_GTE(BACKUP, 1) on the RENAME success
    // branch of link_or_rename. disk_commit always uses rename here.
    trace_make_backup_rename(&file_path.display().to_string());
    // upstream: backup.c:352 - INFO_GTE(BACKUP, 1) fires on success label for
    // every successful backup. Paths are displayed relative to the destination
    // root to match upstream test assertions (testsuite/backup.test). The
    // actual `info_log!` emission happens on the main thread; see
    // `crate::pipeline::receiver::emit_backup_notice`.
    let file_rel = file_path
        .strip_prefix(&config.dest_dir)
        .unwrap_or(file_path)
        .to_path_buf();
    let backup_rel = backup_path
        .strip_prefix(&config.dest_dir)
        .unwrap_or(&backup_path)
        .to_path_buf();
    Ok(Some(BackupNotice {
        original: file_rel,
        backup: backup_rel,
    }))
}

/// Finalizes a checksum verifier into a `ComputedChecksum`.
fn finalize_checksum(verifier: Option<ChecksumVerifier>) -> Option<ComputedChecksum> {
    verifier.map(|v| {
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len = v.finalize_into(&mut buf);
        ComputedChecksum { bytes: buf, len }
    })
}

/// Entry for the `--delay-updates` bulk rename sweep.
///
/// Each entry records the staging path (inside `.~tmp~/`) and the final
/// destination path. After all files are committed by the disk thread, the
/// caller collects these entries and passes them to
/// [`handle_delayed_updates`] for the bulk rename sweep.
#[derive(Debug, Clone)]
pub struct DelayedUpdateEntry {
    /// Path where the file was staged (inside the `.~tmp~/` directory).
    pub staging_path: PathBuf,
    /// Final destination path where the file should appear.
    pub final_path: PathBuf,
}

/// Performs the `--delay-updates` bulk rename sweep.
///
/// After all files have been committed to their staging paths (inside
/// `.~tmp~/` subdirectories), this function renames each staged file to its
/// final destination and removes the now-empty staging directories.
///
/// Mirrors upstream `receiver.c:422-450` which iterates the list of delayed
/// files, calls `handle_partial_dir(partialptr, PDIR_DELETE)` to rename each
/// file from the partial directory to its final path, then removes the
/// partial directory itself.
///
/// # Errors
///
/// Returns the first rename error encountered. Files that have already been
/// renamed are not rolled back - the operation is best-effort, matching
/// upstream rsync behavior where a failed rename is logged and the sweep
/// continues.
pub fn handle_delayed_updates(outcomes: &[DelayedUpdateEntry]) -> io::Result<()> {
    // upstream: receiver.c:422-450 - iterate delayed_bits, rename each file
    // from partialptr to fname, then handle_partial_dir(partialptr, PDIR_DELETE)
    let mut staging_dirs = std::collections::BTreeSet::new();

    for outcome in outcomes {
        // Track staging directories for cleanup.
        if let Some(parent) = outcome.staging_path.parent() {
            if parent
                .file_name()
                .is_some_and(|name| name == super::config::DELAY_UPDATES_PARTIAL_DIR)
            {
                staging_dirs.insert(parent.to_path_buf());
            }
        }

        // upstream: receiver.c:435 - do_rename(partialptr, fname)
        if let Some(parent) = outcome.final_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        rename_with_io_uring_fallback(&outcome.staging_path, &outcome.final_path)?;

        logging::debug_log!(
            Io,
            1,
            "delay-updates: renamed {} -> {}",
            outcome.staging_path.display(),
            outcome.final_path.display()
        );
    }

    // upstream: receiver.c:447 - handle_partial_dir(partialptr, PDIR_DELETE)
    // Remove empty staging directories after all renames complete.
    for dir in staging_dirs.iter().rev() {
        let _ = fs::remove_dir(dir);
    }

    Ok(())
}

/// Computes the staging path for a file under `--delay-updates`.
///
/// Given a file's final destination path, returns the path where it should be
/// staged inside the `.~tmp~/` subdirectory of the file's parent directory.
///
/// # Example
///
/// ```text
/// final_path:   /dest/subdir/file.txt
/// staging_path: /dest/subdir/.~tmp~/file.txt
/// ```
///
/// # Upstream Reference
///
/// - `receiver.c:410-415` - compute `partialptr` from `partial_dir` + `fname`
pub fn delay_updates_staging_path(final_path: &Path) -> PathBuf {
    let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = final_path.file_name().unwrap_or(final_path.as_os_str());
    parent
        .join(super::config::DELAY_UPDATES_PARTIAL_DIR)
        .join(file_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the io_uring RENAMEAT2 fallback renames a file regardless of
    /// whether io_uring handles it or `std::fs::rename` does. Same-device
    /// rename returns `false` (no cross-device copy).
    #[test]
    fn rename_with_io_uring_fallback_moves_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rename_src.txt");
        let dst = dir.path().join("rename_dst.txt");

        fs::write(&src, b"io_uring rename data").unwrap();

        let was_copy = rename_with_io_uring_fallback(&src, &dst).unwrap();

        assert!(!was_copy);
        assert!(!src.exists());
        assert!(dst.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"io_uring rename data");
    }

    /// Verifies the rename replaces an existing destination file.
    #[test]
    fn rename_with_io_uring_fallback_replaces_existing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rename_replace_src.txt");
        let dst = dir.path().join("rename_replace_dst.txt");

        fs::write(&src, b"new data").unwrap();
        fs::write(&dst, b"old data").unwrap();

        let was_copy = rename_with_io_uring_fallback(&src, &dst).unwrap();

        assert!(!was_copy);
        assert!(!src.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"new data");
    }

    /// Verifies the rename fails with an error when the source does not exist.
    #[test]
    fn rename_with_io_uring_fallback_fails_for_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("missing_src.txt");
        let dst = dir.path().join("rename_fail_dst.txt");

        let result = rename_with_io_uring_fallback(&src, &dst);
        assert!(result.is_err());
    }

    /// Verifies `is_cross_device` correctly identifies EXDEV errors.
    #[test]
    fn is_cross_device_detects_exdev() {
        #[cfg(unix)]
        {
            let exdev = io::Error::from_raw_os_error(libc::EXDEV);
            assert!(is_cross_device(&exdev));
        }
        let not_found = io::Error::new(io::ErrorKind::NotFound, "not found");
        assert!(!is_cross_device(&not_found));

        let perm = io::Error::from_raw_os_error(1); // EPERM
        assert!(!is_cross_device(&perm));
    }

    /// Verifies `make_writer` selects [`Writer::Macos`] when sparse mode is
    /// disabled and `append_offset` is zero, so the `F_NOCACHE` + `writev`
    /// optimization is engaged on the common write path.
    #[cfg(target_os = "macos")]
    #[test]
    fn make_writer_selects_macos_for_non_sparse_zero_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("macos_writer_select.bin");
        let file = fs::File::create(&path).unwrap();
        let mut write_buf = Vec::with_capacity(256 * 1024);

        let writer = make_writer(
            file,
            &mut write_buf,
            None,
            None,
            /* use_sparse */ false,
            /* append_offset */ 0,
            /* size_hint */ 0,
        )
        .unwrap();

        assert!(
            matches!(writer, Writer::Macos(_)),
            "macOS non-sparse zero-offset writes must select Writer::Macos"
        );
    }

    /// Verifies `make_writer` falls back to [`Writer::Buffered`] when sparse
    /// mode or append mode is active on macOS, preserving `Seek` semantics.
    #[cfg(target_os = "macos")]
    #[test]
    fn make_writer_falls_back_to_buffered_when_seek_required() {
        let dir = tempfile::tempdir().unwrap();

        // Sparse mode forces buffered.
        let sparse_path = dir.path().join("sparse.bin");
        let sparse_file = fs::File::create(&sparse_path).unwrap();
        let mut sparse_buf = Vec::with_capacity(256 * 1024);
        let sparse_writer = make_writer(
            sparse_file,
            &mut sparse_buf,
            None,
            None,
            /* use_sparse */ true,
            /* append_offset */ 0,
            /* size_hint */ 0,
        )
        .unwrap();
        assert!(
            matches!(sparse_writer, Writer::Buffered(_)),
            "sparse mode must select Writer::Buffered"
        );

        // Append mode forces buffered.
        let append_path = dir.path().join("append.bin");
        let append_file = fs::File::create(&append_path).unwrap();
        let mut append_buf = Vec::with_capacity(256 * 1024);
        let append_writer = make_writer(
            append_file,
            &mut append_buf,
            None,
            None,
            /* use_sparse */ false,
            /* append_offset */ 4096,
            /* size_hint */ 0,
        )
        .unwrap();
        assert!(
            matches!(append_writer, Writer::Buffered(_)),
            "append mode must select Writer::Buffered"
        );
    }

    /// Verifies consistent io_uring availability for RENAMEAT2 across calls.
    #[test]
    fn rename_io_uring_availability_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("avail_src.txt");
        let dst1 = dir.path().join("avail_dst1.txt");
        let dst2 = dir.path().join("avail_dst2.txt");
        fs::write(&src, b"data").unwrap();

        let first = fast_io::try_rename_via_io_uring(&src, &dst1).is_some();
        // If first call consumed the file, recreate it.
        if first {
            fs::write(&src, b"data").unwrap();
            let _ = fs::remove_file(&dst1);
        }
        let second = fast_io::try_rename_via_io_uring(&src, &dst2).is_some();
        assert_eq!(
            first, second,
            "io_uring RENAMEAT2 availability must be consistent"
        );
    }

    /// Verifies `partial_dir_path` constructs `.~tmp~/<basename>` under the
    /// file's parent directory, matching upstream `options.c:tmp_partialdir`.
    #[test]
    fn partial_dir_path_constructs_staging_path() {
        let path = Path::new("/dest/subdir/file.txt");
        let staging = partial_dir_path(path);
        assert_eq!(staging, PathBuf::from("/dest/subdir/.~tmp~/file.txt"));
    }

    /// Verifies `partial_dir_path` handles files directly in the root dest dir.
    #[test]
    fn partial_dir_path_root_level_file() {
        let path = Path::new("/dest/file.txt");
        let staging = partial_dir_path(path);
        assert_eq!(staging, PathBuf::from("/dest/.~tmp~/file.txt"));
    }

    /// Verifies `make_backup` returns the upstream-format backup notice with
    /// destination-relative paths so the main thread can surface upstream's
    /// `INFO_GTE(BACKUP, 1)` line during wire transfers.
    ///
    /// upstream: backup.c:352 - `rprintf(FINFO, "backed up %s to %s\n",
    /// fname, buf)` fires on the `success:` label for every backup written.
    /// We propagate the notice via [`CommitResult::backup_notice`] because
    /// the disk thread's `VerbosityConfig` is not seeded with the user's
    /// `--info=backup` selection.
    #[test]
    fn make_backup_returns_destination_relative_notice() {
        use std::ffi::OsString;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("payload.bin");
        fs::write(&file_path, b"existing content").unwrap();

        let config = BackupConfig {
            dest_dir: dir.path().to_path_buf(),
            backup_dir: None,
            suffix: OsString::from("~"),
        };
        let notice = make_backup(&file_path, &config)
            .expect("make_backup succeeds")
            .expect("notice produced when an existing file is backed up");

        let backup_path = file_path.with_extension("bin~");
        assert!(backup_path.exists(), "backup file must exist after rename");
        assert!(!file_path.exists(), "original file must be renamed away");

        assert_eq!(notice.original, PathBuf::from("payload.bin"));
        assert_eq!(notice.backup, PathBuf::from("payload.bin~"));
    }

    /// Verifies `make_backup` is a no-op (and returns `None`) when the file
    /// does not exist, mirroring upstream `backup.c:make_backup()` which
    /// short-circuits when `stat(fname, &st) != 0`.
    #[test]
    fn make_backup_missing_file_is_noop() {
        use std::ffi::OsString;

        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("absent.bin");

        let config = BackupConfig {
            dest_dir: dir.path().to_path_buf(),
            backup_dir: None,
            suffix: OsString::from("~"),
        };
        let notice = make_backup(&file_path, &config).expect("make_backup succeeds");
        assert!(notice.is_none(), "no notice when nothing was backed up");
    }
}
