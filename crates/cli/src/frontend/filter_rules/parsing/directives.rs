//! Long `merge` and `dir-merge` directive parsers.
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
use super::rule_line::RuleLine;

/// Parses a long-form `merge[,MODS] FILE` directive. Returns `None` when the
/// text does not start with `merge`, or an error when the file path is missing.
pub(super) fn parse_long_merge_directive(
    line: RuleLine<'_>,
) -> Option<Result<FilterDirective, Message>> {
    let text = line.text();
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
    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, line, false) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    // The merge PATH is the rest of the rule verbatim: upstream consumes one
    // separator after the keyword (exclude.c:1444-1445) and then takes
    // `len = strlen(s)` (exclude.c:1465); `parse_merge_name` (exclude.c:696-752)
    // only runs `clean_fname` (:734), which collapses slashes and `..` but never
    // whitespace. MEASURED against rsync 3.5.0 with a filter file literally
    // named `.rsync-filter ` (trailing space) holding `- b`: upstream opens it
    // through `--filter='merge DIR/.rsync-filter '` and copies only `a`; oc
    // trimmed the path and aborted with `failed to open exclude file` (exit 11).
    let mut path_text = remainder;
    if path_text.is_empty() {
        if assume_cvsignore {
            path_text = ".cvsignore";
        } else {
            let message = rsync_error!(
                1,
                format!(
                    "filter merge directive '{}' is missing a file path",
                    line.shown()
                )
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

/// Parses a long-form `dir-merge[,MODS] FILE` directive. Returns `None` when
/// the keyword does not match, or an error when the file name is missing.
pub(super) fn parse_dir_merge_alias(
    line: RuleLine<'_>,
) -> Option<Result<FilterDirective, Message>> {
    let trimmed = line.text();
    // upstream: exclude.c:1143 RULE_STRCMP(s, "dir-merge") is a case-sensitive
    // strncmp reached via `case 'd'`, so `DIR-MERGE`/`Dir-Merge` never match the
    // keyword. Compare bytes exactly; "dir-merge" is ASCII, so a matching prefix
    // lands on a char boundary, keeping the slices below panic-safe. (upstream
    // has no other dir-merge spelling, so no alias is accepted.)
    const KEYWORD: &str = "dir-merge";
    let remainder = trimmed.strip_prefix(KEYWORD)?;
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

    let (options, assume_cvsignore) = match parse_merge_modifiers(modifiers, line, true) {
        Ok(result) => result,
        Err(error) => return Some(Err(error)),
    };

    // The dir-merge FILENAME is the rest of the rule verbatim: one separator is
    // consumed after the keyword (exclude.c:1444-1445), the length is
    // `strlen(s)` (exclude.c:1465), and `parse_merge_name` (exclude.c:696-752)
    // only runs `clean_fname` (:734). MEASURED against rsync 3.5.0 over a source
    // holding `a`, `b` and a filter file literally named `.rsync-filter `
    // (trailing space) that reads `- b`, with
    // `--filter='dir-merge .rsync-filter '`: upstream copies only `a`; oc
    // trimmed the name, found no merge file, and copied `b` too.
    let mut path_text = remainder;
    if path_text.is_empty() {
        if assume_cvsignore {
            path_text = ".cvsignore";
        } else {
            let text = format!(
                "filter rule '{}' is missing a file name after '{KEYWORD}'",
                line.shown()
            );
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
