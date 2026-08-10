//! Process-wide memoization of UID/GID to name resolution.
//!
//! Upstream rsync records each distinct uid/gid the first time it appears while
//! building the file list, so a transfer of N files with K distinct owners
//! performs K name lookups, not N. This module mirrors that behaviour: a
//! process-global id to name memo backs the per-file flist name lookups so that
//! glibc's `getpwuid_r`/`getgrgid_r` (which reopen and reparse `/etc/passwd` and
//! `/etc/group` on every call) run at most once per distinct id per process.
//!
//! The memo caches the "no such name" (`None`) outcome as well, and stores the
//! resolved bytes verbatim so a cached lookup is byte-for-byte identical to the
//! uncached one. When a thread-local name converter is installed the cache is
//! bypassed, because converter results are per-thread and must not be shared.
//!
//! upstream: uidlist.c:456-480 - add_uid()/add_gid() cache each id once.

use super::converter::has_name_converter;
use super::{lookup_group_name, lookup_user_name};
use std::collections::HashMap;
use std::io;
use std::sync::{LazyLock, RwLock};

/// Id to resolved-name memo. The `None` value caches a "no such name" outcome.
type NameMemo = LazyLock<RwLock<HashMap<u32, Option<Box<[u8]>>>>>;

/// Process-wide UID to username memo.
///
/// Uses `RwLock` because the map is read once per file but written only once
/// per distinct uid.
static UID_NAME_CACHE: NameMemo = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Process-wide GID to group-name memo.
static GID_NAME_CACHE: NameMemo = LazyLock::new(|| RwLock::new(HashMap::new()));

/// Counts underlying NSS lookups on the miss path (test-only).
#[cfg(test)]
static NSS_LOOKUP_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Records a miss-path NSS lookup for the regression counter.
#[cfg(test)]
fn record_nss_lookup() {
    NSS_LOOKUP_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// No-op outside tests.
#[cfg(not(test))]
fn record_nss_lookup() {}

/// Looks up the username for `uid`, memoizing the result process-wide.
///
/// Behaviourally identical to [`super::lookup_user_name`], including the cached
/// `None` result for unknown ids, but performs at most one NSS query per
/// distinct uid for the lifetime of the process. Bypasses the cache when a
/// thread-local name converter is installed.
pub fn lookup_user_name_cached(uid: u32) -> Result<Option<Vec<u8>>, io::Error> {
    if has_name_converter() {
        return lookup_user_name(uid);
    }

    if let Ok(cache) = UID_NAME_CACHE.read() {
        if let Some(entry) = cache.get(&uid) {
            return Ok(entry.as_deref().map(<[u8]>::to_vec));
        }
    }

    record_nss_lookup();
    let name = lookup_user_name(uid)?;

    if let Ok(mut cache) = UID_NAME_CACHE.write() {
        cache.insert(uid, name.as_deref().map(Box::<[u8]>::from));
    }

    Ok(name)
}

/// Looks up the group name for `gid`, memoizing the result process-wide.
///
/// Behaviourally identical to [`super::lookup_group_name`], including the cached
/// `None` result for unknown ids, but performs at most one NSS query per
/// distinct gid for the lifetime of the process. Bypasses the cache when a
/// thread-local name converter is installed.
pub fn lookup_group_name_cached(gid: u32) -> Result<Option<Vec<u8>>, io::Error> {
    if has_name_converter() {
        return lookup_group_name(gid);
    }

    if let Ok(cache) = GID_NAME_CACHE.read() {
        if let Some(entry) = cache.get(&gid) {
            return Ok(entry.as_deref().map(<[u8]>::to_vec));
        }
    }

    record_nss_lookup();
    let name = lookup_group_name(gid)?;

    if let Ok(mut cache) = GID_NAME_CACHE.write() {
        cache.insert(gid, name.as_deref().map(Box::<[u8]>::from));
    }

    Ok(name)
}

/// Clears the name memos. Test-only; production caches live for the process.
#[cfg(test)]
pub fn clear_name_caches() {
    if let Ok(mut cache) = UID_NAME_CACHE.write() {
        cache.clear();
    }
    if let Ok(mut cache) = GID_NAME_CACHE.write() {
        cache.clear();
    }
}

/// Resets the miss-path NSS lookup counter. Test-only.
#[cfg(test)]
pub fn reset_nss_lookup_count() {
    NSS_LOOKUP_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// Returns the miss-path NSS lookup count since the last reset. Test-only.
#[cfg(test)]
pub fn nss_lookup_count() -> u64 {
    NSS_LOOKUP_COUNT.load(std::sync::atomic::Ordering::Relaxed)
}
