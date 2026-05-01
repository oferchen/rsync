//! Cross-platform extended-attribute glue.
//!
//! Sits above the platform backends and reproduces upstream rsync's xattr
//! flow:
//!
//! - On Unix the [`xattr_unix`](crate::xattr_unix) module wraps the
//!   `xattr` crate (`get`/`set`/`list`/`remove`).
//! - On Windows the [`xattr_windows`](crate::xattr_windows) module
//!   maps every named xattr onto an NTFS Alternate Data Stream
//!   (`path:name:$DATA`) so the client/daemon surface remains the same.
//!
//! The per-attribute primitives all take and return raw byte slices for
//! attribute names so the wire format (which is byte-oriented) and the
//! POSIX/NTFS native encodings can both be expressed without lossy
//! conversions in this layer.
//!
//! # Upstream Reference
//!
//! - `xattrs.c:rsync_xal_get()` - read xattrs into the wire-list cache.
//! - `xattrs.c:rsync_xal_set()` - apply received xattrs on the receiver.
//! - `xattrs.c:64-68, 254-257` - permitted-namespace policy on Linux.

use crate::error::MetadataError;
use protocol::xattr::XattrList;
use std::collections::HashSet;
use std::io;
use std::path::Path;

#[cfg(unix)]
use crate::xattr_unix as backend;
#[cfg(windows)]
use crate::xattr_windows as backend;

/// Checks whether an xattr name is permitted for the current privilege level.
///
/// Mirrors upstream rsync `xattrs.c:64-68, 254-257`:
/// - Non-root on Linux: only `user.*` xattrs are accessible.
/// - Root on Linux: all namespaces except `system.*`.
/// - On non-Linux platforms (macOS, FreeBSD, Windows): no namespace
///   filtering, since those systems use a single flat namespace (NTFS ADS,
///   `com.apple.*`, FreeBSD `user`-only, etc.).
#[cfg(target_os = "linux")]
fn is_xattr_permitted(name: &str) -> bool {
    const USER_PREFIX: &str = "user.";
    const SYSTEM_PREFIX: &str = "system.";

    /// Caches the euid check since it does not change during a transfer.
    fn is_root() -> bool {
        use std::sync::OnceLock;
        static IS_ROOT: OnceLock<bool> = OnceLock::new();
        *IS_ROOT.get_or_init(|| rustix::process::geteuid().is_root())
    }

    if is_root() {
        // upstream: root skips system.* namespace
        !name.starts_with(SYSTEM_PREFIX)
    } else {
        // upstream: non-root only sees user.* namespace
        name.starts_with(USER_PREFIX)
    }
}

/// On non-Linux platforms (macOS, FreeBSD, Windows), all xattr names are permitted.
#[cfg(not(target_os = "linux"))]
fn is_xattr_permitted(_name: &str) -> bool {
    true
}

fn map_xattr_error(context: &'static str, path: &Path, error: io::Error) -> MetadataError {
    MetadataError::new(context, path, error)
}

/// Returns the byte-encoded xattr names present on `path`, filtered by
/// [`is_xattr_permitted`].
fn list_attributes(path: &Path, follow_symlinks: bool) -> Result<Vec<Vec<u8>>, MetadataError> {
    let attrs = backend::list_attributes(path, follow_symlinks)
        .map_err(|error| map_xattr_error("list extended attributes", path, error))?;
    Ok(attrs
        .into_iter()
        .map(|name| backend::os_name_to_bytes(&name))
        .filter(|bytes| is_xattr_permitted(&String::from_utf8_lossy(bytes)))
        .collect())
}

fn read_attribute(
    path: &Path,
    name: &[u8],
    follow_symlinks: bool,
) -> Result<Option<Vec<u8>>, MetadataError> {
    backend::read_attribute(path, name, follow_symlinks)
        .map_err(|error| map_xattr_error("read extended attribute", path, error))
}

fn write_attribute(
    path: &Path,
    name: &[u8],
    value: &[u8],
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    backend::write_attribute(path, name, value, follow_symlinks)
        .map_err(|error| map_xattr_error("write extended attribute", path, error))
}

fn remove_attribute(
    path: &Path,
    name: &[u8],
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    backend::remove_attribute(path, name, follow_symlinks)
        .map_err(|error| map_xattr_error("remove extended attribute", path, error))
}

/// Reads xattr data from a file and returns it as a wire-format `XattrList`.
///
/// Names are translated to wire format via `local_to_wire()`. Entries are
/// sorted alphabetically by wire name, matching upstream rsync's
/// `rsync_xal_get()` which sorts by name after collection. All values are
/// stored as full data - abbreviation (checksum substitution for large values)
/// is handled by the wire encoder at send time.
///
/// # Upstream Reference
///
/// - `xattrs.c:rsync_xal_get()` - reads xattrs, sorts by name, assigns nums
/// - `xattrs.c:get_xattr()` - entry point called from `make_file()`
pub fn read_xattrs_for_wire(
    path: &Path,
    follow_symlinks: bool,
    am_root: bool,
    _checksum_seed: i32,
) -> Result<XattrList, MetadataError> {
    use protocol::xattr::{XattrEntry, local_to_wire};

    let attrs = list_attributes(path, follow_symlinks)?;
    let mut entries = Vec::with_capacity(attrs.len());

    for name in &attrs {
        // upstream: xattrs.c:509-528 - translate local name to wire format
        let wire_name = match local_to_wire(name, am_root) {
            Some(n) => n,
            None => continue, // Filtered out (rsync internal, namespace issue)
        };

        let value = match read_attribute(path, name, follow_symlinks)? {
            Some(v) => v,
            None => continue,
        };

        entries.push(XattrEntry::new(wire_name, value));
    }

    // upstream: xattrs.c:296-297 - qsort by name
    entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));

    // upstream: xattrs.c:298-299 - assign 1-based num in reverse order
    let count = entries.len();
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.set_num((count - i) as u32);
    }

    Ok(XattrList::with_entries(entries))
}

/// Synchronises the extended attributes from `source` to `destination`.
pub fn sync_xattrs(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<(), MetadataError> {
    let source_attrs = list_attributes(source, follow_symlinks)?;
    let mut retained: HashSet<Vec<u8>> = HashSet::with_capacity(source_attrs.len());

    for name in &source_attrs {
        retained.insert(name.clone());
        let allow = filter.is_none_or(|predicate| predicate(&String::from_utf8_lossy(name)));

        if !allow {
            continue;
        }

        if let Some(value) = read_attribute(source, name, follow_symlinks)? {
            write_attribute(destination, name, &value, follow_symlinks)?;
        } else {
            remove_attribute(destination, name, follow_symlinks)?;
        }
    }

    let destination_attrs = list_attributes(destination, follow_symlinks)?;
    for name in &destination_attrs {
        if retained.contains(name) {
            continue;
        }

        let allow = filter.is_none_or(|predicate| predicate(&String::from_utf8_lossy(name)));

        if allow {
            remove_attribute(destination, name, follow_symlinks)?;
        }
    }

    Ok(())
}

/// Applies parsed xattrs from a wire protocol [`XattrList`] to a destination file.
///
/// This is the receiver-side counterpart to [`sync_xattrs`], used when xattr
/// data arrives over the wire rather than being read from a local source file.
/// The `XattrList` entries are expected to already have local-format names
/// (translated via `wire_to_local()` during file list reception).
///
/// The function performs a full synchronization:
/// - Sets each non-abbreviated entry from the list on the destination.
/// - Removes destination xattrs not present in the source list.
/// - Skips abbreviated entries (checksum-only) that lack full values.
/// - Respects platform namespace filtering via privilege checks.
///
/// # Arguments
///
/// * `destination` - Path to apply xattrs to.
/// * `xattr_list` - Parsed xattr name-value pairs from the wire protocol.
/// * `follow_symlinks` - Whether to follow symlinks when setting xattrs.
///
/// # Upstream Reference
///
/// Mirrors `xattrs.c:set_xattr()` - applies received xattr data to destination files.
pub fn apply_xattrs_from_list(
    destination: &Path,
    xattr_list: &XattrList,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let mut applied_names: HashSet<Vec<u8>> = HashSet::with_capacity(xattr_list.len());

    for entry in xattr_list.iter() {
        // Skip abbreviated entries - they only contain a checksum, not the actual value
        if entry.is_abbreviated() {
            continue;
        }

        let name_str = entry.name_str();
        if !is_xattr_permitted(&name_str) {
            continue;
        }

        let name_bytes = entry.name().to_vec();

        write_attribute(destination, &name_bytes, entry.datum(), follow_symlinks)?;
        applied_names.insert(name_bytes);
    }

    // Remove destination xattrs not in the source list
    let dest_attrs = list_attributes(destination, follow_symlinks)?;
    for name in &dest_attrs {
        if !applied_names.contains(name) {
            let name_str = String::from_utf8_lossy(name);
            if is_xattr_permitted(&name_str) {
                remove_attribute(destination, name, follow_symlinks)?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::xattr::XattrEntry;
    use std::fs;
    use tempfile::tempdir;

    /// Helper to check if xattrs are supported on the current filesystem.
    fn xattrs_supported(path: &Path) -> bool {
        let test_name = test_xattr_name("test_support");
        match write_attribute(path, &test_name, b"test", false) {
            Ok(()) => {
                let _ = remove_attribute(path, &test_name, false);
                true
            }
            Err(_) => false,
        }
    }

    /// Returns the expected local xattr name for test entries.
    ///
    /// On Linux, names need the `user.` prefix. On other platforms (macOS,
    /// BSD, Windows ADS), names are used as-is since there is no namespace
    /// prefix requirement.
    fn test_xattr_name(base: &str) -> Vec<u8> {
        #[cfg(target_os = "linux")]
        {
            format!("user.{base}").into_bytes()
        }
        #[cfg(not(target_os = "linux"))]
        {
            base.as_bytes().to_vec()
        }
    }

    #[test]
    fn list_attributes_returns_empty_for_file_without_xattrs() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attrs = list_attributes(&file, false).expect("list attrs");
        // May have system attributes, but should not error
        assert!(
            attrs
                .iter()
                .all(|a| !String::from_utf8_lossy(a).contains("user.custom"))
        );
    }

    #[test]
    fn write_and_read_attribute_roundtrip() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr_name = test_xattr_name("test_attr");
        let attr_value = b"test value 123";

        write_attribute(&file, &attr_name, attr_value, false).expect("write attr");

        let read_value = read_attribute(&file, &attr_name, false)
            .expect("read attr")
            .expect("attr should exist");

        assert_eq!(read_value, attr_value);
    }

    #[test]
    fn read_nonexistent_attribute_returns_none() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr_name = test_xattr_name("nonexistent");
        let result = read_attribute(&file, &attr_name, false).expect("read attr");
        assert!(result.is_none());
    }

    #[test]
    fn remove_attribute_deletes_xattr() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("test.txt");
        fs::write(&file, "test content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr_name = test_xattr_name("to_remove");
        write_attribute(&file, &attr_name, b"value", false).expect("write attr");

        // Verify it exists
        assert!(
            read_attribute(&file, &attr_name, false)
                .expect("read")
                .is_some()
        );

        remove_attribute(&file, &attr_name, false).expect("remove attr");

        // Verify it's gone
        assert!(
            read_attribute(&file, &attr_name, false)
                .expect("read after remove")
                .is_none()
        );
    }

    #[test]
    fn sync_xattrs_copies_attributes() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr1 = test_xattr_name("attr1");
        let attr2 = test_xattr_name("attr2");
        write_attribute(&source, &attr1, b"value1", false).expect("write attr1");
        write_attribute(&source, &attr2, b"value2", false).expect("write attr2");

        sync_xattrs(&source, &destination, false, None).expect("sync");

        assert_eq!(
            read_attribute(&destination, &attr1, false)
                .expect("read")
                .expect("attr1"),
            b"value1"
        );
        assert_eq!(
            read_attribute(&destination, &attr2, false)
                .expect("read")
                .expect("attr2"),
            b"value2"
        );
    }

    #[test]
    fn sync_xattrs_removes_extra_dest_attributes() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Source has attr1, destination has attr1 and attr2
        let attr1 = test_xattr_name("attr1");
        let attr2 = test_xattr_name("extra");
        write_attribute(&source, &attr1, b"value1", false).expect("write source attr1");
        write_attribute(&destination, &attr1, b"old_value1", false).expect("write dest attr1");
        write_attribute(&destination, &attr2, b"extra_value", false).expect("write dest attr2");

        sync_xattrs(&source, &destination, false, None).expect("sync");

        // attr1 should be updated
        assert_eq!(
            read_attribute(&destination, &attr1, false)
                .expect("read")
                .expect("attr1"),
            b"value1"
        );
        // attr2 should be removed (not in source)
        assert!(
            read_attribute(&destination, &attr2, false)
                .expect("read")
                .is_none()
        );
    }

    #[test]
    fn sync_xattrs_with_filter_skips_filtered_attrs() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let allowed = test_xattr_name("allowed");
        let blocked = test_xattr_name("blocked");
        write_attribute(&source, &allowed, b"allowed_val", false).expect("write allowed");
        write_attribute(&source, &blocked, b"blocked_val", false).expect("write blocked");

        // Filter that only allows attrs NOT containing "blocked"
        let filter = |name: &str| !name.contains("blocked");
        sync_xattrs(&source, &destination, false, Some(&filter)).expect("sync");

        // allowed should be synced
        assert_eq!(
            read_attribute(&destination, &allowed, false)
                .expect("read")
                .expect("allowed"),
            b"allowed_val"
        );
        // blocked should NOT be synced
        assert!(
            read_attribute(&destination, &blocked, false)
                .expect("read")
                .is_none()
        );
    }

    #[test]
    fn is_xattr_permitted_allows_user_namespace() {
        // user.* should always be permitted regardless of platform
        assert!(is_xattr_permitted("user.test"));
        assert!(is_xattr_permitted("user.rsync.%stat"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn is_xattr_permitted_filters_namespaces_on_linux() {
        if rustix::process::geteuid().is_root() {
            // Root: all namespaces except system.*
            assert!(is_xattr_permitted("user.test"));
            assert!(is_xattr_permitted("security.selinux"));
            assert!(is_xattr_permitted("trusted.test"));
            assert!(!is_xattr_permitted("system.posix_acl_access"));
        } else {
            // Non-root: only user.* namespace
            assert!(is_xattr_permitted("user.test"));
            assert!(!is_xattr_permitted("security.selinux"));
            assert!(!is_xattr_permitted("trusted.test"));
            assert!(!is_xattr_permitted("system.posix_acl_access"));
        }
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn is_xattr_permitted_allows_all_on_non_linux() {
        assert!(is_xattr_permitted("user.test"));
        assert!(is_xattr_permitted("com.apple.quarantine"));
        assert!(is_xattr_permitted("security.selinux"));
        assert!(is_xattr_permitted("system.posix_acl_access"));
    }

    #[test]
    fn sync_xattrs_filter_preserves_unfiltered_dest_attrs() {
        let dir = tempdir().expect("create temp dir");
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");
        fs::write(&source, "source").expect("write source");
        fs::write(&destination, "dest").expect("write dest");

        if !xattrs_supported(&source) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let src_attr = test_xattr_name("from_source");
        let preserved = test_xattr_name("preserved");
        write_attribute(&source, &src_attr, b"source_val", false).expect("write source attr");
        write_attribute(&destination, &preserved, b"keep_me", false).expect("write preserved");

        // Filter that blocks "preserved" - it should NOT be touched
        let filter = |name: &str| !name.contains("preserved");
        sync_xattrs(&source, &destination, false, Some(&filter)).expect("sync");

        // src_attr should be synced
        assert_eq!(
            read_attribute(&destination, &src_attr, false)
                .expect("read")
                .expect("src_attr"),
            b"source_val"
        );
        // preserved should still exist (not deleted because filter blocks it)
        assert_eq!(
            read_attribute(&destination, &preserved, false)
                .expect("read")
                .expect("preserved"),
            b"keep_me"
        );
    }

    #[test]
    fn apply_xattrs_from_list_sets_attributes() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            test_xattr_name("attr1"),
            b"value1".to_vec(),
        ));
        list.push(XattrEntry::new(
            test_xattr_name("attr2"),
            b"value2".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false).expect("apply xattrs");

        let attr1 = test_xattr_name("attr1");
        let attr2 = test_xattr_name("attr2");

        assert_eq!(
            read_attribute(&file, &attr1, false)
                .expect("read")
                .expect("attr1"),
            b"value1"
        );
        assert_eq!(
            read_attribute(&file, &attr2, false)
                .expect("read")
                .expect("attr2"),
            b"value2"
        );
    }

    #[test]
    fn apply_xattrs_from_list_removes_stale_dest_attrs() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Pre-set an xattr on the destination that is not in the source list
        let stale = test_xattr_name("stale");
        write_attribute(&file, &stale, b"old", false).expect("write stale");

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            test_xattr_name("kept"),
            b"new_value".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false).expect("apply xattrs");

        // Stale attr should be removed
        assert!(
            read_attribute(&file, &stale, false)
                .expect("read")
                .is_none()
        );

        // New attr should be present
        let kept = test_xattr_name("kept");
        assert_eq!(
            read_attribute(&file, &kept, false)
                .expect("read")
                .expect("kept"),
            b"new_value"
        );
    }

    #[test]
    fn apply_xattrs_from_list_skips_abbreviated_entries() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let mut list = XattrList::new();
        // Abbreviated entry - only has checksum, not full value
        list.push(XattrEntry::abbreviated(
            test_xattr_name("abbrev"),
            vec![0xAA; 16],
            100,
        ));
        // Full entry
        list.push(XattrEntry::new(
            test_xattr_name("full"),
            b"full_value".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false).expect("apply xattrs");

        // Abbreviated entry should not be set
        let abbrev = test_xattr_name("abbrev");
        assert!(
            read_attribute(&file, &abbrev, false)
                .expect("read")
                .is_none()
        );

        // Full entry should be set
        let full = test_xattr_name("full");
        assert_eq!(
            read_attribute(&file, &full, false)
                .expect("read")
                .expect("full"),
            b"full_value"
        );
    }

    #[test]
    fn apply_xattrs_from_list_empty_list_clears_all() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        // Pre-set xattrs on destination
        let attr = test_xattr_name("existing");
        write_attribute(&file, &attr, b"value", false).expect("write existing");

        let list = XattrList::new();
        apply_xattrs_from_list(&file, &list, false).expect("apply empty list");

        // All permitted xattrs should be removed
        assert!(
            read_attribute(&file, &attr, false)
                .expect("read")
                .is_none()
        );
    }

    #[test]
    fn apply_xattrs_from_list_overwrites_existing() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let attr = test_xattr_name("shared");
        write_attribute(&file, &attr, b"old_value", false).expect("write old");

        let mut list = XattrList::new();
        list.push(XattrEntry::new(
            test_xattr_name("shared"),
            b"new_value".to_vec(),
        ));

        apply_xattrs_from_list(&file, &list, false).expect("apply xattrs");

        assert_eq!(
            read_attribute(&file, &attr, false)
                .expect("read")
                .expect("shared"),
            b"new_value"
        );
    }

    #[test]
    fn apply_xattrs_from_list_with_empty_value() {
        let dir = tempdir().expect("create temp dir");
        let file = dir.path().join("dest.txt");
        fs::write(&file, "content").expect("write file");

        if !xattrs_supported(&file) {
            eprintln!("xattrs not supported, skipping test");
            return;
        }

        let mut list = XattrList::new();
        list.push(XattrEntry::new(test_xattr_name("empty_val"), b"".to_vec()));

        apply_xattrs_from_list(&file, &list, false).expect("apply xattrs");

        let attr = test_xattr_name("empty_val");
        let value = read_attribute(&file, &attr, false)
            .expect("read")
            .expect("empty_val should exist");
        assert!(value.is_empty());
    }
}
