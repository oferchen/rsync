//! Top-level filter-rule entry points and the rule-directive dispatcher.
//!
//! Hosts the public `parse_filter_directive` / `parse_old_prefix_rule`
//! entry points plus the `parse_rule_directive` dispatcher that routes a
//! trimmed line to the more specialized parsers.

use std::borrow::Cow;
use std::ffi::OsStr;

use core::client::{FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;
use filters::{ClearToken, RuleSource, classify_clear_token};

use super::super::directive::FilterDirective;
use super::directives::{parse_dir_merge_alias, parse_long_merge_directive};
use super::merge::parse_short_merge_directive;
use super::rule_line::RuleLine;
use super::rules::{
    parse_exclude_if_present, parse_keyword_rule, parse_short_include_rule, parse_shorthand_rules,
};

/// Parses a top-level `--filter` argument into a `FilterDirective`, trying the
/// short and long merge forms before the general rule dispatcher. Leading
/// whitespace is preserved to mirror upstream's non-word-split behaviour.
pub(crate) fn parse_filter_directive(
    argument: &OsStr,
    source: RuleSource<'_>,
) -> Result<FilterDirective, Message> {
    let text = argument.to_string_lossy();
    // upstream: exclude.c:1100-1213 parse_rule_tok - leading whitespace is only
    // skipped under FILTRULE_WORD_SPLIT, which a top-level `--filter` rule never
    // carries. A leading space therefore reaches the prefix `switch` default and
    // raises "Unknown filter rule" (RERR_SYNTAX). Do not trim the leading edge.
    let rule = RuleLine::new(&text, source);

    let directive = parse_short_merge_directive(rule)
        .or_else(|| parse_long_merge_directive(rule))
        .unwrap_or_else(|| parse_rule_directive(rule))?;

    Ok(discard_over_long(directive, source))
}

/// Reports and discards a directive whose pattern reaches `MAXPATHLEN`.
///
/// upstream: exclude.c:1533-1538, in `parse_filter_str`'s token loop. It
/// measures `pat_len` - the pattern *alone*, since `parse_rule_tok` has already
/// consumed the action prefix and any modifiers (exclude.c:1456-1464) - and
/// `continue`s, so the rule contributes nothing and parsing exits 0.
///
/// Consulting the CONSTRUCTED directive is how oc reads that same quantity
/// without duplicating the prefix stripping: the pattern a directive carries is
/// exactly what upstream measures. `Clear`, `Noop` and `CvsDefaults` carry no
/// pattern at all, so upstream's check can never fire for them.
///
/// upstream's check sits above the `FILTRULE_MERGE_FILE` branch (`:1550`), and
/// so does this one in effect: the merge file is opened later, by the caller
/// that consumes the directive, so an over-long merge name never reaches a
/// filesystem here either.
fn discard_over_long(directive: FilterDirective, source: RuleSource<'_>) -> FilterDirective {
    let pattern = match &directive {
        FilterDirective::Rule(spec) => Cow::Borrowed(spec.pattern()),
        FilterDirective::Merge(merge) => merge.source().to_string_lossy(),
        FilterDirective::Clear | FilterDirective::Noop | FilterDirective::CvsDefaults => {
            return directive;
        }
    };

    if !filters::is_over_long(&pattern) {
        return directive;
    }

    // The rule text crosses `rule_text` (exclude.c:88-123): upstream passes a
    // NULL template here, which falls back to the am-I-parsing-a-file state
    // (exclude.c:67-69) - the fact oc carries explicitly on `source`.
    logging::emit_info_coded(
        logging::InfoFlag::Misc,
        0,
        logging::LogCode::Error,
        filters::over_long_filter(&source.rule_text(&pattern)),
    );

    // upstream's `continue`: the rule is dropped and parsing carries on.
    FilterDirective::Noop
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
    source: RuleSource<'_>,
) -> Result<FilterDirective, Message> {
    debug_assert!(
        matches!(
            default_kind,
            FilterRuleKind::Include | FilterRuleKind::Exclude
        ),
        "old-prefix parsing only supports Include or Exclude defaults"
    );

    if line.is_empty() {
        // upstream: exclude.c:1107 parse_rule_tok returns NULL for an empty
        // rule, so a blank `--exclude`/`--include` value adds nothing (exit 0).
        return Ok(FilterDirective::Noop);
    }

    let bytes = line.as_bytes();
    // upstream: exclude.c parse_rule_tok() XFLG_OLD_PREFIXES branch - `*s ==
    // '!'` sets FILTRULE_CLEAR_LIST tentatively WITHOUT advancing `s`, so the
    // later `if (len > 1) rule->rflags &= ~FILTRULE_CLEAR_LIST` measures the
    // whole line. Only a line that is exactly `!` stays a clear; anything
    // longer - including `! ` - is demoted back to a literal pattern. The
    // trailing-characters error cannot fire here, because its guard excludes
    // XFLG_OLD_PREFIXES.
    if line == "!" {
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
        // The rule text crosses `rule_text` (exclude.c:88-123): an
        // `--exclude-from`/`--include-from` line is a file's contents.
        let message = rsync_error!(
            1,
            "filter rule is missing a pattern: '{}'",
            source.rule_text(line)
        )
        .with_role(Role::Client);
        return Err(message);
    }

    let rule = match kind {
        FilterRuleKind::Include => FilterRuleSpec::include(pattern.to_owned()),
        FilterRuleKind::Exclude => FilterRuleSpec::exclude(pattern.to_owned()),
        _ => unreachable!("default_kind is restricted to Include/Exclude above"),
    };
    Ok(discard_over_long(FilterDirective::Rule(rule), source))
}

/// Dispatches a trimmed rule line (no merge prefix) to the matching parser:
/// `!`/`clear`, CVS convenience, shorthands, `exclude-if-present`, `+`/`-`
/// short rules, then the long keyword rules.
pub(super) fn parse_rule_directive(line: RuleLine<'_>) -> Result<FilterDirective, Message> {
    let text = line.text();
    // upstream: exclude.c:1313 parse_rule_tok - the pattern length is strlen, so
    // trailing whitespace is part of the pattern and is never stripped. A rule
    // like `- *.o ` keeps the trailing space in its pattern, so `x.o` is not
    // matched by `*.o ` and stays included.
    let trimmed = text;

    if trimmed.is_empty() {
        // upstream: exclude.c:1107 parse_rule_tok returns NULL for an empty rule
        // string (`if (!*s) return NULL`), so a blank `--filter` value adds
        // nothing and exits 0. A `--filter=" "` (whitespace) value is NOT empty
        // here - a top-level `--filter` never carries FILTRULE_WORD_SPLIT, so the
        // space reaches the prefix switch and still raises "Unknown filter rule".
        return Ok(FilterDirective::Noop);
    }

    // upstream: exclude.c parse_rule_tok() - both the bare `!` and the `clear`
    // long name set FILTRULE_CLEAR_LIST, and both leave exactly one character
    // for the shared `if (*s) s++` to step over, so a lone trailing space is
    // already "trailing characters". `RULE_STRCMP` is a case-sensitive strncmp
    // reached only via `case 'c'`, so `CLEAR`/`Clear` are not the directive and
    // fall through to "Unknown filter rule".
    match classify_clear_token(trimmed.as_bytes()) {
        ClearToken::Clear => return Ok(FilterDirective::Clear),
        ClearToken::TrailingCharacters => {
            let message = rsync_error!(1, "'!' rule has trailing characters: {}", line.shown())
                .with_role(Role::Client);
            return Err(message);
        }
        ClearToken::NotClearToken => {}
    }

    if is_cvs_convenience_rule(trimmed) {
        return Ok(FilterDirective::CvsDefaults);
    }

    if let Some(result) = parse_shorthand_rules(line) {
        return result;
    }

    if let Some(result) = parse_exclude_if_present(line) {
        return result;
    }

    if let Some(result) = parse_short_include_rule(line, '+', FilterRuleSpec::include) {
        return result;
    }

    if let Some(result) = parse_short_include_rule(line, '-', FilterRuleSpec::exclude) {
        return result;
    }

    if let Some(result) = parse_dir_merge_alias(line) {
        return result;
    }

    parse_keyword_rule(line)
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
