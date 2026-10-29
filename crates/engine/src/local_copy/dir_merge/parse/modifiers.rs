use super::types::FilterParseError;
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};
use filters::{ModifierScope, RuleModifiers, RuleSource};

/// Consumes exactly one separator byte from the front of the remainder.
///
/// upstream: exclude.c:1444-1445 - `if (*s) s++;` advances past the single byte
/// that ended the modifier run, and the pattern is then taken verbatim
/// (`len = strlen(s)`, :1465). Consuming more than one turns `- <space><space>x`
/// into a rule matching `x` where upstream matches `<space>x`, and consuming a
/// `,` here would swallow a byte upstream treats as an invalid modifier.
fn consume_rule_separator(remainder: &str) -> &str {
    let mut chars = remainder.chars();
    match chars.next() {
        Some(ch) if ch == '_' || ch == ' ' => chars.as_str(),
        _ => remainder,
    }
}

/// Splits the text that follows a long-form filter keyword into
/// `(modifiers, pattern)`, or returns `None` when the keyword is not terminated
/// by a separator and is therefore not this keyword at all.
///
/// upstream: exclude.c:1218-1227 `rule_strcmp` - the keyword matches only when
/// the byte after it is whitespace, `_`, `,`, or the end of the string. Any
/// other byte returns NULL, `ch` stays 0, and the rule dies with
/// `Unknown filter rule` (exclude.c:1363), so `mergeX` is a syntax error rather
/// than a merge of `X`.
///
/// A `,` returns `str + rule_len` (exclude.c:1225-1226) and the modifier loop
/// starts on the first modifier byte; every other separator returns
/// `str + rule_len - 1` (exclude.c:1223), so the loop stops immediately and a
/// long keyword only carries modifiers after a comma. Exactly ONE separator is
/// then consumed (exclude.c:1444-1445) and the pattern is the remainder
/// verbatim (`len = strlen(s)`, exclude.c:1465).
///
/// MEASURED against rsync 3.5.0: `--filter='merge  X'` (two spaces) opens a
/// merge file named ` X`, and `--filter='dir-merge  '` merges a per-directory
/// file named ` `; oc trimmed the whole run.
pub(super) fn split_long_keyword_tail(after: &str) -> Option<(&str, &str)> {
    let mut chars = after.chars();
    match chars.next() {
        None => Some(("", "")),
        Some(',') => {
            let body = chars.as_str();
            Some(match body.find(['_', ' ']) {
                // The separator is ASCII, so `idx + 1` stays on a char boundary.
                Some(idx) => (&body[..idx], &body[idx + 1..]),
                None => (body, ""),
            })
        }
        Some(ch) if ch == '_' || ch == ' ' => Some(("", &after[ch.len_utf8()..])),
        Some(_) => None,
    }
}

/// Splits the modifier prefix of a short-form `+`/`-` rule from its pattern.
///
/// Returns `(modifiers, pattern)`. The caller validates each modifier byte, so
/// this only has to delimit the run the way upstream does.
///
/// upstream: exclude.c:1365 - the run ends ONLY at `' '`, `'_'` or end of
/// input. A `,` is deliberately NOT a terminator: it reaches the `default:` arm
/// at :1371 and is reported as an invalid modifier. The single optional comma
/// upstream does accept is consumed earlier, in the outer prefix switch
/// (:1325-1329 `if (s[1] == ',') s++;`), which is the `strip_prefix(',')` below.
///
/// With no separator at all the whole remainder is modifiers and the pattern is
/// empty, so `-foo` is rejected on the `f` rather than silently becoming an
/// exclude of `foo`. This matches the sibling CLI scanner; the two previously
/// held opposite conventions for this case.
pub(super) fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    let text = text.strip_prefix(',').unwrap_or(text);

    for (idx, ch) in text.char_indices() {
        if ch == '_' || ch == ' ' {
            return (&text[..idx], consume_rule_separator(&text[idx..]));
        }
    }

    (text, "")
}

/// Parses a modifier run through the shared table, refusing a byte the way
/// upstream does.
///
/// The refusal is upstream's `invalid modifier` (exclude.c:1376), with the
/// byte and its position crossing `rule_detail` and the rule crossing
/// `rule_text`, so a merge file's line is never echoed.
pub(super) fn table_modifiers(
    modifiers: &str,
    text: &str,
    source: RuleSource<'_>,
    scope: ModifierScope,
) -> Result<RuleModifiers, FilterParseError> {
    filters::parse_rule_modifiers_in(text, modifiers, scope).map_err(|invalid| {
        let detail = format!(" '{}' at position {}", invalid.ch, invalid.offset);
        FilterParseError::new(format!(
            "invalid modifier{} in filter rule: {}",
            source.rule_detail(&detail),
            source.rule_text(text)
        ))
    })
}

/// Parses the modifier run of a `.`/`:`/`merge`/`dir-merge` rule into the
/// options its merged rules inherit, plus whether `C` defaults the file name to
/// `.cvsignore`.
///
/// upstream: exclude.c:1365-1443 parse_rule_tok() - one modifier table for every
/// merge rule, eager or per-directory alike, so `.e`, `.n` and `.p` are as legal
/// as their `:` spellings.
pub(super) fn parse_merge_modifiers(
    modifiers: &str,
    text: &str,
    source: RuleSource<'_>,
) -> Result<(DirMergeOptions, bool), FilterParseError> {
    let mods = table_modifiers(modifiers, text, source, ModifierScope::MERGE)?;
    let mut options = DirMergeOptions::default();
    if mods.no_prefixes {
        options = options.with_enforced_kind(Some(if mods.no_prefixes_include {
            DirMergeEnforcedKind::Include
        } else {
            DirMergeEnforcedKind::Exclude
        }));
    }
    if mods.word_split {
        options = options.use_whitespace().allow_comments(false);
    }
    if mods.no_inherit {
        options = options.inherit(false);
    }
    if mods.exclude_self {
        options = options.exclude_filter_file(true);
    }
    if mods.sender_side {
        options = options.sender_modifier();
    }
    if mods.receiver_side {
        options = options.receiver_modifier();
    }
    if mods.abs_path {
        options = options.anchor_root(true);
    }
    if mods.perishable {
        options = options.mark_perishable();
    }
    // `x` is consumed by the table and deliberately dropped: it is not in
    // FILTRULES_FROM_CONTAINER (exclude.c:1229), so it never reaches the merged
    // rules.
    Ok((options, mods.cvs_ignore))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod consume_rule_separator_tests {
        use super::*;

        #[test]
        fn empty_string() {
            assert_eq!(consume_rule_separator(""), "");
        }

        #[test]
        fn consumes_exactly_one_underscore() {
            // upstream: exclude.c:1444-1445 advances by ONE byte. The previous
            // helper used trim_start_matches and ate the whole run, which turned
            // `-__foo` into a rule matching `foo` where upstream matches `_foo`.
            assert_eq!(consume_rule_separator("___"), "__");
        }

        #[test]
        fn consumes_exactly_one_space() {
            assert_eq!(consume_rule_separator("   "), "  ");
        }

        #[test]
        fn does_not_cross_separator_kinds() {
            assert_eq!(consume_rule_separator("_ _ _"), " _ _");
        }

        #[test]
        fn a_comma_is_not_a_separator() {
            // upstream: exclude.c:1365 lists only ' ' and '_' as terminators, so
            // a `,` reaching here is pattern text - it must NOT be eaten. The
            // single comma upstream accepts is consumed by the outer prefix
            // switch (:1325-1329), before this function is ever reached.
            assert_eq!(consume_rule_separator(",pattern"), ",pattern");
        }

        #[test]
        fn leaves_a_non_separator_untouched() {
            assert_eq!(consume_rule_separator("pattern"), "pattern");
        }
    }

    mod split_short_rule_modifiers_tests {
        use super::*;

        #[test]
        fn empty_string() {
            assert_eq!(split_short_rule_modifiers(""), ("", ""));
        }

        #[test]
        fn a_leading_comma_is_skipped_then_the_rest_is_modifiers() {
            // upstream: exclude.c:1325-1329 - the outer prefix switch consumes ONE
            // optional comma (`if (s[1] == ',') s++`). What follows is the modifier
            // run, not the pattern: `-,pattern` reaches the loop at `p` (a valid
            // perishable modifier) and then fails on `a`. The caller does that
            // rejection; this function only delimits.
            let (mods, rest) = split_short_rule_modifiers(",pattern");
            assert_eq!(mods, "pattern");
            assert_eq!(rest, "");
        }

        #[test]
        fn whitespace_separated() {
            let (mods, rest) = split_short_rule_modifiers(" pattern");
            assert_eq!(mods, "");
            assert_eq!(rest, "pattern");
        }

        #[test]
        fn underscore_separated() {
            let (mods, rest) = split_short_rule_modifiers("_pattern");
            assert_eq!(mods, "");
            assert_eq!(rest, "pattern");
        }

        #[test]
        fn an_interior_comma_does_not_end_the_modifier_run() {
            // upstream: exclude.c:1365 - the run ends only at ' ', '_' or NUL, so a
            // `,` here is just another byte handed to the modifier switch, where it
            // hits `default:` and exits 1. Treating it as a terminator is what made
            // `-s,*.o` a silent sender-side exclude of `*.o`.
            let (mods, rest) = split_short_rule_modifiers("abc,pattern");
            assert_eq!(mods, "abc,pattern");
            assert_eq!(rest, "");
        }

        #[test]
        fn modifiers_with_underscore() {
            let (mods, rest) = split_short_rule_modifiers("abc_pattern");
            assert_eq!(mods, "abc");
            assert_eq!(rest, "pattern");
        }

        #[test]
        fn modifiers_with_whitespace() {
            let (mods, rest) = split_short_rule_modifiers("abc pattern");
            assert_eq!(mods, "abc");
            assert_eq!(rest, "pattern");
        }

        #[test]
        fn no_separator_means_all_modifiers_and_no_pattern() {
            // The convention this crate previously used was the exact INVERSE of the
            // CLI sibling's (cli/.../parsing/helpers.rs). Upstream never reaches a
            // "pattern" at all here: with no
            // separator the loop runs to end-of-input over modifier bytes, so `-foo`
            // fails on `f` instead of excluding a file named `foo`.
            let (mods, rest) = split_short_rule_modifiers("pattern");
            assert_eq!(mods, "pattern");
            assert_eq!(rest, "");
        }
    }
}

#[cfg(test)]
mod merge_modifier_tests {
    use super::*;

    fn parse(modifiers: &str) -> Result<(DirMergeOptions, bool), String> {
        parse_merge_modifiers(modifiers, modifiers, RuleSource::Argument)
            .map_err(|error| error.to_string())
    }

    /// #224: upstream's table takes `e`, `n`, `w` and `p` on any merge rule,
    /// eager or per-directory (exclude.c:1410-1422, 1433-1437); oc refused them
    /// on `.`/`merge`, so `.e inner` in a per-directory file was a fatal error.
    #[test]
    fn eager_merge_accepts_every_merge_modifier() {
        let (options, cvs) = parse("enwp").unwrap();
        assert!(options.excludes_self() && !options.inherit_rules() && options.perishable());
        assert!(!cvs);
    }

    #[test]
    fn prefix_modifiers_set_the_enforced_kind() {
        let kind = |m| parse(m).unwrap().0.enforced_kind();
        assert_eq!(kind("-"), Some(DirMergeEnforcedKind::Exclude));
        assert_eq!(kind("+"), Some(DirMergeEnforcedKind::Include));
        assert_eq!(kind("C"), Some(DirMergeEnforcedKind::Exclude));
        assert_eq!(kind(""), None);
    }

    #[test]
    fn c_defaults_the_file_and_word_splits_without_inheriting() {
        let (options, cvs) = parse("C").unwrap();
        assert!(cvs && !options.inherit_rules());
    }

    /// upstream: exclude.c:1381-1404 - after `-`/`+`/`C` none of the three may
    /// follow, and `!` never applies to a merge rule.
    #[test]
    fn refused_bytes_report_their_position() {
        assert_eq!(
            parse("-C").unwrap_err(),
            "invalid modifier 'C' at position 1 in filter rule: -C"
        );
        for modifiers in ["C-", "++", "+-", "!", "c", "z"] {
            assert!(parse(modifiers).is_err(), "{modifiers}");
        }
    }
}
