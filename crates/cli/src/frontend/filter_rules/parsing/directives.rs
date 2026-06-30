//! Long `merge` and `dir-merge` / `per-dir` directive parsers.
//!
//! Parses the verbose merge-file directives that introduce per-file or
//! per-directory filter merges, applying their modifier strings and
//! resolving the merge file path.

use std::ffi::OsString;

use core::client::{DirMergeEnforcedKind, FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

use super::super::directive::{FilterDirective, MergeDirective};
use super::merge::parse_merge_modifiers;

pub(super) fn parse_long_merge_directive(text: &str) -> Option<Result<FilterDirective, Message>> {
    let remainder = text.strip_prefix("merge")?;
    let mut remainder =
        remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    let mut modifiers = "";
    if let Some(next) = remainder.strip_prefix(',') {
        let mut split = next.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        modifiers = split.next().unwrap_or("");
        remainder = split
            .next()
            .unwrap_or("")
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, text, false) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let mut path_text = remainder.trim_end();
    if path_text.is_empty() {
        if assume_cvsignore {
            path_text = ".cvsignore";
        } else {
            let message = rsync_error!(
                1,
                format!("filter merge directive '{text}' is missing a file path")
            )
            .with_role(Role::Client);
            return Some(Err(message));
        }
    }

    let enforced_kind = match options.enforced_kind() {
        Some(DirMergeEnforcedKind::Include) => Some(FilterRuleKind::Include),
        Some(DirMergeEnforcedKind::Exclude) => Some(FilterRuleKind::Exclude),
        None => None,
    };

    let directive =
        MergeDirective::new(OsString::from(path_text), enforced_kind).with_options(options);
    Some(Ok(FilterDirective::Merge(directive)))
}

pub(super) fn parse_dir_merge_alias(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
    const DIR_MERGE_ALIASES: [&str; 2] = ["dir-merge", "per-dir"];

    let mut matched_prefix = None;
    for alias in DIR_MERGE_ALIASES {
        if trimmed.len() >= alias.len() && trimmed[..alias.len()].eq_ignore_ascii_case(alias) {
            matched_prefix = Some((&trimmed[..alias.len()], &trimmed[alias.len()..]));
            break;
        }
    }

    let (label, remainder) = matched_prefix?;
    let mut remainder =
        remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    let mut modifiers = "";
    if let Some(rest) = remainder.strip_prefix(',') {
        let mut split = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        modifiers = split.next().unwrap_or("");
        remainder = split
            .next()
            .unwrap_or("")
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }

    let (mut options, assume_cvsignore) = match parse_merge_modifiers(modifiers, trimmed, true) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    let mut path_text = remainder.trim_end();
    if path_text.is_empty() {
        if assume_cvsignore {
            path_text = ".cvsignore";
        } else {
            let text = format!("filter rule '{trimmed}' is missing a file name after '{label}'");
            return Some(Err(rsync_error!(1, text).with_role(Role::Client)));
        }
    }

    // upstream: exclude.c - a leading '/' on the merge filename means the
    // file is only looked for in the transfer root directory (anchor_root).
    // Strip the '/' so Path::join() produces a relative path, and set the
    // anchor_root flag on options instead.
    if let Some(stripped) = path_text.strip_prefix('/') {
        path_text = stripped;
        options = options.anchor_root(true);
    }

    Some(Ok(FilterDirective::Rule(FilterRuleSpec::dir_merge(
        path_text.to_owned(),
        options,
    ))))
}
