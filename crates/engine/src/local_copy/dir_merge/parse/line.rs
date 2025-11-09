use super::{
    dir_merge::parse_dir_merge_directive,
    merge::{parse_merge_directive, parse_short_merge_directive_line},
    modifiers::split_short_rule_modifiers,
    types::{FilterParseError, ParsedFilterDirective},
};
use crate::local_copy::filter_program::ExcludeIfPresentRule;
use filters::FilterRule;
use std::fmt;

#[derive(Default)]
struct RuleModifierState {
    anchor_root: bool,
    sender: Option<bool>,
    receiver: Option<bool>,
    perishable: bool,
    xattr_only: bool,
}

fn unsupported_modifier_error(directive: &str, modifier: impl fmt::Display) -> FilterParseError {
    FilterParseError::new(format!(
        "filter directive '{directive}' uses unsupported modifier '{modifier}'"
    ))
}

fn parse_rule_modifiers(
    modifiers: &str,
    directive: &str,
    allow_perishable: bool,
    allow_xattr: bool,
) -> Result<RuleModifierState, FilterParseError> {
    let mut state = RuleModifierState::default();

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '/' => state.anchor_root = true,
            's' => {
                state.sender = Some(true);
                if state.receiver.is_none() {
                    state.receiver = Some(false);
                }
            }
            'r' => {
                state.receiver = Some(true);
                if state.sender.is_none() {
                    state.sender = Some(false);
                }
            }
            'p' => {
                if allow_perishable {
                    state.perishable = true;
                } else {
                    return Err(unsupported_modifier_error(directive, modifier));
                }
            }
            'x' => {
                if allow_xattr {
                    state.xattr_only = true;
                } else {
                    return Err(unsupported_modifier_error(directive, modifier));
                }
            }
            _ => {
                return Err(unsupported_modifier_error(directive, modifier));
            }
        }
    }

    Ok(state)
}

fn apply_rule_modifiers(
    mut rule: FilterRule,
    modifiers: RuleModifierState,
    directive: &str,
) -> Result<FilterRule, FilterParseError> {
    if modifiers.anchor_root {
        rule = rule.anchor_to_root();
    }

    if let Some(sender) = modifiers.sender {
        rule = rule.with_sender(sender);
    }

    if let Some(receiver) = modifiers.receiver {
        rule = rule.with_receiver(receiver);
    }

    if modifiers.perishable {
        rule = rule.with_perishable(true);
    }

    if modifiers.xattr_only {
        match rule.action() {
            filters::FilterAction::Include | filters::FilterAction::Exclude => {
                rule = rule
                    .with_xattr_only(true)
                    .with_sender(true)
                    .with_receiver(true);
            }
            _ => {
                return Err(FilterParseError::new(format!(
                    "filter directive '{directive}' cannot combine 'x' modifiers with this directive"
                )));
            }
        }
    }

    Ok(rule)
}

fn split_keyword_modifiers(keyword: &str) -> (&str, &str) {
    if let Some((name, modifiers)) = keyword.split_once(',') {
        (name, modifiers)
    } else {
        (keyword, "")
    }
}

pub(crate) fn parse_filter_directive_line(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    if text.is_empty() || text.starts_with('#') {
        return Ok(None);
    }

    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let trimmed = trimmed.trim_end();

    if trimmed == "!" || trimmed.eq_ignore_ascii_case("clear") {
        return Ok(Some(ParsedFilterDirective::Clear));
    }

    if let Some(directive) = parse_short_merge_directive_line(trimmed)? {
        return Ok(Some(directive));
    }

    if let Some(directive) = parse_merge_directive(trimmed)? {
        return Ok(Some(directive));
    }

    if let Some(directive) = parse_dir_merge_directive(trimmed)? {
        return Ok(Some(directive));
    }

    const EXCLUDE_IF_PRESENT_PREFIX: &str = "exclude-if-present";

    if trimmed.len() >= EXCLUDE_IF_PRESENT_PREFIX.len()
        && trimmed[..EXCLUDE_IF_PRESENT_PREFIX.len()]
            .eq_ignore_ascii_case(EXCLUDE_IF_PRESENT_PREFIX)
    {
        let mut remainder = trimmed[EXCLUDE_IF_PRESENT_PREFIX.len()..]
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        if let Some(rest) = remainder.strip_prefix('=') {
            remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        }

        let pattern_text = remainder.trim();
        if pattern_text.is_empty() {
            return Err(FilterParseError::new(
                "filter directive 'exclude-if-present' requires a marker file",
            ));
        }

        return Ok(Some(ParsedFilterDirective::ExcludeIfPresent(
            ExcludeIfPresentRule::new(pattern_text),
        )));
    }

    if let Some(remainder) = trimmed.strip_prefix('+') {
        let (modifier_text, remainder) = split_short_rule_modifiers(remainder);
        let modifiers = parse_rule_modifiers(modifier_text, trimmed, true, true)?;
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '+' requires a pattern"));
        }
        let rule = FilterRule::include(pattern.to_string());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    if let Some(remainder) = trimmed.strip_prefix('-') {
        let (modifier_text, remainder) = split_short_rule_modifiers(remainder);
        let modifiers = parse_rule_modifiers(modifier_text, trimmed, true, true)?;
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '-' requires a pattern"));
        }
        let rule = FilterRule::exclude(pattern.to_string());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let keyword = parts.next().unwrap_or("");
    let remainder = parts.next().unwrap_or("").trim_start();
    let (keyword, keyword_modifiers) = split_keyword_modifiers(keyword);

    let handle_keyword = |pattern: &str,
                          builder: fn(String) -> FilterRule,
                          allow_perishable: bool,
                          allow_xattr: bool|
     -> Result<Option<ParsedFilterDirective>, FilterParseError> {
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        let modifiers =
            parse_rule_modifiers(keyword_modifiers, trimmed, allow_perishable, allow_xattr)?;
        let rule = builder(pattern.to_string());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        Ok(Some(ParsedFilterDirective::Rule(rule)))
    };

    if keyword.len() == 1 {
        let shorthand = keyword.chars().next().unwrap().to_ascii_lowercase();
        match shorthand {
            'p' => {
                if !keyword_modifiers.is_empty() {
                    return Err(unsupported_modifier_error(trimmed, keyword_modifiers));
                }
                return handle_keyword(remainder, FilterRule::protect, false, false);
            }
            'r' => {
                if !keyword_modifiers.is_empty() {
                    return Err(unsupported_modifier_error(trimmed, keyword_modifiers));
                }
                return handle_keyword(remainder, FilterRule::risk, false, false);
            }
            's' => {
                return handle_keyword(remainder, FilterRule::show, false, false);
            }
            'h' => {
                return handle_keyword(remainder, FilterRule::hide, false, false);
            }
            _ => {}
        }
    }

    if keyword.eq_ignore_ascii_case("include") {
        return handle_keyword(remainder, FilterRule::include, true, true);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return handle_keyword(remainder, FilterRule::exclude, true, true);
    }

    if keyword.eq_ignore_ascii_case("show") {
        return handle_keyword(remainder, FilterRule::show, false, false);
    }

    if keyword.eq_ignore_ascii_case("hide") {
        return handle_keyword(remainder, FilterRule::hide, false, false);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword(remainder, FilterRule::protect, false, false);
    }

    if keyword.eq_ignore_ascii_case("risk") {
        return handle_keyword(remainder, FilterRule::risk, false, false);
    }

    Err(FilterParseError::new(format!(
        "unsupported filter directive '{trimmed}'"
    )))
}
