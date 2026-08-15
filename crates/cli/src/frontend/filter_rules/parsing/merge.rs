use std::ffi::OsString;

use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

use super::super::directive::{FilterDirective, MergeDirective};
use super::helpers::split_short_merge_modifiers;

/// Parses the modifier characters that follow a `.`/`:` merge directive into
/// `DirMergeOptions`. `is_dir_merge` selects the per-directory (`:`) defaults;
/// the modifier set itself is identical for both directives. Returns the options
/// and whether `C` implied `.cvsignore`. Modifiers are matched case-sensitively
/// to mirror upstream.
///
/// upstream: exclude.c:1256-1264 - `e` (FILTRULE_EXCLUDE_SELF) and `n`
/// (FILTRULE_NO_INHERIT) are guarded only by `FILTRULE_MERGE_FILE`, which is set
/// for a plain merge (`.`) as well as a dir-merge (`:`), so both accept them.
pub(crate) fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    is_dir_merge: bool,
) -> Result<(DirMergeOptions, bool), Message> {
    let mut options = if is_dir_merge {
        DirMergeOptions::default()
    } else {
        DirMergeOptions::default().allow_list_clearing(true)
    };
    let mut enforced: Option<DirMergeEnforcedKind> = None;
    let mut saw_include = false;
    let mut saw_exclude = false;
    let mut assume_cvsignore = false;

    // upstream: exclude.c:1214-1287 parse_rule_tok - merge-file modifiers switch
    // on the literal byte, so they are strictly case-sensitive. The cvs-ignore
    // modifier is the uppercase `C`; a lowercase `c` (and any uppercased form of
    // the other modifiers) reaches the `default:` arm and is rejected as an
    // invalid modifier (RERR_SYNTAX). Match each byte verbatim.
    for modifier in modifiers.chars() {
        match modifier {
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
            'C' => {
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
                    .inherit(false)
                    .cvs_mode(true);
                assume_cvsignore = true;
            }
            'e' => {
                options = options.exclude_filter_file(true);
            }
            'n' => {
                options = options.inherit(false);
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
    if !is_dir_merge && !options.list_clear_allowed() {
        options = options.allow_list_clearing(true);
    }
    Ok((options, assume_cvsignore))
}

/// Parses a short merge directive: `. FILE` (merge) or `: FILE` (dir-merge),
/// including inline modifiers. Returns `None` when the text starts with neither
/// `.` nor `:`.
pub(super) fn parse_short_merge_directive(text: &str) -> Option<Result<FilterDirective, Message>> {
    let mut chars = text.chars();
    let first = chars.next()?;
    let (is_dir_merge, label) = match first {
        '.' => (false, "merge"),
        ':' => (true, "dir-merge"),
        _ => return None,
    };

    let remainder = chars.as_str();
    // `remainder` starts one byte past the `.`/`:`, so that is the offset the
    // position in upstream's diagnostic is measured from.
    let (modifiers, rest) = match split_short_merge_modifiers(remainder) {
        Ok(split) => split,
        Err(invalid) => return Some(Err(invalid.into_message(first.len_utf8(), text))),
    };
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, text, is_dir_merge) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let pattern = rest.trim();
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else if is_dir_merge {
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

    if is_dir_merge {
        let rule = FilterRuleSpec::dir_merge(pattern.to_owned(), options);
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
        // upstream: exclude.c:1248-1254 - `C` modifier sets FILTRULE_CVS_IGNORE.
        // We record this so the wire encoder can forward it as cvs_exclude=true.
        assert!(options.is_cvs_mode());
    }

    #[test]
    fn parse_merge_modifiers_cvsignore_with_include_error() {
        let result = parse_merge_modifiers("+C", ":+C file", true);
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_modifiers_exclude_self_dir_merge() {
        let (options, _) = parse_merge_modifiers("e", ":e file", true).unwrap();
        assert!(options.excludes_self());
    }

    #[test]
    fn parse_merge_modifiers_exclude_self_plain_merge() {
        // upstream exclude.c:1256-1259 guards `e` only by FILTRULE_MERGE_FILE,
        // which a plain merge (`.`) sets too, so `.e FILE` is valid.
        let (options, _) = parse_merge_modifiers("e", ".e file", false).unwrap();
        assert!(options.excludes_self());
    }

    #[test]
    fn parse_merge_modifiers_no_inherit_dir_merge() {
        let (options, _) = parse_merge_modifiers("n", ":n file", true).unwrap();
        assert!(!options.inherit_rules());
    }

    #[test]
    fn parse_merge_modifiers_no_inherit_plain_merge() {
        // upstream exclude.c:1260-1264 guards `n` the same way as `e`.
        let (options, _) = parse_merge_modifiers("n", ".n file", false).unwrap();
        assert!(!options.inherit_rules());
    }

    #[test]
    fn parse_short_plain_merge_accepts_exclude_self_modifier() {
        // Regression: `.e FILE` previously mis-split, treating `e` as part of
        // the file name ("e FILE") instead of as a modifier.
        let directive = parse_short_merge_directive(".e rules")
            .expect("recognized")
            .expect("parses");
        match directive {
            FilterDirective::Merge(merge) => {
                assert_eq!(merge.source(), std::ffi::OsStr::new("rules"));
                assert!(merge.options().excludes_self());
            }
            other => panic!("expected a merge directive, got {other:?}"),
        }
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
