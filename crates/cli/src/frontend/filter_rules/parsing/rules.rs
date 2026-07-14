//! Short-prefix, shorthand, exclude-if-present, and keyword rule parsers.
//!
//! These parsers handle the non-merge filter-rule forms that
//! `parse_rule_directive` dispatches to: the `P`/`H`/`S`/`R` shorthands, the
//! `exclude-if-present` directive, the `+`/`-` short rules, and the long
//! `include`/`exclude`/`show`/`hide`/`protect`/`risk` keyword rules.

use core::client::FilterRuleSpec;
use core::message::{Message, Role};
use core::rsync_error;

use super::super::directive::FilterDirective;
use super::helpers::{split_keyword_modifiers, split_short_rule_modifiers};
use super::modifiers::{apply_rule_modifiers, parse_rule_modifiers};
use super::shorthand::parse_filter_shorthand;

pub(super) fn parse_shorthand_rules(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
    if let Some(result) = parse_filter_shorthand(trimmed, 'P', "P", FilterRuleSpec::protect) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'H', "H", FilterRuleSpec::hide) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'S', "S", FilterRuleSpec::show) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(trimmed, 'R', "R", FilterRuleSpec::risk) {
        return Some(result);
    }

    None
}

pub(super) fn parse_exclude_if_present(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
    const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";
    if trimmed.len() < EXCLUDE_IF_PRESENT_PREFIX.len() {
        return None;
    }

    let prefix = &trimmed[..EXCLUDE_IF_PRESENT_PREFIX.len()];
    if !prefix.eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX) {
        return None;
    }

    let mut remainder = trimmed[EXCLUDE_IF_PRESENT_PREFIX.len()..]
        .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if let Some(rest) = remainder.strip_prefix('=') {
        remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }

    let pattern_text = remainder.trim();
    if pattern_text.is_empty() {
        let message = rsync_error!(
            1,
            format!("filter rule '{trimmed}' is missing a marker file after 'exclude-if-present'")
        )
        .with_role(Role::Client);
        return Some(Err(message));
    }

    Some(Ok(FilterDirective::Rule(
        FilterRuleSpec::exclude_if_present(pattern_text.to_owned()),
    )))
}

pub(super) fn parse_short_include_rule(
    trimmed: &str,
    prefix: char,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let remainder = trimmed.strip_prefix(prefix)?;
    let (modifier_text, remainder) = split_short_rule_modifiers(remainder);
    let modifiers = match parse_rule_modifiers(modifier_text, trimmed, true, true) {
        Ok(state) => state,
        Err(error) => return Some(Err(error)),
    };
    // `split_short_rule_modifiers` already consumed the single separator that
    // terminates the modifiers (upstream exclude.c:1290-1291), so the remainder
    // is the pattern verbatim. Do not trim further leading whitespace/`_`.
    let pattern = remainder;
    if pattern.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{prefix}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    let rule = builder(pattern.to_owned());
    let rule = match apply_rule_modifiers(rule, modifiers, trimmed) {
        Ok(rule) => rule,
        Err(error) => return Some(Err(error)),
    };
    Some(Ok(FilterDirective::Rule(rule)))
}

pub(super) fn parse_keyword_rule(trimmed: &str) -> Result<FilterDirective, Message> {
    let mut parts = trimmed.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let keyword = parts.next().expect("split always yields at least one part");
    let remainder = parts.next().unwrap_or("");
    let (keyword, keyword_modifiers) = split_keyword_modifiers(keyword);
    // `splitn` on the first whitespace already consumed the single separator
    // between the keyword and the pattern (upstream exclude.c:1290-1291), so the
    // remainder is the pattern verbatim. Do not trim further leading separators.
    let pattern = remainder;

    let build_rule = |builder: fn(String) -> FilterRuleSpec,
                      allow_perishable: bool,
                      allow_xattr: bool|
     -> Result<FilterDirective, Message> {
        if pattern.is_empty() {
            let text = format!("filter rule '{trimmed}' is missing a pattern after '{keyword}'");
            let message = rsync_error!(1, text).with_role(Role::Client);
            return Err(message);
        }

        let modifiers =
            parse_rule_modifiers(keyword_modifiers, trimmed, allow_perishable, allow_xattr)?;
        let rule = builder(pattern.to_owned());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        Ok(FilterDirective::Rule(rule))
    };

    // upstream: exclude.c:1069-1078 rule_strcmp - the long-form keywords are
    // matched with a case-sensitive strncmp dispatched from a switch on the
    // lowercase first byte (exclude.c:1137-1173). A mixed-case keyword such as
    // `EXCLUDE`/`Include` therefore never matches; it reaches the inner switch
    // default and raises "Unknown filter rule" (RERR_SYNTAX). Compare exactly so
    // this parser mirrors that behaviour rather than silently coercing the case.
    if keyword == "include" {
        return build_rule(FilterRuleSpec::include, true, true);
    }

    if keyword == "exclude" {
        return build_rule(FilterRuleSpec::exclude, true, true);
    }

    if keyword == "show" {
        return build_rule(FilterRuleSpec::show, false, false);
    }

    if keyword == "hide" {
        return build_rule(FilterRuleSpec::hide, false, false);
    }

    if keyword == "protect" {
        return build_rule(FilterRuleSpec::protect, false, false);
    }

    if keyword == "risk" {
        return build_rule(FilterRuleSpec::risk, false, false);
    }

    let message = rsync_error!(
        1,
        "unsupported filter rule '{}': this build currently supports only '+' (include), '-' (exclude), '!' (clear), 'include PATTERN', 'exclude PATTERN', 'show PATTERN', 'hide PATTERN', 'protect PATTERN', 'risk PATTERN', 'merge[,MODS] FILE' or '.[,MODS] FILE', and 'dir-merge[,MODS] FILE' or ':[,MODS] FILE' directives",
        trimmed
    )
    .with_role(Role::Client);
    Err(message)
}
