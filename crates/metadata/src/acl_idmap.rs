//! Cross-host ID remapping for named ACL entries.
//!
//! Named user/group ACL entries carry a sender-side numeric id on the wire.
//! On a cross-namespace transfer that id is meaningless on the receiver, so it
//! must be remapped through the same uid/gid id-list that file ownership uses,
//! plus any `--usermap`/`--groupmap` rules.
//!
//! [`AclIdMapper`] is an owned snapshot of that mapping state. It is built once
//! at receiver setup time from the received id-lists and the parsed mappings,
//! then shared (via `Arc`) with the disk-commit path that applies cached ACLs.
//!
//! # Upstream Reference
//!
//! - `uidlist.c:481-484` `recv_id_list()` calls `match_acl_ids()` after the
//!   uid/gid lists have been read so ACL ids are converted with the same table
//!   as file owners.
//! - `acls.c:1059-1081` `match_racl_ids()`/`match_acl_ids()` walk every named
//!   entry and replace `ida->id` with `match_uid(id)`/`match_gid(id)`.

use std::collections::HashMap;

#[cfg(unix)]
use crate::mapping::{GroupMapping, UserMapping};

/// Remaps sender-side ACL user/group ids to receiver-side ids.
///
/// The remap mirrors upstream `match_uid`/`match_gid`: consult the id-list
/// snapshot first (remote id -> local id, already folded through name-based
/// NSS resolution when the sender sent names), then apply `--usermap` /
/// `--groupmap` on top exactly like `metadata::apply::ownership` does for file
/// owners. When `numeric_ids` is set no remap occurs, matching upstream's
/// `recv_id_list()` guard.
#[derive(Clone, Debug, Default)]
pub struct AclIdMapper {
    /// Remote uid -> local uid snapshot from the received uid id-list.
    uid_map: HashMap<u32, u32>,
    /// Remote gid -> local gid snapshot from the received gid id-list.
    gid_map: HashMap<u32, u32>,
    /// Parsed `--usermap` rules, applied after the id-list.
    #[cfg(unix)]
    user_mapping: Option<UserMapping>,
    /// Parsed `--groupmap` rules, applied after the id-list.
    #[cfg(unix)]
    group_mapping: Option<GroupMapping>,
    /// When set, ids pass through unchanged (upstream `numeric_ids`).
    numeric_ids: bool,
}

impl AclIdMapper {
    /// Builds a mapper from id-list snapshots and the parsed mappings.
    ///
    /// `uid_map`/`gid_map` are the remote->local tables from the received
    /// uid/gid id-lists (see [`crate`]'s receiver id-list handling). Pass empty
    /// maps when no id-list was received; the `--usermap`/`--groupmap` rules and
    /// the numeric-id fallback still apply.
    #[cfg(unix)]
    #[must_use]
    pub fn new(
        uid_map: HashMap<u32, u32>,
        gid_map: HashMap<u32, u32>,
        user_mapping: Option<UserMapping>,
        group_mapping: Option<GroupMapping>,
        numeric_ids: bool,
    ) -> Self {
        Self {
            uid_map,
            gid_map,
            user_mapping,
            group_mapping,
            numeric_ids,
        }
    }

    /// Builds a mapper from id-list snapshots (non-Unix: no `--usermap`).
    #[cfg(not(unix))]
    #[must_use]
    pub fn new(uid_map: HashMap<u32, u32>, gid_map: HashMap<u32, u32>, numeric_ids: bool) -> Self {
        Self {
            uid_map,
            gid_map,
            numeric_ids,
        }
    }

    /// Remaps a named-user ACL id to the local uid.
    ///
    /// upstream: acls.c:1070 `ida->id = match_uid(ida->id)`.
    #[must_use]
    pub fn map_uid(&self, id: u32) -> u32 {
        if self.numeric_ids {
            return id;
        }
        let mapped = self.uid_map.get(&id).copied().unwrap_or(id);
        #[cfg(unix)]
        if let Some(mapping) = &self.user_mapping
            && let Ok(Some(remapped)) = mapping.map_uid(mapped, self.numeric_ids)
        {
            return remapped;
        }
        mapped
    }

    /// Remaps a named-group ACL id to the local gid.
    ///
    /// upstream: acls.c:1072 `ida->id = match_gid(ida->id, NULL)`.
    #[must_use]
    pub fn map_gid(&self, id: u32) -> u32 {
        if self.numeric_ids {
            return id;
        }
        let mapped = self.gid_map.get(&id).copied().unwrap_or(id);
        #[cfg(unix)]
        if let Some(mapping) = &self.group_mapping
            && let Ok(Some(remapped)) = mapping.map_gid(mapped, self.numeric_ids)
        {
            return remapped;
        }
        mapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mapper(uid: HashMap<u32, u32>, gid: HashMap<u32, u32>, numeric: bool) -> AclIdMapper {
        AclIdMapper::new(uid, gid, None, None, numeric)
    }

    #[cfg(not(unix))]
    fn mapper(uid: HashMap<u32, u32>, gid: HashMap<u32, u32>, numeric: bool) -> AclIdMapper {
        AclIdMapper::new(uid, gid, numeric)
    }

    #[test]
    fn id_list_remaps_named_user_across_namespaces() {
        // WHY: a cross-host `-A` transfer carries the sender-side uid (1000). If
        // the receiver applied it verbatim the ACL would land on whatever user
        // owns uid 1000 locally. The id-list (built from the wire uid list where
        // "alice" resolved to 2000) must remap 1000 -> 2000, mirroring upstream
        // match_acl_ids/match_uid (acls.c:1070, uidlist.c:297).
        let mut uid = HashMap::new();
        uid.insert(1000u32, 2000u32);
        let m = mapper(uid, HashMap::new(), false);
        assert_eq!(m.map_uid(1000), 2000);
    }

    #[test]
    fn id_list_remaps_named_group_across_namespaces() {
        let mut gid = HashMap::new();
        gid.insert(1000u32, 2000u32);
        let m = mapper(HashMap::new(), gid, false);
        assert_eq!(m.map_gid(1000), 2000);
    }

    #[test]
    fn unmapped_id_passes_through() {
        // WHY: upstream recv_add_id falls back to `id2 = id` (uidlist.c:282) for
        // ids not in the list, so the raw wire id must survive rather than being
        // dropped or zeroed.
        let m = mapper(HashMap::new(), HashMap::new(), false);
        assert_eq!(m.map_uid(4242), 4242);
        assert_eq!(m.map_gid(4242), 4242);
    }

    #[test]
    fn numeric_ids_disables_remap() {
        // WHY: upstream recv_id_list is guarded by `numeric_ids <= 0`; with
        // --numeric-ids no id-list is exchanged and ACL ids stay numeric.
        let mut uid = HashMap::new();
        uid.insert(1000u32, 2000u32);
        let m = mapper(uid, HashMap::new(), true);
        assert_eq!(m.map_uid(1000), 1000);
    }
}
