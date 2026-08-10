//! UID/GID name-to-ID mapping list reception.
//!
//! Translates remote user/group names from the sender into local numeric IDs
//! using the platform's user database. On non-Unix platforms the lists are
//! still consumed from the wire, but all lookups return `None`, mirroring
//! upstream rsync's effective `--numeric-ids` behaviour on those targets.

use std::io::{self, Read};

use protocol::CompatibilityFlags;

#[cfg(unix)]
use metadata::id_lookup::{lookup_group_by_name, lookup_user_by_name};

use super::super::ReceiverContext;

impl ReceiverContext {
    /// Reads UID/GID name-to-ID mapping lists from the sender.
    ///
    /// When `--numeric-ids` is not set, the sender transmits name mappings so the
    /// receiver can translate remote user/group names to local numeric IDs. When
    /// `--numeric-ids` is set, no mappings are sent and numeric IDs are used as-is.
    ///
    /// # Wire Format
    ///
    /// Each list contains `(varint id, byte name_len, name_bytes)*` tuples terminated
    /// by `varint 0`. With `ID0_NAMES` compat flag, an additional name for id=0
    /// follows the terminator.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:460-479` - `recv_id_list()`
    /// - Condition: `(preserve_uid || preserve_acls) && numeric_ids <= 0`
    #[cfg(unix)]
    pub(crate) fn receive_id_lists<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<()> {
        // upstream: uidlist.c:465,473 - the name-list is read for `numeric_ids
        // <= 0`. Only an explicit client --numeric-ids (`> 0`) skips the read;
        // daemon-forced numeric-ids (-1) still consumes the list from the wire
        // (the sender's own numeric_ids may be 0), so guarding on the bare bool
        // here would misread the list bytes as the next NDX and desync.
        if self.config.flags.numeric_ids.is_explicit() {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        // upstream: uidlist.c:465 - read the uid list when preserving ownership
        // OR ACLs (`preserve_uid || preserve_acls`). The sender injects named
        // ACL-entry ids into this list so the receiver can remap them.
        if self.config.flags.owner || self.config.flags.acls {
            self.uid_list.read_with_kind(
                reader,
                id0_names,
                protocol_version,
                Some(protocol::idlist::IdKind::Uid),
                |name| lookup_user_by_name(name).ok().flatten(),
            )?;
        }

        // upstream: uidlist.c:473 - read the gid list under the same condition.
        if self.config.flags.group || self.config.flags.acls {
            self.gid_list.read_with_kind(
                reader,
                id0_names,
                protocol_version,
                Some(protocol::idlist::IdKind::Gid),
                |name| lookup_group_by_name(name).ok().flatten(),
            )?;
        }

        Ok(())
    }

    /// Reads UID/GID name-to-ID mapping lists from the sender (non-Unix platforms).
    ///
    /// On non-Unix platforms (e.g., Windows), this reads the ID lists from the wire
    /// but does not resolve user/group names to local IDs since the platform lacks
    /// the POSIX user database. All name lookups return `None`, causing ownership
    /// to fall back to numeric IDs.
    ///
    /// # Platform Behavior
    ///
    /// This matches upstream rsync behavior where platforms without user/group
    /// databases effectively operate as if `--numeric-ids` was specified.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:460-479` - `recv_id_list()`
    /// - Condition: `(preserve_uid || preserve_acls) && numeric_ids <= 0`
    #[cfg(not(unix))]
    pub(crate) fn receive_id_lists<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<()> {
        // upstream: uidlist.c:465,473 - read for `numeric_ids <= 0`; only an
        // explicit client --numeric-ids (`> 0`) skips the wire read.
        if self.config.flags.numeric_ids.is_explicit() {
            return Ok(());
        }

        let id0_names = self
            .compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::ID0_NAMES));

        let protocol_version = self.protocol.as_u8();

        // upstream: uidlist.c:465,473 - `preserve_uid || preserve_acls` /
        // `preserve_gid || preserve_acls`.
        if self.config.flags.owner || self.config.flags.acls {
            self.uid_list.read_with_kind(
                reader,
                id0_names,
                protocol_version,
                Some(protocol::idlist::IdKind::Uid),
                |_| None,
            )?;
        }

        if self.config.flags.group || self.config.flags.acls {
            self.gid_list.read_with_kind(
                reader,
                id0_names,
                protocol_version,
                Some(protocol::idlist::IdKind::Gid),
                |_| None,
            )?;
        }

        Ok(())
    }

    /// Remaps every file-list entry's uid/gid from the sender's raw ids to the
    /// local ids resolved from the transmitted name lists.
    ///
    /// Mirrors upstream `recv_id_list()`, which rewrites `F_OWNER`/`F_GROUP` for
    /// every flist entry via `match_uid`/`match_gid` after the name lists are
    /// read. Without this the receiver would chown files to the raw sender id,
    /// which is wrong when that id is absent locally or bound to a different
    /// name than on the sender - the very purpose of the non-numeric name list
    /// is that ownership follows the *name* across hosts with different id
    /// namespaces. Ids not present in the sent list keep their raw value,
    /// matching `match_uid`'s `recv_add_id(..., NULL)` fallback (`--usermap`,
    /// which upstream folds into `match_uid`, is applied later in
    /// `metadata::apply`).
    ///
    /// Only applied for `numeric_ids == 0` (upstream's `!numeric_ids` gate): an
    /// explicit or daemon-forced numeric transfer keeps the raw ids. Non-root
    /// uid remaps are harmless because the chown is gated away in
    /// `metadata::apply`, matching upstream where a non-root receiver cannot
    /// chown a file to another owner.
    ///
    /// # Upstream Reference
    ///
    /// - `uidlist.c:483-494` - `recv_id_list()` remap loop
    pub(crate) fn remap_flist_ownership_from_id_lists(&mut self) {
        if !self.config.flags.numeric_ids.is_off() {
            return;
        }
        if self.config.flags.owner {
            let uid_map = self.uid_list.resolved_map();
            if !uid_map.is_empty() {
                for entry in self.file_list.iter_mut() {
                    if let Some(uid) = entry.uid()
                        && let Some(&local) = uid_map.get(&uid)
                    {
                        entry.set_uid(local);
                    }
                }
            }
        }
        if self.config.flags.group {
            let gid_map = self.gid_list.resolved_map();
            if !gid_map.is_empty() {
                for entry in self.file_list.iter_mut() {
                    if let Some(gid) = entry.gid()
                        && let Some(&local) = gid_map.get(&gid)
                    {
                        entry.set_gid(local);
                    }
                }
            }
        }
    }
}
