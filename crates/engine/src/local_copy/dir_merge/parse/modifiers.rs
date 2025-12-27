use super::types::FilterParseError;
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};

fn trim_short_rule_remainder(remainder: &str) -> &str {
    let remainder = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if let Some(rest) = remainder.strip_prefix(',') {
        return rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }
    remainder
}

pub(super) fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let (modifiers, remainder) = split_short_rule_modifiers(rest);
        if modifiers.is_empty() {
            return ("", remainder);
        }
        return (modifiers, remainder);
    }

    if matches!(text.chars().next(), Some(ch) if ch.is_ascii_whitespace() || ch == '_') {
        return ("", trim_short_rule_remainder(text));
    }

    for (idx, ch) in text.char_indices() {
        if ch == ',' || ch == '_' || ch.is_ascii_whitespace() {
            let modifiers = &text[..idx];
            let remainder = trim_short_rule_remainder(&text[idx..]);
            return (modifiers, remainder);
        }
    }

    ("", text)
}

pub(super) fn split_short_merge_modifiers(text: &str, allow_extended: bool) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let (modifiers, remainder) = split_short_merge_modifiers(rest, allow_extended);
        if modifiers.is_empty() {
            return ("", remainder);
        }
        return (modifiers, remainder);
    }

    if matches!(text.chars().next(), Some(ch) if ch.is_ascii_whitespace() || ch == '_') {
        return ("", trim_short_rule_remainder(text));
    }

    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        if ch == ',' || ch == '_' || ch.is_ascii_whitespace() {
            let modifiers = &text[..end];
            let remainder = trim_short_rule_remainder(&text[idx..]);
            return (modifiers, remainder);
        }

        let lower = ch.to_ascii_lowercase();
        let base_modifier = matches!(lower, '+' | '-' | 'c' | 'w' | 's' | 'r' | 'p' | '/');
        let extended_modifier = matches!(lower, 'e' | 'n');

        if base_modifier || (allow_extended && extended_modifier) {
            end = idx + ch.len_utf8();
            continue;
        }

        let modifiers = &text[..end];
        let remainder = trim_short_rule_remainder(&text[idx..]);
        return (modifiers, remainder);
    }

    (&text[..end], "")
}

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

    mod trim_short_rule_remainder_tests {
        use super::*;

        #[test]
        fn empty_string() {
            assert_eq!(trim_short_rule_remainder(""), "");
        }

        #[test]
        fn only_underscores() {
            assert_eq!(trim_short_rule_remainder("___"), "");
        }

        #[test]
        fn only_whitespace() {
            assert_eq!(trim_short_rule_remainder("   "), "");
        }

        #[test]
        fn mixed_underscore_whitespace() {
            assert_eq!(trim_short_rule_remainder("_ _ _"), "");
        }

        #[test]
        fn comma_prefix() {
            assert_eq!(trim_short_rule_remainder(",pattern"), "pattern");
        }

        #[test]
        fn comma_with_leading_underscores() {
            assert_eq!(trim_short_rule_remainder("__,pattern"), "pattern");
        }

        #[test]
        fn comma_with_trailing_underscores() {
            assert_eq!(trim_short_rule_remainder(",__pattern"), "pattern");
        }

        #[test]
        fn pattern_without_separator() {
            assert_eq!(trim_short_rule_remainder("pattern"), "pattern");
        }
    }

    mod split_short_rule_modifiers_tests {
        use super::*;

        #[test]
        fn empty_string() {
            assert_eq!(split_short_rule_modifiers(""), ("", ""));
        }

        #[test]
        fn comma_separated() {
            let (mods, rest) = split_short_rule_modifiers(",pattern");
            assert_eq!(mods, "");
            assert_eq!(rest, "pattern");
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
        fn modifiers_with_comma() {
            let (mods, rest) = split_short_rule_modifiers("abc,pattern");
            assert_eq!(mods, "abc");
            assert_eq!(rest, "pattern");
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
        fn no_separator_returns_empty_modifiers() {
            let (mods, rest) = split_short_rule_modifiers("pattern");
            assert_eq!(mods, "");
            assert_eq!(rest, "pattern");
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
        fn modifiers_with_comma_separator() {
            let (mods, rest) = split_short_merge_modifiers("+-,pattern", true);
            assert_eq!(mods, "+-");
            assert_eq!(rest, "pattern");
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
            let result = parse_merge_modifiers("x", ".", true);
            assert!(result.is_err());
        }

        #[test]
        fn slash_modifier_sets_anchor_root() {
            let (options, _) = parse_merge_modifiers("/", ".", true).unwrap();
            assert!(options.anchor_root_enabled());
        }
    }
}
