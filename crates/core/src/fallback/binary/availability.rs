use std::collections::HashMap;
use std::env;
use std::ffi::{OsStr, OsString};
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

/// Reports whether the provided fallback executable exists and is runnable.
///
/// The computation memoises its result for the current `PATH` (and `PATHEXT`
/// on Windows) so repeated availability checks avoid re-walking identical
/// search paths.
#[must_use]
pub fn fallback_binary_available(binary: &OsStr) -> bool {
    let key = CacheKey::new(binary);

    {
        let mut cache = availability_cache()
            .lock()
            .expect("fallback availability cache lock poisoned");

        if let Some(entry) = cache.get(&key) {
            if entry.result {
                if let Some(path) = entry.matched_path.as_ref() {
                    if candidate_is_executable(path) {
                        return true;
                    }
                }
            } else if entry.recorded_at.elapsed() < NEGATIVE_CACHE_TTL {
                return false;
            }

            cache.remove(&key);
        }
    }

    let (available, matched_path) = evaluate_availability(binary);

    let mut cache = availability_cache()
        .lock()
        .expect("fallback availability cache lock poisoned");
    cache.insert(key, AvailabilityEntry::new(available, matched_path));
    available
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
    let Ok(metadata) = std::fs::metadata(path) else {
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
