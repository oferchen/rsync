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
///
/// When `is_top_dir` is true, sets `XMIT_TOP_DIR` on the entry flags to match
/// upstream `flist.c:send_file_entry()` behavior for root source entries.
fn build_protocol_file_entry(
    source_path: &Path,
    relative_path: &Path,
    metadata: &fs::Metadata,
    is_top_dir: bool,
    numeric_ids: bool,
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
        let mut dir_entry = protocol::flist::FileEntry::new_directory(name, permissions);
        // upstream: flist.c:send_file_entry() - directory entries include the
        // actual stat.st_size, not zero.
        dir_entry.set_size(metadata.len());
        dir_entry
    } else if file_type.is_symlink() {
        // upstream: flist.c:send_file_entry() - symlink target is read and
        // included in the flist entry so batch replay can recreate symlinks.
        let link_target = fs::read_link(source_path).unwrap_or_default();
        protocol::flist::FileEntry::new_symlink(name, link_target)
    } else {
        protocol::flist::FileEntry::new_file(name, metadata.len(), permissions)
    };

    // upstream: flist.c:send_file_entry() - mtime includes nanoseconds when
    // the protocol supports it (XMIT_MOD_NSEC flag).
    #[cfg(unix)]
    let mtime_nsec = metadata.mtime_nsec().max(0) as u32;
    #[cfg(not(unix))]
    let mtime_nsec = 0u32;

    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    entry.set_mtime(mtime, mtime_nsec);

    #[cfg(unix)]
    {
        let uid = metadata.uid();
        let gid = metadata.gid();
        entry.set_uid(uid);
        entry.set_gid(gid);

        // upstream: flist.c:send_file_entry() - inline user/group names are
        // written when XMIT_USER_NAME_FOLLOWS / XMIT_GROUP_NAME_FOLLOWS flags
        // are set. The FileListWriter xflags computation checks whether the
        // entry has a name set via these accessors.
        if !numeric_ids {
            if let Some(name) = lookup_user_name(uid).ok().flatten() {
                if let Ok(s) = String::from_utf8(name) {
                    entry.set_user_name(s);
                }
            }
            if let Some(name) = lookup_group_name(gid).ok().flatten() {
                if let Ok(s) = String::from_utf8(name) {
                    entry.set_group_name(s);
                }
            }
        }
    }

    // upstream: flist.c:send_file_entry() - FLAG_TOP_DIR marks root source
    // entries so --delete knows which directories are transfer roots.
    if is_top_dir && file_type.is_dir() {
        entry.flags_mut().primary |= protocol::flist::XMIT_TOP_DIR;
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
    source_path: &Path,
    relative_path: &Path,
    metadata: &fs::Metadata,
    is_top_dir: bool,
) -> Result<(), LocalCopyError> {
    if context.batch_writer().is_none() {
        return Ok(());
    }

    #[cfg(unix)]
    let numeric_ids = context.numeric_ids_enabled();
    #[cfg(not(unix))]
    let numeric_ids = true;

    let entry = build_protocol_file_entry(source_path, relative_path, metadata, is_top_dir, numeric_ids);

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
