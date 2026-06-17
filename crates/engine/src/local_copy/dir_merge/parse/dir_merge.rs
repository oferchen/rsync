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

        if text[..alias.len()].eq_ignore_ascii_case(alias) {
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

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '-' => {
                if saw_plus {
                    let message =
                        format!("{label} directive '{text}' cannot combine '+' and '-' modifiers");
                    return Err(FilterParseError::new(message));
                }
                saw_minus = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
            }
            '+' => {
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
            'c' => {
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

    #[test]
    fn parse_dir_merge_case_insensitive() {
        let result = parse_dir_merge_directive("DIR-MERGE .rsync-filter");
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
        let result = parse_dir_merge_directive("dir-merge,c");
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
