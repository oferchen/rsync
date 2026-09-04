use core::client::FilterRuleSpec;
use core::message::{Message, Role};
use core::rsync_error;

use super::super::directive::FilterDirective;
use super::helpers::split_short_rule_modifiers;
use super::modifiers::{apply_rule_modifiers, parse_rule_modifiers};
use super::rule_line::RuleLine;

/// Parses a single-character rule prefix (`short`) followed by a separator and
/// a pattern, building the rule via `builder`. Returns `None` when `short` does
/// not match case-sensitively or no separator follows; an error when the
/// pattern is missing.
pub(super) fn parse_filter_shorthand(
    line: RuleLine<'_>,
    short: char,
    label: &str,
    builder: fn(String) -> FilterRuleSpec,
) -> Option<Result<FilterDirective, Message>> {
    let trimmed = line.text();
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
        return Some(Err(missing_pattern(line, label)));
    }

    // upstream: exclude.c:1330-1445 - the modifier loop runs for EVERY rule
    // prefix, `H`/`S`/`P`/`R` included, so these accept a modifier run before
    // the separator exactly as `+`/`-` do. Character validity is left to
    // `parse_rule_modifiers`, matching `split_short_rule_modifiers`' contract,
    // so an unknown letter reports "invalid modifier" rather than falling
    // through to the unrelated "unsupported filter rule" diagnostic.
    let (modifier_text, pattern) = match split_short_rule_modifiers(remainder) {
        Ok(split) => split,
        Err(invalid) => return Some(Err(invalid.into_message(short.len_utf8(), line))),
    };

    // `prefix_specifies_side = true`: H/S/P/R already bind a side, so upstream
    // rejects a redundant `s`/`r` modifier on them (exclude.c:1423-1432) while
    // leaving `p` and `x` unguarded (exclude.c:1421, :1438).
    let modifiers = match parse_rule_modifiers(modifier_text, line, true, true, true) {
        Ok(state) => state,
        Err(error) => return Some(Err(error)),
    };

    if pattern.is_empty() {
        return Some(Err(missing_pattern(line, label)));
    }

    let rule = builder(pattern.to_owned());
    match apply_rule_modifiers(rule, modifiers) {
        Ok(rule) => Some(Ok(FilterDirective::Rule(rule))),
        Err(error) => Some(Err(error)),
    }
}

/// The rule text crosses `rule_text` (exclude.c:88-123); the rule PREFIX is
/// ours, not the peer's, so it is printed unconditionally.
fn missing_pattern(line: RuleLine<'_>, label: &str) -> Message {
    rsync_error!(
        1,
        format!(
            "filter rule '{}' is missing a pattern after '{label}'",
            line.shown()
        )
    )
    .with_role(Role::Client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filters::RuleSource;

    fn arg(text: &str) -> RuleLine<'_> {
        RuleLine::new(text, RuleSource::Argument)
    }

    fn mock_builder(pattern: String) -> FilterRuleSpec {
        FilterRuleSpec::exclude(pattern)
    }

    #[test]
    fn returns_none_for_non_matching_first_char() {
        let result = parse_filter_shorthand(arg("x pattern"), 'e', "exclude", mock_builder);
        assert!(result.is_none());
    }

    #[test]
    fn no_separator_reports_an_invalid_modifier() {
        // MEASURED against rsync 3.5.0: `Ppattern` -> "invalid modifier 'a' at
        // position 2". With no separator the whole remainder is scanned as
        // modifiers (helpers.rs scan_modifiers, upstream exclude.c:1214-1287),
        // so the first non-modifier byte is reported - it is NOT an unknown
        // rule, and it must not fall through to the keyword parser.
        let result = parse_filter_shorthand(arg("epattern"), 'e', "exclude", mock_builder);
        assert!(
            result
                .expect("prefix matched, so the parser must not decline")
                .is_err()
        );
    }

    #[test]
    fn parses_with_space_separator() {
        let result = parse_filter_shorthand(arg("e pattern"), 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn parses_with_underscore_separator() {
        let result = parse_filter_shorthand(arg("e_pattern"), 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn returns_error_for_missing_pattern() {
        let result = parse_filter_shorthand(arg("e "), 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn returns_error_for_empty_remainder() {
        let result = parse_filter_shorthand(arg("e"), 'e', "exclude", mock_builder);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn case_sensitive_no_match_on_wrong_case() {
        // upstream: exclude.c:1137-1178 - single-char rule prefixes are
        // case-sensitive, so an uppercase char never matches a lowercase prefix
        // (and vice versa). This parser must return None so the caller can reject
        // the line as an unknown rule.
        let result = parse_filter_shorthand(arg("E pattern"), 'e', "exclude", mock_builder);
        assert!(result.is_none());
    }
}
