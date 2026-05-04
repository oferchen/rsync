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
    let positive_present = matches.get_flag(positive);
    let negative_present = matches.get_flag(negative);

    match (positive_present, negative_present) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
        (true, true) => {
            let positive_index = last_occurrence(matches, positive);
            let negative_index = last_occurrence(matches, negative);
            match (positive_index, negative_index) {
                (Some(pos), Some(neg)) => {
                    if pos > neg {
                        Some(true)
                    } else if neg > pos {
                        Some(false)
                    } else if prefer_positive_on_tie {
                        Some(true)
                    } else {
                        Some(false)
                    }
                }
                (Some(_), None) => Some(true),
                (None, Some(_)) => Some(false),
                (None, None) => Some(prefer_positive_on_tie),
            }
        }
    }
}

/// Returns the highest argument index for a given flag id.
fn last_occurrence(matches: &clap::ArgMatches, id: &str) -> Option<usize> {
    matches.indices_of(id).and_then(Iterator::max)
}
