use super::types::FilterParseError;
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};

pub(super) fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    fn trim_remainder(remainder: &str) -> &str {
        let remainder =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if let Some(rest) = remainder.strip_prefix(',') {
            return rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }
        remainder
    }

    fn split_inner(text: &str) -> (&str, &str) {
        if text.is_empty() {
            return ("", "");
        }

        let mut end = 0usize;
        let mut saw_separator = false;
        for (idx, ch) in text.char_indices() {
            if ch == ',' || ch == '_' || ch.is_ascii_whitespace() {
                saw_separator = true;
                break;
            }
            end = idx + ch.len_utf8();
        }

        if saw_separator {
            let modifiers = &text[..end];
            let remainder = trim_remainder(&text[end..]);
            (modifiers, remainder)
        } else {
            ("", text)
        }
    }

    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let (modifiers, remainder) = split_inner(rest);
        if modifiers.is_empty() {
            ("", remainder)
        } else {
            (modifiers, remainder)
        }
    } else if matches!(text.chars().next(), Some(ch) if ch.is_ascii_whitespace() || ch == '_') {
        ("", trim_remainder(text))
    } else {
        split_inner(text)
    }
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
