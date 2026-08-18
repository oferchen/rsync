use super::types::FilterParseError;
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};

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

/// Splits the modifier prefix of a short-form merge directive (`.`/`:`).
///
/// Only the characters valid as merge modifiers are consumed. The base set is
/// `+`, `-`, `c`, `w`, `s`, `r`, `p`, `/`; passing `allow_extended` true
/// additionally accepts `e` and `n` (the `dir-merge`-only modifiers). Stops
/// at the first non-modifier character, separator, or end of input.
pub(super) fn split_short_merge_modifiers(text: &str, allow_extended: bool) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    let text = text.strip_prefix(',').unwrap_or(text);

    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        if ch == '_' || ch == ' ' {
            return (&text[..end], consume_rule_separator(&text[idx..]));
        }

        // upstream: exclude.c:1370 switches on the RAW byte, so the modifier
        // alphabet is case-sensitive. `C` (:1402) is the CVS modifier and `c` is
        // not a modifier at all - folding case here made `:c f` parse as a CVS
        // dir-merge where upstream exits 1. Same defect PR #7361 fixed on the
        // rule side; the merge side never received it.
        //
        // upstream: exclude.c:1438 - `x` is unguarded and legal on every prefix,
        // so it must be consumed here or it terminates the scan and the
        // remainder is misread as the merge filename.
        let base_modifier = matches!(ch, '+' | '-' | 'C' | 'w' | 's' | 'r' | 'p' | '/' | 'x');
        let extended_modifier = matches!(ch, 'e' | 'n');

        if base_modifier || (allow_extended && extended_modifier) {
            end = idx + ch.len_utf8();
            continue;
        }

        return (&text[..end], consume_rule_separator(&text[idx..]));
    }

    (&text[..end], "")
}

/// Parses the modifier characters of a `merge` or `dir-merge` directive into
/// concrete `DirMergeOptions` plus an `assume_cvsignore` flag.
///
/// `allow_extended` selects between plain `merge` semantics (only `+`, `-`,
/// `c` accepted) and `dir-merge` semantics (all modifiers accepted). Errors
/// on unsupported characters and on conflicting `+`/`-` or `+`/`c`
/// combinations. Plain `merge` directives implicitly enable list-clearing to
/// match upstream behaviour.
pub(super) fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    allow_extended: bool,
) -> Result<(DirMergeOptions, bool), FilterParseError> {
    let label = if allow_extended { "dir-merge" } else { "merge" };
    let mut options = if allow_extended {
        DirMergeOptions::default()
    } else {
        DirMergeOptions::default().allow_list_clearing(true)
    };
    let mut enforced: Option<DirMergeEnforcedKind> = None;
    let mut saw_include = false;
    let mut saw_exclude = false;
    let mut assume_cvsignore = false;

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '-' => {
                if saw_include {
                    let message = format!(
                        "{label} directive '{directive}' cannot combine '+' and '-' modifiers"
                    );

                    return Err(FilterParseError::new(message));
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
            }
            '+' => {
                if saw_exclude {
                    let message = format!(
                        "{label} directive '{directive}' cannot combine '+' and '-' modifiers"
                    );
                    return Err(FilterParseError::new(message));
                }
                saw_include = true;
                enforced = Some(DirMergeEnforcedKind::Include);
            }
            'c' => {
                if saw_include {
                    let message = format!(
                        "{label} directive '{directive}' cannot combine 'C' with '+' or '-'"
                    );
                    return Err(FilterParseError::new(message));
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
                options = options
                    .use_whitespace()
                    .allow_comments(false)
                    .allow_list_clearing(true)
                    .inherit(false);
                assume_cvsignore = true;
            }
            'e' => {
                if allow_extended {
                    options = options.exclude_filter_file(true);
                } else {
                    let message = format!(
                        "merge directive '{directive}' uses unsupported modifier '{modifier}'"
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            'n' => {
                if allow_extended {
                    options = options.inherit(false);
                } else {
                    let message = format!(
                        "merge directive '{directive}' uses unsupported modifier '{modifier}'"
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            'w' => {
                if allow_extended {
                    options = options.use_whitespace().allow_comments(false);
                } else {
                    let message = format!(
                        "merge directive '{directive}' uses unsupported modifier '{modifier}'"
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            's' => {
                if allow_extended {
                    options = options.sender_modifier();
                } else {
                    let message = format!(
                        "merge directive '{directive}' uses unsupported modifier '{modifier}'"
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            'r' => {
                if allow_extended {
                    options = options.receiver_modifier();
                } else {
                    let message = format!(
                        "merge directive '{directive}' uses unsupported modifier '{modifier}'"
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            '/' => {
                if allow_extended {
                    options = options.anchor_root(true);
                } else {
                    let message = format!(
                        "merge directive '{directive}' uses unsupported modifier '{modifier}'"
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            // upstream: exclude.c:1438 - `x` carries no guard and is legal on
            // every prefix. Consumed and then deliberately dropped:
            // FILTRULES_FROM_CONTAINER (exclude.c:1229) is
            // ABS_PATH|INCLUDE|DIRECTORY|NEGATE|PERISHABLE, so XATTR is NOT
            // inherited by the merged rules - marking the container xattr-only
            // would silently turn every rule in the file into an xattr rule.
            'x' => {}
            _ => {
                let message = format!(
                    "{label} directive '{directive}' uses unsupported modifier '{modifier}'"
                );
                return Err(FilterParseError::new(message));
            }
        }
    }

    options = options.with_enforced_kind(enforced);
    if !allow_extended && !options.list_clear_allowed() {
        options = options.allow_list_clearing(true);
    }

    Ok((options, assume_cvsignore))
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
            // CLI sibling's (cli/.../parsing/helpers.rs), which PR #7361 already
            // fixed. Upstream never reaches a "pattern" at all here: with no
            // separator the loop runs to end-of-input over modifier bytes, so `-foo`
            // fails on `f` instead of excluding a file named `foo`.
            let (mods, rest) = split_short_rule_modifiers("pattern");
            assert_eq!(mods, "pattern");
            assert_eq!(rest, "");
        }
    }

    mod split_short_merge_modifiers_tests {
        use super::*;

        #[test]
        fn empty_string() {
            assert_eq!(split_short_merge_modifiers("", false), ("", ""));
        }

        #[test]
        fn basic_modifiers() {
            let (mods, rest) = split_short_merge_modifiers("+-C filename", true);
            assert_eq!(mods, "+-C");
            assert_eq!(rest, "filename");
        }

        #[test]
        fn extended_modifiers_allowed() {
            let (mods, rest) = split_short_merge_modifiers("en filename", true);
            assert_eq!(mods, "en");
            assert_eq!(rest, "filename");
        }

        #[test]
        fn extended_modifiers_disallowed() {
            let (mods, rest) = split_short_merge_modifiers("en filename", false);
            assert_eq!(mods, "");
            assert_eq!(rest, "en filename");
        }

        #[test]
        fn path_modifier() {
            let (mods, rest) = split_short_merge_modifiers("/ pattern", true);
            assert_eq!(mods, "/");
            assert_eq!(rest, "pattern");
        }

        #[test]
        fn comma_prefix() {
            // 'f' is not a valid modifier, so ",filename" should return no modifiers
            let (mods, rest) = split_short_merge_modifiers(",filename", true);
            assert_eq!(mods, "");
            assert_eq!(rest, "filename");
        }

        #[test]
        fn an_interior_comma_stops_the_merge_scan_without_being_consumed() {
            // The merge scanner stops at the first non-modifier byte, and `,` is one
            // - it is NOT a separator (exclude.c:1365), so it must stay at the front
            // of the remainder rather than being eaten. Leaving it visible is what
            // lets the caller report it instead of silently reading `pattern` as the
            // merge filename.
            let (mods, rest) = split_short_merge_modifiers("+-,pattern", true);
            assert_eq!(mods, "+-");
            assert_eq!(rest, ",pattern");
        }

        #[test]
        fn w_modifier_allowed_when_extended() {
            let (mods, rest) = split_short_merge_modifiers("w filename", true);
            assert_eq!(mods, "w");
            assert_eq!(rest, "filename");
        }

        #[test]
        fn all_modifiers_no_remainder() {
            let (mods, rest) = split_short_merge_modifiers("+-Cwsrp/", true);
            assert_eq!(mods, "+-Cwsrp/");
            assert_eq!(rest, "");
        }
    }

    mod parse_merge_modifiers_tests {
        use super::*;

        #[test]
        fn empty_modifiers() {
            let (options, assume_cvsignore) = parse_merge_modifiers("", ".", true).unwrap();
            assert!(!assume_cvsignore);
            assert!(options.list_clear_allowed());
        }

        #[test]
        fn exclude_modifier() {
            let (options, _) = parse_merge_modifiers("-", ".", true).unwrap();
            assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        }

        #[test]
        fn include_modifier() {
            let (options, _) = parse_merge_modifiers("+", ".", true).unwrap();
            assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
        }

        #[test]
        fn conflicting_plus_minus_error() {
            let result = parse_merge_modifiers("+-", ".", true);
            assert!(result.is_err());
        }

        #[test]
        fn conflicting_minus_plus_error() {
            let result = parse_merge_modifiers("-+", ".", true);
            assert!(result.is_err());
        }

        #[test]
        fn c_modifier_sets_cvsignore() {
            let (options, assume_cvsignore) = parse_merge_modifiers("C", ".", true).unwrap();
            assert!(assume_cvsignore);
            assert!(!options.inherit_rules());
        }

        #[test]
        fn c_with_plus_is_error() {
            let result = parse_merge_modifiers("+C", ".", true);
            assert!(result.is_err());
        }

        #[test]
        fn n_modifier_disables_inherit() {
            let (options, _) = parse_merge_modifiers("n", ".", true).unwrap();
            assert!(!options.inherit_rules());
        }

        #[test]
        fn n_modifier_without_extended_is_error() {
            let result = parse_merge_modifiers("n", ".", false);
            assert!(result.is_err());
        }

        #[test]
        fn e_modifier_sets_exclude_filter() {
            let (options, _) = parse_merge_modifiers("e", ".", true).unwrap();
            assert!(options.excludes_self());
        }

        #[test]
        fn e_modifier_without_extended_is_error() {
            let result = parse_merge_modifiers("e", ".", false);
            assert!(result.is_err());
        }

        #[test]
        fn unknown_modifier_is_error() {
            // `z` is outside upstream's modifier set `- + / ! C e n p r s w x`
            // (exclude.c:1381-1441); `x` is in it and must parse.
            let result = parse_merge_modifiers("z", ".", true);
            assert!(result.is_err());
        }

        #[test]
        fn slash_modifier_sets_anchor_root() {
            let (options, _) = parse_merge_modifiers("/", ".", true).unwrap();
            assert!(options.anchor_root_enabled());
        }
    }
}
