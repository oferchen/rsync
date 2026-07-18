//! Protocol-aware hardlink cohort tracking for the receiver-side apply path.
//!
//! Tracks completed leaders by `hardlink_idx` (gnum) and defers followers
//! that arrive before their leader has been committed. Once a leader is
//! recorded, deferred followers are linked to it via `fast_io::hard_link`.

use std::fs;
use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

/// Tracks completed hardlink leaders by protocol group index during file apply.
///
/// When the receiver commits a leader file to its final destination, it records
/// the `hardlink_idx` (gnum) and destination path. Subsequent followers with the
/// same gnum can then be created as hard links to the leader via `std::fs::hard_link`
/// instead of receiving a separate copy of the data.
///
/// Deferred followers whose leader has not yet been committed are collected and
/// resolved once the leader arrives. This handles out-of-order completion in
/// pipelined transfers.
///
/// # Upstream Reference
///
/// - `hlink.c:finish_hard_link()` - walks deferred follower list after leader transfer
/// - `hlink.c:hard_link_check()` - defers followers when leader is in-progress
#[derive(Debug)]
pub struct HardlinkApplyTracker {
    /// Map from hardlink group index (gnum) to the leader's committed destination path.
    leaders: FxHashMap<u32, PathBuf>,
    /// Followers waiting for their leader to be committed.
    /// Key: leader gnum, Value: list of follower destination paths.
    pub(super) deferred: FxHashMap<u32, Vec<PathBuf>>,
}

/// Result of attempting to apply a hardlink for a follower entry.
#[derive(Debug, PartialEq, Eq)]
pub enum HardlinkApplyResult {
    /// The leader was found and a hard link was created at the follower path.
    Linked,
    /// The leader has not been committed yet; the follower is deferred.
    Deferred,
}

impl HardlinkApplyTracker {
    /// Creates a new tracker with no recorded leaders.
    #[must_use]
    pub fn new() -> Self {
        Self {
            leaders: FxHashMap::default(),
            deferred: FxHashMap::default(),
        }
    }

    /// Records a leader file's committed destination path.
    ///
    /// Call this after the leader file has been fully written and renamed to its
    /// final destination. Any previously deferred followers for this gnum are
    /// returned so the caller can create hard links for them.
    ///
    /// # upstream: hlink.c:finish_hard_link() - creates links for deferred followers
    pub fn record_leader(&mut self, gnum: u32, dest: PathBuf) -> Vec<PathBuf> {
        self.leaders.insert(gnum, dest);
        self.deferred.remove(&gnum).unwrap_or_default()
    }

    /// Attempts to create a hard link for a follower entry.
    ///
    /// If the leader's destination is already known, creates the hard link and
    /// returns `Linked`. Otherwise, defers the follower and returns `Deferred`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the hard link syscall fails (e.g., cross-device,
    /// permission denied, destination already exists).
    ///
    /// # upstream: hlink.c:hard_link_check() - defers or links depending on leader state
    pub fn apply_follower(
        &mut self,
        gnum: u32,
        follower_dest: &Path,
    ) -> std::io::Result<HardlinkApplyResult> {
        if let Some(leader_path) = self.leaders.get(&gnum) {
            if let Some(parent) = follower_dest.parent() {
                fs::create_dir_all(parent)?;
            }
            // Remove existing file at follower path to avoid AlreadyExists.
            if follower_dest.symlink_metadata().is_ok() {
                fs::remove_file(follower_dest)?;
            }
            fast_io::hard_link(leader_path, follower_dest)?;
            Ok(HardlinkApplyResult::Linked)
        } else {
            self.deferred
                .entry(gnum)
                .or_default()
                .push(follower_dest.to_path_buf());
            Ok(HardlinkApplyResult::Deferred)
        }
    }

    /// Returns the leader destination path for a given group index, if known.
    pub fn leader_path(&self, gnum: u32) -> Option<&Path> {
        self.leaders.get(&gnum).map(PathBuf::as_path)
    }

    /// Returns the number of deferred followers across all groups.
    #[must_use]
    pub fn deferred_count(&self) -> usize {
        self.deferred.values().map(Vec::len).sum()
    }

    /// Returns the number of recorded leader groups.
    #[must_use]
    pub fn leader_count(&self) -> usize {
        self.leaders.len()
    }

    /// Resolves all remaining deferred followers by creating hard links.
    ///
    /// Returns the number of hard links successfully created and a list of
    /// errors for any that failed.
    ///
    /// # upstream: hlink.c:finish_hard_link() - final pass for remaining deferred entries
    pub fn resolve_deferred(&mut self) -> (usize, Vec<(PathBuf, std::io::Error)>) {
        let mut linked = 0;
        let mut errors = Vec::new();

        let deferred = std::mem::take(&mut self.deferred);
        for (gnum, followers) in deferred {
            let leader_path = match self.leaders.get(&gnum) {
                Some(p) => p.clone(),
                None => {
                    for follower in followers {
                        errors.push((
                            follower,
                            std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                format!("hardlink leader for group {gnum} never committed"),
                            ),
                        ));
                    }
                    continue;
                }
            };

            for follower in followers {
                if let Some(parent) = follower.parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        errors.push((follower, e));
                        continue;
                    }
                }
                if follower.symlink_metadata().is_ok() {
                    if let Err(e) = fs::remove_file(&follower) {
                        errors.push((follower, e));
                        continue;
                    }
                }
                match fast_io::hard_link(&leader_path, &follower) {
                    Ok(()) => linked += 1,
                    Err(e) => errors.push((follower, e)),
                }
            }
        }

        (linked, errors)
    }
}

impl Default for HardlinkApplyTracker {
    fn default() -> Self {
        Self::new()
    }
}
