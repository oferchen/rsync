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
#[cfg(unix)]
use metadata::id_lookup::{lookup_group_name, lookup_user_name};

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
pub(crate) fn capture_batch_file_entry(
    context: &mut CopyContext,
    relative_path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), LocalCopyError> {
    if context.batch_writer().is_none() {
        return Ok(());
    }

    let entry = build_protocol_file_entry(relative_path, metadata);

    // upstream: uidlist.c - collect uid/gid name mappings for the ID lists
    // that are written after the flist end marker. Skip when --numeric-ids
    // is set (upstream sends no name lists in that case).
    #[cfg(unix)]
    if !context.numeric_ids_enabled() {
        use std::os::unix::fs::MetadataExt;
        if context.preserve_owner_enabled() {
            let uid = metadata.uid();
            if !context.batch_uid_list().contains(uid) {
                let name = lookup_user_name(uid).ok().flatten();
                context.batch_uid_list_mut().add_id(uid, name);
            }
        }
        if context.preserve_group_enabled() {
            let gid = metadata.gid();
            if !context.batch_gid_list().contains(gid) {
                let name = lookup_group_name(gid).ok().flatten();
                context.batch_gid_list_mut().add_id(gid, name);
            }
        }
    }

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

    // Increment the flist index counter. This assigns each captured entry
    // a sequential index matching upstream's flist numbering. Regular files
    // use (batch_flist_index - 1) as their NDX in the delta stream.
    context.increment_batch_flist_index();

    Ok(())
}
