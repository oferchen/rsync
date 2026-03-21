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
//! # Upstream Reference
//!
//! - `uidlist.c` - UID/GID list management in upstream rsync (uses linked list cache)

mod cache;
mod converter;
mod nss;

pub use cache::{map_gid, map_uid};
pub use converter::{NameConverterCallbacks, clear_name_converter, set_name_converter};
pub use nss::{lookup_group_by_name, lookup_group_name, lookup_user_by_name, lookup_user_name};

#[cfg(test)]
pub use cache::{clear_id_caches, gid_cache_size, uid_cache_size};

#[cfg(test)]
mod tests;
