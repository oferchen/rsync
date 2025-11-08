use std::ffi::OsString;

use oc_rsync_core::client::{
    DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec,
};
use oc_rsync_core::message::{Message, Role};
use oc_rsync_core::rsync_error;

use super::super::directive::{FilterDirective, MergeDirective};
use super::helpers::split_short_merge_modifiers;

pub(crate) fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    allow_extended: bool,
) -> Result<(DirMergeOptions, bool), Message> {
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
                    let message = rsync_error!(
                        1,
                        format!("filter rule '{directive}' cannot combine '+' and '-' modifiers")
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
            }
            '+' => {
                if saw_exclude {
                    let message = rsync_error!(
                        1,
                        format!("filter rule '{directive}' cannot combine '+' and '-' modifiers")
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
                saw_include = true;
                enforced = Some(DirMergeEnforcedKind::Include);
            }
            'c' => {
                if saw_include {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' cannot combine 'C' with '+' or '-'"
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
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
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            'n' => {
                if allow_extended {
                    options = options.inherit(false);
                } else {
                    let message = rsync_error!(
                        1,
                        format!(
                            "filter merge directive '{directive}' uses unsupported modifier '{}'",
                            modifier
                        )
                    )
                    .with_role(Role::Client);
                    return Err(message);
                }
            }
            'w' => {
                options = options.use_whitespace().allow_comments(false);
            }
            's' => {
                options = options.sender_modifier();
            }
            'r' => {
                options = options.receiver_modifier();
            }
            'p' => {
                options = options.mark_perishable();
            }
            '/' => {
                options = options.anchor_root(true);
            }
            _ => {
                let message = rsync_error!(
                    1,
                    format!(
                        "filter merge directive '{directive}' uses unsupported modifier '{}'",
                        modifier
                    )
                )
                .with_role(Role::Client);
                return Err(message);
            }
        }
    }

    options = options.with_enforced_kind(enforced);
    if !allow_extended && !options.list_clear_allowed() {
        options = options.allow_list_clearing(true);
    }
    Ok((options, assume_cvsignore))
}

pub(super) fn parse_short_merge_directive(text: &str) -> Option<Result<FilterDirective, Message>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let (allow_extended, label) = match first {
        '.' => (false, "merge"),
        ':' => (true, "dir-merge"),
        _ => return None,
    };

    let remainder = chars.as_str();
    let (modifiers, rest) = split_short_merge_modifiers(remainder, allow_extended);
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, text, allow_extended) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let pattern = rest.trim();
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else if allow_extended {
            let message = rsync_error!(
                1,
                format!("filter rule '{text}' is missing a file name after '{label}'")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        } else {
            let message = rsync_error!(
                1,
                format!("filter merge directive '{text}' is missing a file path")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        }
    } else {
        pattern
    };

    if allow_extended {
        let rule = FilterRuleSpec::dir_merge(pattern.to_string(), options.clone());
        return Some(Ok(FilterDirective::Rule(rule)));
    }

    let enforced_kind = match options.enforced_kind() {
        Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
        Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
        None => None,
    };

    let directive =
        MergeDirective::new(OsString::from(pattern), enforced_kind).with_options(options);
    Some(Ok(FilterDirective::Merge(directive)))
}
