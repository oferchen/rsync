use super::types::{FilterParseError, ParsedFilterDirective};
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};
use std::path::PathBuf;

/// Parses `dir-merge` and `per-dir` directives.
///
/// Returns `Ok(None)` for inputs that do not begin with either alias, or when
/// the alias is followed by a non-separator character (so that
/// `dir-mergeXXX` is not silently accepted). Modifiers are introduced by
/// `,`; `+` and `-` are mutually exclusive, and `c` activates the CVS
/// preset (whitespace parser, no comments, no inheritance, list-clearing
/// permitted, default file `.cvsignore`).
pub(super) fn parse_dir_merge_directive(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    const DIR_MERGE_ALIASES: [&str; 2] = ["dir-merge", "per-dir"];

    let mut matched = None;
    for alias in DIR_MERGE_ALIASES {
        if text.len() < alias.len() {
            continue;
        }

        // upstream: exclude.c:1294 `RULE_STRCMP(s, "dir-merge")` under
        // `case 'd':`; `rule_strcmp` is `strncmp` (:1218), so the keyword is
        // matched exactly and `Dir-Merge` is an unknown rule. `per-dir` has no
        // upstream counterpart at all; it is held to the same rule for
        // consistency with its sibling alias.
        if &text[..alias.len()] == alias {
            matched = Some((&text[..alias.len()], &text[alias.len()..]));
            break;
        }
    }

    let Some((label, mut remainder)) = matched else {
        return Ok(None);
    };

    if let Some(ch) = remainder.chars().next()
        && ch != ','
        && !ch.is_ascii_whitespace()
    {
        return Ok(None);
    }

    remainder = remainder.trim_start();

    let mut modifiers = "";
    if let Some(rest) = remainder.strip_prefix(',') {
        let mut split = rest.splitn(2, char::is_whitespace);
        modifiers = split.next().unwrap_or("");
        remainder = split.next().unwrap_or("").trim_start();
    }

    let mut options = DirMergeOptions::default();
    let mut saw_plus = false;
    let mut saw_minus = false;
    let mut used_cvs_default = false;

    // upstream: exclude.c:1365-1444 (`parse_rule_tok`) scans modifiers
    // case-sensitively - `C` is the CVS modifier, `c` is not a modifier at all.
    for modifier in modifiers.chars() {
        match modifier {
            // upstream: exclude.c:1381-1390 - `-`/`+` require
            // BITS_SETnUNSET(FILTRULE_MERGE_FILE, FILTRULE_NO_PREFIXES), and `C`
            // sets NO_PREFIXES, so a `C` scanned earlier in the same run
            // invalidates them. Order matters: `:-C` is legal, `:C-` is not.
            '-' => {
                if used_cvs_default {
                    let message = format!(
                        "{label} directive '{text}' cannot combine '-' with the 'C' modifier"
                    );
                    return Err(FilterParseError::new(message));
                }
                if saw_plus {
                    let message =
                        format!("{label} directive '{text}' cannot combine '+' and '-' modifiers");
                    return Err(FilterParseError::new(message));
                }
                saw_minus = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
            }
            '+' => {
                if used_cvs_default {
                    let message = format!(
                        "{label} directive '{text}' cannot combine '+' with the 'C' modifier"
                    );
                    return Err(FilterParseError::new(message));
                }
                if saw_minus {
                    let message =
                        format!("{label} directive '{text}' cannot combine '+' and '-' modifiers");
                    return Err(FilterParseError::new(message));
                }
                saw_plus = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Include));
            }
            'n' => {
                options = options.inherit(false);
            }
            'e' => {
                options = options.exclude_filter_file(true);
            }
            'w' => {
                options = options.use_whitespace();
                options = options.allow_comments(false);
            }
            's' => {
                options = options.sender_modifier();
            }
            'r' => {
                options = options.receiver_modifier();
            }
            '/' => {
                options = options.anchor_root(true);
            }
            // upstream: exclude.c:1419-1421 - `p` is unguarded, and
            // FILTRULE_PERISHABLE is in FILTRULES_FROM_CONTAINER (exclude.c:1229)
            // so it propagates to the rules read out of the merged file.
            'p' => {
                options = options.mark_perishable();
            }
            // upstream: exclude.c:1402-1409 - `C` is invalid once NO_PREFIXES is
            // already set, which a preceding `C` in the same run does.
            'C' => {
                if used_cvs_default {
                    let message = format!("{label} directive '{text}' repeats the 'C' modifier");
                    return Err(FilterParseError::new(message));
                }
                used_cvs_default = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
                options = options.use_whitespace();
                options = options.allow_comments(false);
                options = options.inherit(false);
                options = options.allow_list_clearing(true);
            }
            _ => {
                let message =
                    format!("{label} directive '{text}' uses unsupported modifier '{modifier}'");
                return Err(FilterParseError::new(message));
            }
        }
    }

    let path_text = if remainder.is_empty() {
        if used_cvs_default {
            ".cvsignore"
        } else {
            let message = format!("{label} directive '{text}' is missing a file name");
            return Err(FilterParseError::new(message));
        }
    } else {
        remainder
    };

    Ok(Some(ParsedFilterDirective::DirMerge {
        pattern: PathBuf::from(path_text),
        options,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dir_merge_returns_none_for_non_dir_merge() {
        let result = parse_dir_merge_directive("include *.txt");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_dir_merge_returns_none_for_short_text() {
        let result = parse_dir_merge_directive("dir-");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_dir_merge_returns_none_for_prefix_only() {
        // dir-merge followed by non-whitespace/comma should not match
        let result = parse_dir_merge_directive("dir-mergeXXX");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_dir_merge_parses_dir_merge_prefix() {
        let result = parse_dir_merge_directive("dir-merge .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { pattern, .. } => {
                assert_eq!(pattern, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_parses_per_dir_prefix() {
        let result = parse_dir_merge_directive("per-dir .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { pattern, .. } => {
                assert_eq!(pattern, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    /// upstream: exclude.c:1294 - `RULE_STRCMP(s, "dir-merge")` is `strncmp`
    /// (:1218), so the keyword is lower case only. MEASURED against rsync
    /// 3.5.0: `--filter='DIR-MERGE .rsync-filter'` reports
    /// `Unknown filter rule` and exits 1.
    #[test]
    fn parse_dir_merge_keyword_is_case_sensitive() {
        assert!(
            parse_dir_merge_directive("DIR-MERGE .rsync-filter")
                .expect("uppercase is not a parse error, just not a dir-merge")
                .is_none()
        );
    }

    /// Non-vacuity companion for `parse_dir_merge_keyword_is_case_sensitive`:
    /// without it the case test would also pass if the parser recognised no
    /// spelling at all.
    #[test]
    fn parse_dir_merge_lower_case_keyword_still_parses() {
        let directive = parse_dir_merge_directive("dir-merge .rsync-filter")
            .expect("parse")
            .expect("dir-merge directive");
        match directive {
            ParsedFilterDirective::DirMerge { pattern, .. } => {
                assert_eq!(pattern, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_n_modifier() {
        let result = parse_dir_merge_directive("dir-merge,n .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { pattern, options } => {
                assert_eq!(pattern, PathBuf::from(".rsync-filter"));
                assert!(!options.inherit_rules());
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_e_modifier() {
        let result = parse_dir_merge_directive("dir-merge,e .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { options, .. } => {
                assert!(options.excludes_self());
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_minus_modifier() {
        let result = parse_dir_merge_directive("dir-merge,- .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { options, .. } => {
                assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_plus_modifier() {
        let result = parse_dir_merge_directive("dir-merge,+ .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { options, .. } => {
                assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Include));
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_error_plus_and_minus() {
        let result = parse_dir_merge_directive("dir-merge,+- .rsync-filter");
        assert!(result.is_err());
    }

    #[test]
    fn parse_dir_merge_error_minus_and_plus() {
        let result = parse_dir_merge_directive("dir-merge,-+ .rsync-filter");
        assert!(result.is_err());
    }

    #[test]
    fn parse_dir_merge_error_unknown_modifier() {
        let result = parse_dir_merge_directive("dir-merge,x .rsync-filter");
        assert!(result.is_err());
    }

    #[test]
    fn parse_dir_merge_error_missing_filename() {
        let result = parse_dir_merge_directive("dir-merge ");
        assert!(result.is_err());
    }

    #[test]
    fn parse_dir_merge_c_modifier_defaults_cvsignore() {
        let result = parse_dir_merge_directive("dir-merge,C");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { pattern, options } => {
                assert_eq!(pattern, PathBuf::from(".cvsignore"));
                assert_eq!(options.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    /// upstream: exclude.c:1365-1444 - the modifier scan is case-sensitive, so
    /// the lower-case spelling of a modifier letter is not that modifier.
    /// `c` is the case that mattered: it used to be accepted as an alias for `C`.
    #[test]
    fn parse_dir_merge_modifier_letters_are_case_sensitive() {
        for directive in [
            "dir-merge,c",
            "dir-merge,N f",
            "dir-merge,E f",
            "dir-merge,P f",
        ] {
            assert!(
                parse_dir_merge_directive(directive).is_err(),
                "{directive} must be rejected - upstream matches modifiers case-sensitively"
            );
        }
    }

    /// Non-vacuity companion for the case-sensitivity pin: without it, that test
    /// would also pass if the parser simply rejected every one of these letters.
    #[test]
    fn parse_dir_merge_upper_and_lower_case_letters_are_not_interchangeable() {
        for directive in [
            "dir-merge,C",
            "dir-merge,n f",
            "dir-merge,e f",
            "dir-merge,p f",
        ] {
            assert!(
                parse_dir_merge_directive(directive).is_ok(),
                "{directive} is the correctly-cased spelling and must parse"
            );
        }
    }

    /// upstream: exclude.c:1419-1421 - `p` sets FILTRULE_PERISHABLE, which is in
    /// FILTRULES_FROM_CONTAINER (exclude.c:1229) and so reaches the merged rules.
    #[test]
    fn parse_dir_merge_p_modifier_marks_rules_perishable() {
        let directive = parse_dir_merge_directive("dir-merge,p .rsync-filter")
            .expect("parses")
            .expect("directive");
        match directive {
            ParsedFilterDirective::DirMerge { options, .. } => {
                assert!(options.perishable());
            }
            _ => panic!("expected DirMerge directive"),
        }
        // Without `p` the flag must stay clear, or the assertion above holds for
        // the wrong reason.
        let plain = parse_dir_merge_directive("dir-merge .rsync-filter")
            .expect("parses")
            .expect("directive");
        match plain {
            ParsedFilterDirective::DirMerge { options, .. } => {
                assert!(!options.perishable());
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    /// upstream: exclude.c:1381-1390 - `-`/`+` require NO_PREFIXES to be unset,
    /// and `C` (exclude.c:1402-1409) sets it. The scan is left-to-right, so the
    /// rejection is order-dependent: `:-C` is legal, `:C-` is not.
    #[test]
    fn parse_dir_merge_cvs_modifier_forbids_a_later_sign_modifier() {
        assert!(parse_dir_merge_directive("dir-merge,C- f").is_err());
        assert!(parse_dir_merge_directive("dir-merge,C+ f").is_err());
        assert!(parse_dir_merge_directive("dir-merge,CC f").is_err());
        // The reverse order is accepted upstream - `C` only guards what follows it.
        assert!(parse_dir_merge_directive("dir-merge,-C f").is_ok());
        assert!(parse_dir_merge_directive("dir-merge,+C f").is_ok());
    }

    #[test]
    fn parse_dir_merge_multiple_modifiers() {
        let result = parse_dir_merge_directive("dir-merge,ne .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { options, .. } => {
                assert!(!options.inherit_rules());
                assert!(options.excludes_self());
            }
            _ => panic!("expected DirMerge directive"),
        }
    }
}
