//! Hard link tracking for local copy operations.
//!
//! Uses [`FxHashMap`] for fast lookups with integer-based device/inode keys.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use rustc_hash::FxHashMap;

#[cfg(unix)]
pub(crate) struct HardLinkTracker {
    entries: FxHashMap<HardLinkKey, PathBuf>,
}

#[cfg(unix)]
impl Default for HardLinkTracker {
    fn default() -> Self {
        Self {
            entries: FxHashMap::default(),
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HardLinkKey {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl HardLinkTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn existing_target(&self, metadata: &fs::Metadata) -> Option<PathBuf> {
        Self::key(metadata).and_then(|key| self.entries.get(&key).cloned())
    }

    pub(crate) fn record(&mut self, metadata: &fs::Metadata, destination: &Path) {
        if let Some(key) = Self::key(metadata) {
            self.entries.insert(key, destination.to_path_buf());
        }
    }

    fn key(metadata: &fs::Metadata) -> Option<HardLinkKey> {
        use std::os::unix::fs::MetadataExt;

        if metadata.nlink() > 1 {
            Some(HardLinkKey {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
#[derive(Default)]
pub(crate) struct HardLinkTracker;

#[cfg(not(unix))]
impl HardLinkTracker {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) fn existing_target(&self, _metadata: &fs::Metadata) -> Option<PathBuf> {
        None
    }

    pub(crate) fn record(&mut self, _metadata: &fs::Metadata, _destination: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_tracker() {
        let tracker = HardLinkTracker::new();
        let _ = tracker;
    }

    #[test]
    fn default_creates_tracker() {
        let tracker = HardLinkTracker::default();
        let _ = tracker;
    }

    #[test]
    fn existing_target_returns_none_for_new_tracker() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();
        let metadata = std::fs::metadata(&file).unwrap();

        let tracker = HardLinkTracker::new();
        assert!(tracker.existing_target(&metadata).is_none());
    }

    #[test]
    fn record_does_not_panic() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("test.txt");
        std::fs::write(&file, "content").unwrap();
        let metadata = std::fs::metadata(&file).unwrap();

        let mut tracker = HardLinkTracker::new();
        tracker.record(&metadata, Path::new("/dest/test.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_eq() {
        let key1 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key2 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key3 = HardLinkKey {
            device: 2,
            inode: 100,
        };
        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_hash() {
        use std::collections::HashSet;
        let key1 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let key2 = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let mut set = HashSet::new();
        set.insert(key1);
        assert!(set.contains(&key2));
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_debug() {
        let key = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let debug = format!("{key:?}");
        assert!(debug.contains("HardLinkKey"));
    }

    #[cfg(unix)]
    #[test]
    fn hard_link_key_clone() {
        let key = HardLinkKey {
            device: 1,
            inode: 100,
        };
        let cloned = key;
        assert_eq!(key, cloned);
    }
}
