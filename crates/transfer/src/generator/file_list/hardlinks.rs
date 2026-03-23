//! Hardlink index assignment and UID/GID collection for the generator role.
//!
//! After sorting the file list, hardlink indices are assigned so that
//! entries sharing the same (dev, ino) pair reference a leader entry.
//! UID/GID collection gathers unique ownership values for name-based
//! transfer.
//!
//! # Upstream Reference
//!
//! - `hlink.c:match_hard_links()` - post-sort hardlink index assignment
//! - `uidlist.c:add_uid()` / `add_gid()` - ID collection during file list building

#[cfg(unix)]
use protocol::flist::{DevIno, HardlinkLookup, HardlinkTable};

use super::super::GeneratorContext;

impl GeneratorContext {
    /// Assigns hardlink indices to entries sharing the same (dev, ino) pair.
    ///
    /// Must be called after sorting since indices are post-sort file list positions.
    /// The first occurrence in sorted order becomes the leader (`u32::MAX`); subsequent
    /// occurrences become followers pointing to the leader's index.
    ///
    /// Entries with `hardlink_dev`/`hardlink_ino` set during `create_entry()` are
    /// matched here. After assignment, the temporary dev/ino fields are cleared for
    /// protocol >= 30 (which uses index-based hardlink encoding on the wire).
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:match_hard_links()` - called after `sort_file_list()`
    /// - `hlink.c:idev_find()` - two-level (dev, ino) hashtable lookup
    #[cfg(unix)]
    pub(in crate::generator) fn assign_hardlink_indices(&mut self) {
        let mut table = HardlinkTable::new();

        for i in 0..self.file_list.len() {
            let entry = &self.file_list[i];
            let (Some(dev), Some(ino)) = (entry.hardlink_dev(), entry.hardlink_ino()) else {
                continue;
            };

            let dev_ino = DevIno::new(dev as u64, ino as u64);
            match table.find_or_insert(dev_ino, i as u32) {
                HardlinkLookup::First(_) => {
                    // Leader: mark with u32::MAX (XMIT_HLINK_FIRST on wire)
                    self.file_list[i].set_hardlink_idx(u32::MAX);
                }
                HardlinkLookup::LinkTo(leader_ndx) => {
                    // Follower: point to leader's sorted index
                    self.file_list[i].set_hardlink_idx(leader_ndx);
                }
            }

            // Clear temporary dev/ino for proto 30+ (not sent on wire)
            if self.protocol.as_u8() >= 30 {
                self.file_list[i].set_hardlink_dev(0);
                self.file_list[i].set_hardlink_ino(0);
            }
        }
    }

    /// Collects unique UID/GID values from the file list and looks up their names.
    ///
    /// This must be called after `build_file_list` and before `send_id_lists`.
    /// On non-Unix platforms, this is a no-op since ownership is not preserved.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:add_uid()` / `add_gid()` - called during file list building
    #[cfg(unix)]
    pub fn collect_id_mappings(&mut self) {
        use metadata::id_lookup::{lookup_group_name, lookup_user_name};

        // Skip if numeric_ids is set - no name mapping needed
        if self.config.flags.numeric_ids {
            return;
        }

        self.uid_list.clear();
        self.gid_list.clear();

        for entry in &self.file_list {
            // Collect UIDs if preserving ownership
            if self.config.flags.owner {
                if let Some(uid) = entry.uid() {
                    // Skip expensive lookup if we already have this UID
                    if !self.uid_list.contains(uid) {
                        let name = lookup_user_name(uid).ok().flatten();
                        self.uid_list.add_id(uid, name);
                    }
                }
            }

            // Collect GIDs if preserving group
            if self.config.flags.group {
                if let Some(gid) = entry.gid() {
                    // Skip expensive lookup if we already have this GID
                    if !self.gid_list.contains(gid) {
                        let name = lookup_group_name(gid).ok().flatten();
                        self.gid_list.add_id(gid, name);
                    }
                }
            }
        }
    }

    /// No-op on non-Unix platforms - ownership is not preserved.
    #[cfg(not(unix))]
    pub fn collect_id_mappings(&mut self) {}
}
