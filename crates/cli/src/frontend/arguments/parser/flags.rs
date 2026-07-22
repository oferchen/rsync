//! Tri-state flag resolution for paired `--foo` / `--no-foo` options.
//!
//! When both flags appear on the command line the one that occurs later wins,
//! matching upstream rsync's left-to-right option processing.

/// Resolves a tri-state flag pair where the positive flag takes precedence on tie.
///
/// Returns `Some(true)` if the positive flag is set (and appears last),
/// `Some(false)` if the negative flag is set (and appears last),
/// or `None` if neither is present.
pub(crate) fn tri_state_flag_positive_first(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
) -> Option<bool> {
    tri_state_flag_with_order(matches, positive, negative, true)
}

/// Resolves a tri-state flag pair where the negative flag takes precedence on tie.
///
/// Returns `Some(true)` if the positive flag is set (and appears last),
/// `Some(false)` if the negative flag is set (and appears last),
/// or `None` if neither is present.
pub(crate) fn tri_state_flag_negative_first(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
) -> Option<bool> {
    tri_state_flag_with_order(matches, positive, negative, false)
}

/// Resolves a tri-state flag pair using argument index ordering.
///
/// When both positive and negative flags are present, the one that appears
/// last on the command line wins. If indices are identical (e.g., bundled
/// short options), `prefer_positive_on_tie` breaks the tie.
fn tri_state_flag_with_order(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
    prefer_positive_on_tie: bool,
) -> Option<bool> {
    tri_state_flag_indexed(matches, positive, negative, prefer_positive_on_tie)
        .map(|(value, _)| value)
}

/// Resolves a tri-state flag pair, returning both the winning value and the
/// command-line index that decided it.
///
/// Behaves exactly like [`tri_state_flag_with_order`] but exposes the deciding
/// index so callers can compose the result with another flag in argv order
/// (see [`archive_aware_flag`]).
fn tri_state_flag_indexed(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
    prefer_positive_on_tie: bool,
) -> Option<(bool, usize)> {
    let positive_present = matches.get_flag(positive);
    let negative_present = matches.get_flag(negative);

    match (positive_present, negative_present) {
        (true, false) => Some((
            true,
            last_occurrence(matches, positive).unwrap_or(usize::MAX),
        )),
        (false, true) => Some((
            false,
            last_occurrence(matches, negative).unwrap_or(usize::MAX),
        )),
        (false, false) => None,
        (true, true) => {
            let positive_index = last_occurrence(matches, positive);
            let negative_index = last_occurrence(matches, negative);
            match (positive_index, negative_index) {
                (Some(pos), Some(neg)) => {
                    if pos > neg {
                        Some((true, pos))
                    } else if neg > pos {
                        Some((false, neg))
                    } else if prefer_positive_on_tie {
                        Some((true, pos))
                    } else {
                        Some((false, neg))
                    }
                }
                (Some(pos), None) => Some((true, pos)),
                (None, Some(neg)) => Some((false, neg)),
                (None, None) => Some((prefer_positive_on_tie, usize::MAX)),
            }
        }
    }
}

/// Resolves an archive-implied flag pair honoring `-a`'s command-line position.
///
/// upstream: options.c:1562 `case 'a'` expands `-a` in place during the argv
/// scan, assigning `preserve_* = 1` for every `-rlptgoD` dimension. Because that
/// scan is left-to-right and last-wins, a later individual `--no-X` overrides the
/// archive default and a later `-a` re-enables the dimension (`-a --no-perms`
/// clears perms; `--no-perms -a` keeps them). clap collapses repeated flags, so
/// the last index of `-a` (`archive_index`) is compared against the explicit
/// flag's deciding index: when `-a` comes afterwards the explicit setting is
/// discarded (returns `None`) so the archive default applies downstream via
/// `unwrap_or(archive)`; otherwise the explicit value stands.
pub(crate) fn archive_aware_flag(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
    archive_index: Option<usize>,
    prefer_positive_on_tie: bool,
) -> Option<bool> {
    match tri_state_flag_indexed(matches, positive, negative, prefer_positive_on_tie) {
        Some((_, explicit_index)) if archive_index.is_some_and(|a| a > explicit_index) => None,
        Some((value, _)) => Some(value),
        None => None,
    }
}

/// Returns the highest argument index for a given flag id.
fn last_occurrence(matches: &clap::ArgMatches, id: &str) -> Option<usize> {
    matches.indices_of(id).and_then(Iterator::max)
}

/// Resolves a repeatable positive flag paired with a `--no-` negative into a
/// preservation level (0, 1, or 2).
///
/// The positive flag must use `ArgAction::Count` and mutually `overrides_with`
/// the negative so clap's left-to-right resolution already reflects the winner:
/// a later `--no-foo` clears the count, and a later `-ff` clears the negation.
/// This mirrors upstream rsync's popt processing where `--foo` does `level++`
/// and `--no-foo` resets `level = 0` (e.g. options.c:1601 `++preserve_atimes`,
/// options.c:1893 `preserve_xattrs++`). The level is capped at 2 because that is
/// the highest doubled letter upstream `server_options()` emits.
///
/// Returns `None` when neither flag is present, so callers can distinguish
/// "unset" from an explicit `--no-foo` (`Some(0)`).
pub(crate) fn leveled_flag_pair(
    matches: &clap::ArgMatches,
    positive: &str,
    negative: &str,
) -> Option<u8> {
    let count = matches.get_count(positive);
    let negated = matches.get_flag(negative);
    if count > 0 {
        Some(count.min(2))
    } else if negated {
        Some(0)
    } else {
        None
    }
}
