//! File processing logic for the disk commit thread.
//!
//! Handles chunked file writes, whole-file coalesced writes, output file
//! opening (device, inplace, temp+rename), and post-commit metadata
//! application.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use engine::compute_backup_path;
use protocol::acl::AclCache;

use crate::delta_apply::{ChecksumVerifier, SparseWriteState};
use crate::pipeline::messages::{BeginMessage, CommitResult, ComputedChecksum, FileMessage};
use crate::pipeline::spsc;
use crate::temp_guard::{TempFileGuard, open_tmpfile};

use super::config::{BackupConfig, DiskCommitConfig};
use super::writer::ReusableBufWriter;

/// Processes a single file: open, write chunks, commit or abort.
///
/// After writing each chunk, the owned `Vec<u8>` is returned through
/// `buf_return_tx` for reuse by the network thread.
pub(super) fn process_file(
    file_rx: &spsc::Receiver<FileMessage>,
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    mut begin: BeginMessage,
    write_buf: &mut Vec<u8>,
) -> io::Result<CommitResult> {
    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin, config)?;

    let mut output = ReusableBufWriter::new(file, write_buf);

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
        let msg = file_rx.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "disk thread: channel disconnected while processing file",
            )
        })?;

        match msg {
            FileMessage::Chunk(data) => {
                // Update per-file checksum before writing (mirrors upstream
                // receiver.c:315 which hashes each token before writing).
                if let Some(ref mut verifier) = checksum_verifier {
                    verifier.update(&data);
                }

                if let Some(ref mut sparse) = sparse_state {
                    sparse.write(&mut output, &data)?;
                } else {
                    output.write_all(&data)?;
                }
                bytes_written += data.len() as u64;
                // Return the buffer for reuse. Ignore errors - the network
                // thread may have moved on (e.g. after an error).
                let _ = buf_return_tx.send(data);
            }
            FileMessage::Commit => {
                if let Some(ref mut sparse) = sparse_state {
                    let _final_pos = sparse.finish(&mut output)?;
                }

                flush_and_sync(&mut output, config.do_fsync, &begin.file_path)?;
                drop(output);

                commit_file(
                    &begin,
                    config,
                    &mut cleanup_guard,
                    needs_rename,
                    bytes_written,
                )?;

                let metadata_error = apply_post_commit_metadata(&begin, config);

                let computed_checksum = finalize_checksum(checksum_verifier);

                return Ok(CommitResult {
                    bytes_written,
                    file_entry_index: begin.file_entry_index,
                    metadata_error,
                    computed_checksum,
                });
            }
            FileMessage::Abort { reason } => {
                drop(output);
                drop(cleanup_guard);
                return Err(io::Error::other(reason));
            }
            FileMessage::Shutdown => {
                drop(output);
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
/// futex overhead from 3+ sends/recvs to 1 for small files.
pub(super) fn process_whole_file(
    buf_return_tx: &spsc::Sender<Vec<u8>>,
    config: &DiskCommitConfig,
    mut begin: BeginMessage,
    data: Vec<u8>,
    write_buf: &mut Vec<u8>,
) -> io::Result<CommitResult> {
    let (file, mut cleanup_guard, needs_rename) = open_output_file(&begin, config)?;

    let mut output = ReusableBufWriter::new(file, write_buf);
    let bytes_written = data.len() as u64;

    let mut checksum_verifier = begin.checksum_verifier.take();
    if let Some(ref mut verifier) = checksum_verifier {
        verifier.update(&data);
    }

    if config.use_sparse {
        let mut sparse = SparseWriteState::default();
        sparse.write(&mut output, &data)?;
        let _final_pos = sparse.finish(&mut output)?;
    } else {
        output.write_all(&data)?;
    }

    let _ = buf_return_tx.send(data);

    flush_and_sync(&mut output, config.do_fsync, &begin.file_path)?;
    drop(output);

    commit_file(
        &begin,
        config,
        &mut cleanup_guard,
        needs_rename,
        bytes_written,
    )?;

    let metadata_error = apply_post_commit_metadata(&begin, config);

    let computed_checksum = finalize_checksum(checksum_verifier);

    Ok(CommitResult {
        bytes_written,
        file_entry_index: begin.file_entry_index,
        metadata_error,
        computed_checksum,
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
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&begin.file_path)?;
        // upstream: receiver.c:307-308 - in append mode, seek past existing content
        if begin.append_offset > 0 {
            use std::io::Seek;
            file.seek(io::SeekFrom::Start(begin.append_offset))?;
        }
        Ok((file, TempFileGuard::new(begin.file_path.clone()), false))
    } else {
        let (file, guard) = open_tmpfile(&begin.file_path, config.temp_dir.as_deref())?;
        Ok((file, guard, true))
    }
}

/// Flushes the writer and optionally calls `sync_all`.
fn flush_and_sync(
    output: &mut ReusableBufWriter<'_>,
    do_fsync: bool,
    file_path: &Path,
) -> io::Result<()> {
    if do_fsync {
        output
            .sync()
            .map_err(|e| io::Error::new(e.kind(), format!("fsync failed for {file_path:?}: {e}")))
    } else {
        output
            .flush()
            .map_err(|e| io::Error::other(format!("flush failed for {file_path:?}: {e}")))
    }
}

/// Performs backup, atomic rename, and inplace truncation after writing.
fn commit_file(
    begin: &BeginMessage,
    config: &DiskCommitConfig,
    cleanup_guard: &mut TempFileGuard,
    needs_rename: bool,
    bytes_written: u64,
) -> io::Result<()> {
    // upstream: backup.c:make_backup() - rename existing file before overwrite
    if let Some(ref backup_config) = config.backup {
        make_backup(&begin.file_path, backup_config)?;
    }

    if needs_rename {
        fs::rename(cleanup_guard.path(), &begin.file_path)?;
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
    }
    cleanup_guard.keep();
    Ok(())
}

/// Applies metadata, ACLs, and xattrs after file commit.
///
/// Skip metadata for device targets: changing perms/ownership on a device
/// node after writing data is not appropriate.
fn apply_post_commit_metadata(
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
            &begin.file_path,
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

    if let Err(e) = metadata::apply_metadata_from_file_entry(file_path, entry, opts) {
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
/// `--backup-dir`.
fn make_backup(file_path: &Path, config: &BackupConfig) -> io::Result<()> {
    if !file_path.exists() {
        return Ok(());
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

    fs::rename(file_path, &backup_path)
}

/// Finalizes a checksum verifier into a `ComputedChecksum`.
fn finalize_checksum(verifier: Option<ChecksumVerifier>) -> Option<ComputedChecksum> {
    verifier.map(|v| {
        let mut buf = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let len = v.finalize_into(&mut buf);
        ComputedChecksum { bytes: buf, len }
    })
}
