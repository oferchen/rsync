//! Thread-safe UID/GID mapping caches.
//!
//! Caches avoid repeated NSS queries (getpwuid_r, getgrgid_r) that would
//! otherwise trigger multiple syscalls per file - including systemd userdb
//! connections on systems with libnss_systemd, causing 15x slowdown on
//! workloads with many files.
//!
//! upstream: uidlist.c - uses linked-list caches for the same purpose

#![allow(unsafe_code)]

use super::nss::{lookup_group_by_name, lookup_group_name, lookup_user_by_name, lookup_user_name};
use crate::ownership;
use rustix::fs::{Gid, Uid};
use rustix::process::{RawGid, RawUid};
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

/// Thread-safe cache for UID mappings.
///
/// Uses `RwLock` to allow concurrent reads - the cache is read much more
/// frequently than written (once per unique UID vs once per file).
static UID_CACHE: LazyLock<RwLock<HashMap<RawUid, RawUid>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Thread-safe cache for GID mappings.
///
/// Uses `RwLock` to allow concurrent reads - the cache is read much more
/// frequently than written (once per unique GID vs once per file).
static GID_CACHE: LazyLock<RwLock<HashMap<RawGid, RawGid>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Maps a remote UID to a local UID.
///
/// When `numeric_ids` is true, returns the UID unchanged. UID 0 (root) is
/// never mapped via name lookup, matching upstream rsync behavior. Otherwise,
/// looks up the name for the remote UID and finds the local UID with that name.
/// If lookup fails, returns the original UID.
///
/// Results are cached to avoid repeated NSS lookups for files with the same owner.
pub fn map_uid(uid: RawUid, numeric_ids: bool) -> Option<Uid> {
    if numeric_ids || uid == 0 {
        return Some(ownership::uid_from_raw(uid));
    }

    if let Ok(cache) = UID_CACHE.read() {
        if let Some(&cached) = cache.get(&uid) {
            return Some(ownership::uid_from_raw(cached));
        }
    }

    let mapped = map_uid_uncached(uid);

    if let Ok(mut cache) = UID_CACHE.write() {
        cache.insert(uid, mapped);
    }

    Some(ownership::uid_from_raw(mapped))
}

/// Performs uncached UID mapping via NSS lookup.
fn map_uid_uncached(uid: RawUid) -> RawUid {
    match lookup_user_name(uid) {
        Ok(Some(bytes)) => match lookup_user_by_name(&bytes) {
            Ok(Some(mapped)) => mapped,
            Ok(None) | Err(_) => uid,
        },
        Ok(None) | Err(_) => uid,
    }
}

/// Maps a remote GID to a local GID.
///
/// When `numeric_ids` is true, returns the GID unchanged. GID 0 (root/wheel) is
/// never mapped via name lookup, matching upstream rsync behavior. Otherwise,
/// looks up the name for the remote GID and finds the local GID with that name.
/// If lookup fails, returns the original GID.
///
/// Results are cached to avoid repeated NSS lookups for files with the same group.
pub fn map_gid(gid: RawGid, numeric_ids: bool) -> Option<Gid> {
    if numeric_ids || gid == 0 {
        return Some(ownership::gid_from_raw(gid));
    }

    if let Ok(cache) = GID_CACHE.read() {
        if let Some(&cached) = cache.get(&gid) {
            return Some(ownership::gid_from_raw(cached));
        }
    }

    let mapped = map_gid_uncached(gid);

    if let Ok(mut cache) = GID_CACHE.write() {
        cache.insert(gid, mapped);
    }

    Some(ownership::gid_from_raw(mapped))
}

/// Performs uncached GID mapping via NSS lookup.
fn map_gid_uncached(gid: RawGid) -> RawGid {
    match lookup_group_name(gid) {
        Ok(Some(bytes)) => match lookup_group_by_name(&bytes) {
            Ok(Some(mapped)) => mapped,
            Ok(None) | Err(_) => gid,
        },
        Ok(None) | Err(_) => gid,
    }
}

/// Clears the UID/GID mapping caches.
///
/// Primarily useful for testing to ensure a clean state between tests.
/// In production, the caches persist for the lifetime of the process.
#[cfg(test)]
pub fn clear_id_caches() {
    if let Ok(mut cache) = UID_CACHE.write() {
        cache.clear();
    }
    if let Ok(mut cache) = GID_CACHE.write() {
        cache.clear();
    }
}

/// Returns the current size of the UID cache.
#[cfg(test)]
pub fn uid_cache_size() -> usize {
    UID_CACHE.read().map(|c| c.len()).unwrap_or(0)
}

/// Returns the current size of the GID cache.
#[cfg(test)]
pub fn gid_cache_size() -> usize {
    GID_CACHE.read().map(|c| c.len()).unwrap_or(0)
}
