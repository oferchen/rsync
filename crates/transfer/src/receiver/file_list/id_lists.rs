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
    /// local ids chosen by upstream `match_uid`/`match_gid`.
    ///
    /// Mirrors upstream `recv_id_list()` (uidlist.c:483-494), which rewrites
    /// `F_OWNER`/`F_GROUP` for every flist entry after the name lists are read.
    /// For each entry this applies `recv_add_id`'s precedence (uidlist.c:255-282)
    /// on the RAW sender id and its transmitted name:
    ///
    /// 1. Scan the `--usermap`/`--groupmap` rules FIRST - numeric rules keyed on
    ///    the raw sender id, name/wildcard rules on the transmitted wire name
    ///    (uidlist.c:255-268). The map result wins.
    /// 2. Otherwise fall back to `user_to_uid(name)` - the id-list's
    ///    name-resolved local id (uidlist.c:273-280).
    /// 3. Otherwise keep the raw id (uidlist.c:281-282 / `recv_add_id(.., NULL)`).
    ///
    /// The map is keyed on the raw sender id and the wire name here, before
    /// `F_OWNER` is overwritten: a numeric rule (`--usermap=1000:5000`) keys on
    /// the raw id and a name/wildcard rule (`--usermap=deploy:www-data`) on the
    /// sender name, neither of which survives a premature local-name rewrite.
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
    /// - `uidlist.c:255-282` - `recv_add_id()` map-then-name precedence
    pub(crate) fn remap_flist_ownership_from_id_lists(&mut self) {
        if !self.config.flags.numeric_ids.is_off() {
            return;
        }
        if self.config.flags.owner {
            let mapping = self.config.user_mapping.clone();
            let uid_map = self.uid_list.resolved_map();
            let names = self.uid_list.names_snapshot();
            let has_rules = mapping.as_ref().is_some_and(|m| !m.is_empty());
            if !uid_map.is_empty() || has_rules {
                for entry in self.file_list.iter_mut() {
                    if let Some(uid) = entry.uid() {
                        let mapped = mapping.as_ref().and_then(|m| {
                            m.map_uid_named(uid, names.get(&uid).map(Vec::as_slice), false)
                                .ok()
                                .flatten()
                        });
                        let resolved = mapped.unwrap_or_else(|| *uid_map.get(&uid).unwrap_or(&uid));
                        entry.set_uid(resolved);
                    }
                }
            }
        }
        if self.config.flags.group {
            let mapping = self.config.group_mapping.clone();
            let gid_map = self.gid_list.resolved_map();
            let names = self.gid_list.names_snapshot();
            let has_rules = mapping.as_ref().is_some_and(|m| !m.is_empty());
            if !gid_map.is_empty() || has_rules {
                for entry in self.file_list.iter_mut() {
                    if let Some(gid) = entry.gid() {
                        let mapped = mapping.as_ref().and_then(|m| {
                            m.map_gid_named(gid, names.get(&gid).map(Vec::as_slice), false)
                                .ok()
                                .flatten()
                        });
                        let resolved = mapped.unwrap_or_else(|| *gid_map.get(&gid).unwrap_or(&gid));
                        entry.set_gid(resolved);
                    }
                }
            }
        }
    }
}
