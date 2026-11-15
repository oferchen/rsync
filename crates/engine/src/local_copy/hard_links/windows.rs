//! Windows source-side hardlink tracker keyed by `(volume_id, file_index)`.
//!
//! Uses `fast_io::same_fs::file_identity` (`GetFileInformationByHandle`) to
//! detect source files with `nNumberOfLinks > 1` on NTFS/ReFS volumes.

use std::fs;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HardLinkKey {
    volume_id: u64,
    file_index: u64,
}

#[derive(Default)]
pub(crate) struct HardLinkTracker {
    entries: FxHashMap<HardLinkKey, PathBuf>,
    /// Reference paths whose hardlink cohort has already had metadata applied
    /// once. On NTFS the DACL lives on the MFT record (inode); writing the
    /// same DACL through each alias produces N identical inode-level writes.
    ///
    /// upstream: hlink.c::hard_link_check returns 1 for followers so
    /// generator.c:1943-1945 skips set_file_attrs(); the inode keeps the
    /// leader's DACL for free.
    #[cfg(any(test, feature = "acl"))]
    acl_cohort_leaders: rustc_hash::FxHashSet<PathBuf>,
}

impl HardLinkTracker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn existing_target(&self, _metadata: &fs::Metadata) -> Option<PathBuf> {
        None
    }

    pub(crate) fn existing_target_for_path(
        &self,
        source: &Path,
        _metadata: &fs::Metadata,
    ) -> Option<PathBuf> {
        Self::key_for_path(source).and_then(|key| self.entries.get(&key).cloned())
    }

    pub(crate) fn record(&mut self, _metadata: &fs::Metadata, _destination: &Path) {}

    pub(crate) fn record_for_path(
        &mut self,
        source: &Path,
        _metadata: &fs::Metadata,
        destination: &Path,
    ) {
        if let Some(key) = Self::key_for_path(source) {
            self.entries.insert(key, destination.to_path_buf());
        }
    }

    /// Returns `true` the first time `reference` is registered as the source
    /// of a hardlink cohort (the leader); subsequent calls with the same
    /// `reference` return `false` (followers).
    ///
    /// On Windows the leader's `SetNamedSecurityInfoW` call populates the
    /// shared NTFS inode; every follower would re-write the same DACL bytes
    /// to the same inode if not skipped. The wire receiver already skips
    /// follower ACL writes (`create_hardlinks` itemizes only); this tracker
    /// brings the local-copy `--copy-dest` Link branch into parity.
    #[cfg(any(test, feature = "acl"))]
    pub(crate) fn register_acl_cohort_leader(&mut self, reference: &Path) -> bool {
        self.acl_cohort_leaders.insert(reference.to_path_buf())
    }

    fn key_for_path(source: &Path) -> Option<HardLinkKey> {
        let id = fast_io::same_fs::file_identity(source)?;
        if id.nlink > 1 {
            Some(HardLinkKey {
                volume_id: id.volume_id,
                file_index: id.file_index,
            })
        } else {
            None
        }
    }
}
