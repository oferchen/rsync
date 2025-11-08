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

fn has_explicit_path(path: &Path) -> bool {
    path.is_absolute() || path.components().count() > 1
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
