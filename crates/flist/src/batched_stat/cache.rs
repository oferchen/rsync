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
///
/// # Confinement constraint
///
/// **This type is unreached, and it must not be wired as written.** It is keyed
/// on a path *string*, so a hit returns the metadata the path denoted when the
/// entry was filled, not the metadata it denotes now. There is no invalidation
/// hook and no generation counter: nothing here observes filesystem mutation,
/// so an entry stays authoritative for the cache's whole lifetime.
///
/// Wiring it into a walk that crosses a confinement boundary would reopen the
/// TOCTOU window the per-component resolver exists to close. An attacker who
/// replaces a path component with a symlink between the fill and the hit gets
/// the pre-swap answer back, and on a miss the re-resolve
/// (`fs::metadata`/`fs::symlink_metadata` on an absolute path) walks the
/// mutated path with the process's full authority rather than through the
/// per-component ownership walk.
///
/// Any future wiring must key on a resolved handle - a held directory fd plus a
/// single component - rather than on a path. The sibling `DirectoryStatBatch`
/// already has that shape: it holds the directory open and issues `fstatat`
/// relative to that fd, so a component swapped after the open cannot redirect
/// the lookup. See `docs/design/path-confinement-resolver-api.md` section 5,
/// which states the contract this type would breach.
///
/// # Upstream
///
/// Upstream has no counterpart to re-key against: there is no pathname-keyed
/// stat cache anywhere in rsync's file-list build. `flist.c:1547-1556`'s
/// `lastdir` interns a directory *name* (a `char *`) and a derived component
/// count, never a `STRUCT_STAT`; `make_file()` destructures each stat into
/// scalar `file_struct` fields and drops it. Every hashtable upstream keeps is
/// keyed on integers, not paths - `(dev, ino)` in `hlink.c:74-84`, `gnum`,
/// `fs_dev`, an xattr-content checksum - and the one path-shaped cache
/// (`syscall.c:3540-3548`) holds open dirfds, which upstream's own comment
/// argues are race-safe precisely because an fd pins an inode where a resolved
/// path snapshot does not. Where a stale snapshot would be dangerous upstream
/// re-stats and diffs against the flist value (`sender.c:428`, `failed_op =
/// "re-lstat"`). An oc-invented cache with no upstream analogue is a design
/// decision, not an inherited one.
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
        // Ordering: results must correspond 1:1 with input paths by position.
        // Preserved by par_iter().map().collect() (rayon preserves index order).
        // Violation mismatches metadata with paths, corrupting file list construction.
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
