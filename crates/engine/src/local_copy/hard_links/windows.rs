//! Non-Unix (Windows and other) source-side hardlink tracker stub.
//!
//! On platforms without inode metadata, source-side hardlink detection is not
//! available, so `existing_target` and `record` are no-ops. The ACL cohort
//! gate is still tracked to keep NTFS DACL writes O(1) per cohort.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct HardLinkTracker {
    /// Reference paths whose hardlink cohort has already had metadata applied
    /// once. On NTFS the DACL lives on the MFT record (inode); writing the
    /// same DACL through each alias produces N identical inode-level writes.
    ///
    /// upstream: hlink.c::hard_link_check returns 1 for followers so
    /// generator.c:1552 exits before set_file_attrs(); the inode keeps the
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

    pub(crate) fn record(&mut self, _metadata: &fs::Metadata, _destination: &Path) {}

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
}
