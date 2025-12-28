use super::types::{FilterParseError, ParsedFilterDirective};
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};
use std::path::PathBuf;

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

    let (label, mut remainder) = match matched {
        Some(values) => values,
        None => return Ok(None),
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

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(path_text),
        options: Some(options),
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
            ParsedFilterDirective::Merge { path, options } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
                assert!(options.is_some());
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_parses_per_dir_prefix() {
        let result = parse_dir_merge_directive("per-dir .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, options } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
                assert!(options.is_some());
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_case_insensitive() {
        let result = parse_dir_merge_directive("DIR-MERGE .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, .. } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_n_modifier() {
        let result = parse_dir_merge_directive("dir-merge,n .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, options } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
                let opts = options.unwrap();
                assert!(!opts.inherit_rules());
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_e_modifier() {
        let result = parse_dir_merge_directive("dir-merge,e .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { options, .. } => {
                let opts = options.unwrap();
                assert!(opts.excludes_self());
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_minus_modifier() {
        let result = parse_dir_merge_directive("dir-merge,- .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { options, .. } => {
                let opts = options.unwrap();
                assert_eq!(
                    opts.enforced_kind(),
                    Some(DirMergeEnforcedKind::Exclude)
                );
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_with_plus_modifier() {
        let result = parse_dir_merge_directive("dir-merge,+ .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { options, .. } => {
                let opts = options.unwrap();
                assert_eq!(
                    opts.enforced_kind(),
                    Some(DirMergeEnforcedKind::Include)
                );
            }
            _ => panic!("expected Merge directive"),
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
            ParsedFilterDirective::Merge { path, options } => {
                assert_eq!(path, PathBuf::from(".cvsignore"));
                let opts = options.unwrap();
                assert_eq!(
                    opts.enforced_kind(),
                    Some(DirMergeEnforcedKind::Exclude)
                );
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_dir_merge_multiple_modifiers() {
        let result = parse_dir_merge_directive("dir-merge,ne .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { options, .. } => {
                let opts = options.unwrap();
                assert!(!opts.inherit_rules());
                assert!(opts.excludes_self());
            }
            _ => panic!("expected Merge directive"),
        }
    }
}
