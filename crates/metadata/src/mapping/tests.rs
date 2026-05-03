#![allow(unsafe_code)]

use super::*;
use parse::{parse_matcher, parse_numeric_range, parse_target};
use types::{MappingMatcher, MappingTarget};
use wildcard::{match_bracket, wildcard_matches};

#[test]
fn mapping_kind_user_flag() {
    assert_eq!(MappingKind::User.flag(), "--usermap");
}

#[test]
fn mapping_kind_group_flag() {
    assert_eq!(MappingKind::Group.flag(), "--groupmap");
}

#[test]
fn mapping_kind_default() {
    let kind: MappingKind = Default::default();
    assert_eq!(kind, MappingKind::User);
}

#[test]
fn mapping_kind_clone() {
    let kind = MappingKind::Group;
    let cloned = kind;
    assert_eq!(cloned, MappingKind::Group);
}

#[test]
fn mapping_kind_debug() {
    let kind = MappingKind::User;
    let debug = format!("{kind:?}");
    assert!(debug.contains("User"));
}

#[test]
fn mapping_parse_error_kind() {
    let error = MappingParseError::new(MappingKind::Group, "test error");
    assert_eq!(error.kind(), MappingKind::Group);
}

#[test]
fn mapping_parse_error_display() {
    let error = MappingParseError::new(MappingKind::User, "custom error message");
    assert_eq!(error.to_string(), "custom error message");
}

#[test]
fn mapping_parse_error_debug() {
    let error = MappingParseError::new(MappingKind::User, "test");
    let debug = format!("{error:?}");
    assert!(debug.contains("MappingParseError"));
}

#[test]
fn mapping_parse_error_clone() {
    let error = MappingParseError::new(MappingKind::User, "test");
    let cloned = error.clone();
    assert_eq!(cloned, error);
}

#[test]
fn parse_numeric_usermap() {
    let mapping = NameMapping::parse(MappingKind::User, "100:200").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
    assert!(!mapping.is_empty());
}

#[test]
fn parse_rejects_invalid_number() {
    let error =
        NameMapping::parse(MappingKind::User, "abc:999999999999999999999999999999").unwrap_err();
    assert!(error.to_string().contains("Invalid number"));
}

#[test]
fn parse_empty_spec_fails() {
    let error = NameMapping::parse(MappingKind::User, "").unwrap_err();
    assert!(error.to_string().contains("requires a non-empty"));
}

#[test]
fn parse_whitespace_only_fails() {
    let error = NameMapping::parse(MappingKind::User, "   ").unwrap_err();
    assert!(error.to_string().contains("requires a non-empty"));
}

#[test]
fn parse_empty_entry_fails() {
    let error = NameMapping::parse(MappingKind::User, "100:200,,300:400").unwrap_err();
    assert!(error.to_string().contains("must not be empty"));
}

#[test]
fn parse_no_colon_fails() {
    let error = NameMapping::parse(MappingKind::User, "100-200").unwrap_err();
    assert!(error.to_string().contains("No colon found"));
}

#[test]
fn parse_empty_target_fails() {
    let error = NameMapping::parse(MappingKind::User, "100:").unwrap_err();
    assert!(error.to_string().contains("No name found after colon"));
}

#[test]
fn parse_wildcard_source() {
    let mapping = NameMapping::parse(MappingKind::User, "*:0").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
}

#[test]
fn parse_range_source() {
    let mapping = NameMapping::parse(MappingKind::User, "100-200:1000").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
}

#[test]
fn parse_pattern_source() {
    let mapping = NameMapping::parse(MappingKind::User, "test*:nobody").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
}

#[test]
fn parse_exact_name_source() {
    let mapping = NameMapping::parse(MappingKind::User, "testuser:0").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
}

#[test]
fn parse_multiple_rules() {
    let mapping =
        NameMapping::parse(MappingKind::User, "100:200, 300:400, *:0").expect("parse mapping");
    assert_eq!(mapping.len(), 3);
}

#[test]
fn parse_empty_source_fails() {
    let error = NameMapping::parse(MappingKind::User, ":100").unwrap_err();
    assert!(error.to_string().contains("must specify a source"));
}

#[test]
fn parse_target_as_name() {
    let mapping = NameMapping::parse(MappingKind::User, "100:nobody").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
}

#[test]
fn numeric_range_single_value() {
    assert_eq!(parse_numeric_range("100"), Some((100, 100)));
}

#[test]
fn numeric_range_two_values() {
    assert_eq!(parse_numeric_range("100-200"), Some((100, 200)));
}

#[test]
fn numeric_range_reversed_values() {
    assert_eq!(parse_numeric_range("200-100"), Some((100, 200)));
}

#[test]
fn numeric_range_empty_fails() {
    assert_eq!(parse_numeric_range(""), None);
}

#[test]
fn numeric_range_non_numeric_fails() {
    assert_eq!(parse_numeric_range("abc"), None);
}

#[test]
fn numeric_range_empty_start_fails() {
    assert_eq!(parse_numeric_range("-100"), None);
}

#[test]
fn numeric_range_empty_end_fails() {
    assert_eq!(parse_numeric_range("100-"), None);
}

#[test]
fn numeric_range_non_numeric_end_fails() {
    assert_eq!(parse_numeric_range("100-abc"), None);
}

#[test]
fn numeric_range_triple_range_fails() {
    assert_eq!(parse_numeric_range("100-200-300"), None);
}

#[test]
fn wildcard_matches_pattern() {
    assert!(wildcard_matches("ab*", "abc"));
    assert!(wildcard_matches("a?c", "abc"));
    assert!(!wildcard_matches("a?d", "abc"));
}

#[test]
fn wildcard_matches_exact() {
    assert!(wildcard_matches("abc", "abc"));
    assert!(!wildcard_matches("abc", "abd"));
}

#[test]
fn wildcard_matches_star_anywhere() {
    assert!(wildcard_matches("*abc", "xyzabc"));
    assert!(wildcard_matches("abc*", "abcxyz"));
    assert!(wildcard_matches("*abc*", "xyzabcdef"));
}

#[test]
fn wildcard_matches_multiple_stars() {
    assert!(wildcard_matches("a*b*c", "aXYZbXYZc"));
    assert!(wildcard_matches("*a*b*", "xaxbx"));
}

#[test]
fn wildcard_matches_question_mark() {
    assert!(wildcard_matches("a?c", "abc"));
    assert!(wildcard_matches("???", "abc"));
    assert!(!wildcard_matches("???", "ab"));
    assert!(!wildcard_matches("???", "abcd"));
}

#[test]
fn wildcard_matches_bracket_simple() {
    assert!(wildcard_matches("a[bc]d", "abd"));
    assert!(wildcard_matches("a[bc]d", "acd"));
    assert!(!wildcard_matches("a[bc]d", "aed"));
}

#[test]
fn wildcard_matches_bracket_range() {
    assert!(wildcard_matches("a[a-z]c", "abc"));
    assert!(wildcard_matches("a[0-9]c", "a5c"));
    assert!(!wildcard_matches("a[a-z]c", "a5c"));
}

#[test]
fn wildcard_matches_bracket_negation() {
    assert!(wildcard_matches("a[!b]c", "adc"));
    assert!(!wildcard_matches("a[!b]c", "abc"));
    assert!(wildcard_matches("a[^b]c", "adc"));
    assert!(!wildcard_matches("a[^b]c", "abc"));
}

#[test]
fn wildcard_matches_empty_pattern() {
    assert!(wildcard_matches("", ""));
    assert!(!wildcard_matches("", "abc"));
}

#[test]
fn wildcard_matches_only_star() {
    assert!(wildcard_matches("*", ""));
    assert!(wildcard_matches("*", "anything"));
}

#[test]
fn wildcard_matches_trailing_stars() {
    assert!(wildcard_matches("abc***", "abc"));
}

#[test]
fn wildcard_no_match_shorter_text() {
    assert!(!wildcard_matches("abcd", "abc"));
}

#[test]
fn match_bracket_simple() {
    assert_eq!(match_bracket(b"[abc]", 0, b'a'), Some((true, 5)));
    assert_eq!(match_bracket(b"[abc]", 0, b'b'), Some((true, 5)));
    assert_eq!(match_bracket(b"[abc]", 0, b'd'), Some((false, 5)));
}

#[test]
fn match_bracket_negated() {
    assert_eq!(match_bracket(b"[!abc]", 0, b'd'), Some((true, 6)));
    assert_eq!(match_bracket(b"[!abc]", 0, b'a'), Some((false, 6)));
    assert_eq!(match_bracket(b"[^abc]", 0, b'd'), Some((true, 6)));
}

#[test]
fn match_bracket_range() {
    assert_eq!(match_bracket(b"[a-z]", 0, b'm'), Some((true, 5)));
    assert_eq!(match_bracket(b"[a-z]", 0, b'0'), Some((false, 5)));
}

#[test]
fn match_bracket_unclosed() {
    assert_eq!(match_bracket(b"[abc", 0, b'a'), None);
}

#[test]
fn match_bracket_empty() {
    assert_eq!(match_bracket(b"[", 0, b'a'), None);
}

#[test]
fn match_bracket_literal_close() {
    assert_eq!(match_bracket(b"[]abc]", 0, b']'), Some((true, 6)));
}

#[test]
fn match_bracket_escaped() {
    assert_eq!(match_bracket(b"[\\]a]", 0, b']'), Some((true, 5)));
}

#[test]
fn match_bracket_escaped_in_range() {
    assert_eq!(match_bracket(b"[a-\\z]", 0, b'z'), Some((true, 6)));
}

#[test]
fn user_mapping_parse() {
    let mapping = UserMapping::parse("100:200").expect("parse");
    assert!(!mapping.is_empty());
}

#[test]
fn user_mapping_parse_error() {
    let error = UserMapping::parse("").unwrap_err();
    assert_eq!(error.kind(), MappingKind::User);
}

#[test]
fn user_mapping_default() {
    let mapping = UserMapping::default();
    assert!(mapping.is_empty());
}

#[test]
fn user_mapping_from_name_mapping() {
    let name_mapping = NameMapping::parse(MappingKind::User, "100:200").unwrap();
    let user_mapping: UserMapping = name_mapping.into();
    assert!(!user_mapping.is_empty());
}

#[test]
fn group_mapping_parse() {
    let mapping = GroupMapping::parse("100:200").expect("parse");
    assert!(!mapping.is_empty());
}

#[test]
fn group_mapping_parse_error() {
    let error = GroupMapping::parse("").unwrap_err();
    assert_eq!(error.kind(), MappingKind::Group);
}

#[test]
fn group_mapping_default() {
    let mapping = GroupMapping::default();
    assert!(mapping.is_empty());
}

#[test]
fn group_mapping_from_name_mapping() {
    let name_mapping = NameMapping::parse(MappingKind::Group, "100:200").unwrap();
    let group_mapping: GroupMapping = name_mapping.into();
    assert!(!group_mapping.is_empty());
}

#[test]
fn name_mapping_clone() {
    let mapping = NameMapping::parse(MappingKind::User, "100:200").unwrap();
    let cloned = mapping.clone();
    assert_eq!(cloned.len(), mapping.len());
}

#[test]
fn name_mapping_debug() {
    let mapping = NameMapping::parse(MappingKind::User, "100:200").unwrap();
    let debug = format!("{mapping:?}");
    assert!(debug.contains("NameMapping"));
}

#[test]
fn name_mapping_default() {
    let mapping = NameMapping::default();
    assert!(mapping.is_empty());
    assert_eq!(mapping.len(), 0);
}

#[test]
fn mapping_target_id() {
    let target = MappingTarget::Id(100);
    let uid = target.resolve_uid().unwrap();
    assert_eq!(uid, 100);
}

#[test]
fn mapping_target_id_as_gid() {
    let target = MappingTarget::Id(100);
    let gid = target.resolve_gid().unwrap();
    assert_eq!(gid, 100);
}

#[test]
fn mapping_matcher_any() {
    let matcher = MappingMatcher::Any;
    let result = matcher
        .matches(12345, || Ok(Some("test".to_owned())))
        .unwrap();
    assert!(result);
}

#[test]
fn mapping_matcher_id_range_in_range() {
    let matcher = MappingMatcher::IdRange {
        start: 100,
        end: 200,
    };
    assert!(matcher.matches(150, || Ok(None)).unwrap());
    assert!(matcher.matches(100, || Ok(None)).unwrap());
    assert!(matcher.matches(200, || Ok(None)).unwrap());
}

#[test]
fn mapping_matcher_id_range_out_of_range() {
    let matcher = MappingMatcher::IdRange {
        start: 100,
        end: 200,
    };
    assert!(!matcher.matches(50, || Ok(None)).unwrap());
    assert!(!matcher.matches(250, || Ok(None)).unwrap());
}

#[test]
fn mapping_matcher_exact_name_match() {
    let matcher = MappingMatcher::ExactName("testuser".to_owned());
    let result = matcher
        .matches(1000, || Ok(Some("testuser".to_owned())))
        .unwrap();
    assert!(result);
}

#[test]
fn mapping_matcher_exact_name_no_match() {
    let matcher = MappingMatcher::ExactName("testuser".to_owned());
    let result = matcher
        .matches(1000, || Ok(Some("otheruser".to_owned())))
        .unwrap();
    assert!(!result);
}

#[test]
fn mapping_matcher_exact_name_no_name() {
    let matcher = MappingMatcher::ExactName("testuser".to_owned());
    let result = matcher.matches(1000, || Ok(None)).unwrap();
    assert!(!result);
}

#[test]
fn mapping_matcher_pattern_match() {
    let matcher = MappingMatcher::Pattern("test*".to_owned());
    let result = matcher
        .matches(1000, || Ok(Some("testuser".to_owned())))
        .unwrap();
    assert!(result);
}

#[test]
fn mapping_matcher_pattern_no_match() {
    let matcher = MappingMatcher::Pattern("test*".to_owned());
    let result = matcher
        .matches(1000, || Ok(Some("otheruser".to_owned())))
        .unwrap();
    assert!(!result);
}

#[test]
fn mapping_matcher_pattern_no_name() {
    let matcher = MappingMatcher::Pattern("test*".to_owned());
    let result = matcher.matches(1000, || Ok(None)).unwrap();
    assert!(!result);
}

#[test]
fn mapping_matcher_clone() {
    let matcher = MappingMatcher::IdRange {
        start: 100,
        end: 200,
    };
    let cloned = matcher.clone();
    assert_eq!(cloned, matcher);
}

#[test]
fn mapping_matcher_debug() {
    let matcher = MappingMatcher::Any;
    let debug = format!("{matcher:?}");
    assert!(debug.contains("Any"));
}

#[test]
fn parse_matcher_star() {
    let matcher = parse_matcher(MappingKind::User, "*", "*:0").unwrap();
    assert!(matches!(matcher, MappingMatcher::Any));
}

#[test]
fn parse_matcher_range() {
    let matcher = parse_matcher(MappingKind::User, "100-200", "100-200:0").unwrap();
    assert!(matches!(matcher, MappingMatcher::IdRange { .. }));
}

#[test]
fn parse_matcher_single_id() {
    let matcher = parse_matcher(MappingKind::User, "100", "100:0").unwrap();
    assert!(matches!(
        matcher,
        MappingMatcher::IdRange {
            start: 100,
            end: 100
        }
    ));
}

#[test]
fn parse_matcher_pattern_star() {
    let matcher = parse_matcher(MappingKind::User, "test*", "test*:0").unwrap();
    assert!(matches!(matcher, MappingMatcher::Pattern(_)));
}

#[test]
fn parse_matcher_pattern_question() {
    let matcher = parse_matcher(MappingKind::User, "test?", "test?:0").unwrap();
    assert!(matches!(matcher, MappingMatcher::Pattern(_)));
}

#[test]
fn parse_matcher_pattern_bracket() {
    let matcher = parse_matcher(MappingKind::User, "test[abc]", "test[abc]:0").unwrap();
    assert!(matches!(matcher, MappingMatcher::Pattern(_)));
}

#[test]
fn parse_matcher_exact_name() {
    let matcher = parse_matcher(MappingKind::User, "testuser", "testuser:0").unwrap();
    assert!(matches!(matcher, MappingMatcher::ExactName(_)));
}

#[test]
fn parse_matcher_empty_fails() {
    let error = parse_matcher(MappingKind::User, "", ":0").unwrap_err();
    assert!(error.to_string().contains("must specify a source"));
}

#[test]
fn parse_target_numeric() {
    let target = parse_target(MappingKind::User, "100", "x:100").unwrap();
    assert!(matches!(target, MappingTarget::Id(100)));
}

#[test]
fn parse_target_name() {
    let target = parse_target(MappingKind::User, "nobody", "x:nobody").unwrap();
    assert!(matches!(target, MappingTarget::Name(_)));
}

#[test]
fn parse_target_empty_fails() {
    let error = parse_target(MappingKind::User, "", "x:").unwrap_err();
    assert!(error.to_string().contains("No name found after colon"));
}
