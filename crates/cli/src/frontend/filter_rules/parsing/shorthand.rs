use core::client::FilterRuleSpec;
use core::message::{Message, Role};
use core::rsync_error;

use super::super::directive::FilterDirective;
use super::helpers::consume_rule_separator;

/// Parses a single-character rule prefix (`short`) followed by a separator and
/// a pattern, building the rule via `builder`. Returns `None` when `short` does
/// not match case-sensitively or no separator follows; an error when the
/// pattern is missing.
pub(super) fn parse_filter_shorthand(
    trimmed: &str,
    short: char,
    label: &str,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    // upstream: exclude.c:1137-1178 - the single-char rule prefixes H/S/P/R are
    // matched case-sensitively (they reach the `switch (*s)` default arm). A
    // lowercase `h`/`s`/`p`/`r` is instead the first byte of a long keyword and,
    // when it is not one, raises "Unknown filter rule". Match the prefix exactly
    // so `s foo`/`h foo` are rejected rather than treated as show/hide.
    if first != short {
        return None;
    }

    let remainder = chars.as_str();
    if remainder.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{label}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    if !remainder
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_whitespace() || ch == '_')
    {
        return None;
    }

    let pattern = consume_rule_separator(remainder);
    if pattern.is_empty() {
        let text = format!("filter rule '{trimmed}' is missing a pattern after '{label}'");
        let message = rsync_error!(1, text).with_role(Role::Client);
        return Some(Err(message));
    }

    Some(Ok(FilterDirective::Rule(builder(pattern.to_owned()))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_builder(pattern: String) -> FilterRuleSpec {
        FilterRuleSpec::exclude(pattern)
    }

    #[test]
    fn returns_none_for_non_matching_first_char() {
        let result = parse_filter_shorthand("x pattern", 'e', "exclude", mock_builder);
        assert!(result.is_none());
    }

    #[test]
    fn returns_none_without_separator() {
        let result = parse_filter_shorthand("epattern", 'e', "exclude", mock_builder);
        assert!(result.is_none());
    }

    #[test]
    fn parses_with_space_separator() {
        let result = parse_filter_shorthand("e pattern", 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn parses_with_underscore_separator() {
        let result = parse_filter_shorthand("e_pattern", 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn returns_error_for_missing_pattern() {
        let result = parse_filter_shorthand("e ", 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn returns_error_for_empty_remainder() {
        let result = parse_filter_shorthand("e", 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn case_sensitive_no_match_on_wrong_case() {
        // upstream: exclude.c:1137-1178 - single-char rule prefixes are
        // case-sensitive, so an uppercase char never matches a lowercase prefix
        // (and vice versa). This parser must return None so the caller can reject
        // the line as an unknown rule.
        let result = parse_filter_shorthand("E pattern", 'e', "exclude", mock_builder);
        assert!(result.is_none());
    }
}
