//! Recognition of the `!` clear-list token in full filter-rule syntax.
//!
//! Upstream accepts the clear-list token in two spellings - the bare `!`
//! character and the `clear` long name - and is exact about what may follow
//! each. Anything left over once the token has been consumed is a "trailing
//! characters" syntax error, and that includes a single trailing space. The one
//! exception is a comma, which introduces the (here always empty) modifier
//! list, so both `!,` and `clear,` are valid clears.
//!
//! The same rule governs a `--filter` argument and a line inside a merge file,
//! so it lives here rather than being restated at each call site.
//!
//! upstream: exclude.c `parse_rule_tok()`. The single-character spelling lands
//! in the prefix switch's `default:` arm - `ch = *s; if (s[1] == ',') s++;` -
//! and the long name goes through `RULE_STRCMP(s, "clear")`, whose comma arm
//! returns `str + rule_len`. Either way `s` ends on the last consumed character
//! and the shared `if (*s) s++` steps onto the remainder, whose length is the
//! `len` that `"'!' rule has trailing characters"` tests. That is why the comma
//! is tolerated identically by both spellings.

/// The `clear` long name. upstream: exclude.c `RULE_STRCMP(s, "clear")`.
const CLEAR_KEYWORD: &[u8] = b"clear";

/// How a rule's text relates to the clear-list token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClearToken {
    /// The text is exactly the clear-list token: clear the list in scope.
    Clear,
    /// The text names the clear-list token but has characters left over.
    /// upstream reports `'!' rule has trailing characters` and exits
    /// `RERR_SYNTAX`.
    TrailingCharacters,
    /// The text does not name the clear-list token; keep dispatching.
    NotClearToken,
}

/// Whether `byte` separates a long rule name from what follows it.
///
/// upstream: exclude.c `rule_strcmp()` accepts `isspace()`, `_` or the end of
/// the string after a keyword. `isspace()` in the C locale also covers the
/// vertical tab, which Rust's `is_ascii_whitespace` omits, so the set is
/// written out rather than borrowed.
const fn is_name_separator(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r' | b'_')
}

/// Classifies `rule` against upstream's clear-list token.
///
/// `rule` is the raw rule text with no prefix stripped, as bytes - filter rules
/// are byte strings, not required to be UTF-8.
///
/// upstream: exclude.c `parse_rule_tok()`. Both spellings leave `s` on the
/// token's final character and then run `if (*s) s++`, so the remainder that
/// decides the trailing-characters error is everything past the token - a lone
/// separator included. `rule_strcmp()` returns `str + rule_len` (rather than
/// `- 1`) for a comma, which makes the comma itself the character `s++` steps
/// over, so `clear,` clears while `clear,x` does not.
#[must_use]
pub fn classify_clear_token(rule: &[u8]) -> ClearToken {
    let remainder = match rule.first() {
        // The bare `!` spelling consumes one character, plus a comma
        // immediately after it. upstream: the prefix switch's `default:` arm
        // does `ch = *s; if (s[1] == ',') s++;`.
        Some(b'!') => {
            let after_token = &rule[1..];
            match after_token.first() {
                Some(b',') => &after_token[1..],
                _ => after_token,
            }
        }
        _ => {
            let Some(after_keyword) = rule.strip_prefix(CLEAR_KEYWORD) else {
                return ClearToken::NotClearToken;
            };
            match after_keyword.first() {
                None => after_keyword,
                Some(b',') => &after_keyword[1..],
                Some(&byte) if is_name_separator(byte) => after_keyword,
                // Any other character means this is a different word that
                // merely starts with "clear"; upstream falls through to
                // "Unknown filter rule".
                Some(_) => return ClearToken::NotClearToken,
            }
        }
    };

    if remainder.is_empty() {
        ClearToken::Clear
    } else {
        ClearToken::TrailingCharacters
    }
}

#[cfg(test)]
mod tests {
    use super::{ClearToken, classify_clear_token};

    fn classify(rule: &str) -> ClearToken {
        classify_clear_token(rule.as_bytes())
    }

    #[test]
    fn bare_token_clears_bare_or_with_a_lone_comma() {
        assert_eq!(classify("!"), ClearToken::Clear);
        // upstream: `if (s[1] == ',') s++` swallows one comma, exactly as the
        // `clear` spelling does.
        assert_eq!(classify("!,"), ClearToken::Clear);
        // Anything else is left over - even a lone trailing space.
        assert_eq!(classify("! "), ClearToken::TrailingCharacters);
        assert_eq!(classify("!x"), ClearToken::TrailingCharacters);
        assert_eq!(classify("!_"), ClearToken::TrailingCharacters);
        assert_eq!(classify("!,x"), ClearToken::TrailingCharacters);
        assert_eq!(classify("!,,"), ClearToken::TrailingCharacters);
    }

    /// Once a spelling IS recognised, both share the same `if (*s) s++` and the
    /// same one-comma tolerance, so every separator suffix must classify alike.
    #[test]
    fn both_spellings_agree_on_every_separator() {
        for suffix in ["", ",", " ", "_", ",x", ",,"] {
            assert_eq!(
                classify(&format!("!{suffix}")),
                classify(&format!("clear{suffix}")),
                "`!{suffix}` and `clear{suffix}` must classify alike"
            );
        }
    }

    /// The one place the spellings legitimately differ: a non-separator suffix.
    ///
    /// `!` is a single-character prefix, so it is always consumed and whatever
    /// follows is trailing. `clear` is a keyword that upstream's `rule_strcmp()`
    /// only accepts when a separator follows, so `clearx` is a different word
    /// entirely and never reaches the clear-list branch.
    #[test]
    fn a_non_separator_suffix_separates_the_two_spellings() {
        assert_eq!(classify("!x"), ClearToken::TrailingCharacters);
        assert_eq!(classify("clearx"), ClearToken::NotClearToken);
    }

    #[test]
    fn keyword_clears_bare_or_with_a_lone_comma() {
        assert_eq!(classify("clear"), ClearToken::Clear);
        // upstream: rule_strcmp() returns past the comma, so `s++` steps over
        // it and nothing remains.
        assert_eq!(classify("clear,"), ClearToken::Clear);
        assert_eq!(classify("clear "), ClearToken::TrailingCharacters);
        assert_eq!(classify("clear_"), ClearToken::TrailingCharacters);
        assert_eq!(classify("clear,x"), ClearToken::TrailingCharacters);
    }

    #[test]
    fn a_longer_word_is_not_the_keyword() {
        // upstream: rule_strcmp() requires a separator, so these reach the
        // prefix switch and become "Unknown filter rule".
        assert_eq!(classify("clearx"), ClearToken::NotClearToken);
        assert_eq!(classify("cle"), ClearToken::NotClearToken);
        assert_eq!(classify(""), ClearToken::NotClearToken);
        assert_eq!(classify("- x"), ClearToken::NotClearToken);
    }

    #[test]
    fn classification_is_byte_oriented() {
        // A non-UTF-8 rule must classify without going through a lossy
        // conversion. upstream compares bytes.
        assert_eq!(
            classify_clear_token(&[b'!', 0xFF]),
            ClearToken::TrailingCharacters
        );
        assert_eq!(
            classify_clear_token(b"clear\xFF"),
            ClearToken::NotClearToken
        );
    }
}
