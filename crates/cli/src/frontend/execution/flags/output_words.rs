//! Shared token grammar for `--info=` and `--debug=` values.
//!
//! upstream: options.c `parse_output_words()`. Upstream has ONE tokenizer and
//! passes the word table in as a parameter (`parse_output_words(words, levels,
//! str, priority)`), which is why `--info` and `--debug` accept exactly the
//! same syntax and differ only in which names they recognise. This module is
//! that tokenizer; each caller keeps its own table and applies the result.

/// Whether the token walk should carry on to the next comma-separated item.
///
/// upstream: options.c:465-468 - a `help` item prints the word table and calls
/// `exit_cleanup(0)` from inside `parse_output_words()`, so nothing after it is
/// ever examined.
#[derive(Debug)]
pub(super) enum TokenFlow {
    Continue,
    Stop,
}

/// Largest level any `word<N>` token may select.
///
/// upstream: options.c `#define MAX_OUT_LEVEL 4`. Upstream clamps to this
/// bound (`if (lev > MAX_OUT_LEVEL || lev < 0) lev = MAX_OUT_LEVEL;`) and
/// never rejects an out-of-range level.
pub(super) const MAX_OUT_LEVEL: u8 = 4;

/// One classified token from a `--info=` / `--debug=` list.
pub(super) enum OutputWord<'a> {
    /// `help`, with or without a level suffix. upstream prints the word table
    /// and calls `exit_cleanup(0)`.
    Help,
    /// `all<N>`, `none`, or `none<N>`: apply this level to every word in the
    /// caller's table. upstream expresses both by setting the compared length
    /// to 0 so the table loop matches every entry, with `none` additionally
    /// forcing the level to 0 regardless of any suffix.
    Every(u8),
    /// A category name and its level. `name` is the token with its level
    /// suffix removed but its case preserved, because upstream matches the
    /// table with `strncasecmp` yet reports an unknown item with `"%.*s"` over
    /// the original bytes (options.c:475, 485-486): `--debug=BOGUS2` is
    /// reported as `BOGUS`. The caller resolves it case-insensitively against
    /// its own table and reports an unknown name verbatim.
    Named { name: &'a str, level: u8 },
}

/// Classifies one comma-delimited token, verbatim.
///
/// upstream: options.c:455-472 `parse_output_words()`. The trailing-digit scan
/// is skipped when the token itself starts with a digit (`if (!isDigit(str))`),
/// so a bare integer such as `2` stays its own name and falls through to the
/// unknown-item error rather than selecting a level.
pub(super) fn classify(token: &str) -> OutputWord<'_> {
    let base = if token.starts_with(|c: char| c.is_ascii_digit()) {
        token
    } else {
        token.trim_end_matches(|c: char| c.is_ascii_digit())
    };

    // A suffix that overflows `u8` is clamped, not rejected: upstream notes
    // that `atoi()` of an overflowing digit string can return a negative int
    // and folds that into the same `lev = MAX_OUT_LEVEL` clamp.
    let level = match &token[base.len()..] {
        "" => 1,
        digits => digits.parse::<u8>().unwrap_or(MAX_OUT_LEVEL),
    }
    .min(MAX_OUT_LEVEL);

    if base.eq_ignore_ascii_case("help") {
        return OutputWord::Help;
    }
    if base.eq_ignore_ascii_case("none") {
        return OutputWord::Every(0);
    }
    if base.eq_ignore_ascii_case("all") {
        return OutputWord::Every(level);
    }

    OutputWord::Named { name: base, level }
}

/// Splits one `--info=`/`--debug=` value into tokens, dropping empty ones.
///
/// upstream: options.c:448-454 `parse_output_words()` walks the comma list and
/// does `if (!len) continue;`, so `--info=`, `--info=,` and `--info=name,` are
/// all accepted no-ops rather than errors.
///
/// Tokens are passed on verbatim. Upstream compares `str` against the word
/// table with `strncasecmp` over the raw segment bytes and never trims
/// surrounding whitespace, so `--debug= flist` is the unknown item `" flist"`
/// and exits `RERR_SYNTAX` (options.c:484-488) rather than selecting `flist`.
///
/// `apply` returns [`TokenFlow::Stop`] to end the walk, which is how a `help`
/// token short-circuits the rest of the list the way upstream's
/// `exit_cleanup(0)` does at options.c:465-468.
pub(super) fn for_each_token<E>(
    value: &str,
    mut apply: impl FnMut(&str) -> Result<TokenFlow, E>,
) -> Result<TokenFlow, E> {
    for token in value.split(',') {
        if token.is_empty() {
            continue;
        }
        if matches!(apply(token)?, TokenFlow::Stop) {
            return Ok(TokenFlow::Stop);
        }
    }
    Ok(TokenFlow::Continue)
}
