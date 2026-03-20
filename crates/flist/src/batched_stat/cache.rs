//! Sharded stat cache for parallel metadata lookups.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Number of shards for the stat cache.
///
/// 16 shards provides good parallelism without excessive memory overhead.
/// Must be a power of 2 for efficient modular hashing.
const SHARD_COUNT: usize = 16;

/// A single shard in the stat cache.
type StatShard = Mutex<HashMap<PathBuf, Arc<fs::Metadata>>>;

/// Cache for batched stat operations.
///
/// Uses sharded locking (16 independent `Mutex<HashMap>` shards) to reduce
/// contention under parallel stat workloads. Paths are routed to shards via
/// a fast hash of their byte representation.
#[derive(Debug)]
pub struct BatchedStatCache {
    shards: Arc<[StatShard; SHARD_COUNT]>,
}

impl Default for BatchedStatCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BatchedStatCache {
    /// Creates a new empty cache with 16 shards.
    #[must_use]
    pub fn new() -> Self {
        Self {
            shards: Arc::new(std::array::from_fn(|_| Mutex::new(HashMap::new()))),
        }
    }

    /// Creates a cache with pre-allocated capacity distributed across shards.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let per_shard = capacity / SHARD_COUNT + 1;
        Self {
            shards: Arc::new(std::array::from_fn(|_| {
                Mutex::new(HashMap::with_capacity(per_shard))
            })),
        }
    }

    /// Routes a path to a shard index using FNV-1a hash.
    fn shard_index(path: &Path) -> usize {
        let bytes = path.as_os_str().as_encoded_bytes();
        let mut hash: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash as usize & (SHARD_COUNT - 1)
    }

    /// Gets cached metadata for a path, if present.
    pub fn get(&self, path: &Path) -> Option<Arc<fs::Metadata>> {
        let idx = Self::shard_index(path);
        self.shards[idx].lock().unwrap().get(path).cloned()
    }

    /// Inserts metadata into the cache.
    pub fn insert(&self, path: PathBuf, metadata: fs::Metadata) {
        let idx = Self::shard_index(&path);
        self.shards[idx]
            .lock()
            .unwrap()
            .insert(path, Arc::new(metadata));
    }

    /// Checks the cache and fetches if not present.
    ///
    /// Returns cached metadata if available, otherwise performs stat and caches.
    pub fn get_or_fetch(
        &self,
        path: &Path,
        follow_symlinks: bool,
    ) -> io::Result<Arc<fs::Metadata>> {
        let idx = Self::shard_index(path);

        // Fast path: check shard
        {
            let shard = self.shards[idx].lock().unwrap();
            if let Some(metadata) = shard.get(path) {
                return Ok(Arc::clone(metadata));
            }
        }

        // Slow path: fetch outside lock, then insert
        let metadata = if follow_symlinks {
            fs::metadata(path)?
        } else {
            fs::symlink_metadata(path)?
        };

        let metadata = Arc::new(metadata);
        self.shards[idx]
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), Arc::clone(&metadata));
        Ok(metadata)
    }

    /// Fetches metadata for multiple paths in parallel.
    ///
    /// Uses rayon to parallelize stat syscalls across CPU cores.
    /// Each result is cached for future lookups. Sharded locking
    /// ensures minimal contention between parallel workers.
    #[cfg(feature = "parallel")]
    pub fn stat_batch(
        &self,
        paths: &[&Path],
        follow_symlinks: bool,
    ) -> Vec<io::Result<Arc<fs::Metadata>>> {
        paths
            .par_iter()
            .map(|path| self.get_or_fetch(path, follow_symlinks))
            .collect()
    }

    /// Fetches metadata for multiple paths sequentially.
    ///
    /// Non-parallel fallback when the `parallel` feature is disabled.
    #[cfg(not(feature = "parallel"))]
    pub fn stat_batch(
        &self,
        paths: &[&Path],
        follow_symlinks: bool,
    ) -> Vec<io::Result<Arc<fs::Metadata>>> {
        paths
            .iter()
            .map(|path| self.get_or_fetch(path, follow_symlinks))
            .collect()
    }

    /// Clears all cached metadata across all shards.
    pub fn clear(&self) {
        for shard in self.shards.iter() {
            shard.lock().unwrap().clear();
        }
    }

    /// Returns the number of cached entries across all shards.
    #[must_use]
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().unwrap().len()).sum()
    }

    /// Returns true if the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.lock().unwrap().is_empty())
    }
}

impl Clone for BatchedStatCache {
    fn clone(&self) -> Self {
        Self {
            shards: Arc::clone(&self.shards),
        }
    }
}
