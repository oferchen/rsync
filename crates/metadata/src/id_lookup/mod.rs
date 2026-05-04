//! UID/GID lookup and mapping utilities.
//!
//! Provides functions for looking up user and group names from numeric IDs and
//! vice versa. These are used for rsync's UID/GID name mapping feature, which
//! translates user/group names between systems rather than using raw numeric IDs.
//!
//! # Performance
//!
//! UID/GID lookups are cached to avoid repeated NSS queries. Without caching,
//! each lookup triggers multiple syscalls (getpwuid_r, getgrgid_r) plus systemd
//! userdb connections on systems with libnss_systemd, causing 15x slowdown on
//! workloads with many files.
//!
//! # Cross-Platform
//!
//! The name converter trait and thread-local storage are available on all
//! platforms. NSS lookups and caching are Unix-only; non-Unix platforms delegate
//! to the converter when one is installed, otherwise return `None`.
//!
//! # Upstream Reference
//!
//! - `uidlist.c` - UID/GID list management in upstream rsync (uses linked list cache)

#[cfg(unix)]
mod cache;
mod converter;
#[cfg(unix)]
mod nss;
#[cfg(not(unix))]
mod nss_stub;

#[cfg(unix)]
pub use cache::{map_gid, map_uid};
pub use converter::{NameConverterCallbacks, clear_name_converter, set_name_converter};
#[cfg(unix)]
pub use nss::{lookup_group_by_name, lookup_group_name, lookup_user_by_name, lookup_user_name};
#[cfg(not(unix))]
pub use nss_stub::{
    lookup_group_by_name, lookup_group_name, lookup_user_by_name, lookup_user_name,
};

/// Maps a remote UID to a local UID.
///
/// On non-Unix platforms, always returns `Some(uid)` unchanged since there
/// is no NSS database to consult.
#[cfg(not(unix))]
pub fn map_uid(uid: u32, _numeric_ids: bool) -> Option<u32> {
    Some(uid)
}

/// Maps a remote GID to a local GID.
///
/// On non-Unix platforms, always returns `Some(gid)` unchanged since there
/// is no NSS database to consult.
#[cfg(not(unix))]
pub fn map_gid(gid: u32, _numeric_ids: bool) -> Option<u32> {
    Some(gid)
}

#[cfg(test)]
#[cfg(unix)]
pub use cache::{clear_id_caches, gid_cache_size, uid_cache_size};

#[cfg(test)]
mod tests;
