use super::types::{FilterParseError, ParsedFilterDirective};
use crate::local_copy::filter_program::{DirMergeEnforcedKind, DirMergeOptions};
use std::path::PathBuf;

pub(super) fn parse_dir_merge_directive(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    const DIR_MERGE_PREFIX: &str = "dir-merge";

    if text.len() < DIR_MERGE_PREFIX.len() {
        return Ok(None);
    }

    let (prefix, mut remainder) = text.split_at(DIR_MERGE_PREFIX.len());
    if !prefix.eq_ignore_ascii_case(DIR_MERGE_PREFIX) {
        return Ok(None);
    }

    if let Some(ch) = remainder.chars().next() {
        if ch != ',' && !ch.is_ascii_whitespace() {
            return Ok(None);
        }
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
                    let message = format!(
                        "dir-merge directive '{text}' cannot combine '+' and '-' modifiers"
                    );
                    return Err(FilterParseError::new(message));
                }
                saw_minus = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
            }
            '+' => {
                if saw_minus {
                    let message = format!(
                        "dir-merge directive '{text}' cannot combine '+' and '-' modifiers"
                    );
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
                    format!("dir-merge directive '{text}' uses unsupported modifier '{modifier}'");
                return Err(FilterParseError::new(message));
            }
        }
    }

    let path_text = if remainder.is_empty() {
        if used_cvs_default {
            ".cvsignore"
        } else {
            let message = format!("dir-merge directive '{text}' is missing a file name");
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
