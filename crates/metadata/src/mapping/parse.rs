#![allow(unsafe_code)]

//! Parsing logic for mapping specification strings.
//!
//! Handles the `source:target` format used by `--usermap` and `--groupmap`,
//! including numeric IDs, numeric ranges, exact names, and wildcard patterns.
//! Mirrors upstream rsync's `uidlist.c` parsing.

use std::cmp::Ordering;

use super::types::{MappingKind, MappingMatcher, MappingParseError, MappingTarget};

/// Parses the source (left-hand) side of a mapping entry.
///
/// Recognized in priority order:
/// - `*` as a wildcard-all matcher
/// - Numeric values or ranges (e.g., `100`, `100-200`)
/// - Glob patterns containing `*`, `?`, or `[`
/// - Otherwise, treated as an exact name string
///
/// An empty source (e.g., `:1`) is accepted as an exact-empty-name matcher,
/// which maps the nameless id (root, when the sender omits the id-0 name).
/// upstream: uidlist.c:parse_name_map - an empty from-part is not numeric and
/// not a wildcard, so it falls through to the `NFLAGS_NAME_MATCH` branch with
/// `noiu.name = ""`, matching the nameless id (recv_add_id normalizes a missing
/// name to "" before the strcmp).
pub(crate) fn parse_matcher(
    _kind: MappingKind,
    source: &str,
    _entry: &str,
) -> Result<MappingMatcher, MappingParseError> {
    if source == "*" {
        return Ok(MappingMatcher::Any);
    }

    if let Some((start, end)) = parse_numeric_range(source) {
        return Ok(MappingMatcher::IdRange { start, end });
    }

    if source.chars().any(|ch| matches!(ch, '*' | '?' | '[')) {
        return Ok(MappingMatcher::Pattern(source.to_owned()));
    }

    Ok(MappingMatcher::ExactName(source.to_owned()))
}

/// Parses a numeric value or range from a source string.
///
/// Returns `Some((start, end))` for single values (where `start == end`) and
/// `start-end` ranges. Reversed ranges (`end-start`) are normalized so
/// `start <= end`. Returns `None` for any non-numeric or malformed input.
pub(super) fn parse_numeric_range(source: &str) -> Option<(u32, u32)> {
    let mut parts = source.split('-');
    let start = parts.next()?;
    if start.is_empty() || !start.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }

    if let Some(rest) = parts.next() {
        if rest.is_empty() || !rest.chars().all(|ch| ch.is_ascii_digit()) || parts.next().is_some()
        {
            return None;
        }
        let start_value = start.parse::<u32>().ok()?;
        let end_value = rest.parse::<u32>().ok()?;
        let (start, end) = match start_value.cmp(&end_value) {
            Ordering::Greater => (end_value, start_value),
            _ => (start_value, end_value),
        };
        Some((start, end))
    } else {
        start.parse::<u32>().ok().map(|value| (value, value))
    }
}

/// Parses the target (right-hand) side of a mapping entry.
///
/// Returns `MappingTarget::Id` for all-digit strings, or
/// `MappingTarget::Name` for name strings that will be resolved later.
pub(crate) fn parse_target(
    kind: MappingKind,
    target: &str,
    entry: &str,
) -> Result<MappingTarget, MappingParseError> {
    if target.is_empty() {
        return Err(MappingParseError::new(
            kind,
            format!("No name found after colon {}: {}", kind.flag(), entry),
        ));
    }

    if target.chars().all(|ch| ch.is_ascii_digit()) {
        return target.parse::<u32>().map(MappingTarget::Id).map_err(|_| {
            MappingParseError::new(
                kind,
                format!("Invalid number in {}: {}", kind.flag(), entry),
            )
        });
    }

    Ok(MappingTarget::Name(target.to_owned()))
}
