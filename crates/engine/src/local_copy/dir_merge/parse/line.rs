use super::{
    dir_merge::parse_dir_merge_directive,
    merge::{parse_merge_directive, parse_short_merge_directive_line},
    types::{FilterParseError, ParsedFilterDirective},
};
use crate::local_copy::filter_program::ExcludeIfPresentRule;
use rsync_filters::FilterRule;

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
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '+' requires a pattern"));
        }
        return Ok(Some(ParsedFilterDirective::Rule(FilterRule::include(
            pattern.to_string(),
        ))));
    }

    if let Some(remainder) = trimmed.strip_prefix('-') {
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '-' requires a pattern"));
        }
        return Ok(Some(ParsedFilterDirective::Rule(FilterRule::exclude(
            pattern.to_string(),
        ))));
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let keyword = parts.next().unwrap_or("");
    let remainder = parts.next().unwrap_or("").trim_start();

    let handle_keyword = |pattern: &str,
                          builder: fn(String) -> FilterRule|
     -> Result<Option<ParsedFilterDirective>, FilterParseError> {
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        Ok(Some(ParsedFilterDirective::Rule(builder(
            pattern.to_string(),
        ))))
    };

    if keyword.len() == 1 {
        let shorthand = keyword.chars().next().unwrap().to_ascii_lowercase();
        match shorthand {
            'p' => {
                return handle_keyword(remainder, FilterRule::protect);
            }
            'r' => {
                return handle_keyword(remainder, FilterRule::risk);
            }
            's' => {
                if remainder.is_empty() {
                    return Err(FilterParseError::new("filter directive missing pattern"));
                }
                let rule = FilterRule::show(remainder.to_string());
                return Ok(Some(ParsedFilterDirective::Rule(rule)));
            }
            'h' => {
                if remainder.is_empty() {
                    return Err(FilterParseError::new("filter directive missing pattern"));
                }
                let rule = FilterRule::hide(remainder.to_string());
                return Ok(Some(ParsedFilterDirective::Rule(rule)));
            }
            _ => {}
        }
    }

    if keyword.eq_ignore_ascii_case("include") {
        return handle_keyword(remainder, FilterRule::include);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return handle_keyword(remainder, FilterRule::exclude);
    }

    if keyword.eq_ignore_ascii_case("show") {
        if remainder.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        let rule = FilterRule::show(remainder.to_string());
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    if keyword.eq_ignore_ascii_case("hide") {
        if remainder.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        let rule = FilterRule::hide(remainder.to_string());
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword(remainder, FilterRule::protect);
    }

    if keyword.eq_ignore_ascii_case("risk") {
        return handle_keyword(remainder, FilterRule::risk);
    }

    Err(FilterParseError::new(format!(
        "unsupported filter directive '{trimmed}'"
    )))
}
