//! Optimized filesystem metadata operations with caching.
//!
//! This module provides high-performance stat operations that reduce syscall overhead
//! through several strategies:
//!
//! 1. **Cached metadata results**: Stores recent stat results to avoid redundant syscalls
//! 2. **Conditional stat operations**: Only fetches metadata when needed
//!
//! # Performance Impact
//!
//! Profiling shows stat calls can consume 4-5% of execution time in
//! metadata-heavy workloads. Caching recently-accessed metadata eliminates
//! repeated syscalls, and fast-path comparisons avoid a full stat when the
//! cached mode/ownership already answers the question. The readback goes
//! through the libc `lstat(2)` symbol so `fakeroot` observes it.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// A cache for filesystem metadata to avoid redundant stat syscalls.
///
/// This cache is designed for short-lived, sequential metadata operations
/// where the same paths may be stat'd multiple times in quick succession.
/// It's particularly effective during permission and ownership updates.
#[derive(Debug)]
pub struct MetadataCache {
    cache: HashMap<PathBuf, CachedMetadata>,
    hits: usize,
    misses: usize,
}

/// Cached metadata fields extracted from a stat call.
///
/// Contains only the fields needed for permission and ownership checks,
/// keeping the struct small and cheap to clone.
#[derive(Debug, Clone)]
pub struct CachedMetadata {
    /// File mode (permission bits + file type).
    #[cfg(unix)]
    pub mode: u32,
    /// User ID of the file owner.
    #[cfg(unix)]
    pub uid: u32,
    /// Group ID of the file owner.
    #[cfg(unix)]
    pub gid: u32,
    /// Read-only flag (Windows only).
    #[cfg(not(unix))]
    pub readonly: bool,
}

impl MetadataCache {
    /// Creates a new empty metadata cache.
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Creates a cache with pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            cache: HashMap::with_capacity(capacity),
            hits: 0,
            misses: 0,
        }
    }

    /// Gets cached metadata for a path, or fetches it if not cached.
    ///
    /// Fetches via the libc `lstat(2)` symbol on Linux (fakeroot-visible),
    /// falling back to `std::fs::metadata` on other Unix targets.
    pub fn get_or_fetch(&mut self, path: &Path) -> io::Result<CachedMetadata> {
        if let Some(cached) = self.cache.get(path) {
            self.hits += 1;
            return Ok(cached.clone());
        }

        self.misses += 1;
        let meta = fetch_metadata_optimized(path)?;
        self.cache.insert(path.to_path_buf(), meta.clone());
        Ok(meta)
    }

    /// Invalidates cached metadata for a path.
    ///
    /// Call this after modifying a file's metadata to ensure subsequent
    /// lookups reflect the new state.
    pub fn invalidate(&mut self, path: &Path) {
        self.cache.remove(path);
    }

    /// Clears all cached metadata.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Returns the number of cache hits.
    pub fn hits(&self) -> usize {
        self.hits
    }

    /// Returns the number of cache misses.
    pub fn misses(&self) -> usize {
        self.misses
    }

    /// Compares current file mode with cached value without fetching metadata.
    #[cfg(unix)]
    pub fn mode_matches(&mut self, path: &Path, expected_mode: u32) -> io::Result<bool> {
        let cached = self.get_or_fetch(path)?;
        Ok((cached.mode & 0o7777) == (expected_mode & 0o7777))
    }

    /// Compares current ownership with cached values without fetching metadata.
    #[cfg(unix)]
    pub fn ownership_matches(
        &mut self,
        path: &Path,
        expected_uid: u32,
        expected_gid: u32,
    ) -> io::Result<bool> {
        let cached = self.get_or_fetch(path)?;
        Ok(cached.uid == expected_uid && cached.gid == expected_gid)
    }
}

impl Default for MetadataCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetches metadata for the permission/ownership quick-check.
///
/// On Linux, reads through the libc `lstat(2)` symbol (see
/// [`fetch_metadata_lstat`]) so `fakeroot` observes the readback. On other
/// Unix targets, uses `std::fs::metadata`, which also routes through libc.
#[cfg(all(unix, target_os = "linux"))]
fn fetch_metadata_optimized(path: &Path) -> io::Result<CachedMetadata> {
    fetch_metadata_lstat(path)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn fetch_metadata_optimized(path: &Path) -> io::Result<CachedMetadata> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    Ok(CachedMetadata {
        mode: metadata.mode(),
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}

#[cfg(not(unix))]
fn fetch_metadata_optimized(path: &Path) -> io::Result<CachedMetadata> {
    let metadata = std::fs::metadata(path)?;
    Ok(CachedMetadata {
        readonly: metadata.permissions().readonly(),
    })
}

/// Reads mode/uid/gid via the libc `lstat(2)` symbol through `nix`.
///
/// The readback must go through a libc symbol rather than a rustix raw statx
/// syscall so `fakeroot`'s LD_PRELOAD interposition observes it. rustix issues
/// `statx` as a raw syscall, which bypasses the LD_PRELOAD wrapper and returns
/// the *real* on-disk owner/mode. Under `fakeroot` that makes the batch
/// applier's `needs_chown` / `needs_chmod` quick-check (via [`MetadataCache`])
/// wrongly conclude the destination "already matches" and skip a chown/chmod
/// that fakeroot would have faked. `nix::sys::stat::lstat` calls the libc
/// `lstat` symbol, which fakeroot interposes, so the faked owner/mode is seen.
/// `lstat` keeps the `AT_SYMLINK_NOFOLLOW` semantics the old statx path used.
// upstream: syscall.c:do_lstat() calls the lstat(2) libc symbol.
#[cfg(all(unix, target_os = "linux"))]
fn fetch_metadata_lstat(path: &Path) -> io::Result<CachedMetadata> {
    let stat = nix::sys::stat::lstat(path).map_err(io::Error::from)?;

    Ok(CachedMetadata {
        mode: stat.st_mode as u32,
        uid: stat.st_uid,
        gid: stat.st_gid,
    })
}

/// Checks if file permissions match without allocating full Metadata.
///
/// This is a fast-path operation that uses cached data when available.
#[cfg(unix)]
pub fn check_mode_matches(
    cache: &mut MetadataCache,
    path: &Path,
    expected_mode: u32,
) -> io::Result<bool> {
    cache.mode_matches(path, expected_mode)
}

/// Checks if file ownership matches without allocating full Metadata.
///
/// This is a fast-path operation that uses cached data when available.
#[cfg(unix)]
pub fn check_ownership_matches(
    cache: &mut MetadataCache,
    path: &Path,
    expected_uid: u32,
    expected_gid: u32,
) -> io::Result<bool> {
    cache.ownership_matches(path, expected_uid, expected_gid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn metadata_cache_new_is_empty() {
        let cache = MetadataCache::new();
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 0);
    }

    #[test]
    fn metadata_cache_with_capacity() {
        let cache = MetadataCache::with_capacity(10);
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 0);
    }

    #[test]
    fn get_or_fetch_caches_result() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let mut cache = MetadataCache::new();

        let result1 = cache.get_or_fetch(&path);
        assert!(result1.is_ok());
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 1);

        let result2 = cache.get_or_fetch(&path);
        assert!(result2.is_ok());
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn cache_hit_miss_ratio() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path1 = temp.path().join("file1.txt");
        let path2 = temp.path().join("file2.txt");
        fs::write(&path1, b"content1").expect("write");
        fs::write(&path2, b"content2").expect("write");

        let mut cache = MetadataCache::new();

        cache.get_or_fetch(&path1).expect("fetch");
        cache.get_or_fetch(&path1).expect("fetch");
        cache.get_or_fetch(&path1).expect("fetch");
        cache.get_or_fetch(&path2).expect("fetch");
        cache.get_or_fetch(&path2).expect("fetch");

        assert_eq!(cache.hits(), 3);
        assert_eq!(cache.misses(), 2);
    }

    #[test]
    fn invalidate_removes_cached_entry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let mut cache = MetadataCache::new();

        cache.get_or_fetch(&path).expect("fetch");
        assert_eq!(cache.misses(), 1);

        cache.invalidate(&path);

        cache.get_or_fetch(&path).expect("fetch");
        assert_eq!(cache.misses(), 2);
    }

    #[test]
    fn clear_removes_all_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path1 = temp.path().join("test1.txt");
        let path2 = temp.path().join("test2.txt");
        fs::write(&path1, b"content1").expect("write");
        fs::write(&path2, b"content2").expect("write");

        let mut cache = MetadataCache::new();

        cache.get_or_fetch(&path1).expect("fetch");
        cache.get_or_fetch(&path2).expect("fetch");
        assert_eq!(cache.misses(), 2);

        cache.clear();

        cache.get_or_fetch(&path1).expect("fetch");
        cache.get_or_fetch(&path2).expect("fetch");
        assert_eq!(cache.misses(), 4);
    }

    #[test]
    fn multiple_paths_are_cached_independently() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths: Vec<PathBuf> = (0..10)
            .map(|i| {
                let path = temp.path().join(format!("file{i}.txt"));
                fs::write(&path, format!("content{i}")).expect("write");
                path
            })
            .collect();

        let mut cache = MetadataCache::new();

        for path in &paths {
            cache.get_or_fetch(path).expect("fetch");
        }
        assert_eq!(cache.misses(), 10);
        assert_eq!(cache.hits(), 0);

        for path in &paths {
            cache.get_or_fetch(path).expect("fetch");
        }
        assert_eq!(cache.misses(), 10);
        assert_eq!(cache.hits(), 10);
    }

    #[cfg(unix)]
    #[test]
    fn mode_matches_returns_true_for_matching_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&path, perms).expect("chmod");

        let mut cache = MetadataCache::new();
        let matches = cache.mode_matches(&path, 0o644);
        assert!(matches.is_ok());
        assert!(matches.unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn mode_matches_returns_false_for_different_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&path, perms).expect("chmod");

        let mut cache = MetadataCache::new();
        let matches = cache.mode_matches(&path, 0o755);
        assert!(matches.is_ok());
        assert!(!matches.unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn mode_matches_uses_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&path, perms).expect("chmod");

        let mut cache = MetadataCache::new();

        cache.mode_matches(&path, 0o644).expect("mode_matches");
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);

        cache.mode_matches(&path, 0o644).expect("mode_matches");
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn ownership_matches_returns_true_for_matching_ownership() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(&path).expect("metadata");
        let uid = meta.uid();
        let gid = meta.gid();

        let mut cache = MetadataCache::new();
        let matches = cache.ownership_matches(&path, uid, gid);
        assert!(matches.is_ok());
        assert!(matches.unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_matches_returns_false_for_different_ownership() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let mut cache = MetadataCache::new();
        // Use UIDs unlikely to belong to the test process.
        let matches = cache.ownership_matches(&path, 99999, 99999);
        assert!(matches.is_ok());
        assert!(!matches.unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_matches_uses_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(&path).expect("metadata");
        let uid = meta.uid();
        let gid = meta.gid();

        let mut cache = MetadataCache::new();

        cache
            .ownership_matches(&path, uid, gid)
            .expect("ownership_matches");
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);

        cache
            .ownership_matches(&path, uid, gid)
            .expect("ownership_matches");
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 1);
    }

    #[test]
    fn fetch_metadata_optimized_works_for_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let result = fetch_metadata_optimized(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn fetch_metadata_optimized_works_for_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("subdir");
        fs::create_dir(&dir).expect("mkdir");

        let result = fetch_metadata_optimized(&dir);
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn fetch_metadata_optimized_works_for_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        let link = temp.path().join("link.txt");
        fs::write(&target, b"content").expect("write");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let result = fetch_metadata_optimized(&link);
        assert!(result.is_ok());
    }

    #[test]
    fn fetch_metadata_optimized_fails_for_nonexistent() {
        let result = fetch_metadata_optimized(Path::new("/nonexistent/path/12345"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn get_or_fetch_handles_error_paths() {
        let mut cache = MetadataCache::new();

        let result = cache.get_or_fetch(Path::new("/nonexistent/xyz/abc"));
        assert!(result.is_err());

        // Errors must not populate the cache.
        assert_eq!(cache.misses(), 1);
        assert_eq!(cache.hits(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn check_mode_matches_helper_function() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o644);
        fs::set_permissions(&path, perms).expect("chmod");

        let mut cache = MetadataCache::new();
        let matches = check_mode_matches(&mut cache, &path, 0o644);
        assert!(matches.is_ok());
        assert!(matches.unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn check_ownership_matches_helper_function() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(&path).expect("metadata");
        let uid = meta.uid();
        let gid = meta.gid();

        let mut cache = MetadataCache::new();
        let matches = check_ownership_matches(&mut cache, &path, uid, gid);
        assert!(matches.is_ok());
        assert!(matches.unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fetch_metadata_lstat_works() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let result = fetch_metadata_lstat(&path);
        assert!(result.is_ok(), "unexpected error: {result:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fetch_metadata_lstat_returns_correct_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("mode_test.txt");
        fs::write(&path, b"content").expect("write");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");

        let meta = fetch_metadata_lstat(&path).expect("lstat");
        assert_eq!(meta.mode & 0o7777, 0o644);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fetch_metadata_lstat_returns_correct_ownership() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("owner_test.txt");
        fs::write(&path, b"content").expect("write");

        let std_meta = fs::symlink_metadata(&path).expect("metadata");
        let expected_uid = std_meta.uid();
        let expected_gid = std_meta.gid();

        let meta = fetch_metadata_lstat(&path).expect("lstat");
        assert_eq!(meta.uid, expected_uid);
        assert_eq!(meta.gid, expected_gid);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fetch_metadata_lstat_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("lstat_dir");
        fs::create_dir(&dir).expect("mkdir");

        assert!(fetch_metadata_lstat(&dir).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fetch_metadata_lstat_symlink() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("lstat_target.txt");
        let link = temp.path().join("lstat_link.txt");
        fs::write(&target, b"content").expect("write");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("chmod");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        // lstat keeps SYMLINK_NOFOLLOW semantics, so this stats the link
        // itself (mode 0o120xxx) rather than the 0o600 target.
        let meta = fetch_metadata_lstat(&link).expect("lstat");
        assert_eq!(
            meta.mode & 0o170000,
            0o120000,
            "must stat the symlink itself"
        );
    }

    #[test]
    fn cache_default_is_empty() {
        let cache = MetadataCache::default();
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_readonly_metadata() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let result = fetch_metadata_optimized(&path);
        assert!(result.is_ok());

        let meta = result.unwrap();
        let _readonly = meta.readonly;
    }

    #[test]
    fn stress_test_many_cache_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut cache = MetadataCache::with_capacity(1000);

        for i in 0..1000 {
            let path = temp.path().join(format!("file{i}.txt"));
            fs::write(&path, format!("content{i}")).expect("write");
            cache.get_or_fetch(&path).expect("fetch");
        }

        assert_eq!(cache.misses(), 1000);
        assert_eq!(cache.hits(), 0);

        for i in (0..1000).step_by(17) {
            let path = temp.path().join(format!("file{i}.txt"));
            cache.get_or_fetch(&path).expect("fetch");
        }

        assert!(cache.hits() > 0);
    }

    #[test]
    fn invalidate_nonexistent_path_is_safe() {
        let mut cache = MetadataCache::new();
        let path = PathBuf::from("/this/path/does/not/exist");

        cache.invalidate(&path);
    }

    #[test]
    fn paths_with_special_characters() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file with spaces.txt");
        fs::write(&path, b"content").expect("write");

        let mut cache = MetadataCache::new();
        let result = cache.get_or_fetch(&path);
        assert!(result.is_ok());

        let unicode_path = temp.path().join("файл.txt");
        fs::write(&unicode_path, b"content").expect("write");
        let result = cache.get_or_fetch(&unicode_path);
        assert!(result.is_ok());
    }
}
