//! The one ssh_config tokeniser: a faithful port of OpenSSH's `argv_split`.
//!
//! Upstream splits the value half of *every* config line with a single
//! call - `argv_split(str, &oac, &oav, 1)` at openssh/readconf.c:1196 -
//! and each keyword arm then consumes tokens from that one vector via
//! `argv_next`. So quoting, backslash escapes and comment termination are
//! properties of the *line*, not of any individual directive, and a
//! second tokeniser anywhere in the parser is a divergence by
//! construction. Both of oc's ssh_config readers call [`argv_split`]
//! here; nothing else in `crate::ssh` may split a config value.
//!
//! # The rules, each from the C
//!
//! * Separators are `' '` and `'\t'` only (openssh/misc.c:2141, :2165).
//!   Not `\n`, not `\r`, and never a comma - a comma is ordinary token
//!   text, which is why `Host a,b` does not match the alias `a`.
//! * A `#` ends the line, but **only at a token boundary**
//!   (openssh/misc.c:2143-2144): the test sits in the outer loop, after
//!   leading whitespace has been skipped and before a token is opened.
//!   A `#` reached from inside the inner copy loop falls through to the
//!   final `else` and is emitted literally, so `HostName x#y` resolves to
//!   `x#y` and `IdentityFile /k#1` keeps its `#1`.
//! * A quote character (`"` or `'`) is a *state toggle*, not a delimiter
//!   (openssh/misc.c:2167-2170). It is consumed rather than emitted, it
//!   may open mid-token (`a"b c"d` is the single token `ab cd`), and
//!   inside one quote kind the other kind is ordinary text.
//! * A backslash escapes exactly four things: `'`, `"`, `\`, and - only
//!   while unquoted - a space (openssh/misc.c:2154-2158). Anything else
//!   is an *unrecognised* escape and upstream emits the backslash itself
//!   and re-processes the following character normally
//!   (openssh/misc.c:2161-2164), so `x\ny` stays `x\ny` and a trailing
//!   `\` survives as `\`. Note the `quote == 0` guard on the space arm:
//!   inside quotes `\ ` is *not* an escape, and both characters are kept.
//! * A quote left open when the string ends is `SSH_ERR_INVALID_FORMAT`
//!   (openssh/misc.c:2174-2179), which readconf reports as
//!   `"%s line %d: invalid quotes"` and turns into a bad-option count
//!   that aborts the whole load (openssh/readconf.c:1196-1199, :2667).
//!
//! # What this function does *not* decide
//!
//! An **empty token is produced, not rejected**: `Host a "" b` yields
//! three tokens, the middle one empty. The refusal is per keyword, in
//! the `oHost` arm's `if (*arg == '\0')` guard
//! (openssh/readconf.c:1832-1836) and its siblings, so it belongs to the
//! caller. Keeping it out of the tokeniser is upstream's own split, and
//! it matters: `Match` criteria and several list options do *not* carry
//! that guard.

/// Why [`argv_split`] refused a line.
///
/// One variant because upstream's `argv_split` has exactly one failure
/// return, `SSH_ERR_INVALID_FORMAT` from the ran-out-of-string-looking-
/// for-a-close-quote branch (openssh/misc.c:2174-2179). Adding a second
/// variant would mean oc had invented a refusal upstream does not have.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(in crate::ssh) enum ArgvSplitError {
    /// A `"` or `'` was opened and the line ended before it closed.
    UnterminatedQuote,
}

impl ArgvSplitError {
    /// The reason text upstream prints for this failure, verbatim.
    ///
    /// upstream: openssh/readconf.c:1197
    /// `error("%s line %d: invalid quotes", filename, linenum)`.
    pub(in crate::ssh) fn reason(self) -> &'static str {
        match self {
            Self::UnterminatedQuote => "invalid quotes",
        }
    }
}

/// Splits one ssh_config value into tokens exactly as upstream's
/// `argv_split` does (openssh/misc.c:2130-2196).
///
/// `terminate_on_comment` mirrors upstream's fourth parameter; readconf
/// always passes `1` (openssh/readconf.c:1196). It is a parameter rather
/// than a constant so the two behaviours stay distinguishable in tests
/// and so a caller that needs upstream's `0` mode cannot get it by
/// accident.
///
/// Returns the tokens in order, including any empty ones - see the
/// module docs for why the empty-token refusal is the caller's.
///
/// # Errors
///
/// [`ArgvSplitError::UnterminatedQuote`] when a quote is still open at
/// end of input.
pub(in crate::ssh) fn argv_split(
    value: &str,
    terminate_on_comment: bool,
) -> Result<Vec<String>, ArgvSplitError> {
    // Indexed rather than iterated because the C reads `s[i + 1]` to
    // classify an escape and then advances past it; a peeking iterator
    // would have to reproduce that lookahead less legibly. Every
    // character the algorithm branches on is ASCII, so a `char` vector
    // and upstream's byte indexing agree on every decision.
    let chars: Vec<char> = value.chars().collect();
    let mut argv: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < chars.len() {
        // Skip leading whitespace (openssh/misc.c:2140-2142).
        if chars[i] == ' ' || chars[i] == '\t' {
            i += 1;
            continue;
        }
        // Comment start - but only here, at a token boundary
        // (openssh/misc.c:2143-2144).
        if terminate_on_comment && chars[i] == '#' {
            break;
        }

        // Start of a token (openssh/misc.c:2145-2150). The token is
        // created unconditionally, which is how an empty one survives.
        let mut quote: Option<char> = None;
        let mut arg = String::new();

        // Copy the token in, removing escapes (openssh/misc.c:2152-2173).
        while i < chars.len() {
            let c = chars[i];
            if c == '\\' {
                let next = chars.get(i + 1).copied();
                let escapes = matches!(next, Some('\'') | Some('"') | Some('\\'))
                    || (quote.is_none() && next == Some(' '));
                if escapes {
                    i += 1; // skip the '\'
                    arg.push(chars[i]);
                } else {
                    // Unrecognised escape: upstream emits the backslash
                    // and leaves the next character to the following
                    // iteration (openssh/misc.c:2161-2164).
                    arg.push('\\');
                }
            } else if quote.is_none() && (c == ' ' || c == '\t') {
                break; // done
            } else if quote.is_none() && (c == '"' || c == '\'') {
                quote = Some(c); // quote start
            } else if quote == Some(c) {
                quote = None; // quote end
            } else {
                arg.push(c);
            }
            i += 1;
        }

        argv.push(arg);

        if i >= chars.len() {
            // Ran off the end of the string (openssh/misc.c:2174-2181).
            if quote.is_some() {
                return Err(ArgvSplitError::UnterminatedQuote);
            }
            break;
        }
        // The outer loop's own `i++`, which steps past the separator the
        // inner loop stopped on (openssh/misc.c:2139).
        i += 1;
    }

    Ok(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(value: &str) -> Vec<String> {
        argv_split(value, true).expect("value tokenises")
    }

    /// Separators are space and TAB, and nothing else
    /// (openssh/misc.c:2141, :2165).
    #[test]
    fn space_and_tab_separate_and_a_comma_does_not() {
        assert_eq!(split("a b"), ["a", "b"]);
        assert_eq!(split("a\tb"), ["a", "b"]);
        assert_eq!(split("  a \t  b  "), ["a", "b"]);
        assert_eq!(split("a,b"), ["a,b"]);
    }

    /// An empty value produces no tokens at all: upstream's outer `for`
    /// never executes, leaving `argc == 0` (openssh/misc.c:2139).
    #[test]
    fn an_empty_value_produces_no_tokens() {
        assert_eq!(split(""), Vec::<String>::new());
        assert_eq!(split("   "), Vec::<String>::new());
    }

    /// Quotes group a separator into one token and are themselves
    /// consumed (openssh/misc.c:2167-2170).
    #[test]
    fn quotes_group_a_separator_and_are_not_emitted() {
        assert_eq!(split(r#""web 1" other"#), ["web 1", "other"]);
        assert_eq!(split("'web 1' other"), ["web 1", "other"]);
    }

    /// A quote is a state toggle, so it may open and close *inside* a
    /// token: upstream builds `ab cd` from `a"b c"d`.
    #[test]
    fn a_quote_may_open_and_close_mid_token() {
        assert_eq!(split(r#"a"b c"d"#), ["ab cd"]);
        assert_eq!(split(r#""a"b"#), ["ab"]);
    }

    /// Inside one quote kind the other kind is ordinary text: the
    /// `quote != 0 && s[i] == quote` arm only fires for the *same*
    /// character that opened the quote (openssh/misc.c:2169).
    #[test]
    fn the_other_quote_kind_is_literal_inside_a_quote() {
        assert_eq!(split(r#""a'b""#), ["a'b"]);
        assert_eq!(split(r#"'a"b'"#), [r#"a"b"#]);
    }

    /// The empty token is PRODUCED. The refusal lives in the keyword
    /// arms (openssh/readconf.c:1832-1836), not here, so a tokeniser
    /// that dropped it would make that refusal unreachable.
    #[test]
    fn an_empty_quoted_token_is_produced_not_dropped() {
        assert_eq!(split(r#"a "" b"#), ["a", "", "b"]);
        assert_eq!(split(r#""" a"#), ["", "a"]);
        assert_eq!(split(r#"a """#), ["a", ""]);
        assert_eq!(split("a '' b"), ["a", "", "b"]);
    }

    /// `#` terminates the line only at a token boundary, and is literal
    /// anywhere inside a token (openssh/misc.c:2143-2144 vs :2171-2172).
    #[test]
    fn hash_ends_the_line_only_at_a_token_boundary() {
        assert_eq!(split("a #b"), ["a"]);
        assert_eq!(split("a\t#b c"), ["a"]);
        assert_eq!(split("#b"), Vec::<String>::new());
        assert_eq!(split("a#b"), ["a#b"]);
        assert_eq!(split("x#y z"), ["x#y", "z"]);
    }

    /// A quoted `#` is inside a token by the time it is read, so it is
    /// literal for the same reason `a#b` is.
    #[test]
    fn hash_inside_quotes_is_literal() {
        assert_eq!(split(r#""a#b""#), ["a#b"]);
        assert_eq!(split(r#""a #b""#), ["a #b"]);
    }

    /// With `terminate_on_comment` off, `#` is never a comment - the
    /// non-vacuity companion proving the parameter is consulted rather
    /// than ignored.
    #[test]
    fn comment_termination_is_the_parameters_to_decide() {
        assert_eq!(argv_split("a #b", false).expect("tokenises"), ["a", "#b"]);
    }

    /// The four recognised escapes (openssh/misc.c:2155-2160): the
    /// backslash is dropped and the next character emitted verbatim.
    #[test]
    fn the_four_recognised_escapes_drop_the_backslash() {
        assert_eq!(split(r#"a\ b"#), ["a b"]);
        assert_eq!(split(r#"a\"b"#), [r#"a"b"#]);
        assert_eq!(split(r"a\'b"), ["a'b"]);
        assert_eq!(split(r"a\\b"), [r"a\b"]);
    }

    /// An unrecognised escape keeps BOTH characters: upstream emits the
    /// backslash and does not skip the next byte
    /// (openssh/misc.c:2161-2164).
    #[test]
    fn an_unrecognised_escape_keeps_the_backslash() {
        assert_eq!(split(r"x\ny"), [r"x\ny"]);
        assert_eq!(split(r"x\#y"), [r"x\#y"]);
        assert_eq!(split(r"x\ty"), [r"x\ty"]);
    }

    /// A trailing backslash has no next character, so it is an
    /// unrecognised escape and survives as itself.
    #[test]
    fn a_trailing_backslash_survives() {
        assert_eq!(split(r"xy\"), [r"xy\"]);
        assert_eq!(split(r"a\ b\"), [r"a b\"]);
    }

    /// The space escape is guarded on `quote == 0`
    /// (openssh/misc.c:2158), so inside quotes `\ ` is unrecognised and
    /// both characters are kept - the space needed no escaping there.
    #[test]
    fn the_space_escape_does_not_apply_inside_quotes() {
        assert_eq!(split(r#""x\ y""#), [r"x\ y"]);
        assert_eq!(split(r"x\ y"), ["x y"]);
    }

    /// An unclosed quote is upstream's one failure return
    /// (openssh/misc.c:2174-2179).
    #[test]
    fn an_unterminated_quote_is_refused() {
        assert_eq!(
            argv_split(r#""a b"#, true),
            Err(ArgvSplitError::UnterminatedQuote)
        );
        assert_eq!(
            argv_split("a 'b", true),
            Err(ArgvSplitError::UnterminatedQuote)
        );
        assert_eq!(ArgvSplitError::UnterminatedQuote.reason(), "invalid quotes");
    }

    /// A quote closed before the end is not a failure - the control for
    /// the cell above, without which a tokeniser that refused every
    /// quote would also satisfy it.
    #[test]
    fn a_closed_quote_is_not_a_failure() {
        assert_eq!(split(r#""a b""#), ["a b"]);
    }

    /// Non-ASCII text passes through untouched: every character the
    /// algorithm branches on is ASCII, so a multi-byte code point can
    /// only reach the final `else`.
    #[test]
    fn non_ascii_token_text_is_preserved() {
        assert_eq!(split("héllo wörld"), ["héllo", "wörld"]);
        assert_eq!(split(r#""héllo wörld""#), ["héllo wörld"]);
    }
}
