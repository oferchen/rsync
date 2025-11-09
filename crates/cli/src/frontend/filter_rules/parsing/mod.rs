use std::ffi::{OsStr, OsString};

use core::client::{DirMergeEnforcedKind, FilterRuleKind, FilterRuleSpec};
use core::message::{Message, Role};
use core::rsync_error;

use super::directive::{FilterDirective, MergeDirective};

mod helpers;
mod merge;
mod modifiers;
mod shorthand;

use helpers::{split_keyword_modifiers, split_short_rule_modifiers};
use merge::parse_short_merge_directive;
use modifiers::{apply_rule_modifiers, parse_rule_modifiers};
use shorthand::parse_filter_shorthand;

pub(crate) use merge::parse_merge_modifiers;

pub(crate) fn parse_filter_directive(argument: &OsStr) -> Result<FilterDirective, Message> {
    let text = argument.to_string_lossy();
    let trimmed_leading = text.trim_start();

    if let Some(result) = parse_short_merge_directive(trimmed_leading) {
        return result;
    }

    if let Some(result) = parse_long_merge_directive(trimmed_leading) {
        return result;
    }

    parse_rule_directive(trimmed_leading)
}

fn parse_long_merge_directive(text: &str) -> Option<Result<FilterDirective, Message>> {
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

fn parse_rule_directive(text: &str) -> Result<FilterDirective, Message> {
    let trimmed = text.trim_end();

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

fn parse_shorthand_rules(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
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

fn parse_exclude_if_present(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
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
        FilterRuleSpec::exclude_if_present(pattern_text.to_string()),
    )))
}

fn parse_short_include_rule(
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
    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    if pattern.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{prefix}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    let rule = builder(pattern.to_string());
    let rule = match apply_rule_modifiers(rule, modifiers, trimmed) {
        Ok(rule) => rule,
        Err(error) => return Some(Err(error)),
    };
    Some(Ok(FilterDirective::Rule(rule)))
}

fn parse_dir_merge_alias(trimmed: &str) -> Option<Result<FilterDirective, Message>> {
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

    Some(Ok(FilterDirective::Rule(FilterRuleSpec::dir_merge(
        path_text.to_string(),
        options,
    ))))
}

fn parse_keyword_rule(trimmed: &str) -> Result<FilterDirective, Message> {
    let mut parts = trimmed.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let keyword = parts.next().expect("split always yields at least one part");
    let remainder = parts.next().unwrap_or("");
    let (keyword, keyword_modifiers) = split_keyword_modifiers(keyword);
    let pattern = remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());

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
        let rule = builder(pattern.to_string());
        let rule = apply_rule_modifiers(rule, modifiers, trimmed)?;
        Ok(FilterDirective::Rule(rule))
    };

    if keyword.eq_ignore_ascii_case("include") {
        return build_rule(FilterRuleSpec::include, true, true);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return build_rule(FilterRuleSpec::exclude, true, true);
    }

    if keyword.eq_ignore_ascii_case("show") {
        return build_rule(FilterRuleSpec::show, false, false);
    }

    if keyword.eq_ignore_ascii_case("hide") {
        return build_rule(FilterRuleSpec::hide, false, false);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return build_rule(FilterRuleSpec::protect, false, false);
    }

    if keyword.eq_ignore_ascii_case("risk") {
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
