//! Generic receiver helpers accepting `FileEntryAccessor`.
//!
//! Provides `FileEntryAccessor`-generic equivalents of receiver-side consumer
//! functions that currently take `&FileEntry`. These enable the receiver to
//! work with both the legacy `FileEntry` (heap-allocated) and the arena-backed
//! `FlatFileEntry` through a single interface.
//!
//! Each function mirrors its concrete counterpart in the same crate:
//!
//! - [`quick_check_matches_generic`] - [`super::quick_check::quick_check_matches`]
//! - [`dest_mtime_newer_generic`] - [`super::quick_check::dest_mtime_newer`]
//! - [`is_hardlink_follower_generic`] - [`super::quick_check::is_hardlink_follower`]
//! - [`apply_acls_generic`] - [`super::apply_acls_from_receiver_cache`]
//!
//! # Feature Gate
//!
//! All code in this module is behind `#[cfg(feature = "flat-flist")]`.

use std::fs;
use std::path::Path;

use protocol::flist::FileEntryAccessor;

/// Pure-function quick-check: compares destination stat against a generic entry.
///
/// Returns `true` when the destination file matches the source entry (skip
/// transfer). Equivalent to [`super::quick_check::quick_check_matches`] but
/// accepts any `T: FileEntryAccessor` instead of a concrete `&FileEntry`.
///
/// Follows upstream `generator.c:617 quick_check_ok()` evaluation order:
/// 1. Size mismatch - always needs transfer
/// 2. `always_checksum` - compute file checksum and compare (ignores mtime)
/// 3. `size_only` - size matched, skip transfer
/// 4. `!preserve_times` (implies `ignore_times`) - force transfer
/// 5. mtime comparison
pub fn quick_check_matches_generic<T: FileEntryAccessor + ?Sized>(
    entry: &T,
    dest_path: &Path,
    dest_meta: &fs::Metadata,
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
) -> bool {
    // upstream: generator.c:621 - size check first
    if dest_meta.len() != entry.size() {
        return false;
    }
    // upstream: generator.c:626 - always_checksum compares file checksums
    if let Some(algorithm) = always_checksum {
        return match entry.checksum() {
            Some(expected) => {
                file_checksum_matches(dest_path, dest_meta.len(), algorithm, expected)
            }
            None => false,
        };
    }
    // upstream: generator.c:632 - `if (size_only) return 1;`
    if size_only {
        return true;
    }
    // upstream: generator.c:635 - ignore_times forces transfer
    if !preserve_times {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() == entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| d.as_secs() as i64 == entry.mtime())
    }
}

/// Returns `true` when the destination mtime is strictly newer than the source.
///
/// Used by `--update` (`-u`) to skip files where the destination is already
/// newer. Equivalent to [`super::quick_check::dest_mtime_newer`] but accepts
/// any `T: FileEntryAccessor`.
///
/// upstream: generator.c:1709
pub fn dest_mtime_newer_generic<T: FileEntryAccessor + ?Sized>(
    dest_meta: &fs::Metadata,
    source_entry: &T,
) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() > source_entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| (d.as_secs() as i64) > source_entry.mtime())
    }
}

/// Returns true if this entry is a hardlink follower that should be created as
/// a hard link rather than transferred via delta.
///
/// A follower has XMIT_HLINKED set but NOT XMIT_HLINK_FIRST. Leaders have
/// both flags set. Equivalent to [`super::quick_check::is_hardlink_follower`]
/// but accepts any `T: FileEntryAccessor`.
///
/// # Upstream Reference
///
/// - `generator.c:1539` - `F_HLINK_NOT_FIRST(file)` check
/// - `hlink.c:284` - `hard_link_check()` called for non-first entries
pub fn is_hardlink_follower_generic<T: FileEntryAccessor + ?Sized>(entry: &T) -> bool {
    entry.hlinked() && !entry.hlink_first()
}

/// Applies ACLs from the receiver's ACL cache to a destination file.
///
/// Looks up the entry's `acl_ndx` and optional `def_acl_ndx` in the cache and
/// applies the corresponding ACL to `destination`. No-op when `acl_cache` is
/// `None` or the entry has no ACL index.
///
/// Equivalent to [`super::apply_acls_from_receiver_cache`] but accepts any
/// `T: FileEntryAccessor`.
///
/// # Upstream Reference
///
/// Mirrors upstream `set_file_attrs()` in receiver.c which calls `set_acl()`
/// after setting permissions, times, and ownership.
pub fn apply_acls_generic<T: FileEntryAccessor + ?Sized>(
    destination: &Path,
    entry: &T,
    acl_cache: Option<&protocol::acl::AclCache>,
    follow_symlinks: bool,
) -> Result<(), metadata::MetadataError> {
    let cache = match acl_cache {
        Some(c) => c,
        None => return Ok(()),
    };
    let access_ndx = match entry.acl_ndx() {
        Some(ndx) => ndx,
        None => return Ok(()),
    };
    metadata::apply_acls_from_cache(
        destination,
        cache,
        access_ndx,
        entry.def_acl_ndx(),
        follow_symlinks,
        Some(entry.mode()),
    )
}

/// Computes a file's checksum and compares it against an expected value.
///
/// Used by `--checksum` (`-c`) mode. Returns `true` when checksums match.
///
/// upstream: checksum.c:402 `file_checksum()`
fn file_checksum_matches(
    path: &Path,
    file_size: u64,
    algorithm: protocol::ChecksumAlgorithm,
    expected: &[u8],
) -> bool {
    use std::io::Read;

    use crate::delta_apply::ChecksumVerifier;

    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
    let mut buf = [0u8; 64 * 1024];
    let mut remaining = file_size;
    while remaining > 0 {
        let to_read = buf.len().min(remaining as usize);
        if file.read_exact(&mut buf[..to_read]).is_err() {
            return false;
        }
        hasher.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }
    let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len = hasher.finalize_into(&mut digest);
    let cmp_len = expected.len().min(len);
    digest[..cmp_len] == expected[..cmp_len]
}

#[cfg(test)]
mod tests {
    use protocol::flist::FileEntry;
    use protocol::flist::FileEntryAccessor;

    use super::*;

    // -- is_hardlink_follower_generic --

    /// Verifies a plain file is not a hardlink follower.
    #[test]
    fn non_hardlink_entry_is_not_follower() {
        let entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
        assert!(!is_hardlink_follower_generic(&entry));
    }

    /// Verifies a hardlink leader (hlink_first=true) is not a follower.
    #[test]
    fn hardlink_leader_is_not_follower() {
        let mut entry = FileEntry::new_file("leader.txt".into(), 100, 0o644);
        entry.set_hlinked(true);
        entry.set_hlink_first(true);
        assert!(!is_hardlink_follower_generic(&entry));
    }

    /// Verifies a hardlink follower (hlinked=true, hlink_first=false) is detected.
    #[test]
    fn hardlink_follower_detected() {
        let mut entry = FileEntry::new_file("follower.txt".into(), 100, 0o644);
        entry.set_hlinked(true);
        entry.set_hlink_first(false);
        assert!(is_hardlink_follower_generic(&entry));
    }

    /// Verifies generic and concrete implementations agree for non-hardlink entries.
    #[test]
    fn follower_generic_matches_concrete() {
        let entry = FileEntry::new_file("f.txt".into(), 100, 0o644);
        assert_eq!(
            is_hardlink_follower_generic(&entry),
            super::super::quick_check::is_hardlink_follower(&entry),
        );
    }

    /// Verifies generic and concrete implementations agree for hardlink leaders.
    #[test]
    fn follower_generic_matches_concrete_leader() {
        let mut entry = FileEntry::new_file("leader.txt".into(), 100, 0o644);
        entry.set_hlinked(true);
        entry.set_hlink_first(true);
        assert_eq!(
            is_hardlink_follower_generic(&entry),
            super::super::quick_check::is_hardlink_follower(&entry),
        );
    }

    /// Verifies generic and concrete implementations agree for hardlink followers.
    #[test]
    fn follower_generic_matches_concrete_follower() {
        let mut entry = FileEntry::new_file("follower.txt".into(), 100, 0o644);
        entry.set_hlinked(true);
        assert_eq!(
            is_hardlink_follower_generic(&entry),
            super::super::quick_check::is_hardlink_follower(&entry),
        );
    }

    // -- quick_check_matches_generic --

    /// Verifies that a size mismatch causes quick-check to return false.
    #[test]
    fn quick_check_size_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("size_mismatch.txt");
        std::fs::write(&path, b"short").unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file("size_mismatch.txt".into(), 999, 0o644);
        assert!(!quick_check_matches_generic(
            &entry, &path, &meta, true, false, None
        ));
    }

    /// Verifies that `size_only` mode returns true when sizes match.
    #[test]
    fn quick_check_size_only_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("size_only.txt");
        let data = b"hello world";
        std::fs::write(&path, data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file("size_only.txt".into(), data.len() as u64, 0o644);
        assert!(quick_check_matches_generic(
            &entry, &path, &meta, true, true, None
        ));
    }

    /// Verifies that `!preserve_times` forces transfer (returns false).
    #[test]
    fn quick_check_no_preserve_times_forces_transfer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_times.txt");
        let data = b"content";
        std::fs::write(&path, data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file("no_times.txt".into(), data.len() as u64, 0o644);
        assert!(!quick_check_matches_generic(
            &entry, &path, &meta, false, false, None
        ));
    }

    /// Verifies the generic quick-check agrees with the concrete implementation.
    #[test]
    fn quick_check_generic_matches_concrete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parity.txt");
        let data = b"parity check data";
        std::fs::write(&path, data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file("parity.txt".into(), data.len() as u64, 0o644);

        // size_only mode
        assert_eq!(
            quick_check_matches_generic(&entry, &path, &meta, true, true, None),
            super::super::quick_check::quick_check_matches(&entry, &path, &meta, true, true, None),
        );

        // no preserve_times
        assert_eq!(
            quick_check_matches_generic(&entry, &path, &meta, false, false, None),
            super::super::quick_check::quick_check_matches(
                &entry, &path, &meta, false, false, None
            ),
        );
    }

    // -- dest_mtime_newer_generic --

    /// Verifies a destination with mtime 0 is not newer than a source with
    /// a positive mtime.
    #[test]
    fn dest_mtime_not_newer_when_source_is_future() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("older.txt");
        std::fs::write(&path, b"old").unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        // Set source mtime far in the future so dest is older.
        let mut entry = FileEntry::new_file("older.txt".into(), 3, 0o644);
        entry.set_mtime(i64::MAX, 0);
        assert!(!dest_mtime_newer_generic(&meta, &entry));
    }

    /// Verifies the generic dest_mtime_newer agrees with the concrete version.
    #[test]
    fn dest_mtime_newer_generic_matches_concrete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mtime_parity.txt");
        std::fs::write(&path, b"data").unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let mut entry = FileEntry::new_file("mtime_parity.txt".into(), 4, 0o644);
        entry.set_mtime(i64::MAX, 0);

        assert_eq!(
            dest_mtime_newer_generic(&meta, &entry),
            super::super::quick_check::dest_mtime_newer(&meta, &entry),
        );
    }

    // -- apply_acls_generic --

    /// Verifies that apply_acls_generic returns Ok when no cache is provided.
    #[test]
    fn apply_acls_no_cache_is_noop() {
        let entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let result = apply_acls_generic(Path::new("/nonexistent"), &entry, None, true);
        assert!(result.is_ok());
    }

    /// Verifies that apply_acls_generic returns Ok when the entry has no ACL index.
    #[test]
    fn apply_acls_no_acl_ndx_is_noop() {
        let entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let cache = protocol::acl::AclCache::new();
        let result = apply_acls_generic(Path::new("/nonexistent"), &entry, Some(&cache), true);
        assert!(result.is_ok());
    }

    // -- Generic trait dispatch verification --

    /// Verifies that `quick_check_matches_generic` can be called via trait object.
    #[test]
    fn quick_check_via_trait_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trait_obj.txt");
        let data = b"trait object test";
        std::fs::write(&path, data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file("trait_obj.txt".into(), data.len() as u64, 0o644);
        let acc: &dyn FileEntryAccessor = &entry;

        // size_only mode should match
        assert!(quick_check_matches_generic(
            acc, &path, &meta, true, true, None
        ));
    }

    /// Verifies that `is_hardlink_follower_generic` can be called via trait object.
    #[test]
    fn hardlink_follower_via_trait_object() {
        let mut entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        entry.set_hlinked(true);
        let acc: &dyn FileEntryAccessor = &entry;
        assert!(is_hardlink_follower_generic(acc));
    }

    /// Verifies that `dest_mtime_newer_generic` can be called via trait object.
    #[test]
    fn dest_mtime_newer_via_trait_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trait_mtime.txt");
        std::fs::write(&path, b"m").unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let mut entry = FileEntry::new_file("trait_mtime.txt".into(), 1, 0o644);
        entry.set_mtime(i64::MAX, 0);
        let acc: &dyn FileEntryAccessor = &entry;
        assert!(!dest_mtime_newer_generic(&meta, acc));
    }

    /// Verifies that `apply_acls_generic` can be called via trait object.
    #[test]
    fn apply_acls_via_trait_object() {
        let entry = FileEntry::new_file("f.txt".into(), 0, 0o644);
        let acc: &dyn FileEntryAccessor = &entry;
        let result = apply_acls_generic(Path::new("/nonexistent"), acc, None, true);
        assert!(result.is_ok());
    }

    /// Verifies that generic quick-check detects a checksum mismatch when
    /// entry has no checksum in always_checksum mode.
    #[test]
    fn quick_check_always_checksum_no_entry_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no_cksum.txt");
        let data = b"no checksum on entry";
        std::fs::write(&path, data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();

        let entry = FileEntry::new_file("no_cksum.txt".into(), data.len() as u64, 0o644);
        // No checksum set on entry, always_checksum should return false
        assert!(!quick_check_matches_generic(
            &entry,
            &path,
            &meta,
            true,
            false,
            Some(protocol::ChecksumAlgorithm::MD5),
        ));
    }
}
