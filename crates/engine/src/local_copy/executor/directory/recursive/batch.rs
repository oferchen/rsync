//! Batch file entry capture using protocol wire format.
//!
//! When `--write-batch` is active, each transferred entry is serialized into
//! the protocol's file-list wire format for later replay.
//!
//! // upstream: batch.c:write_batch_flist_info()
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::local_copy::{CopyContext, LocalCopyError};

/// Builds a protocol [`FileEntry`](protocol::flist::FileEntry) from filesystem metadata.
///
/// Converts the local `fs::Metadata` and relative path into the protocol crate's
/// `FileEntry` type, which can then be encoded using the protocol wire format
/// for upstream-compatible batch files.
fn build_protocol_file_entry(
    relative_path: &Path,
    metadata: &fs::Metadata,
) -> protocol::flist::FileEntry {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    let mode = metadata.mode();

    #[cfg(not(unix))]
    let mode = if metadata.is_dir() {
        0o040755
    } else if metadata.file_type().is_symlink() {
        0o120777
    } else {
        0o100644
    };

    let permissions = mode & 0o7777;
    let file_type = metadata.file_type();
    let name = PathBuf::from(relative_path);

    let mut entry = if file_type.is_dir() {
        protocol::flist::FileEntry::new_directory(name, permissions)
    } else if file_type.is_symlink() {
        // Read symlink target for the protocol entry
        protocol::flist::FileEntry::new_symlink(name, PathBuf::new())
    } else {
        protocol::flist::FileEntry::new_file(name, metadata.len(), permissions)
    };

    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    entry.set_mtime(mtime, 0);

    #[cfg(unix)]
    {
        entry.set_uid(metadata.uid());
        entry.set_gid(metadata.gid());
    }

    entry
}

/// Captures a file entry to the batch file using the protocol wire format.
///
/// When batch mode is active, encodes the file entry using the protocol
/// flist wire encoder (same format as network transfers) and writes the
/// raw bytes to the batch file via [`BatchWriter::write_data`]. This
/// produces batch files compatible with upstream rsync's `--read-batch`.
///
/// The `FileListWriter` in the context maintains cross-entry compression
/// state (name prefix sharing, same-mode/same-time flags) matching the
/// upstream flist encoding in `flist.c:send_file_entry()`.
pub(super) fn capture_batch_file_entry(
    context: &mut CopyContext,
    relative_path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), LocalCopyError> {
    if context.batch_writer().is_none() {
        return Ok(());
    }

    let entry = build_protocol_file_entry(relative_path, metadata);

    let mut buf = Vec::with_capacity(128);
    let flist_writer = context
        .batch_flist_writer_mut()
        .expect("batch_flist_writer must exist when batch_writer is set");
    flist_writer.write_entry(&mut buf, &entry).map_err(|e| {
        LocalCopyError::io("encode batch flist entry", relative_path.to_path_buf(), e)
    })?;

    let batch_writer_arc = context.batch_writer().unwrap().clone();
    let mut writer_guard = batch_writer_arc.lock().unwrap();
    writer_guard.write_data(&buf).map_err(|e| {
        LocalCopyError::io(
            "write batch file entry",
            relative_path.to_path_buf(),
            std::io::Error::other(e),
        )
    })?;

    Ok(())
}
