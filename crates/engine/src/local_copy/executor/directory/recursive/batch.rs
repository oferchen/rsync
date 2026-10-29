//! Batch file entry capture using protocol wire format.
//!
//! When `--write-batch` is active, each transferred entry is serialized into
//! the protocol's file-list wire format for later replay.
//!
//! upstream: no dedicated writer - `io.c:2747` `start_write_batch()` tees the
//! flist wire bytes into the batch file.

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
        // upstream: flist.c:1887 - symlinks carry stat.st_size (the target byte
        // length), matching the directory case above; only devices/specials are
        // zeroed.
        let mut symlink_entry =
            protocol::flist::FileEntry::new_symlink(name, permissions, link_target);
        symlink_entry.set_size(metadata.len());
        symlink_entry
    } else {
        // upstream: flist.c:send_file_entry() - FIFOs, sockets, and device
        // nodes carry their own S_IFMT type (and, for devices, the rdev),
        // never a regular-file entry, and devices/specials are size-zeroed.
        // Encoding them as a regular file makes an upstream --read-batch peer
        // expect file data (exit 23, receiver.c:738 no_batched_update) and
        // mis-count the node as regular. `-a`/`-D` selects them for transfer,
        // so the batch flist has to preserve the type.
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if file_type.is_fifo() {
                protocol::flist::FileEntry::new_fifo(name, permissions)
            } else if file_type.is_socket() {
                protocol::flist::FileEntry::new_socket(name, permissions)
            } else if file_type.is_block_device() {
                let rdev = metadata.rdev();
                protocol::flist::FileEntry::new_block_device(
                    name,
                    permissions,
                    rdev_major(rdev),
                    rdev_minor(rdev),
                )
            } else if file_type.is_char_device() {
                let rdev = metadata.rdev();
                protocol::flist::FileEntry::new_char_device(
                    name,
                    permissions,
                    rdev_major(rdev),
                    rdev_minor(rdev),
                )
            } else {
                protocol::flist::FileEntry::new_file(name, metadata.len(), permissions)
            }
        }
        #[cfg(not(unix))]
        {
            protocol::flist::FileEntry::new_file(name, metadata.len(), permissions)
        }
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
            if let Some(name) = lookup_user_name_cached(uid).ok().flatten()
                && let Ok(s) = String::from_utf8(name)
            {
                entry.set_user_name(s);
            }
            if let Some(name) = lookup_group_name_cached(gid).ok().flatten()
                && let Ok(s) = String::from_utf8(name)
            {
                entry.set_group_name(s);
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

/// Stamps the hardlink identity of a cluster member onto its flist entry.
///
/// Protocol 30 and above name the cluster by the leader's file-list index:
/// `FileListWriter` reads `u32::MAX` as "this entry is the leader" and emits
/// `XMIT_HLINKED | XMIT_HLINK_FIRST`, and any other value as a follower whose
/// abbreviated entry is just flags, name and `write_varint(first_hlink_ndx)`.
/// Protocols 28 and 29 have no index; the cluster is named by the trailing
/// `(dev, ino)` pair instead.
///
/// upstream: `flist.c:824-850` computes `first_hlink_ndx` and the two xflags,
/// `flist.c:893-897` writes the follower's index and abbreviates the entry, and
/// `flist.c:966-978` writes the pre-30 `(dev, ino)` pair.
fn apply_hardlink_fields(
    entry: &mut protocol::flist::FileEntry,
    metadata: &fs::Metadata,
    hlink_group: Option<(i32, bool)>,
    protocol_version: i32,
) {
    let Some((leader_ndx, is_leader)) = hlink_group else {
        return;
    };

    if protocol_version >= 30 {
        // upstream: flist.c:827-830 - a new group records its own index and is
        // flagged XMIT_HLINK_FIRST; the sentinel is how the writer spells that.
        entry.set_hardlink_idx(if is_leader {
            u32::MAX
        } else {
            leader_ndx.max(0) as u32
        });
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        entry.set_hardlink_dev(metadata.dev() as i64);
        entry.set_hardlink_ino(metadata.ino() as i64);
    }
    #[cfg(not(unix))]
    let _ = metadata;
}

/// Captures a file entry to the batch file using the protocol wire format.
///
/// When batch mode is active, encodes the file entry using the protocol
/// flist wire encoder (same format as network transfers) and writes the
/// raw bytes to the batch file via `BatchWriter::write_data`. This
/// produces batch files compatible with upstream rsync's `--read-batch`.
///
/// The `FileListWriter` in the context maintains cross-entry compression
/// state (name prefix sharing, same-mode/same-time flags) matching the
/// upstream flist encoding in `flist.c:send_file_entry()`.
pub(crate) fn capture_batch_file_entry(
    context: &mut CopyContext,
    source_path: &Path,
    destination: &Path,
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

    // The stream flags recorded in the header are what the reader configures
    // itself from, so the hardlink decision has to come from the same place.
    let (protocol_version, preserve_hard_links) =
        context.batch_hard_link_context().unwrap_or((0, false));
    // Must run for every captured entry, cluster member or not: the group
    // numbers it records are indexed by traversal position.
    let hlink_group = context.assign_batch_hlink_group(metadata, preserve_hard_links);

    let mut entry = build_protocol_file_entry(
        source_path,
        relative_path,
        metadata,
        is_top_dir,
        numeric_ids,
    );
    apply_hardlink_fields(&mut entry, metadata, hlink_group, protocol_version);
    let xattr_report = capture_batch_xattrs(context, source_path, destination, &mut entry);
    context.set_batch_xattr_report(xattr_report);
    let acl = if context.batch_preserve_acls() {
        read_batch_source_acl(source_path, &entry, context.options().fake_super_enabled())
    } else {
        None
    };
    if context.batch_inc_recurse() {
        context.hold_batch_entry(entry, acl);
    } else {
        write_batch_entry(context, &entry, acl, relative_path)?;
        context.note_batch_entry_owner(entry.uid(), entry.gid());
    }

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

/// Reads a source entry's xattrs into its batch flist entry and returns the
/// `ITEM_REPORT_XATTR` value block its iflags carry when the destination's
/// xattrs differ.
///
/// The list goes into the file list whole; the flist writer abbreviates each
/// value over `MAX_FULL_DATUM` to its digest. Every such value the destination
/// lacks is marked to be sent in full after the iflags, so the replaying
/// receiver can apply it.
///
/// upstream: `flist.c:2058-2068 send_file_name()` - `get_xattr()` reads the
/// list (`xattrs.c:322-345` gates links, specials and devices on their
/// preserve options); `generator.c:571-582 itemize()` sets ITEM_REPORT_XATTR
/// when `xattr_diff()` finds a difference, marking each long value the
/// destination lacks XSTATE_TODO; `sender.c:482-484` then writes those values
/// after the iflags (`xattrs.c:651-703 send_xattr_request()`), which the batch
/// records (`io.c:2747 start_write_batch()`).
#[cfg(all(any(unix, windows), feature = "xattr"))]
fn capture_batch_xattrs(
    context: &CopyContext,
    source_path: &Path,
    destination: &Path,
    entry: &mut protocol::flist::FileEntry,
) -> Option<Vec<u8>> {
    let options = context.options();
    let readable = entry.is_file()
        || entry.is_dir()
        || (entry.is_symlink() && options.links_enabled())
        || (entry.is_device() && options.devices_enabled())
        || (entry.is_special() && options.specials_enabled());
    if !context.batch_preserve_xattrs() || !readable {
        return None;
    }
    let rules = context
        .filter_program()
        .filter(|program| program.has_xattr_rules());
    let sender_filter = rules.map(|program| {
        move |name: &str| program.allows_xattr(name, ::filters::XattrSide::Sender)
    });
    let receiver_filter = rules.map(|program| {
        move |name: &str| program.allows_xattr(name, ::filters::XattrSide::Receiver)
    });
    let sender_opts = ::metadata::XattrSendOptions {
        role: ::metadata::XattrRole::Sender,
        follow_symlinks: !entry.is_symlink(),
        // upstream: xattrs.c:249 - the sender reads every namespace but
        // system.*, whoever it runs as.
        am_root: false,
        // The local-copy options carry `-X` as a bool. upstream: xattrs.c:274.
        preserve_xattrs: 1,
        fake_super: context.options().fake_super_enabled(),
        filter: sender_filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
        checksum_seed: 0,
    };
    let list = ::metadata::read_xattrs_for_wire(source_path, &sender_opts).ok()?;
    let dest_opts = ::metadata::XattrSendOptions {
        role: ::metadata::XattrRole::Generator,
        am_root: ::metadata::am_root(),
        filter: receiver_filter.as_ref().map(|f| f as &dyn Fn(&str) -> bool),
        ..sender_opts
    };
    // upstream: generator.c:581 - an absent destination compares against an
    // empty list (xattr_diff(file, NULL, 1)).
    let dest = ::metadata::read_xattrs_for_wire(destination, &dest_opts).unwrap_or_default();
    let report = protocol::xattr::xattr_diff(&list, &dest, 0).then(|| {
        let mut request = list.clone();
        for value in request.entries_mut() {
            // upstream: xattrs.c:612-618 - only a long value is ever requested,
            // and only when the destination does not hold the same one.
            let held = dest
                .entries()
                .iter()
                .any(|d| d.name() == value.name() && d.datum() == value.datum());
            if value.should_abbreviate() && !held {
                value.mark_todo();
            }
        }
        let mut block = Vec::new();
        protocol::xattr::send_sender_xattr_response(&mut block, &mut request)
            .expect("writing to a Vec cannot fail");
        block
    });
    if !list.is_empty() {
        entry.set_xattr_list(list);
    }
    report
}

/// No xattrs are captured in a build without xattr support.
#[cfg(not(all(any(unix, windows), feature = "xattr")))]
fn capture_batch_xattrs(
    _context: &CopyContext,
    _source_path: &Path,
    _destination: &Path,
    _entry: &mut protocol::flist::FileEntry,
) -> Option<Vec<u8>> {
    None
}

/// Reads a source entry's access ACL and, for a directory, its default ACL -
/// what the generator reads for a network send (`prepare_pending_acl`).
///
/// Returns `None` for a symlink, which carries no ACL. Under `--fake-super`
/// an ACL stashed in the source's `%aacl`/`%dacl` xattr wins over the real
/// one, as on the network path.
///
/// upstream: `flist.c:2048-2055 send_file_name()` calls `get_acl()` for every
/// non-symlink under `preserve_acls`, and `flist.c:2077-2081` sends it right
/// after the entry.
fn read_batch_source_acl(
    source_path: &Path,
    entry: &protocol::flist::FileEntry,
    fake_super: bool,
) -> Option<crate::local_copy::context::BatchSourceAcl> {
    if entry.is_symlink() {
        return None;
    }
    let read = |is_default: bool| {
        if fake_super
            && let Ok(Some(acl)) = ::metadata::load_fake_super_acl(source_path, !is_default)
        {
            return acl;
        }
        ::metadata::get_rsync_acl(source_path, entry.mode(), is_default)
    };
    Some((read(false), entry.is_dir().then(|| read(true))))
}

/// Encodes one flist entry, and its ACL when one was read, straight into a
/// flat batch file list.
fn write_batch_entry(
    context: &mut CopyContext,
    entry: &protocol::flist::FileEntry,
    acl: Option<crate::local_copy::context::BatchSourceAcl>,
    relative_path: &Path,
) -> Result<(), LocalCopyError> {
    let mut buf = Vec::with_capacity(128);
    let flist_writer = context
        .batch_flist_writer_mut()
        .expect("batch_flist_writer must exist when batch_writer is set");
    if let Some((access, default)) = acl {
        flist_writer.set_pending_acl(access, default);
    }
    flist_writer.write_entry(&mut buf, entry).map_err(|e| {
        LocalCopyError::io("encode batch flist entry", relative_path.to_path_buf(), e)
    })?;

    let batch_writer_arc = context
        .batch_writer()
        .expect("batch writer set on the write-batch path")
        .clone();
    let mut writer_guard = batch_writer_arc
        .lock()
        .expect("batch writer mutex poisoned");
    writer_guard.write_data(&buf).map_err(|e| {
        LocalCopyError::io(
            "write batch file entry",
            relative_path.to_path_buf(),
            std::io::Error::other(e),
        )
    })
}

/// Extracts the major device number from a combined `rdev` (Linux glibc
/// layout). `rustix::fs::major` is unavailable on Apple targets, so the split
/// is done here, matching `metadata::fake_super`.
#[cfg(all(unix, target_os = "linux"))]
fn rdev_major(rdev: u64) -> u32 {
    ((rdev >> 8) & 0xfff) as u32 | (((rdev >> 32) & !0xfff) as u32)
}

/// Extracts the major device number from a combined `rdev` (BSD/macOS layout).
#[cfg(all(unix, not(target_os = "linux")))]
fn rdev_major(rdev: u64) -> u32 {
    ((rdev >> 24) & 0xff) as u32
}

/// Extracts the minor device number from a combined `rdev` (Linux glibc
/// layout, where the minor spans non-contiguous bits).
#[cfg(all(unix, target_os = "linux"))]
fn rdev_minor(rdev: u64) -> u32 {
    (rdev & 0xff) as u32 | (((rdev >> 12) & !0xff) as u32)
}

/// Extracts the minor device number from a combined `rdev` (BSD/macOS layout).
#[cfg(all(unix, not(target_os = "linux")))]
fn rdev_minor(rdev: u64) -> u32 {
    (rdev & 0xffffff) as u32
}

#[cfg(all(test, unix))]
mod tests {
    use super::build_protocol_file_entry;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// upstream: flist.c:1887 - a symlink's `F_LENGTH` is its `st_size`, which
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

    /// upstream: flist.c:1894 - `file->mode = st.st_mode` is one assignment for
    /// every type. A batch capture is the same flist the network path would
    /// send, so a symlink's recorded mode must be its lstat mode. On Linux that
    /// is 0o777 by kernel fiat; on the `CAN_CHMOD_SYMLINK` platforms
    /// (rsync.h:456-457) it is whatever `lchmod` set, and a hardcoded constant
    /// would lose it. Asserting equality with lstat covers both.
    #[test]
    fn symlink_batch_entry_carries_the_lstat_mode() {
        use std::os::unix::fs::MetadataExt;

        let tmp = TempDir::new().expect("tempdir");
        let link = tmp.path().join("link");
        symlink("target", &link).expect("create symlink");
        let meta = std::fs::symlink_metadata(&link).expect("metadata");

        let entry = build_protocol_file_entry(&link, &PathBuf::from("link"), &meta, false, true);

        assert!(entry.is_symlink());
        assert_eq!(
            entry.permissions() & 0o7777,
            meta.mode() & 0o7777,
            "batch symlink mode must mirror lstat st_mode, not a constant",
        );
    }
}
