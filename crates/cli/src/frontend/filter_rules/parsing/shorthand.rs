use core::client::FilterRuleSpec;
use core::message::{Message, Role};
use core::rsync_error;

use super::super::directive::FilterDirective;
use super::helpers::trim_short_rule_remainder;

pub(super) fn parse_filter_shorthand(
    trimmed: &str,
    short: char,
    label: &str,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if !first.eq_ignore_ascii_case(&short) {
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

    let pattern = trim_short_rule_remainder(remainder);
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
    fn case_insensitive_matching() {
        let result = parse_filter_shorthand("E pattern", 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }
}
