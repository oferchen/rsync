#![allow(unsafe_code)]

//! Wildcard pattern matching for name-based mapping rules.
//!
//! Supports `*`, `?`, and bracket expressions (`[abc]`, `[a-z]`, `[!x]`).
//! This mirrors the glob matching used by upstream rsync for `--usermap` and
//! `--groupmap` name patterns.

/// Tests whether `text` matches a wildcard `pattern`.
///
/// Supported metacharacters:
/// - `*` matches zero or more characters
/// - `?` matches exactly one character
/// - `[abc]` matches any character in the set
/// - `[a-z]` matches any character in the range
/// - `[!x]` or `[^x]` matches any character not in the set
pub(crate) fn wildcard_matches(pattern: &str, text: &str) -> bool {
    let pattern = pattern.as_bytes();
    let text = text.as_bytes();
    let mut pat_index = 0usize;
    let mut text_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut match_index = 0usize;

    while text_index < text.len() {
        if pat_index < pattern.len() {
            match pattern[pat_index] {
                b'?' => {
                    pat_index += 1;
                    text_index += 1;
                    continue;
                }
                b'*' => {
                    star_index = Some(pat_index);
                    pat_index += 1;
                    match_index = text_index;
                    continue;
                }
                b'[' => {
                    if let Some((matched, consumed)) =
                        match_bracket(pattern, pat_index, text[text_index])
                    {
                        if matched {
                            pat_index = consumed;
                            text_index += 1;
                            continue;
                        }
                    } else if pattern[pat_index] == text[text_index] {
                        pat_index += 1;
                        text_index += 1;
                        continue;
                    }
                }
                byte if byte == text[text_index] => {
                    pat_index += 1;
                    text_index += 1;
                    continue;
                }
                _ => {}
            }
        }

        if let Some(star_pos) = star_index {
            pat_index = star_pos + 1;
            match_index += 1;
            text_index = match_index;
        } else {
            return false;
        }
    }

    while pat_index < pattern.len() && pattern[pat_index] == b'*' {
        pat_index += 1;
    }

    pat_index == pattern.len()
}

/// Matches a bracket expression (`[...]`) at the given position in `pattern`.
///
/// Returns `Some((matched, consumed))` where `matched` indicates whether the
/// byte was in the character class, and `consumed` is the index past the
/// closing `]`. Returns `None` for malformed (unclosed) bracket expressions.
pub(super) fn match_bracket(pattern: &[u8], start: usize, byte: u8) -> Option<(bool, usize)> {
    let mut index = start + 1;
    if index >= pattern.len() {
        return None;
    }

    let mut negate = false;
    if pattern[index] == b'!' || pattern[index] == b'^' {
        negate = true;
        index += 1;
    }

    let mut matched = false;
    let mut first = true;

    while index < pattern.len() {
        let mut current = pattern[index];
        if current == b']' && !first {
            let result = if negate { !matched } else { matched };
            return Some((result, index + 1));
        }

        if current == b'\\' && index + 1 < pattern.len() {
            index += 1;
            current = pattern[index];
        }

        if index + 2 < pattern.len() && pattern[index + 1] == b'-' {
            let mut end_index = index + 2;
            let mut end = pattern[end_index];
            if end == b'\\' && end_index + 1 < pattern.len() {
                end_index += 1;
                end = pattern[end_index];
            }

            if end_index < pattern.len() {
                if current <= byte && byte <= end {
                    matched = true;
                }
                index = end_index + 1;
                first = false;
                continue;
            }
        }

        if current == b']' && first {
            if byte == current {
                matched = true;
            }
            index += 1;
            first = false;
            continue;
        }

        if byte == current {
            matched = true;
        }
        index += 1;
        first = false;
    }

    None
}
