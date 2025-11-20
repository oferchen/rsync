use std::collections::HashMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::candidates::fallback_binary_candidates;
#[cfg(unix)]
use super::unix::unix_can_execute;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(super) struct CacheKey {
    binary: OsString,
    path: Option<OsString>,
    #[cfg(windows)]
    pathext: Option<OsString>,
}

impl CacheKey {
    #[inline]
    fn new(binary: &OsStr) -> Self {
        Self {
            binary: binary.to_os_string(),
            path: env::var_os("PATH"),
            #[cfg(windows)]
            pathext: env::var_os("PATHEXT"),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct AvailabilityEntry {
    result: bool,
    matched_path: Option<PathBuf>,
    recorded_at: Instant,
}

impl AvailabilityEntry {
    fn new(result: bool, matched_path: Option<PathBuf>) -> Self {
        Self {
            result,
            matched_path,
            recorded_at: Instant::now(),
        }
    }
}

pub(super) fn availability_cache() -> &'static Mutex<HashMap<CacheKey, AvailabilityEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, AvailabilityEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
pub(super) const NEGATIVE_CACHE_TTL: Duration = Duration::from_millis(100);

#[cfg(not(test))]
pub(super) const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(1);

/// Resolves the provided fallback executable to an executable path if one is available.
///
/// The computation memoises its result for the current `PATH` (and `PATHEXT`
/// on Windows) so repeated availability checks avoid re-walking identical
/// search paths.
#[must_use]
pub fn fallback_binary_path(binary: &OsStr) -> Option<PathBuf> {
    let key = CacheKey::new(binary);
    let cache = availability_cache();

    let cached_entry = {
        let guard = cache
            .lock()
            .expect("fallback availability cache lock poisoned");
        guard.get(&key).cloned()
    };

    if let Some(entry) = cached_entry {
        if entry.result {
            if let Some(path) = entry.matched_path.clone() {
                if candidate_is_executable(&path) {
                    return Some(path);
                }
            }
        } else if entry.recorded_at.elapsed() < NEGATIVE_CACHE_TTL {
            return None;
        }

        cache
            .lock()
            .expect("fallback availability cache lock poisoned")
            .remove(&key);
    }

    let (available, matched_path) = evaluate_availability(binary);

    cache
        .lock()
        .expect("fallback availability cache lock poisoned")
        .insert(key, AvailabilityEntry::new(available, matched_path.clone()));

    if available { matched_path } else { None }
}

/// Reports whether the provided fallback executable exists and is runnable.
///
/// The computation memoises its result for the current `PATH` (and `PATHEXT`
/// on Windows) so repeated availability checks avoid re-walking identical
/// search paths.
#[must_use]
pub fn fallback_binary_available(binary: &OsStr) -> bool {
    fallback_binary_path(binary).is_some()
}

/// Reports whether the provided executable path resolves to the current process binary.
#[must_use]
pub fn fallback_binary_is_self(path: &Path) -> bool {
    let Ok(current_exe) = env::current_exe() else {
        return false;
    };

    let canonical_current = current_exe.canonicalize().unwrap_or(current_exe);
    let canonical_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    canonical_current == canonical_target
}

fn evaluate_availability(binary: &OsStr) -> (bool, Option<PathBuf>) {
    for candidate in fallback_binary_candidates(binary) {
        if candidate_is_executable(&candidate) {
            return (true, Some(candidate));
        }
    }

    (false, None)
}

fn candidate_is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };

    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        unix_can_execute(&metadata)
    }

    #[cfg(not(unix))]
    {
        true
    }
}
