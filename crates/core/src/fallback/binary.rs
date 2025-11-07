use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct CacheKey {
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
struct AvailabilityEntry {
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

fn availability_cache() -> &'static Mutex<HashMap<CacheKey, AvailabilityEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, AvailabilityEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

#[cfg(test)]
const NEGATIVE_CACHE_TTL: Duration = Duration::from_millis(100);

#[cfg(not(test))]
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(1);

/// Returns the set of candidate executable paths derived from `binary`.
///
/// When the supplied value contains path separators the helper returns a
/// single-element vector with the provided path. Otherwise it expands the
/// candidates across the directories listed in the current `PATH` environment
/// variable, mirroring the behaviour used by [`std::process::Command`] when
/// launching child processes.
#[must_use]
pub fn fallback_binary_candidates(binary: &OsStr) -> Vec<PathBuf> {
    let direct_path = Path::new(binary);
    if has_explicit_path(direct_path) {
        return candidates_for_explicit_path(direct_path);
    }

    let Some(path_env) = env::var_os("PATH") else {
        return Vec::new();
    };

    let mut results = Vec::new();
    let mut seen = HashSet::new();

    #[cfg(windows)]
    let extensions = collect_windows_extensions(direct_path.extension());

    #[cfg(not(windows))]
    let extensions: Vec<OsString> = vec![OsString::new()];

    for dir in env::split_paths(&path_env) {
        let base = if dir.as_os_str().is_empty() {
            direct_path.to_path_buf()
        } else {
            dir.join(direct_path)
        };
        for ext in &extensions {
            if let Some(candidate) = apply_extension(&base, ext) {
                if seen.insert(candidate.clone()) {
                    results.push(candidate);
                }
            }
        }
    }

    results
}

#[cfg(windows)]
fn candidates_for_explicit_path(path: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    push_candidate(path.to_path_buf(), &mut seen, &mut results);

    for ext in collect_windows_extensions(path.extension()) {
        if let Some(candidate) = apply_extension(path, &ext) {
            push_candidate(candidate, &mut seen, &mut results);
        }
    }

    results
}

#[cfg(not(windows))]
fn candidates_for_explicit_path(path: &Path) -> Vec<PathBuf> {
    vec![path.to_path_buf()]
}

#[cfg(windows)]
fn push_candidate(candidate: PathBuf, seen: &mut HashSet<PathBuf>, results: &mut Vec<PathBuf>) {
    if seen.insert(candidate.clone()) {
        results.push(candidate);
    }
}

fn apply_extension(base: &Path, ext: &OsStr) -> Option<PathBuf> {
    if ext.is_empty() {
        return Some(base.to_path_buf());
    }

    let ext_text = ext.to_string_lossy();
    let trimmed = ext_text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let ext_without_dot = trimmed.strip_prefix('.').unwrap_or(trimmed);
    let mut candidate = base.to_path_buf();
    candidate.set_extension(ext_without_dot);
    Some(candidate)
}

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

/// Formats a diagnostic explaining that a fallback executable is unavailable.
#[must_use]
pub fn describe_missing_fallback_binary(binary: &OsStr, env_vars: &[&str]) -> String {
    let display = Path::new(binary).display();
    let directive = match env_vars.len() {
        0 => String::from("set an override environment variable to an explicit path"),
        1 => {
            let var = env_vars[0];
            format!("set {var} to an explicit path")
        }
        2 => {
            let first = env_vars[0];
            let second = env_vars[1];
            format!("set {first} or {second} to an explicit path")
        }
        _ => {
            let (head, tail) = env_vars.split_at(env_vars.len() - 1);
            let mut joined = head.join(", ");
            joined.push_str(", or ");
            joined.push_str(tail[0]);
            format!("set {joined} to an explicit path")
        }
    };

    format!(
        "fallback rsync binary '{display}' is not available on PATH or is not executable; install upstream rsync or {directive}"
    )
}

fn has_explicit_path(path: &Path) -> bool {
    path.is_absolute() || path.components().count() > 1
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

#[cfg(unix)]
fn unix_can_execute(metadata: &std::fs::Metadata) -> bool {
    let identity = UnixProcessIdentity::current();
    unix_mode_allows_execution(metadata.mode(), metadata.uid(), metadata.gid(), &identity)
}

#[cfg(unix)]
fn unix_mode_allows_execution(
    mode: u32,
    owner: u32,
    group: u32,
    identity: &UnixProcessIdentity,
) -> bool {
    if mode & 0o111 == 0 {
        return false;
    }

    if identity.is_root() {
        return true;
    }

    if owner == identity.euid {
        return mode & 0o100 != 0;
    }

    if identity.in_group(group) {
        return mode & 0o010 != 0;
    }

    mode & 0o001 != 0
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct UnixProcessIdentity {
    euid: u32,
    egid: u32,
    groups: Vec<u32>,
}

#[cfg(unix)]
impl UnixProcessIdentity {
    fn current() -> Self {
        let euid = nix::unistd::geteuid().as_raw() as u32;
        let egid = nix::unistd::getegid().as_raw() as u32;
        let groups = collect_supplementary_groups();
        Self { euid, egid, groups }
    }

    #[inline]
    fn is_root(&self) -> bool {
        self.euid == 0
    }

    #[inline]
    fn in_group(&self, gid: u32) -> bool {
        if self.egid == gid {
            return true;
        }

        self.groups.iter().copied().any(|group| group == gid)
    }
}

#[cfg(all(unix, test))]
impl UnixProcessIdentity {
    fn for_tests(euid: u32, egid: u32, groups: &[u32]) -> Self {
        Self {
            euid,
            egid,
            groups: groups.to_vec(),
        }
    }
}

#[cfg(all(unix, not(target_vendor = "apple")))]
fn collect_supplementary_groups() -> Vec<u32> {
    match nix::unistd::getgroups() {
        Ok(groups) => groups.into_iter().map(|gid| gid.as_raw()).collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(all(unix, target_vendor = "apple"))]
fn collect_supplementary_groups() -> Vec<u32> {
    const INITIAL_CAPACITY: usize = 32;
    const MAX_CAPACITY: usize = 1 << 12;

    let mut capacity = INITIAL_CAPACITY;

    loop {
        let mut groups: Vec<libc::gid_t> = Vec::with_capacity(capacity);
        let result = unsafe { libc::getgroups(capacity as libc::c_int, groups.as_mut_ptr()) };

        if result >= 0 {
            let len = result as usize;
            unsafe {
                groups.set_len(len);
            }

            return groups.into_iter().map(|gid| gid as u32).collect();
        }

        let errno = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or_default();

        if errno == libc::EINVAL && capacity < MAX_CAPACITY {
            capacity = capacity.saturating_mul(2);
            continue;
        }

        return Vec::new();
    }
}

#[cfg(windows)]
fn collect_windows_extensions(current_ext: Option<&OsStr>) -> Vec<OsString> {
    let mut exts = Vec::new();
    let mut seen = HashSet::new();

    if let Some(ext) = current_ext {
        let encoded: Vec<u16> = ext.encode_wide().collect();
        push_segment(&encoded, &mut exts, &mut seen);
    }

    if let Some(path_ext) = env::var_os("PATHEXT") {
        push_pathext_segments(&path_ext, &mut exts, &mut seen);
    }

    if exts.is_empty() {
        for default in [".exe", ".com", ".bat", ".cmd"] {
            let encoded: Vec<u16> = default.encode_utf16().collect();
            push_segment(&encoded, &mut exts, &mut seen);
        }
    }

    exts
}

#[cfg(windows)]
fn push_pathext_segments(value: &OsStr, exts: &mut Vec<OsString>, seen: &mut HashSet<Vec<u16>>) {
    let units: Vec<u16> = value.encode_wide().collect();
    if units.is_empty() {
        return;
    }

    let mut start = 0;
    for (idx, unit) in units.iter().enumerate() {
        if *unit == b';' as u16 {
            push_segment(&units[start..idx], exts, seen);
            start = idx + 1;
        }
    }

    push_segment(&units[start..], exts, seen);
}

#[cfg(windows)]
fn push_segment(segment: &[u16], exts: &mut Vec<OsString>, seen: &mut HashSet<Vec<u16>>) {
    let mut start = 0;
    let mut end = segment.len();

    while start < end && is_windows_whitespace(segment[start]) {
        start += 1;
    }

    while end > start && is_windows_whitespace(segment[end - 1]) {
        end -= 1;
    }

    if start == end {
        return;
    }

    let trimmed = &segment[start..end];
    let mut normalized = Vec::with_capacity(trimmed.len());
    normalized.extend(trimmed.iter().copied().map(ascii_uppercase_u16));

    if seen.insert(normalized) {
        exts.push(OsString::from_wide(trimmed));
    }
}

#[cfg(windows)]
fn is_windows_whitespace(unit: u16) -> bool {
    const SPACE: u16 = b' ' as u16;
    const TAB: u16 = b'\t' as u16;
    const NEWLINE: u16 = b'\n' as u16;
    const CARRIAGE_RETURN: u16 = b'\r' as u16;

    matches!(unit, SPACE | TAB | NEWLINE | CARRIAGE_RETURN)
}

#[cfg(windows)]
fn ascii_uppercase_u16(unit: u16) -> u16 {
    const LOWER_A: u16 = b'a' as u16;
    const LOWER_Z: u16 = b'z' as u16;
    const CASE_DIFF: u16 = (b'a' - b'A') as u16;

    if (LOWER_A..=LOWER_Z).contains(&unit) {
        unit - CASE_DIFF
    } else {
        unit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::fs::File;
    #[cfg(not(unix))]
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use tempfile::{NamedTempFile, TempDir};

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set_os(key: &'static str, value: &OsStr) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }

        #[cfg(windows)]
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                #[allow(unsafe_code)]
                unsafe {
                    env::set_var(self.key, previous);
                }
            } else {
                #[allow(unsafe_code)]
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn describe_missing_fallback_binary_lists_single_env() {
        let message =
            describe_missing_fallback_binary(OsStr::new("/usr/bin/rsync"), &["OC_RSYNC_FALLBACK"]);
        assert!(message.contains("install upstream rsync"));
        assert!(message.contains("OC_RSYNC_FALLBACK"));
        assert!(!message.contains(","));
    }

    #[test]
    fn describe_missing_fallback_binary_lists_multiple_envs() {
        let message = describe_missing_fallback_binary(
            OsStr::new("/usr/bin/rsync"),
            &["OC_RSYNC_DAEMON_FALLBACK", "OC_RSYNC_FALLBACK"],
        );
        assert!(message.contains("OC_RSYNC_DAEMON_FALLBACK"));
        assert!(message.contains("OC_RSYNC_FALLBACK"));
        assert!(message.contains("or"));
    }

    #[test]
    fn fallback_binary_available_detects_executable() {
        #[allow(unused_mut)]
        let mut temp = NamedTempFile::new().expect("tempfile");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = temp.as_file().metadata().expect("metadata").permissions();
            permissions.set_mode(0o755);
            temp.as_file().set_permissions(permissions).expect("chmod");
        }

        #[cfg(not(unix))]
        {
            writeln!(temp, "echo ok").expect("write");
        }

        assert!(fallback_binary_available(temp.path().as_os_str()));
    }

    #[cfg(unix)]
    fn identity(euid: u32, egid: u32, groups: &[u32]) -> super::UnixProcessIdentity {
        super::UnixProcessIdentity::for_tests(euid, egid, groups)
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_respects_owner_permissions() {
        let subject = identity(1_000, 1_000, &[]);
        assert!(super::unix_mode_allows_execution(
            0o700, 1_000, 2_000, &subject
        ));
        assert!(!super::unix_mode_allows_execution(
            0o070, 2_000, 2_000, &subject
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_respects_group_membership() {
        let subject = identity(2_000, 100, &[200, 300]);
        assert!(super::unix_mode_allows_execution(
            0o070, 3_000, 200, &subject
        ));
        assert!(!super::unix_mode_allows_execution(
            0o070, 3_000, 500, &subject
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_considers_other_permissions() {
        let subject = identity(2_000, 100, &[]);
        assert!(super::unix_mode_allows_execution(
            0o001, 3_000, 500, &subject
        ));
        assert!(!super::unix_mode_allows_execution(
            0o000, 3_000, 500, &subject
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_mode_treats_root_as_universal_executor() {
        let root = identity(0, 0, &[]);
        assert!(super::unix_mode_allows_execution(0o100, 3_000, 500, &root));
        assert!(!super::unix_mode_allows_execution(0o000, 3_000, 500, &root));
    }

    #[test]
    fn fallback_binary_available_detects_removal() {
        #[allow(unused_mut)]
        let mut temp = NamedTempFile::new().expect("tempfile");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = temp.as_file().metadata().expect("metadata").permissions();
            permissions.set_mode(0o755);
            temp.as_file().set_permissions(permissions).expect("chmod");
        }

        let os_path = temp.path().as_os_str().to_os_string();

        assert!(fallback_binary_available(os_path.as_os_str()));

        drop(temp);

        assert!(!fallback_binary_available(os_path.as_os_str()));
    }

    #[test]
    fn fallback_binary_available_rejects_missing_file() {
        let missing = Path::new("/nonexistent/path/to/rsync-binary");
        assert!(!fallback_binary_available(missing.as_os_str()));
    }

    #[test]
    fn fallback_binary_available_respects_path_changes() {
        let _lock = env_lock().lock().expect("lock env");

        let temp_dir = TempDir::new().expect("tempdir");
        let binary_name = if cfg!(windows) { "rsync.exe" } else { "rsync" };
        let binary_path = temp_dir.path().join(binary_name);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let file = File::create(&binary_path).expect("create helper placeholder");
            let mut permissions = file.metadata().expect("metadata").permissions();
            permissions.set_mode(0o755);
            file.set_permissions(permissions).expect("chmod");
        }

        #[cfg(not(unix))]
        {
            File::create(&binary_path).expect("create helper placeholder");
        }

        {
            let _path_guard = EnvGuard::set_os("PATH", OsStr::new(""));
            assert!(
                !fallback_binary_available(OsStr::new("rsync")),
                "empty PATH should not locate the fallback binary"
            );
        }

        let joined = env::join_paths([temp_dir.path()]).expect("join paths");
        let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());
        assert!(
            fallback_binary_available(OsStr::new("rsync")),
            "updated PATH should locate the fallback binary"
        );
    }

    #[test]
    fn fallback_binary_available_rechecks_after_negative_cache_ttl() {
        let _lock = env_lock().lock().expect("lock env");

        super::availability_cache()
            .lock()
            .expect("fallback availability cache lock poisoned")
            .clear();

        let temp_dir = TempDir::new().expect("tempdir");
        let joined = env::join_paths([temp_dir.path()]).expect("join paths");
        let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

        #[cfg(windows)]
        let _pathext_guard = EnvGuard::set("PATHEXT", ".exe");

        let binary_name = if cfg!(windows) { "rsync.exe" } else { "rsync" };
        let binary_path = temp_dir.path().join(binary_name);

        assert!(
            !fallback_binary_available(OsStr::new("rsync")),
            "missing fallback binary should not be reported as available",
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let file = File::create(&binary_path).expect("create fallback binary placeholder");
            let mut permissions = file.metadata().expect("metadata").permissions();
            permissions.set_mode(0o755);
            file.set_permissions(permissions).expect("chmod");
        }

        #[cfg(not(unix))]
        {
            File::create(&binary_path).expect("create fallback binary placeholder");
        }

        let wait = super::NEGATIVE_CACHE_TTL + std::time::Duration::from_millis(50);
        std::thread::sleep(wait);

        assert!(
            fallback_binary_available(OsStr::new("rsync")),
            "fallback availability should be recomputed after the negative cache TTL expires",
        );
    }

    #[test]
    fn fallback_binary_candidates_deduplicates_duplicate_path_entries() {
        let _lock = env_lock().lock().expect("lock env");

        let temp_dir = TempDir::new().expect("tempdir");
        let joined = env::join_paths([temp_dir.path(), temp_dir.path()]).expect("join paths");
        let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

        #[cfg(windows)]
        let _pathext_guard = EnvGuard::set("PATHEXT", ".exe");

        let expected_name = if cfg!(windows) { "rsync.exe" } else { "rsync" };
        let expected_path = temp_dir.path().join(expected_name);
        let candidates = fallback_binary_candidates(OsStr::new("rsync"));

        assert!(
            candidates
                .iter()
                .any(|candidate| candidate == &expected_path),
            "expected candidate {expected_path:?} missing from {candidates:?}"
        );

        let occurrences = candidates
            .iter()
            .filter(|candidate| *candidate == &expected_path)
            .count();
        assert_eq!(
            occurrences, 1,
            "candidate should only appear once even when PATH repeats entries"
        );
    }

    #[test]
    fn fallback_binary_candidates_include_current_directory_for_empty_path_entries() {
        let _lock = env_lock().lock().expect("lock env");

        let temp_dir = TempDir::new().expect("tempdir");
        let joined =
            env::join_paths([PathBuf::new(), temp_dir.path().to_path_buf()]).expect("join paths");
        let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

        #[cfg(windows)]
        let _pathext_guard = EnvGuard::set("PATHEXT", ".exe");

        let candidates = fallback_binary_candidates(OsStr::new("rsync"));

        #[cfg(not(windows))]
        let expected = Path::new("rsync");
        #[cfg(windows)]
        let expected = Path::new("rsync.exe");

        assert!(
            candidates.iter().any(|candidate| candidate == expected),
            "current-directory candidate missing from {candidates:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn fallback_binary_candidates_deduplicate_pathext_variants() {
        use std::fs;

        let _lock = env_lock().lock().expect("lock env");

        let temp_dir = TempDir::new().expect("tempdir");
        let joined = env::join_paths([temp_dir.path()]).expect("join paths");
        let _path_guard = EnvGuard::set_os("PATH", joined.as_os_str());

        let _pathext_guard = EnvGuard::set("PATHEXT", ".EXE;.exe;.Com");

        let expected_path = temp_dir.path().join("rsync.EXE");
        fs::write(&expected_path, b"echo rsync").expect("write fallback binary candidate");

        let candidates = fallback_binary_candidates(OsStr::new("rsync"));
        let occurrences = candidates
            .iter()
            .filter(|candidate| *candidate == &expected_path)
            .count();

        assert_eq!(
            occurrences, 1,
            "PATHEXT entries that differ only by case should not duplicate candidates"
        );
    }

    #[cfg(windows)]
    #[test]
    fn fallback_binary_candidates_expand_explicit_windows_paths() {
        let _lock = env_lock().lock().expect("lock env");

        use std::fs;

        let temp_dir = TempDir::new().expect("tempdir");
        let base_path = temp_dir.path().join("bin").join("rsync");
        fs::create_dir_all(base_path.parent().expect("parent")).expect("create parent directory");

        let exe_path = base_path.with_extension("exe");
        fs::write(&exe_path, b"echo rsync").expect("write fallback binary candidate");

        let _pathext_guard = EnvGuard::set("PATHEXT", ".exe;.cmd");

        let candidates = fallback_binary_candidates(base_path.as_os_str());

        assert!(
            candidates.iter().any(|candidate| candidate == &base_path),
            "explicit path without extension should remain in candidate list"
        );
        assert!(
            candidates.iter().any(|candidate| candidate == &exe_path),
            "explicit Windows path should expand to include PATHEXT variants"
        );
    }
}
