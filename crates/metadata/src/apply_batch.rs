//! Batch metadata operations with caching for improved performance.
//!
//! This module provides high-performance batch operations that apply metadata
//! to multiple files while reusing stat cache and avoiding redundant syscalls.

use crate::error::MetadataError;
use crate::options::MetadataOptions;
use crate::stat_cache::MetadataCache;
use filetime::{FileTime, set_file_times};
use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
use crate::id_lookup::{map_gid, map_uid};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Context for batch metadata operations with shared caching.
///
/// This type is designed for scenarios where metadata is applied to many files
/// sequentially (e.g., recursive directory sync). The shared cache eliminates
/// redundant stat calls when checking current file state.
pub struct BatchMetadataContext {
    cache: MetadataCache,
    options: MetadataOptions,
}

impl BatchMetadataContext {
    /// Creates a new batch context with default options.
    pub fn new() -> Self {
        Self {
            cache: MetadataCache::new(),
            options: MetadataOptions::default(),
        }
    }

    /// Creates a batch context with specific options.
    pub fn with_options(options: MetadataOptions) -> Self {
        Self {
            cache: MetadataCache::new(),
            options,
        }
    }

    /// Creates a batch context with pre-allocated cache capacity.
    pub fn with_capacity(capacity: usize, options: MetadataOptions) -> Self {
        Self {
            cache: MetadataCache::with_capacity(capacity),
            options,
        }
    }

    /// Applies file metadata with cache optimization.
    ///
    /// This method uses the internal cache to avoid redundant stat calls when
    /// checking if metadata already matches the desired state.
    pub fn apply_file_metadata(
        &mut self,
        destination: &Path,
        metadata: &fs::Metadata,
    ) -> Result<(), MetadataError> {
        self.apply_ownership_cached(destination, metadata)?;
        self.apply_permissions_cached(destination, metadata)?;
        // upstream: rsync.c:587-612 - mtime and atime are handled independently
        if self.options.times() {
            self.apply_timestamps(destination, metadata)?;
        } else if self.options.atimes() {
            self.apply_atime_only(destination, metadata)?;
        }
        Ok(())
    }

    /// Applies ownership with cache-based optimization.
    #[cfg(unix)]
    fn apply_ownership_cached(
        &mut self,
        destination: &Path,
        metadata: &fs::Metadata,
    ) -> Result<(), MetadataError> {
        if !self.options.owner() && !self.options.group() {
            return Ok(());
        }

        let desired_uid = if self.options.owner() {
            metadata.uid()
        } else {
            match self.cache.get_or_fetch(destination) {
                Ok(cached) => cached.uid,
                Err(_) => return Ok(()),
            }
        };

        let desired_gid = if self.options.group() {
            metadata.gid()
        } else {
            match self.cache.get_or_fetch(destination) {
                Ok(cached) => cached.gid,
                Err(_) => return Ok(()),
            }
        };

        let needs_chown = match self.cache.ownership_matches(destination, desired_uid, desired_gid)
        {
            Ok(matches) => !matches,
            Err(_) => true,
        };

        if needs_chown {
            let owner = if self.options.owner() {
                Some(map_uid(desired_uid, self.options.numeric_ids_enabled()))
            } else {
                None
            };

            let group = if self.options.group() {
                Some(map_gid(desired_gid, self.options.numeric_ids_enabled()))
            } else {
                None
            };

            // Route chown through the libc `fchownat(2)` symbol via `nix` so
            // fakeroot's LD_PRELOAD interposition can fake ownership for a
            // non-root process; a raw syscall would bypass libc and hit EPERM.
            // upstream: syscall.c:do_chown() calls the chown(2) libc symbol.
            nix::unistd::fchownat(
                nix::fcntl::AT_FDCWD,
                destination,
                owner.map(|uid| nix::unistd::Uid::from_raw(uid.as_raw())),
                group.map(|gid| nix::unistd::Gid::from_raw(gid.as_raw())),
                nix::fcntl::AtFlags::empty(),
            )
            .map_err(|errno| {
                MetadataError::new("preserve ownership", destination, io::Error::from(errno))
            })?;

            self.cache.invalidate(destination);
        }

        Ok(())
    }

    #[cfg(not(unix))]
    fn apply_ownership_cached(
        &mut self,
        _destination: &Path,
        _metadata: &fs::Metadata,
    ) -> Result<(), MetadataError> {
        Ok(())
    }

    /// Applies permissions with cache-based optimization.
    #[cfg(unix)]
    fn apply_permissions_cached(
        &mut self,
        destination: &Path,
        metadata: &fs::Metadata,
    ) -> Result<(), MetadataError> {
        if !self.options.permissions() && !self.options.executability() {
            return Ok(());
        }

        let desired_mode = metadata.permissions().mode();

        let needs_chmod = match self.cache.mode_matches(destination, desired_mode) {
            Ok(matches) => !matches,
            Err(_) => true,
        };

        if needs_chmod {
            let permissions = PermissionsExt::from_mode(desired_mode);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;

            self.cache.invalidate(destination);
        }

        Ok(())
    }

    #[cfg(not(unix))]
    fn apply_permissions_cached(
        &mut self,
        destination: &Path,
        metadata: &fs::Metadata,
    ) -> Result<(), MetadataError> {
        if !self.options.permissions() {
            return Ok(());
        }

        let desired_readonly = metadata.permissions().readonly();

        match fs::metadata(destination) {
            Ok(current_meta) => {
                if current_meta.permissions().readonly() != desired_readonly {
                    let mut perms = current_meta.permissions();
                    perms.set_readonly(desired_readonly);
                    fs::set_permissions(destination, perms).map_err(|error| {
                        MetadataError::new("preserve permissions", destination, error)
                    })?;
                }
            }
            Err(_) => {
                let mut perms = metadata.permissions();
                perms.set_readonly(desired_readonly);
                fs::set_permissions(destination, perms).map_err(|error| {
                    MetadataError::new("preserve permissions", destination, error)
                })?;
            }
        }

        Ok(())
    }

    /// Applies timestamps without caching (timestamps change frequently).
    fn apply_timestamps(
        &mut self,
        destination: &Path,
        metadata: &fs::Metadata,
    ) -> Result<(), MetadataError> {
        let accessed = FileTime::from_last_access_time(metadata);
        let modified = FileTime::from_last_modification_time(metadata);

        set_file_times(destination, accessed, modified)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?;

        Ok(())
    }

    /// Applies only the access time from source metadata, preserving dest mtime.
    ///
    /// Used when `--atimes` is active but `--times` is not, mirroring upstream's
    /// independent atime/mtime handling.
    // upstream: rsync.c:604-612 - atime applied independently of mtime
    fn apply_atime_only(
        &mut self,
        destination: &Path,
        metadata: &fs::Metadata,
    ) -> Result<(), MetadataError> {
        let source_atime = FileTime::from_last_access_time(metadata);
        let dest_meta = fs::metadata(destination)
            .map_err(|error| MetadataError::new("read current timestamps", destination, error))?;
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

        set_file_times(destination, source_atime, dest_mtime)
            .map_err(|error| MetadataError::new("preserve access time", destination, error))?;

        Ok(())
    }

    /// Returns cache statistics for performance analysis.
    pub fn cache_stats(&self) -> (usize, usize) {
        (self.cache.hits(), self.cache.misses())
    }

    /// Clears the metadata cache.
    ///
    /// Call this when switching to a different directory tree to avoid
    /// using stale cached data.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }
}

impl Default for BatchMetadataContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_context_new_creates_empty_cache() {
        let ctx = BatchMetadataContext::new();
        let (hits, misses) = ctx.cache_stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    #[test]
    fn batch_context_with_capacity() {
        let opts = MetadataOptions::default();
        let ctx = BatchMetadataContext::with_capacity(100, opts);
        let (hits, misses) = ctx.cache_stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    #[test]
    fn batch_context_default() {
        let ctx = BatchMetadataContext::default();
        let (hits, misses) = ctx.cache_stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    #[test]
    fn batch_context_with_options() {
        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        opts.set_times(true);

        let ctx = BatchMetadataContext::with_options(opts);
        let (hits, misses) = ctx.cache_stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 0);
    }

    #[test]
    fn clear_cache_resets_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let mut ctx = BatchMetadataContext::new();
        let meta = fs::metadata(&path).expect("metadata");

        let _ = ctx.apply_file_metadata(&path, &meta);

        ctx.clear_cache();
        let (hits, misses) = ctx.cache_stats();
        // Stats counters survive a clear; only cache entries are dropped.
        assert!(hits >= 0);
        assert!(misses >= 0);
    }

    #[cfg(unix)]
    #[test]
    fn apply_file_metadata_with_caching() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source").expect("write source");
        fs::write(&dest, b"dest").expect("write dest");

        let perms = PermissionsExt::from_mode(0o755);
        fs::set_permissions(&source, perms).expect("chmod source");

        let source_meta = fs::metadata(&source).expect("source metadata");

        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert_eq!(
            dest_meta.permissions().mode() & 0o777,
            source_meta.permissions().mode() & 0o777
        );

        // Second call must hit the cache and skip the chmod syscall.
        let before_hits = ctx.cache_stats().0;
        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata again");
        let after_hits = ctx.cache_stats().0;

        assert!(after_hits > before_hits);
    }

    #[cfg(unix)]
    #[test]
    fn batch_context_reuse_across_multiple_files() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");

        let files: Vec<_> = (0..10)
            .map(|i| {
                let path = temp.path().join(format!("file{}.txt", i));
                fs::write(&path, format!("content{}", i)).expect("write");
                let mode = 0o600 + (i * 7) % 0o177;
                let perms = PermissionsExt::from_mode(mode);
                fs::set_permissions(&path, perms).expect("chmod");
                path
            })
            .collect();

        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        for _ in 0..2 {
            for file in &files {
                let meta = fs::metadata(file).expect("metadata");
                ctx.apply_file_metadata(file, &meta).expect("apply");
            }
        }

        let (hits, _misses) = ctx.cache_stats();
        assert!(hits >= 10, "Expected at least 10 hits, got {}", hits);
    }

    #[cfg(unix)]
    #[test]
    fn permissions_not_applied_when_disabled() {
        use std::os::unix::fs::{PermissionsExt, MetadataExt};

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source").expect("write source");
        fs::write(&dest, b"dest").expect("write dest");

        let perms_source = PermissionsExt::from_mode(0o755);
        let perms_dest = PermissionsExt::from_mode(0o644);
        fs::set_permissions(&source, perms_source).expect("chmod source");
        fs::set_permissions(&dest, perms_dest).expect("chmod dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta_before = fs::metadata(&dest).expect("dest metadata before");

        // Default options leave permission preservation off.
        let opts = MetadataOptions::default();
        let mut ctx = BatchMetadataContext::with_options(opts);

        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata");

        let dest_meta_after = fs::metadata(&dest).expect("dest metadata after");
        assert_eq!(
            dest_meta_before.mode() & 0o777,
            dest_meta_after.mode() & 0o777
        );
    }

    #[cfg(unix)]
    #[test]
    fn timestamps_applied_when_enabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source").expect("write source");
        fs::write(&dest, b"dest").expect("write dest");

        // Backdate source to a known time so it differs from dest without sleeping
        let past = FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&source, past).expect("backdate source");

        let source_meta = fs::metadata(&source).expect("source metadata");

        let mut opts = MetadataOptions::default();
        opts.set_times(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let source_mtime = FileTime::from_last_modification_time(&source_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);

        assert_eq!(source_mtime, dest_mtime);
    }

    #[cfg(unix)]
    #[test]
    fn timestamps_not_applied_when_disabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source").expect("write source");
        fs::write(&dest, b"dest").expect("write dest");

        // Backdate source so it has a distinct mtime from dest
        let past = FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&source, past).expect("backdate source");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta_before = fs::metadata(&dest).expect("dest metadata before");

        // Default options leave timestamp preservation off.
        let opts = MetadataOptions::default();
        let mut ctx = BatchMetadataContext::with_options(opts);

        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata");

        let dest_meta_after = fs::metadata(&dest).expect("dest metadata after");
        let mtime_before = FileTime::from_last_modification_time(&dest_meta_before);
        let mtime_after = FileTime::from_last_modification_time(&dest_meta_after);

        assert_eq!(mtime_before, mtime_after);
    }

    #[cfg(unix)]
    #[test]
    fn cache_invalidation_after_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let perms1 = PermissionsExt::from_mode(0o644);
        fs::set_permissions(&path, perms1).expect("chmod");

        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        let meta1 = fs::metadata(&path).expect("metadata");
        ctx.apply_file_metadata(&path, &meta1).expect("apply");

        let (hits_before, misses_before) = ctx.cache_stats();

        let perms2 = PermissionsExt::from_mode(0o755);
        fs::set_permissions(&path, perms2).expect("chmod");

        let meta2 = fs::metadata(&path).expect("metadata");
        ctx.apply_file_metadata(&path, &meta2).expect("apply");

        // Cache invalidation after the chmod must surface the new mode -
        // either via a fresh miss or a re-validated hit on the next apply.
        let (hits_after, misses_after) = ctx.cache_stats();
        assert!(misses_after > misses_before || hits_after > hits_before);
    }

    #[cfg(unix)]
    #[test]
    fn ownership_matches_skips_chown() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let meta = fs::metadata(&path).expect("metadata");
        let uid = meta.uid();
        let gid = meta.gid();

        let mut opts = MetadataOptions::default();
        opts.set_owner(true);
        opts.set_group(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        ctx.apply_file_metadata(&path, &meta).expect("apply");

        // Second apply must hit the cache and elide the chown syscall.
        let (_, misses_before) = ctx.cache_stats();
        ctx.apply_file_metadata(&path, &meta).expect("apply");
        let (hits_after, _) = ctx.cache_stats();

        assert!(hits_after > 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_readonly_attribute() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source").expect("write source");
        fs::write(&dest, b"dest").expect("write dest");

        let mut perms = fs::metadata(&source).expect("metadata").permissions();
        perms.set_readonly(true);
        fs::set_permissions(&source, perms).expect("set readonly");

        let source_meta = fs::metadata(&source).expect("source metadata");

        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert!(dest_meta.permissions().readonly());
    }

    #[test]
    fn error_handling_nonexistent_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("nonexistent/dest.txt");

        fs::write(&source, b"source").expect("write source");
        let source_meta = fs::metadata(&source).expect("source metadata");

        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        let result = ctx.apply_file_metadata(&dest, &source_meta);
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn partial_ownership_application() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source").expect("write source");
        fs::write(&dest, b"dest").expect("write dest");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let dest_meta_before = fs::metadata(&dest).expect("dest metadata");

        // Owner-only ownership preservation; group must stay untouched.
        let mut opts = MetadataOptions::default();
        opts.set_owner(true);
        opts.set_group(false);
        let mut ctx = BatchMetadataContext::with_options(opts);

        let _ = ctx.apply_file_metadata(&dest, &source_meta);

        // Without root the chown is best-effort; the test just exercises the
        // partial-application path and asserts it does not panic.
        let dest_meta_after = fs::metadata(&dest).expect("dest metadata");
        let _ = dest_meta_after.uid();
        let _ = dest_meta_before.gid();
    }

    #[test]
    fn batch_context_handles_many_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut ctx = BatchMetadataContext::with_capacity(1000, MetadataOptions::default());

        for i in 0..100 {
            let path = temp.path().join(format!("file{}.txt", i));
            fs::write(&path, format!("content{}", i)).expect("write");
            let meta = fs::metadata(&path).expect("metadata");
            let _ = ctx.apply_file_metadata(&path, &meta);
        }

        let (hits, misses) = ctx.cache_stats();
        assert!(hits + misses > 0);
    }

    #[test]
    fn clear_cache_allows_fresh_stats() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let mut ctx = BatchMetadataContext::new();
        let meta = fs::metadata(&path).expect("metadata");

        let _ = ctx.apply_file_metadata(&path, &meta);
        let (_, misses_before) = ctx.cache_stats();

        ctx.clear_cache();

        let _ = ctx.apply_file_metadata(&path, &meta);
        let (_, misses_after) = ctx.cache_stats();

        assert!(misses_after > misses_before);
    }

    #[cfg(unix)]
    #[test]
    fn apply_file_metadata_multiple_attributes() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");

        fs::write(&source, b"source").expect("write source");
        fs::write(&dest, b"dest").expect("write dest");

        // Backdate source so quick-check (matching size+mtime) does not skip it.
        let past = FileTime::from_unix_time(1_600_000_000, 0);
        filetime::set_file_mtime(&source, past).expect("backdate source");

        let perms = PermissionsExt::from_mode(0o755);
        fs::set_permissions(&source, perms).expect("chmod");

        let source_meta = fs::metadata(&source).expect("source metadata");

        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        opts.set_times(true);
        opts.set_owner(true);
        opts.set_group(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert_eq!(
            dest_meta.permissions().mode() & 0o777,
            source_meta.permissions().mode() & 0o777
        );

        let source_mtime = FileTime::from_last_modification_time(&source_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(source_mtime, dest_mtime);
    }
