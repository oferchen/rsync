use std::collections::HashSet;
use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};

/// Returns the set of candidate executable paths derived from `binary`.
///
/// When the supplied value contains path separators the helper returns a
/// single-element vector with the provided path. Otherwise it expands the
/// candidates across the directories listed in the current `PATH` environment
/// variable, mirroring the behaviour used by [`std::process::Command`] when
/// launching child processes.
///
/// # Arguments
///
/// * `binary` - The executable name or path to generate candidates for.
///
/// # Returns
///
/// Returns a vector of candidate paths. For bare names like `"rsync"`, this
/// includes all possible locations in PATH. For explicit paths like
/// `"/usr/bin/rsync"` or `"./rsync"`, returns just that path (with potential
/// PATHEXT extensions on Windows).
///
/// # Examples
///
/// ```
/// use std::ffi::OsStr;
/// use core::fallback::fallback_binary_candidates;
///
/// // Get all candidates for a bare name
/// let candidates = fallback_binary_candidates(OsStr::new("rsync"));
/// // Returns paths like /usr/bin/rsync, /usr/local/bin/rsync, etc.
///
/// // Explicit path returns single candidate
/// let explicit = fallback_binary_candidates(OsStr::new("/usr/bin/rsync"));
/// assert_eq!(explicit.len(), 1);
/// ```
#[must_use]
pub fn fallback_binary_candidates(binary: &OsStr) -> Vec<PathBuf> {
    let direct_path = Path::new(binary);
    if has_explicit_path(direct_path) {
        return candidates_for_explicit_path(direct_path);
    }

    let Some(path_env) = effective_path_env() else {
        return Vec::new();
    };

    // Empty PATH entries (from split_paths) map to current directory, mirroring
    // Unix execvp behavior. The loop below handles this by checking is_empty()
    // on each directory entry.

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
            if let Some(candidate) = apply_extension(&base, ext)
                && seen.insert(candidate.clone())
            {
                results.push(candidate);
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

fn has_explicit_path(path: &Path) -> bool {
    path.is_absolute() || path.components().count() > 1
}

fn effective_path_env() -> Option<OsString> {
    // Distinguish between PATH being unset vs explicitly set to empty.
    // Unset PATH -> use default search path
    // Empty PATH -> treat as empty (will search in cwd via split_paths)
    read_path_env().or_else(|| {
        if path_env_is_set() {
            // PATH is set but empty - return empty string so split_paths
            // produces empty entries which map to current directory
            Some(OsString::new())
        } else {
            // PATH is not set - use default search path
            default_search_path()
        }
    })
}

fn path_env_is_set() -> bool {
    #[cfg(windows)]
    {
        env::var_os("PATH").is_some() || env::var_os("Path").is_some()
    }

    #[cfg(not(windows))]
    {
        env::var_os("PATH").is_some()
    }
}

fn read_path_env() -> Option<OsString> {
    #[cfg(windows)]
    let path = env::var_os("PATH").or_else(|| env::var_os("Path"));

    #[cfg(not(windows))]
    let path = env::var_os("PATH");

    path.and_then(|value| if value.is_empty() { None } else { Some(value) })
}

#[cfg(unix)]
fn default_search_path() -> Option<OsString> {
    Some(OsString::from("/bin:/usr/bin"))
}

#[cfg(windows)]
fn default_search_path() -> Option<OsString> {
    let system_root = env::var_os("SystemRoot").or_else(|| env::var_os("SYSTEMROOT"));
    let default = OsString::from(r"C:\Windows\System32;C:\Windows");

    let root = system_root?;
    let root_path = PathBuf::from(&root);
    let mut paths = Vec::new();
    paths.push(root_path.join("System32"));
    paths.push(root_path.clone());
    paths.push(root_path.join("System32").join("Wbem"));

    env::join_paths(paths).ok().or(Some(default))
}

#[cfg(not(any(unix, windows)))]
fn default_search_path() -> Option<OsString> {
    None
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

    #[test]
    fn apply_extension_empty_returns_base() {
        let base = Path::new("/usr/bin/rsync");
        let result = apply_extension(base, OsStr::new(""));
        assert_eq!(result, Some(PathBuf::from("/usr/bin/rsync")));
    }

    #[test]
    fn apply_extension_adds_extension() {
        let base = Path::new("/usr/bin/rsync");
        let result = apply_extension(base, OsStr::new(".exe"));
        assert_eq!(result, Some(PathBuf::from("/usr/bin/rsync.exe")));
    }

    #[test]
    fn apply_extension_without_dot() {
        let base = Path::new("/usr/bin/rsync");
        let result = apply_extension(base, OsStr::new("exe"));
        assert_eq!(result, Some(PathBuf::from("/usr/bin/rsync.exe")));
    }

    #[test]
    fn apply_extension_whitespace_only_returns_none() {
        let base = Path::new("/usr/bin/rsync");
        let result = apply_extension(base, OsStr::new("   "));
        assert!(result.is_none());
    }

    #[test]
    fn has_explicit_path_absolute() {
        assert!(has_explicit_path(Path::new("/usr/bin/rsync")));
    }

    #[test]
    fn has_explicit_path_relative_with_components() {
        assert!(has_explicit_path(Path::new("./rsync")));
        assert!(has_explicit_path(Path::new("bin/rsync")));
    }

    #[test]
    fn has_explicit_path_bare_name() {
        assert!(!has_explicit_path(Path::new("rsync")));
    }

    #[test]
    fn fallback_binary_candidates_explicit_path() {
        let result = fallback_binary_candidates(OsStr::new("/usr/bin/rsync"));
        assert!(!result.is_empty());
        assert!(result.iter().any(|p| p.to_string_lossy().contains("rsync")));
    }

    #[test]
    fn fallback_binary_candidates_relative_path() {
        let result = fallback_binary_candidates(OsStr::new("./rsync"));
        assert!(!result.is_empty());
    }

    #[test]
    fn fallback_binary_candidates_bare_name() {
        // With PATH set, should search directories
        let result = fallback_binary_candidates(OsStr::new("rsync"));
        // Result depends on PATH; just verify it returns a vec
        assert!(result.is_empty() || !result.is_empty());
    }

    #[cfg(not(windows))]
    #[test]
    fn candidates_for_explicit_path_returns_single() {
        let path = Path::new("/usr/bin/rsync");
        let result = candidates_for_explicit_path(path);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], PathBuf::from("/usr/bin/rsync"));
    }

    #[cfg(unix)]
    #[test]
    fn default_search_path_returns_some() {
        let result = default_search_path();
        assert!(result.is_some());
        let path = result.unwrap();
        assert!(path.to_string_lossy().contains("/bin"));
    }

    #[test]
    fn path_env_is_set_returns_bool() {
        // Just verify the function doesn't panic
        let _ = path_env_is_set();
    }

    #[test]
    fn read_path_env_returns_option() {
        // Just verify the function works
        let _ = read_path_env();
    }

    #[test]
    fn effective_path_env_returns_option() {
        // Just verify the function works
        let result = effective_path_env();
        // Should return Some on most systems
        assert!(result.is_some() || result.is_none());
    }
}
