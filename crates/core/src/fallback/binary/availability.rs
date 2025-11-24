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
    cwd: Option<PathBuf>,
    #[cfg(windows)]
    pathext: Option<OsString>,
}

impl CacheKey {
    #[inline]
    fn new(binary: &OsStr) -> Self {
        let path = env::var_os("PATH");
        let cwd = if cache_key_depends_on_cwd(binary, path.as_deref()) {
            env::current_dir().ok()
        } else {
            None
        };
        Self {
            binary: binary.to_os_string(),
            path,
            cwd,
            #[cfg(windows)]
            pathext: env::var_os("PATHEXT"),
        }
    }

    #[cfg(test)]
    #[inline]
    pub(super) fn binary(&self) -> &OsStr {
        &self.binary
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
    let mut cache = availability_cache()
        .lock()
        .expect("fallback availability cache lock poisoned");

    prune_cache(&mut cache);

    if let Some(result) = cached_result(&mut cache, &key) {
        return result;
    }

    let (available, matched_path) = evaluate_availability(binary);

    cache.insert(key, AvailabilityEntry::new(available, matched_path.clone()));

    if available { matched_path } else { None }
}

fn cached_result(
    cache: &mut HashMap<CacheKey, AvailabilityEntry>,
    key: &CacheKey,
) -> Option<Option<PathBuf>> {
    let (result, matched_path, recorded_at) = {
        let entry = cache.get(key)?;
        (entry.result, entry.matched_path.clone(), entry.recorded_at)
    };

    if result {
        if let Some(path) = matched_path.as_ref() {
            if candidate_is_executable(path) {
                return Some(Some(path.clone()));
            }
        }
    } else if recorded_at.elapsed() < NEGATIVE_CACHE_TTL {
        return Some(None);
    }

    cache.remove(key);
    None
}

fn prune_cache(cache: &mut HashMap<CacheKey, AvailabilityEntry>) {
    let now = Instant::now();

    cache.retain(|_, entry| {
        if entry.result {
            if let Some(path) = entry.matched_path.as_ref() {
                return candidate_is_executable(path);
            }

            return false;
        }

        now.duration_since(entry.recorded_at) < NEGATIVE_CACHE_TTL
    });
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
    let Some(canonical_current) = cached_current_executable() else {
        return false;
    };
    let canonical_target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

    canonical_current == &canonical_target
}

fn cached_current_executable() -> Option<&'static PathBuf> {
    static CURRENT: OnceLock<Option<PathBuf>> = OnceLock::new();

    CURRENT
        .get_or_init(|| {
            env::current_exe()
                .ok()
                .map(|path| path.canonicalize().unwrap_or(path))
        })
        .as_ref()
}

fn evaluate_availability(binary: &OsStr) -> (bool, Option<PathBuf>) {
    for candidate in fallback_binary_candidates(binary) {
        if candidate_is_executable(&candidate) {
            return (true, Some(candidate));
        }
    }

    (false, None)
}

fn cache_key_depends_on_cwd(binary: &OsStr, path_env: Option<&OsStr>) -> bool {
    let path = Path::new(binary);
    if !path.is_absolute() && path.components().count() > 1 {
        return true;
    }

    let Some(path_env) = path_env else {
        return false;
    };

    env::split_paths(path_env).any(|entry| entry.as_os_str().is_empty() || entry.is_relative())
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
