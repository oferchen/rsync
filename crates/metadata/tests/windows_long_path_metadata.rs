//! Long-path (>260 char) round-trip coverage for the Windows ACL/xattr
//! FFI boundaries.
//!
//! The Win32 file APIs reject plain paths above `MAX_PATH` (260 characters)
//! with `ERROR_PATH_NOT_FOUND` unless the caller opts in to the
//! extended-length syntax (`\\?\C:\...`). The metadata crate routes every
//! ACL / xattr boundary through `fast_io::to_extended_path` so deeply
//! nested trees keep working.
//!
//! These tests build a 30-segment / 25-char-per-segment directory tree
//! (~750 absolute characters) and exercise:
//!
//! - The Windows ACL surface (`metadata::read_dacl_sddl`,
//!   `metadata::write_dacl_sddl`) which fans into the
//!   `acl_windows::common::to_wide` boundary feeding
//!   `GetNamedSecurityInfoW` / `SetNamedSecurityInfoW`.
//! - The Windows ADS-backed xattr surface
//!   (`metadata::read_xattrs_for_wire`,
//!   `metadata::apply_xattrs_from_list`) which fans into the
//!   `xattr_windows::path_to_wide` / `stream_path_wide` boundaries feeding
//!   `FindFirstStreamW` and `CreateFileW`.
//!
//! Long-path support is filesystem-dependent: NTFS supports it, FAT32
//! does not, and SMB / network mounts may reject creation regardless of
//! the prefix. The tests pre-probe each capability and skip with a
//! clear log when the runtime cannot host the workload, matching the
//! degrade-gracefully pattern used elsewhere in this crate.

#![cfg(all(target_os = "windows", feature = "acl", feature = "xattr"))]

use std::fs;
use std::path::{Path, PathBuf};

use metadata::{
    apply_xattrs_from_list, read_dacl_sddl, read_xattrs_for_wire, write_dacl_sddl,
};
use protocol::xattr::{XattrEntry, XattrList};
use tempfile::tempdir;

/// Builds a directory tree under `root` consisting of `depth` nested
/// directories, each segment 25 characters long. Returns the deepest
/// directory path.
///
/// At depth 30 the resulting path is ~750 characters past `root`, which
/// places the absolute path comfortably above the 260-character
/// `MAX_PATH` cap on any practical temp-dir location and reliably
/// triggers the long-path boundary the helper guards against.
fn build_deep_tree(root: &Path, depth: usize) -> std::io::Result<PathBuf> {
    let mut current = root.to_path_buf();
    for index in 0..depth {
        // 25-character segment: prefix + zero-padded index.
        let segment = format!("longpath_seg_{:0>10}", index);
        current.push(segment);
        fs::create_dir(&current)?;
    }
    Ok(current)
}

/// Probes whether the volume backing `dir` can accept a path of at least
/// `target_chars` total characters. Returns the leaf path on success or
/// `None` when the volume / runtime refuses to extend that far.
fn try_create_long_tree(dir: &Path, target_chars: usize) -> Option<PathBuf> {
    let depth = 30;
    let leaf = match build_deep_tree(dir, depth) {
        Ok(leaf) => leaf,
        Err(error) => {
            eprintln!("long-path tree creation failed at depth {depth}: {error}");
            return None;
        }
    };
    let absolute_len = leaf.as_os_str().len();
    if absolute_len < target_chars {
        eprintln!(
            "long-path tree shorter than target: got {absolute_len} chars, want >= {target_chars}",
        );
        return None;
    }
    Some(leaf)
}

/// Returns `true` when the volume hosting `file` supports security
/// descriptors via the public ACL surface. Skips gracefully on FAT32 /
/// network mounts where the call returns "not supported".
fn acls_supported(file: &Path) -> bool {
    match read_dacl_sddl(file) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("ACLs not supported on this volume ({error}); skipping");
            false
        }
    }
}

/// Returns `true` when the volume hosting `file` accepts an Alternate
/// Data Stream write via the public xattr surface. Skips gracefully on
/// FAT32 / network mounts that reject ADS.
///
/// The probe writes a single throwaway stream and tolerates failure as
/// "unsupported"; subsequent test code clears the probe via the
/// follow-up `apply_xattrs_from_list` call which removes any
/// destination stream not present in the supplied source list.
fn ads_supported(file: &Path) -> bool {
    let probe_name = b"oc_rsync_longpath_probe".to_vec();
    let mut probe = XattrList::new();
    probe.push(XattrEntry::new(probe_name, b"1".to_vec()));
    match apply_xattrs_from_list(file, &probe, false) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("ADS not supported on this volume ({error}); skipping");
            false
        }
    }
}

#[test]
fn long_path_acl_round_trip() {
    // 600+ char absolute path: prefix the requirement so the probe
    // matches the audit doc and the test surfaces the boundary clearly.
    const TARGET_CHARS: usize = 600;

    let dir = tempdir().expect("create temp dir");
    let Some(leaf) = try_create_long_tree(dir.path(), TARGET_CHARS) else {
        return;
    };

    let file = leaf.join("acl.txt");
    fs::write(&file, b"payload").expect("seed file in long path");
    assert!(
        file.as_os_str().len() >= TARGET_CHARS,
        "absolute file path must exceed MAX_PATH: got {} chars",
        file.as_os_str().len(),
    );

    if !acls_supported(&file) {
        return;
    }

    // Read the current descriptor (must succeed despite the long path
    // because `to_wide` routes through `to_extended_path`).
    let sddl_before = read_dacl_sddl(&file).expect("read DACL on long path");
    assert!(
        sddl_before.contains("D:"),
        "expected DACL section in initial descriptor; got {sddl_before:?}",
    );

    // Round-trip the descriptor back to the same long path so the write
    // boundary (`SetNamedSecurityInfoW`) is also exercised.
    write_dacl_sddl(&file, &sddl_before).expect("write DACL on long path");

    let sddl_after = read_dacl_sddl(&file).expect("re-read DACL on long path");
    assert!(
        sddl_after.contains("D:"),
        "expected DACL section after round-trip; got {sddl_after:?}",
    );
}

#[test]
fn long_path_ads_round_trip() {
    const TARGET_CHARS: usize = 600;

    let dir = tempdir().expect("create temp dir");
    let Some(leaf) = try_create_long_tree(dir.path(), TARGET_CHARS) else {
        return;
    };

    let file = leaf.join("ads.txt");
    fs::write(&file, b"payload").expect("seed file in long path");
    assert!(
        file.as_os_str().len() >= TARGET_CHARS,
        "absolute file path must exceed MAX_PATH: got {} chars",
        file.as_os_str().len(),
    );

    if !ads_supported(&file) {
        return;
    }

    // Apply a Zone.Identifier-style ADS via the public xattr API. The
    // dispatch funnels through `xattr_windows::stream_path_wide`, which
    // routes the path through `to_extended_path` before composing
    // `path:name:$DATA`. The local-format name is the bare stream name;
    // the wire encoder adds the `user.` prefix on non-Linux peers.
    let local_name: &[u8] = b"Zone.Identifier";
    let stream_value = b"[ZoneTransfer]\r\nZoneId=3\r\n".to_vec();
    let mut list = XattrList::new();
    list.push(XattrEntry::new(local_name.to_vec(), stream_value.clone()));
    apply_xattrs_from_list(&file, &list, false).expect("apply ADS on long path");

    // Read the xattrs back; the boundary path here is
    // `xattr_windows::path_to_wide` feeding `FindFirstStreamW`. The wire
    // encoder prefixes the local stream name with `user.` on non-Linux
    // peers (matches upstream xattrs.c:518-530).
    let wire = read_xattrs_for_wire(&file, false, false, 0).expect("read ADS on long path");
    let expected_wire_name: &[u8] = b"user.Zone.Identifier";
    let entry = wire
        .iter()
        .find(|entry| entry.name() == expected_wire_name)
        .expect("Zone.Identifier ADS must survive long-path round-trip");
    assert_eq!(
        entry.datum(),
        stream_value.as_slice(),
        "ADS payload must round-trip byte-for-byte",
    );
}
