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
        // upstream: exclude.c:1143 RULE_STRCMP(s, "dir-merge") is a case-sensitive
        // strncmp reached via `case 'd'`, so `DIR-MERGE`/`Dir-Merge` never match
        // the keyword. Compare bytes exactly (the `per-dir` alias is an oc-rsync
        // extension held to the same case-sensitivity for consistency). Every
        // alias is ASCII, so an equal prefix guarantees `alias.len()` is a char
        // boundary, keeping the slices below panic-safe.
        if trimmed.as_bytes().starts_with(alias.as_bytes()) {
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

    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, trimmed, true) {
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

    // upstream: exclude.c:599-617 parse_merge_name - a leading '/' on the merge
    // FILENAME only affects where the merge file is looked up (an ancestor
    // parent_dirscan); the '/' is stripped from the name and does NOT anchor
    // the rules loaded from the file. Rule anchoring to the merge directory
    // happens per-rule in add_rule (exclude.c:200-207) only when the RULE
    // pattern itself starts with '/'. Setting anchor_root here would wrongly
    // root-anchor every rule (e.g. `- secret*` in `d1/d2/.rsync-filter` would
    // become `/d1/d2/secret*` and stop matching `d1/d2/d3/secret.deeper`).
    // The '/' modifier (dir-merge,/ file) is the real anchor_root source and is
    // handled in parse_merge_modifiers.
    if let Some(stripped) = path_text.strip_prefix('/') {
        path_text = stripped;
    }

    Some(Ok(FilterDirective::Rule(FilterRuleSpec::dir_merge(
        path_text.to_owned(),
        options,
    ))))
}
