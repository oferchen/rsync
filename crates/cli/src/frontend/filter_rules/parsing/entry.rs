//! Top-level filter-rule entry points and the rule-directive dispatcher.
//!
//! Hosts the public `parse_filter_directive` / `parse_old_prefix_rule`
//! entry points plus the `parse_rule_directive` dispatcher that routes a
//! trimmed line to the more specialized parsers.

use std::ffi::OsStr;

use core::client::{FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

use super::super::directive::FilterDirective;
use super::directives::{parse_dir_merge_alias, parse_long_merge_directive};
use super::merge::parse_short_merge_directive;
use super::rules::{
    parse_exclude_if_present, parse_keyword_rule, parse_short_include_rule, parse_shorthand_rules,
};

pub(crate) fn parse_filter_directive(argument: &OsStr) -> Result<FilterDirective, Message> {
    let text = argument.to_string_lossy();
    // upstream: exclude.c:1100-1213 parse_rule_tok - leading whitespace is only
    // skipped under FILTRULE_WORD_SPLIT, which a top-level `--filter` rule never
    // carries. A leading space therefore reaches the prefix `switch` default and
    // raises "Unknown filter rule" (RERR_SYNTAX). Do not trim the leading edge.
    let rule: &str = &text;

    if let Some(result) = parse_short_merge_directive(rule) {
        return result;
    }

    if let Some(result) = parse_long_merge_directive(rule) {
        return result;
    }

    parse_rule_directive(rule)
}

/// Parses a line under upstream rsync's `XFLG_OLD_PREFIXES` compatibility mode
/// used by `--exclude`, `--exclude-from`, `--include`, and `--include-from`.
///
/// The only recognized prefixes are `- ` (exclude), `+ ` (include), and `!`
/// (clear). Everything else is treated as a raw pattern that takes the
/// `default_kind` (the rule kind associated with the option that introduced
/// this line). Empty patterns are rejected to match upstream
/// `exclude.c:parse_rule_tok()` which reports unexpected-end-of-rule.
///
/// upstream: exclude.c:parse_rule_tok() XFLG_OLD_PREFIXES branch (lines 1125-1133).
pub(crate) fn parse_old_prefix_rule(
    line: &str,
    default_kind: FilterRuleKind,
) -> Result<FilterDirective, Message> {
    debug_assert!(
        matches!(
            default_kind,
            FilterRuleKind::Include | FilterRuleKind::Exclude
        ),
        "old-prefix parsing only supports Include or Exclude defaults"
    );

    if line.is_empty() {
        let message = rsync_error!(1, "filter rule is empty").with_role(Role::Client);
        return Err(message);
    }

    let bytes = line.as_bytes();
    // upstream: `*s == '!'` triggers FILTRULE_CLEAR_LIST tentatively. Any
    // trailing non-whitespace then turns the rule back into a pattern, so
    // we honor `!` (optionally followed by whitespace) as a clear and let
    // `!pattern` fall through to the default rule kind.
    if bytes[0] == b'!' && (line.len() == 1 || line[1..].trim().is_empty()) {
        return Ok(FilterDirective::Clear);
    }

    let (kind, pattern) = if bytes.len() >= 2 && bytes[1] == b' ' {
        match bytes[0] {
            b'-' => (FilterRuleKind::Exclude, &line[2..]),
            b'+' => (FilterRuleKind::Include, &line[2..]),
            _ => (default_kind, line),
        }
    } else {
        (default_kind, line)
    };

    if pattern.is_empty() {
        let message =
            rsync_error!(1, "filter rule is missing a pattern: '{}'", line).with_role(Role::Client);
        return Err(message);
    }

    let rule = match kind {
        FilterRuleKind::Include => FilterRuleSpec::include(pattern.to_owned()),
        FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern.to_owned()),
        _ => unreachable!("default_kind is restricted to Include/Exclude above"),
    };
    Ok(FilterDirective::Rule(rule))
}

pub(super) fn parse_rule_directive(text: &str) -> Result<FilterDirective, Message> {
    // upstream: exclude.c:1313 parse_rule_tok - the pattern length is strlen, so
    // trailing whitespace is part of the pattern and is never stripped. A rule
    // like `- *.o ` keeps the trailing space in its pattern, so `x.o` is not
    // matched by `*.o ` and stays included.
    let trimmed = text;

    if trimmed.is_empty() {
        let message = rsync_error!(
            1,
            "filter rule is empty: supply '+', '-', '!', or 'merge FILE'"
        )
        .with_role(Role::Client);
        return Err(message);
    }

    if let Some(remainder) = trimmed.strip_prefix('!') {
        if remainder.trim().is_empty() {
            return Ok(FilterDirective::Clear);
        }

        let message = rsync_error!(1, "'!' rule has trailing characters: {}", trimmed)
            .with_role(Role::Client);
        return Err(message);
    }

    if trimmed.eq_ignore_ascii_case("clear") {
        return Ok(FilterDirective::Clear);
    }

    if is_cvs_convenience_rule(trimmed) {
        return Ok(FilterDirective::CvsDefaults);
    }

    if let Some(result) = parse_shorthand_rules(trimmed) {
        return result;
    }

    if let Some(result) = parse_exclude_if_present(trimmed) {
        return result;
    }

    if let Some(result) = parse_short_include_rule(trimmed, '+', FilterRuleSpec::include) {
        return result;
    }

    if let Some(result) = parse_short_include_rule(trimmed, '-', FilterRuleSpec::exclude) {
        return result;
    }

    if let Some(result) = parse_dir_merge_alias(trimmed) {
        return result;
    }

    parse_keyword_rule(trimmed)
}

/// Detects the cvs-convenience filter rule (`-C` or `+C`, with an optional
/// comma between the action and the modifier). Such a rule carries only the
/// `C` (cvs-ignore) modifier and no pattern; upstream expands it into the
/// global CVS default excludes rather than treating it as a literal pattern.
///
/// The per-directory `:C` / `.C` merge forms are handled earlier by the
/// merge-directive parser, so they never reach this check.
///
/// upstream: exclude.c:1441-1443 - a FILTRULE_CVS_IGNORE rule that is not a
/// merge triggers get_cvs_excludes().
pub(super) fn is_cvs_convenience_rule(trimmed: &str) -> bool {
    let body = match trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix('+'))
    {
        Some(rest) => rest,
        None => return false,
    };
    let body = body.strip_prefix(',').unwrap_or(body);
    // upstream: exclude.c:1252 the cvs-ignore modifier is uppercase `C`; a
    // lowercase `c` is rejected as an invalid modifier, so match exactly.
    body == "C"
}
