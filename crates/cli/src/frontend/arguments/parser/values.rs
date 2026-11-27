//! Helpers for combining repeatable `OsString` argument values.

use std::ffi::{OsStr, OsString};

use crate::frontend::arguments::StopOption;

/// Joins multiple `OsString` values with commas into a single `OsString`.
///
/// Returns `None` if the iterator is empty, or the single value if there is
/// exactly one. Multiple values are comma-separated so the downstream mapping
/// parser (`NameMapping::parse`) sees them as additional rules.
pub(crate) fn join_os_values(values: Option<impl Iterator<Item = OsString>>) -> Option<OsString> {
    let mut iter = values?.peekable();
    let first = iter.next()?;
    if iter.peek().is_none() {
        return Some(first);
    }
    let mut joined = first;
    for value in iter {
        joined.push(",");
        joined.push(&value);
    }
    Some(joined)
}

/// Collects every `--stop-after`/`--time-limit` and `--stop-at` value in argv order.
pub(crate) fn stop_options_in_order(matches: &mut clap::ArgMatches) -> Vec<StopOption> {
    let mut indexed = indexed_values(matches, "stop-after", StopOption::After);
    indexed.extend(indexed_values(matches, "stop-at", StopOption::At));
    indexed.sort_unstable_by_key(|(index, _)| *index);
    indexed.into_iter().map(|(_, option)| option).collect()
}

/// Pairs each value of the argument `id` with its position on the command line.
fn indexed_values(
    matches: &mut clap::ArgMatches,
    id: &str,
    wrap: fn(OsString) -> StopOption,
) -> Vec<(usize, StopOption)> {
    let indices: Vec<usize> = matches
        .indices_of(id)
        .map(Iterator::collect)
        .unwrap_or_default();
    matches
        .remove_many::<OsString>(id)
        .map(|values| indices.into_iter().zip(values.map(wrap)).collect())
        .unwrap_or_default()
}

/// Strips leading `./` pairs and then a lone `.` from a `--backup-dir` value.
///
/// Only that ASCII prefix is removed; the rest of the value is kept byte for
/// byte, so a non-UTF-8 name survives. An empty result means the value named
/// the destination directory itself. No other normalisation is applied: a
/// value such as `.//` becomes `/`, exactly as in upstream.
// upstream: options.c:2514-2517
pub(crate) fn strip_backup_dir_dot_prefix(dir: OsString) -> OsString {
    let skip = backup_dir_dot_prefix_len(dir.as_encoded_bytes());
    if skip == 0 {
        dir
    } else {
        drop_ascii_prefix(&dir, skip)
    }
}

/// Returns the length of the leading `./` pairs and lone `.` that rsync strips
/// from a `--backup-dir` value. The prefix is ASCII, so the length is also a
/// valid `str` boundary.
// upstream: options.c:2514-2517
pub(crate) fn backup_dir_dot_prefix_len(bytes: &[u8]) -> usize {
    let mut skip = 0;
    while bytes[skip..].starts_with(b"./") {
        skip += 2;
    }
    if &bytes[skip..] == b"." {
        skip += 1;
    }
    skip
}

/// Upstream's refusal of an empty backup suffix when no usable backup dir is
/// left; `backup_dir_given` selects which of its two wordings applies.
// upstream: options.c:2536-2540
pub(crate) fn empty_backup_suffix_error(backup_dir_given: bool) -> String {
    let reason = if backup_dir_given {
        "when --backup-dir is the same as the dest dir"
    } else {
        "without a --backup-dir"
    };
    format!("--suffix cannot be empty {reason}")
}

/// Returns `value` without its first `len` bytes, which must all be ASCII.
#[cfg(unix)]
fn drop_ascii_prefix(value: &OsStr, len: usize) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(&value.as_bytes()[len..]).to_os_string()
}

/// Returns `value` without its first `len` bytes, which must all be ASCII.
///
/// Each ASCII byte is one UTF-16 unit, so skipping `len` units is exact.
#[cfg(windows)]
fn drop_ascii_prefix(value: &OsStr, len: usize) -> OsString {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let units: Vec<u16> = value.encode_wide().skip(len).collect();
    OsString::from_wide(&units)
}
