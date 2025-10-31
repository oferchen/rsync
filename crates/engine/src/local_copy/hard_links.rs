//! Hard link tracking for local copy operations.

use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::collections::HashMap;

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct HardLinkTracker {
    entries: HashMap<HardLinkKey, PathBuf>,
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
        Self {
            entries: HashMap::new(),
        }
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
