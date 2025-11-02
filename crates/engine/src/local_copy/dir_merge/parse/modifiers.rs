use super::types::FilterParseError;
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};

pub(super) fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let mut parts = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        let modifiers = parts.next().unwrap_or("");
        let remainder = parts.next().unwrap_or("");
        let remainder =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        return (modifiers, remainder);
    }

    let mut chars = text.chars();
    match chars.next() {
        None => ("", ""),
        Some(first) if first.is_ascii_whitespace() || first == '_' => {
            let remainder =
                text.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            ("", remainder)
        }
        Some(_) => {
            let mut len = 0;
            for ch in text.chars() {
                if ch.is_ascii_whitespace() || ch == '_' {
                    break;
                }
                len += ch.len_utf8();
            }
            let modifiers = &text[..len];
            let remainder =
                text[len..].trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            (modifiers, remainder)
        }
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
