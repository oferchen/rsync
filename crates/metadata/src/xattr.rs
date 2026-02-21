use crate::error::MetadataError;
use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::path::Path;

/// Checks whether an xattr name is permitted for the current privilege level.
///
/// Mirrors upstream rsync `xattrs.c:64-68, 254-257`:
/// - Non-root on Linux: only `user.*` xattrs are accessible.
/// - Root on Linux: all namespaces except `system.*`.
/// - On non-Linux Unix (macOS, FreeBSD): no namespace filtering (different model).
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

/// On non-Linux Unix (macOS, FreeBSD), all xattr names are permitted.
#[cfg(not(target_os = "linux"))]
fn is_xattr_permitted(_name: &str) -> bool {
    true
}

fn map_xattr_error(context: &'static str, path: &Path, error: io::Error) -> MetadataError {
    MetadataError::new(context, path, error)
}

fn list_attributes(path: &Path, follow_symlinks: bool) -> Result<Vec<OsString>, MetadataError> {
    let attrs = if follow_symlinks {
        xattr::list_deref(path)
    } else {
        xattr::list(path)
    }
    .map_err(|error| map_xattr_error("list extended attributes", path, error))?;
    Ok(attrs
        .filter(|name| is_xattr_permitted(&name.to_string_lossy()))
        .collect())
}

fn read_attribute(
    path: &Path,
    name: &OsString,
    follow_symlinks: bool,
) -> Result<Option<Vec<u8>>, MetadataError> {
    let result = if follow_symlinks {
        xattr::get_deref(path, name)
    } else {
        xattr::get(path, name)
    };
    result.map_err(|error| map_xattr_error("read extended attribute", path, error))
}

fn write_attribute(
    path: &Path,
    name: &OsString,
    value: &[u8],
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let result = if follow_symlinks {
        xattr::set_deref(path, name, value)
    } else {
        xattr::set(path, name, value)
    };
    result.map_err(|error| map_xattr_error("write extended attribute", path, error))
}

fn remove_attribute(
    path: &Path,
    name: &OsString,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let result = if follow_symlinks {
        xattr::remove_deref(path, name)
    } else {
        xattr::remove(path, name)
    };
    result.map_err(|error| map_xattr_error("remove extended attribute", path, error))
}

/// Synchronises the extended attributes from `source` to `destination`.
pub fn sync_xattrs(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<(), MetadataError> {
    let source_attrs = list_attributes(source, follow_symlinks)?;
    let mut retained = HashSet::with_capacity(source_attrs.len());

    for name in &source_attrs {
        retained.insert(name.clone());
        let allow = filter.is_none_or(|predicate| predicate(&name.to_string_lossy()));

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

        let allow = filter.is_none_or(|predicate| predicate(&name.to_string_lossy()));

        if allow {
            remove_attribute(destination, name, follow_symlinks)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use tempfile::tempdir;

    /// Helper to check if xattrs are supported on the current filesystem.
    fn xattrs_supported(path: &Path) -> bool {
        let test_name = OsStr::new("user.test_support");
        match xattr::set(path, test_name, b"test") {
            Ok(()) => {
                let _ = xattr::remove(path, test_name);
                true
            }
            Err(_) => false,
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
                .all(|a| !a.to_string_lossy().contains("user.custom"))
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

        let attr_name = OsString::from("user.test_attr");
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

        let attr_name = OsString::from("user.nonexistent");
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

        let attr_name = OsString::from("user.to_remove");
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

        let attr1 = OsString::from("user.attr1");
        let attr2 = OsString::from("user.attr2");
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
        let attr1 = OsString::from("user.attr1");
        let attr2 = OsString::from("user.extra");
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

        let allowed = OsString::from("user.allowed");
        let blocked = OsString::from("user.blocked");
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

        let src_attr = OsString::from("user.from_source");
        let preserved = OsString::from("user.preserved");
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
}
