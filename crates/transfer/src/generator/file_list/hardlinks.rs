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
use protocol::flist::{DevIno, HardlinkLookup, HardlinkTable, ProcessRole};

use super::super::GeneratorContext;

impl GeneratorContext {
    /// Assigns hardlink indices to entries sharing the same (dev, ino) pair.
    ///
    /// Must run on the final send order: after sorting, and under INC_RECURSE
    /// after `partition_file_list_for_inc_recurse` has reordered the list into
    /// segments. The first entry of a group in send order becomes the leader (`u32::MAX`, XMIT_HLINK_FIRST on the
    /// wire); later ones become followers carrying the leader's wire NDX.
    ///
    /// The wire NDX of flat index `i` is `ndx_start + i` plus one gap for every
    /// sub-list opened at or before `i` (flist.c:2966 opens each sub-list at
    /// `prev->ndx_start + prev->used + 1`). A follower whose leader sits in an earlier sub-list
    /// therefore names that leader's absolute NDX, which upstream's receiver
    /// resolves through `prior_hlinks` (hlink.c:125-141) and rejects when it
    /// lands before the current sub-list without a recorded leader.
    ///
    /// Entries with `hardlink_dev`/`hardlink_ino` set during `create_entry()` are
    /// matched here. After assignment, the temporary dev/ino fields are cleared for
    /// protocol >= 30 (which uses index-based hardlink encoding on the wire).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:599-606` `send_file_entry()` - the idev table persists across
    ///   sub-lists and stores `first_ndx + ndx`, the send-time wire NDX
    /// - `hlink.c:idev_find()` - two-level (dev, ino) hashtable lookup
    #[cfg(unix)]
    pub(in crate::generator) fn assign_hardlink_indices(&mut self) {
        let mut table = HardlinkTable::new();
        let ndx_start = self.incremental.ndx_map.first_ndx_start() as u32;
        let segments = &self.incremental.pending_segments;
        let mut gaps = 0usize;

        for i in 0..self.file_list.len() {
            while segments.get(gaps).is_some_and(|seg| seg.flist_start <= i) {
                gaps += 1;
            }
            let entry = &self.file_list[i];
            let (Some(dev), Some(ino)) = (entry.hardlink_dev(), entry.hardlink_ino()) else {
                continue;
            };

            let wire_ndx = ndx_start + (i + gaps) as u32;
            let dev_ino = DevIno::new(dev as u64, ino as u64);
            // upstream: hlink.c HLINK debug emissions - announce per-device
            // hashtable creation on first observation.
            table.announce_device(ProcessRole::Generator, dev_ino.dev);
            match table.find_or_insert(dev_ino, wire_ndx) {
                HardlinkLookup::First(_) => {
                    // Leader: mark with u32::MAX (XMIT_HLINK_FIRST on wire)
                    self.file_list[i].set_hardlink_idx(u32::MAX);
                }
                HardlinkLookup::LinkTo(leader_wire_ndx) => {
                    // Follower: point to leader's wire NDX
                    self.file_list[i].set_hardlink_idx(leader_wire_ndx);
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
    pub fn collect_id_mappings(&mut self) -> std::io::Result<()> {
        use metadata::id_lookup::{
            lookup_group_name_cached, lookup_user_name_cached, no_id_unless_converter_failed,
        };

        // upstream: flist.c:490 gates add_uid()/add_gid() on `!numeric_ids`, so
        // the id-list is populated only when names are active (`numeric_ids ==
        // 0`). Both daemon-forced and explicit numeric-ids leave it empty.
        if self.config.flags.numeric_ids.maps_numeric() {
            return Ok(());
        }

        self.uid_list.clear();
        self.gid_list.clear();

        for entry in &self.file_list {
            // Collect UIDs if preserving ownership
            if self.config.flags.owner
                && let Some(uid) = entry.uid()
            {
                // Skip expensive lookup if we already have this UID
                if !self.uid_list.contains(uid) {
                    // A converter that could not answer is not "this uid
                    // has no name": upstream exits rather than send a file
                    // list whose ownership the operator's converter never
                    // vouched for (clientserver.c:1326, :1333).
                    let name = no_id_unless_converter_failed(lookup_user_name_cached(uid))?;
                    self.uid_list.add_id(uid, name);
                }
            }

            // Collect GIDs if preserving group
            if self.config.flags.group
                && let Some(gid) = entry.gid()
            {
                // Skip expensive lookup if we already have this GID
                if !self.gid_list.contains(gid) {
                    let name = no_id_unless_converter_failed(lookup_group_name_cached(gid))?;
                    self.gid_list.add_id(gid, name);
                }
            }
        }

        Ok(())
    }

    /// No-op on non-Unix platforms - ownership is not preserved.
    #[cfg(not(unix))]
    pub fn collect_id_mappings(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
