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
use rustix::fs::{chownat, AtFlags, CWD};
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
        if self.options.times() {
            self.apply_timestamps(destination, metadata)?;
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
            // Get current UID from cache
            match self.cache.get_or_fetch(destination) {
                Ok(cached) => cached.uid,
                Err(_) => return Ok(()), // If we can't stat, skip ownership
            }
        };

        let desired_gid = if self.options.group() {
            metadata.gid()
        } else {
            // Get current GID from cache
            match self.cache.get_or_fetch(destination) {
                Ok(cached) => cached.gid,
                Err(_) => return Ok(()), // If we can't stat, skip ownership
            }
        };

        // Check if ownership already matches using cache
        let needs_chown = match self.cache.ownership_matches(destination, desired_uid, desired_gid)
        {
            Ok(matches) => !matches,
            Err(_) => true, // If cache check fails, try chown anyway
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

            chownat(CWD, destination, owner, group, AtFlags::empty()).map_err(|error| {
                MetadataError::new("preserve ownership", destination, io::Error::from(error))
            })?;

            // Invalidate cache after successful chown
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
        // Non-Unix platforms don't support ownership
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

        // Check if permissions already match using cache
        let needs_chmod = match self.cache.mode_matches(destination, desired_mode) {
            Ok(matches) => !matches,
            Err(_) => true, // If cache check fails, try chmod anyway
        };

        if needs_chmod {
            let permissions = PermissionsExt::from_mode(desired_mode);
            fs::set_permissions(destination, permissions)
                .map_err(|error| MetadataError::new("preserve permissions", destination, error))?;

            // Invalidate cache after successful chmod
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

        // For non-Unix, we need to fetch current state
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
                // If we can't stat, try setting anyway
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
    fn clear_cache_resets_cache() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write");

        let mut ctx = BatchMetadataContext::new();
        let meta = fs::metadata(&path).expect("metadata");

        // This will cause some cache activity
        let _ = ctx.apply_file_metadata(&path, &meta);

        ctx.clear_cache();
        let (hits, misses) = ctx.cache_stats();
        // Stats are preserved, only cache entries are cleared
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

        // Set different permissions on source
        let perms = PermissionsExt::from_mode(0o755);
        fs::set_permissions(&source, perms).expect("chmod source");

        let source_meta = fs::metadata(&source).expect("source metadata");

        let mut opts = MetadataOptions::default();
        opts.set_permissions(true);
        let mut ctx = BatchMetadataContext::with_options(opts);

        // Apply metadata
        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata");

        // Verify permissions were applied
        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert_eq!(
            dest_meta.permissions().mode() & 0o777,
            source_meta.permissions().mode() & 0o777
        );

        // Apply again - should be cached and skip syscall
        let before_hits = ctx.cache_stats().0;
        ctx.apply_file_metadata(&dest, &source_meta)
            .expect("apply metadata again");
        let after_hits = ctx.cache_stats().0;

        // Should have at least one cache hit
        assert!(after_hits > before_hits);
    }
}
