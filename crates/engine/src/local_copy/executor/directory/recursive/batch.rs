//! Batch file entry capture using protocol wire format.
//!
//! When `--write-batch` is active, each transferred entry is serialized into
//! the protocol's file-list wire format for later replay.
//!
//! upstream: batch.c:write_batch_flist_info()

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::local_copy::{CopyContext, LocalCopyError};
#[cfg(unix)]
use metadata::id_lookup::{lookup_group_name_cached, lookup_user_name_cached};

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
    #[cfg(not(unix))]
    let _ = numeric_ids;

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
        // upstream: flist.c:1465 - symlinks carry stat.st_size (the target byte
        // length), matching the directory case above; only devices/specials are
        // zeroed.
        let mut symlink_entry = protocol::flist::FileEntry::new_symlink(name, link_target);
        symlink_entry.set_size(metadata.len());
        symlink_entry
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
            if let Some(name) = lookup_user_name_cached(uid).ok().flatten() {
                if let Ok(s) = String::from_utf8(name) {
                    entry.set_user_name(s);
                }
            }
            if let Some(name) = lookup_group_name_cached(gid).ok().flatten() {
                if let Ok(s) = String::from_utf8(name) {
                    entry.set_group_name(s);
                }
            }
        }
    }

    // upstream: flist.c:send_file_entry() - FLAG_TOP_DIR marks root source
    // entries so --delete knows which directories are transfer roots.
    if is_top_dir && file_type.is_dir() {
        entry.set_top_dir(true);
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

    let entry = build_protocol_file_entry(
        source_path,
        relative_path,
        metadata,
        is_top_dir,
        numeric_ids,
    );

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

    // Record sort metadata for this entry so flush_batch_delta_to_batch()
    // can compute the traversal-to-sorted index mapping that upstream's
    // flist_sort_and_clean() produces after reading the batch flist.
    //
    // The sort key MUST use the wire-canonical name (always '/'-separated),
    // because batch replay re-sorts the flist by the names it decodes off the
    // batch, which are wire-canonical. On Windows the OS-native separator is
    // '\\' (0x5C), which sorts after '/' (0x2F); recording raw OS bytes here
    // would diverge from the replay-side sort and misalign the NDX-to-entry
    // mapping. Normalise '\\' -> '/' to match path_bytes_to_wire().
    let raw_name = relative_path.as_os_str().as_encoded_bytes();
    let wire_name: Vec<u8> = if std::path::MAIN_SEPARATOR == '/' {
        raw_name.to_vec()
    } else {
        raw_name
            .iter()
            .map(|&b| if b == b'\\' { b'/' } else { b })
            .collect()
    };
    context.record_batch_entry_sort_data(&wire_name, metadata.is_dir());

    // Increment the flist index counter. This assigns each captured entry
    // a sequential index matching upstream's flist numbering. Regular files
    // use (batch_flist_index - 1) as their NDX in the delta stream.
    context.increment_batch_flist_index();

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::build_protocol_file_entry;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// upstream: flist.c:1465 - a symlink's `F_LENGTH` is its `st_size`, which
    /// lstat reports as the target byte length. Batch capture must record the
    /// same value the network flist would, otherwise `--list-only`/`%l` and the
    /// `--stats` total computed from a replayed batch diverge from upstream.
    #[test]
    fn symlink_batch_entry_carries_target_length() {
        let tmp = TempDir::new().expect("tempdir");
        let link = tmp.path().join("link");
        let target = "some/relative/target";
        symlink(target, &link).expect("create symlink");
        let meta = std::fs::symlink_metadata(&link).expect("metadata");

        let entry = build_protocol_file_entry(&link, &PathBuf::from("link"), &meta, false, true);

        assert!(entry.is_symlink());
        assert_eq!(
            entry.size(),
            target.len() as u64,
            "batch symlink F_LENGTH must equal the target byte length, \
             not the hardcoded 0 from FileEntry::new_symlink",
        );
        assert_eq!(
            entry.size(),
            meta.len(),
            "F_LENGTH must mirror lstat st_size"
        );
    }
}
