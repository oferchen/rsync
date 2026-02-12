//! No-op UID/GID lookup stubs for non-Unix platforms.
//!
//! On platforms without Unix-style user/group databases (NSS), all lookups
//! return `None` and mappings pass through the raw numeric ID unchanged.
//! This allows callers to use the same API unconditionally without
//! cfg-gating their own code.

#![allow(dead_code)]

use std::io;

/// Raw user ID type (u32 on all platforms for wire compatibility).
pub type RawUid = u32;

/// Raw group ID type (u32 on all platforms for wire compatibility).
pub type RawGid = u32;

/// Maps a remote UID to a local UID.
///
/// On non-Unix platforms, always returns `Some(uid)` unchanged since there
/// is no NSS database to consult.
pub fn map_uid(uid: RawUid, _numeric_ids: bool) -> Option<RawUid> {
    Some(uid)
}

/// Maps a remote GID to a local GID.
///
/// On non-Unix platforms, always returns `Some(gid)` unchanged since there
/// is no NSS database to consult.
pub fn map_gid(gid: RawGid, _numeric_ids: bool) -> Option<RawGid> {
    Some(gid)
}

/// Looks up the username for a given UID.
///
/// On non-Unix platforms, always returns `Ok(None)`.
pub fn lookup_user_name(_uid: RawUid) -> Result<Option<Vec<u8>>, io::Error> {
    Ok(None)
}

/// Looks up the UID for a given username.
///
/// On non-Unix platforms, always returns `Ok(None)`.
pub fn lookup_user_by_name(_name: &[u8]) -> Result<Option<RawUid>, io::Error> {
    Ok(None)
}

/// Looks up the group name for a given GID.
///
/// On non-Unix platforms, always returns `Ok(None)`.
pub fn lookup_group_name(_gid: RawGid) -> Result<Option<Vec<u8>>, io::Error> {
    Ok(None)
}

/// Looks up the GID for a given group name.
///
/// On non-Unix platforms, always returns `Ok(None)`.
pub fn lookup_group_by_name(_name: &[u8]) -> Result<Option<RawGid>, io::Error> {
    Ok(None)
}
