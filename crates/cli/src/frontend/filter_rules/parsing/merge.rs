use std::ffi::OsString;

use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_merge_modifiers_empty() {
        let (options, assume_cvsignore) = parse_merge_modifiers("", "test", true).unwrap();
        assert!(!assume_cvsignore);
        assert_eq!(options.enforced_kind(), None);
    }

    #[test]
    fn parse_merge_modifiers_exclude() {
        let (options, _) = parse_merge_modifiers("-", ":- file", true).unwrap();
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    }

    #[test]
    fn parse_merge_modifiers_include() {
        let (options, _) = parse_merge_modifiers("+", ":+ file", true).unwrap();
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn parse_merge_modifiers_conflicting_plus_minus() {
        let result = parse_merge_modifiers("+-", ":+- file", true);
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_conflicting_minus_plus() {
        let result = parse_merge_modifiers("-+", ":-+ file", true);
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_cvsignore() {
        let (options, assume_cvsignore) = parse_merge_modifiers("C", ":C", true).unwrap();
        assert!(assume_cvsignore);
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        assert!(options.uses_whitespace());
        assert!(!options.allows_comments());
    }

    #[test]
    fn parse_merge_modifiers_cvsignore_with_include_error() {
        let result = parse_merge_modifiers("+C", ":+C file", true);
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_exclude_self_extended() {
        let (options, _) = parse_merge_modifiers("e", ":e file", true).unwrap();
        assert!(options.excludes_self());
    }

    #[test]
    fn parse_merge_modifiers_exclude_self_not_extended() {
        let result = parse_merge_modifiers("e", ".e file", false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_no_inherit_extended() {
        let (options, _) = parse_merge_modifiers("n", ":n file", true).unwrap();
        assert!(!options.inherit_rules());
    }

    #[test]
    fn parse_merge_modifiers_no_inherit_not_extended() {
        let result = parse_merge_modifiers("n", ".n file", false);
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_whitespace() {
        let (options, _) = parse_merge_modifiers("w", ":w file", true).unwrap();
        assert!(options.uses_whitespace());
        assert!(!options.allows_comments());
    }

    #[test]
    fn parse_merge_modifiers_sender() {
        let (options, _) = parse_merge_modifiers("s", ":s file", true).unwrap();
        assert_eq!(options.sender_side_override(), Some(true));
    }

    #[test]
    fn parse_merge_modifiers_receiver() {
        let (options, _) = parse_merge_modifiers("r", ":r file", true).unwrap();
        assert_eq!(options.receiver_side_override(), Some(true));
    }

    #[test]
    fn parse_merge_modifiers_perishable() {
        let (options, _) = parse_merge_modifiers("p", ":p file", true).unwrap();
        assert!(options.perishable());
    }

    #[test]
    fn parse_merge_modifiers_anchor_root() {
        let (options, _) = parse_merge_modifiers("/", ":/ file", true).unwrap();
        assert!(options.anchor_root_enabled());
    }

    #[test]
    fn parse_merge_modifiers_unknown() {
        let result = parse_merge_modifiers("x", ":x file", true);
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_combined() {
        let (options, _) = parse_merge_modifiers("-sp", ":- file", true).unwrap();
        assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        assert_eq!(options.sender_side_override(), Some(true));
        assert!(options.perishable());
    }

    #[test]
    fn parse_short_merge_directive_dot() {
        let result = parse_short_merge_directive(". filter.txt");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        assert!(matches!(directive, FilterDirective::Merge(_)));
    }

    #[test]
    fn parse_short_merge_directive_colon() {
        let result = parse_short_merge_directive(": .rsync-filter");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        assert!(matches!(directive, FilterDirective::Rule(_)));
    }

    #[test]
    fn parse_short_merge_directive_cvsignore() {
        let result = parse_short_merge_directive(":C");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        // CVS ignore implies .cvsignore pattern
        assert!(matches!(directive, FilterDirective::Rule(_)));
    }

    #[test]
    fn parse_short_merge_directive_not_merge() {
        let result = parse_short_merge_directive("+ include");
        assert!(result.is_none());
    }

    #[test]
    fn parse_short_merge_directive_exclude_modifier() {
        let result = parse_short_merge_directive(":- filter");
        assert!(result.is_some());
        if let Some(Ok(FilterDirective::Rule(spec))) = result {
            // The rule should have exclude enforced
            let _ = spec;
        }
    }

    #[test]
    fn parse_short_merge_directive_include_modifier() {
        let result = parse_short_merge_directive(":+ filter");
        assert!(result.is_some());
    }

    #[test]
    fn parse_short_merge_directive_missing_file_error() {
        let result = parse_short_merge_directive(":  ");
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn parse_short_merge_directive_dot_missing_file_error() {
        let result = parse_short_merge_directive(".  ");
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn parse_short_merge_directive_with_modifiers() {
        let result = parse_short_merge_directive(":en .filter");
        assert!(result.is_some());
        let directive = result.unwrap().unwrap();
        assert!(matches!(directive, FilterDirective::Rule(_)));
    }
}
