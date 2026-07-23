//! Unix-specific source-side hardlink tracking by (device, inode).

use std::fs;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

#[derive(Default)]
pub(crate) struct HardLinkTracker {
    entries: FxHashMap<HardLinkKey, PathBuf>,
    /// Reference paths whose hardlink cohort has already had metadata applied
    /// once. Used to make per-inode metadata writes (e.g. Windows DACLs) O(1)
    /// per cohort instead of O(N) per follower.
    ///
    /// upstream: hlink.c::hard_link_check returns 1 for followers so
    /// generator.c:1552 exits before set_file_attrs(); the inode keeps the
    /// leader's metadata for free.
    #[cfg(any(test, feature = "acl"))]
    acl_cohort_leaders: rustc_hash::FxHashSet<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct HardLinkKey {
    pub(super) device: u64,
    pub(super) inode: u64,
}

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

    /// Returns `true` the first time `reference` is registered as the source
    /// of a hardlink cohort (the leader); subsequent calls with the same
    /// `reference` return `false` (followers).
    ///
    /// The leader is the destination whose creation should trigger a
    /// per-inode metadata write (POSIX ACL on Unix, NTFS DACL on Windows).
    /// Followers share the same underlying inode and inherit the leader's
    /// metadata without an additional write.
    #[cfg(any(test, feature = "acl"))]
    pub(crate) fn register_acl_cohort_leader(&mut self, reference: &Path) -> bool {
        self.acl_cohort_leaders.insert(reference.to_path_buf())
    }

    pub(super) fn key(metadata: &fs::Metadata) -> Option<HardLinkKey> {
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
