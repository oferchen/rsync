//! Short-prefix, shorthand, and keyword rule parsers.
//!
//! These parsers handle the non-merge filter-rule forms that
//! `parse_rule_directive` dispatches to: the `P`/`H`/`S`/`R` shorthands, the
//! `+`/`-` short rules, and the long
//! `include`/`exclude`/`show`/`hide`/`protect`/`risk` keyword rules.

use core::client::FilterRuleSpec;
use core::message::Message;

use super::super::directive::FilterDirective;
use super::helpers::{split_keyword_modifiers, split_short_modifiers, unknown_filter_rule};
use super::modifiers::{parse_rule_modifiers, rule_directive};
use super::rule_line::RuleLine;
use super::shorthand::parse_filter_shorthand;

/// Parses the `P`/`H`/`S`/`R` single-letter shorthand rules (protect, hide,
/// show, risk). Returns `None` when the text is not one of these forms.
pub(super) fn parse_shorthand_rules(
    line: RuleLine<'_>,
) -> Option<Result<FilterDirective, Message>> {
    if let Some(result) = parse_filter_shorthand(line, 'P', FilterRuleSpec::protect) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(line, 'H', FilterRuleSpec::hide) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(line, 'S', FilterRuleSpec::show) {
        return Some(result);
    }

    if let Some(result) = parse_filter_shorthand(line, 'R', FilterRuleSpec::risk) {
        return Some(result);
    }

    None
}

/// Parses a `+`/`-` short rule (include/exclude) identified by `prefix`,
/// applying any rule modifiers to the pattern via `builder`. Returns `None`
/// when the text does not begin with `prefix`.
pub(super) fn parse_short_include_rule(
    line: RuleLine<'_>,
    prefix: char,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let trimmed = line.text();
    let remainder = trimmed.strip_prefix(prefix)?;
    let (modifier_text, remainder) = split_short_modifiers(remainder);
    // `+`/`-` rules never bind a side via their prefix, so the `s`/`r` modifiers
    // stay valid (prefix_specifies_side = false). upstream: exclude.c:1340-1344.
    let modifiers = match parse_rule_modifiers(modifier_text, line, false) {
        Ok(state) => state,
        Err(error) => return Some(Err(error)),
    };
    // `split_short_modifiers` already consumed the single separator that
    // terminates the modifiers (upstream exclude.c:1444-1445), so the remainder
    // is the pattern verbatim. Do not trim further leading whitespace/`_`.
    Some(rule_directive(
        line,
        builder,
        prefix == '+',
        remainder,
        modifiers,
    ))
}

/// Parses a long-form keyword rule (`include`/`exclude`/`show`/`hide`/
/// `protect`/`risk` plus pattern). Keywords are matched case-sensitively to
/// mirror upstream; an unknown keyword yields a syntax error.
pub(super) fn parse_keyword_rule(line: RuleLine<'_>) -> Result<FilterDirective, Message> {
    let trimmed = line.text();
    // upstream: exclude.c:1222 `rule_strcmp` accepts `_` as a keyword separator
    // alongside `isspace`, and exclude.c:1444-1445 then consumes exactly that one
    // byte. MEASURED against rsync 3.5.0: `--filter='exclude_b.txt'` excludes
    // `b.txt`.
    let mut parts = trimmed.splitn(2, |ch: char| ch == '_' || ch.is_ascii_whitespace());
    let keyword = parts.next().expect("split always yields at least one part");
    let remainder = parts.next().unwrap_or("");
    let (keyword, keyword_modifiers) = split_keyword_modifiers(keyword);
    // `splitn` on the first separator already consumed the single separator
    // between the keyword and the pattern (upstream exclude.c:1444-1445), so the
    // remainder is the pattern verbatim. Do not trim further leading separators.
    let pattern = remainder;

    let build_rule = |builder: fn(String) -> FilterRuleSpec,
                      include: bool,
                      prefix_specifies_side: bool|
     -> Result<FilterDirective, Message> {
        let modifiers = parse_rule_modifiers(keyword_modifiers, line, prefix_specifies_side)?;
        rule_directive(line, builder, include, pattern, modifiers)
    };

    // upstream: exclude.c:1218-1228 rule_strcmp - the long-form keywords are
    // matched with a case-sensitive strncmp dispatched from a switch on the
    // lowercase first byte (exclude.c:1288-1330). A mixed-case keyword such as
    // `EXCLUDE`/`Include` therefore never matches; it reaches the inner switch
    // default and raises "Unknown filter rule" (RERR_SYNTAX). Compare exactly so
    // this parser mirrors that behaviour rather than silently coercing the case.
    // upstream: exclude.c:1340-1344 - include/exclude do not bind a side, so
    // `s`/`r`/`p` modifiers are all valid (prefix_specifies_side = false).
    if keyword == "include" {
        return build_rule(FilterRuleSpec::include, true, false);
    }

    if keyword == "exclude" {
        return build_rule(FilterRuleSpec::exclude, false, false);
    }

    // upstream: exclude.c:1345-1358 - show/hide/protect/risk map to the S/H/R/P
    // prefixes that set prefix_specifies_side, so `s`/`r` are invalid while `p`
    // (perishable) stays valid.
    if keyword == "show" {
        return build_rule(FilterRuleSpec::show, true, true);
    }

    if keyword == "hide" {
        return build_rule(FilterRuleSpec::hide, false, true);
    }

    if keyword == "protect" {
        return build_rule(FilterRuleSpec::protect, false, true);
    }

    if keyword == "risk" {
        return build_rule(FilterRuleSpec::risk, true, true);
    }

    // upstream: exclude.c:1363 `filter_rule_err("Unknown filter rule", *rulestr_ptr)` -
    // an unrecognized prefix or keyword reaches the inner switch `default:`.
    Err(unknown_filter_rule(line))
}
