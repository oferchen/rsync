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
fn spec_preserves_wildcard_for_daemon_round_trip() {
    // Regression: --groupmap=*:1234 must reach the daemon receiver with `*`
    // intact so `uidlist.c:parse_name_map()` recognises the wildcard matcher.
    let mapping = NameMapping::parse(MappingKind::Group, "*:1234").expect("parse mapping");
    assert_eq!(mapping.spec(), "*:1234");
}

#[test]
fn spec_preserves_multi_rule_ordering() {
    let spec = "100-200:1234,wheel:9999,*:0";
    let mapping = NameMapping::parse(MappingKind::Group, spec).expect("parse mapping");
    assert_eq!(mapping.spec(), spec);
}

#[test]
fn spec_trims_surrounding_whitespace() {
    let mapping = NameMapping::parse(MappingKind::User, "  *:0  ").expect("parse mapping");
    assert_eq!(mapping.spec(), "*:0");
}

#[test]
fn user_mapping_spec_accessor_round_trip() {
    use super::UserMapping;
    let mapping = UserMapping::parse("*:5678").expect("parse usermap");
    assert_eq!(mapping.spec(), "*:5678");
}

#[test]
fn group_mapping_spec_accessor_round_trip() {
    use super::GroupMapping;
    let mapping = GroupMapping::parse("*:1234").expect("parse groupmap");
    assert_eq!(mapping.spec(), "*:1234");
}

#[test]
fn parse_range_source() {
    let mapping = NameMapping::parse(MappingKind::User, "100-200:1000").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
}

#[test]
fn parse_pattern_source() {
    // `root` is universally present, so the name target resolves and the rule
    // survives parse-time resolution (upstream uidlist.c:547-561).
    let mapping = NameMapping::parse(MappingKind::User, "test*:root").expect("parse mapping");
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
fn parse_empty_source_maps_nameless_id() {
    // upstream uidlist.c:parse_name_map accepts an empty from-part as the
    // empty-name (nameless id) matcher; ":100" maps the nameless user to 100.
    let mapping = NameMapping::parse(MappingKind::User, ":100").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
}

#[test]
fn parse_target_as_name() {
    // A known name target (`root`) resolves at parse time and the rule is kept.
    let mapping = NameMapping::parse(MappingKind::User, "100:root").expect("parse mapping");
    assert_eq!(mapping.len(), 1);
    // upstream stores the resolved id in the idmap list, so the target is now
    // numeric: applying the rule maps id 100 to root's uid (0).
    let mapped = mapping.map_uid(100, false).expect("map uid");
    assert_eq!(mapped, Some(0));
}

/// A name that cannot exist as a system account: `:` is not a legal username
/// character and the string is long enough to never collide with a real entry.
const UNRESOLVABLE_NAME: &str = "oc_rsync_nonexistent_map_target_00000000";

#[test]
fn parse_unknown_user_target_drops_rule_non_fatally() {
    // upstream: uidlist.c:547-561 - an unknown --usermap target name is warned
    // about once and the rule is skipped; parse still succeeds (non-fatal).
    let spec = format!("100:{UNRESOLVABLE_NAME}");
    let mapping = NameMapping::parse(MappingKind::User, &spec).expect("parse must not fail");
    assert!(
        mapping.is_empty(),
        "unknown target rule must be dropped, not retained"
    );
    // The dropped rule no longer maps its source id: apply falls through to the
    // default (unmapped) behaviour instead of aborting the metadata apply.
    assert_eq!(mapping.map_uid(100, false).expect("map uid"), None);
}

#[test]
fn parse_unknown_group_target_drops_rule_non_fatally() {
    let spec = format!("100:{UNRESOLVABLE_NAME}");
    let mapping = NameMapping::parse(MappingKind::Group, &spec).expect("parse must not fail");
    assert!(mapping.is_empty());
    assert_eq!(mapping.map_gid(100, false).expect("map gid"), None);
}

#[test]
fn parse_keeps_valid_rule_alongside_unknown_target() {
    // A valid rule declared before an unknown-target rule still applies; the
    // unknown one is silently dropped (after its one-time warning) and the
    // transfer continues. Mirrors upstream dropping only the offending rule.
    let spec = format!("100:root, 200:{UNRESOLVABLE_NAME}, 300:0");
    let mapping = NameMapping::parse(MappingKind::User, &spec).expect("parse must not fail");
    assert_eq!(mapping.len(), 2, "only the unknown-target rule is dropped");
    assert_eq!(mapping.map_uid(100, false).expect("map uid"), Some(0));
    // The dropped rule's source id is now unmapped.
    assert_eq!(mapping.map_uid(200, false).expect("map uid"), None);
    assert_eq!(mapping.map_uid(300, false).expect("map uid"), Some(0));
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
fn numeric_range_reversed_values_preserved() {
    // upstream: uidlist.c:parse_name_map stores id1/max_id unswapped, so a
    // reversed range keeps start > end and consequently matches no id.
    assert_eq!(parse_numeric_range("200-100"), Some((200, 100)));
}

#[test]
fn reversed_range_matches_nothing() {
    // A reversed numeric range (`from > to`) must match no identifier, mirroring
    // upstream's `id < node->id || id > node->u.max_id` test which rejects every
    // id when from > to. Without this, silently swapping would over-map ids.
    let matcher = parse_matcher(MappingKind::User, "500-400", "500-400:0").unwrap();
    assert_eq!(
        matcher,
        MappingMatcher::IdRange {
            start: 500,
            end: 400
        }
    );
    for id in [399, 400, 450, 500, 501] {
        assert!(!matcher.matches(id, || Ok(None)).unwrap());
    }
}

#[test]
fn malformed_numeric_source_is_fatal() {
    // upstream: uidlist.c:parse_name_map - once a source begins with a digit it
    // must be a valid number/range; junk digits are a fatal syntax error, not a
    // name to be looked up. `12x` and `1*` must both fail to parse.
    for source in ["12x", "1*", "100-abc", "100-", "1-2-3"] {
        let entry = format!("{source}:0");
        let error = parse_matcher(MappingKind::User, source, &entry)
            .expect_err("malformed numeric source must be a hard error");
        assert!(error.to_string().contains("Invalid number"));
    }
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
fn parse_matcher_empty_source_is_empty_exact_name() {
    // upstream: uidlist.c:parse_name_map accepts an empty from-part (e.g. the
    // `:1` in `--groupmap=:1`). It is neither numeric nor a wildcard, so it
    // falls through to the `NFLAGS_NAME_MATCH` branch with `noiu.name = ""`,
    // i.e. an exact-empty-name matcher. Rejecting it would diverge from
    // upstream and break mapping of the nameless id.
    let user = parse_matcher(MappingKind::User, "", ":1").unwrap();
    assert_eq!(user, MappingMatcher::ExactName(String::new()));
    let group = parse_matcher(MappingKind::Group, "", ":1").unwrap();
    assert_eq!(group, MappingMatcher::ExactName(String::new()));
}

#[test]
fn empty_source_matcher_maps_nameless_id() {
    // upstream: uidlist.c:recv_add_id normalizes a missing name to "" before
    // the strcmp, so the empty-name matcher matches the nameless id (root when
    // the sender omits the id-0 name). The lookup returning `None` (no name)
    // must therefore match `ExactName("")`, and any non-empty exact name must
    // not.
    let matcher = MappingMatcher::ExactName(String::new());
    assert!(matcher.matches(0, || Ok(None)).unwrap());
    assert!(matcher.matches(0, || Ok(Some(String::new()))).unwrap());
    assert!(!matcher.matches(0, || Ok(Some("root".to_owned()))).unwrap());

    // A non-empty exact name must not match the nameless id.
    let named = MappingMatcher::ExactName("root".to_owned());
    assert!(!named.matches(0, || Ok(None)).unwrap());
}

#[test]
fn empty_source_groupmap_parses_and_targets_gid() {
    // `--groupmap=:1` means "map the nameless (root) group to gid 1".
    let mapping = NameMapping::parse(MappingKind::Group, ":1").unwrap();
    assert_eq!(mapping.len(), 1);
    let rule = &mapping.rules[0];
    assert_eq!(rule.matcher, MappingMatcher::ExactName(String::new()));
    assert_eq!(rule.target, MappingTarget::Id(1));
}

#[test]
fn numeric_ids_makes_empty_name_matcher_match_every_id() {
    // upstream: uidlist.c under `--numeric-ids` the sender transmits no id
    // names, so recv_add_id matches every id against the empty name. An
    // empty-name matcher (e.g. `--groupmap=:4`, as exercised by the
    // ownership-depth testsuite `--numeric-ids --groupmap=:sec` leg) therefore
    // remaps a named local id like gid 1000 to the target.
    let mapping = NameMapping::parse(MappingKind::Group, ":4").unwrap();

    // With numeric ids the name lookup is bypassed (treated as nameless), so a
    // named id still matches the empty-name matcher and remaps to gid 4. This
    // path performs no system lookup, so the assertion is host-independent.
    let rule = mapping.resolve_rule(1000, true).unwrap();
    assert_eq!(rule.map(|r| &r.target), Some(&MappingTarget::Id(4)));

    // A named matcher never matches under numeric ids, since every id is
    // treated as nameless. upstream: the sender omits names, so a NAME_MATCH
    // node cannot match. Uses gid 1000 with a name-based matcher.
    let named = NameMapping::parse(MappingKind::Group, "staff:4").unwrap();
    assert!(named.resolve_rule(1000, true).unwrap().is_none());
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

// --- Sender-name-keyed resolution (upstream recv_add_id, uidlist.c:255-268) ---

#[test]
fn map_uid_named_matches_sender_name_not_receiver_lookup() {
    // upstream: uidlist.c:257-261 - a NAME/WILD rule is compared against the
    // name the SENDER transmitted, not a name re-derived from the raw id on the
    // receiver. The raw sender uid (1500) need not exist locally; the rule keys
    // on "deploy". Target 0 is numeric so the assertion needs no /etc/passwd.
    let mapping = UserMapping::parse("deploy:0").unwrap();
    assert_eq!(
        mapping.map_uid_named(1500, Some(b"deploy"), false).unwrap(),
        Some(0),
        "a name usermap must map by the sender-transmitted name"
    );
    // A different transmitted name does not match, and there is no receiver-local
    // reverse-lookup fallback inside the matcher.
    assert_eq!(
        mapping
            .map_uid_named(1500, Some(b"someone-else"), false)
            .unwrap(),
        None,
        "the rule must not match a non-matching sender name"
    );
}

#[test]
fn map_uid_named_wildcard_matches_sender_name() {
    // upstream: uidlist.c:256-258 - NFLAGS_WILD_NAME_MATCH uses wildmatch on the
    // transmitted name. `dep*` matches "deploy".
    let mapping = UserMapping::parse("dep*:0").unwrap();
    assert_eq!(
        mapping.map_uid_named(1500, Some(b"deploy"), false).unwrap(),
        Some(0)
    );
    assert_eq!(
        mapping.map_uid_named(1500, Some(b"other"), false).unwrap(),
        None
    );
}

#[test]
fn map_uid_named_numeric_rule_keys_on_raw_id_regardless_of_name() {
    // upstream: uidlist.c:262-267 - a numeric (max_id / exact-id) rule matches on
    // the raw sender id, independent of the transmitted name. This is the case a
    // premature local-name F_OWNER rewrite would break (the id would no longer be
    // 1500 by the time the map ran).
    let mapping = UserMapping::parse("1500:5000").unwrap();
    assert_eq!(
        mapping.map_uid_named(1500, Some(b"deploy"), false).unwrap(),
        Some(5000)
    );
    // Name is irrelevant to a numeric rule.
    assert_eq!(
        mapping.map_uid_named(1500, None, false).unwrap(),
        Some(5000)
    );
    assert_eq!(
        mapping.map_uid_named(1499, Some(b"deploy"), false).unwrap(),
        None
    );
}

#[test]
fn map_uid_named_numeric_ids_drops_name_but_keeps_numeric_rules() {
    // upstream: uidlist.c - under --numeric-ids the sender omits names, so
    // recv_add_id sees name="": NAME/WILD rules cannot match, numeric rules still can.
    let name_rule = UserMapping::parse("deploy:0").unwrap();
    assert_eq!(
        name_rule
            .map_uid_named(1500, Some(b"deploy"), true)
            .unwrap(),
        None,
        "name rules must not match under numeric-ids (name dropped)"
    );
    let numeric_rule = UserMapping::parse("1500:5000").unwrap();
    assert_eq!(
        numeric_rule
            .map_uid_named(1500, Some(b"deploy"), true)
            .unwrap(),
        Some(5000),
        "numeric rules still key on the raw id under numeric-ids"
    );
}

#[test]
fn map_gid_named_matches_sender_name_symmetric() {
    // Group counterpart: `--groupmap` keys on the sender-transmitted group name.
    let mapping = GroupMapping::parse("build:0").unwrap();
    assert_eq!(
        mapping.map_gid_named(2500, Some(b"build"), false).unwrap(),
        Some(0)
    );
    assert_eq!(
        mapping.map_gid_named(2500, Some(b"nope"), false).unwrap(),
        None
    );
    let numeric = GroupMapping::parse("2500:6000").unwrap();
    assert_eq!(
        numeric.map_gid_named(2500, None, false).unwrap(),
        Some(6000)
    );
}

#[test]
fn map_uid_named_empty_name_matcher_matches_nameless_id() {
    // upstream: uidlist.c:252-253 + 259-261 - `if (!name) name = ""`, so an
    // empty-name matcher (`:7`) matches a nameless id and remaps it.
    let mapping = UserMapping::parse(":7").unwrap();
    assert_eq!(mapping.map_uid_named(4242, None, false).unwrap(), Some(7));
    assert_eq!(
        mapping.map_uid_named(4242, Some(b""), false).unwrap(),
        Some(7)
    );
    // A non-empty transmitted name does not match the empty-name matcher.
    assert_eq!(
        mapping.map_uid_named(4242, Some(b"joe"), false).unwrap(),
        None
    );
}

#[test]
fn map_uid_named_first_match_wins() {
    // upstream: uidlist.c:255-269 - recv_add_id breaks on the first matching
    // node; rules are stored in declaration order.
    let mapping = UserMapping::parse("dep*:1,deploy:2").unwrap();
    assert_eq!(
        mapping.map_uid_named(1500, Some(b"deploy"), false).unwrap(),
        Some(1),
        "the earlier wildcard rule wins over a later exact rule"
    );
}

// --- wildmatch parity (upstream lib/wildmatch.c) for the cases --usermap uses ---

#[test]
fn wildmatch_parity_common_cases() {
    // upstream: lib/wildmatch.c dowild - the patterns actually reachable from a
    // `--usermap`/`--groupmap` source. Names contain no '/', so `*` and `**`
    // behave identically (both span the whole remaining name).
    assert!(wildcard_matches("dep*", "deploy"));
    assert!(wildcard_matches("**", "anything"));
    assert!(wildcard_matches("*", "anything"));
    assert!(wildcard_matches("w?w-data", "www-data"));
    assert!(wildcard_matches("[a-z]ww-data", "www-data"));
    assert!(wildcard_matches("[!x]ww-data", "www-data"));
    assert!(!wildcard_matches("dep?", "deploy"));
    assert!(!wildcard_matches("adm*", "deploy"));
    assert!(!wildcard_matches("[a-z]ww", "1ww"));
}

#[test]
fn wildmatch_parity_star_spans_multiple_chars() {
    // upstream: lib/wildmatch.c - `*` backtracks to span any run of characters.
    assert!(wildcard_matches("a*z", "abcxyz"));
    assert!(wildcard_matches("*data", "www-data"));
    assert!(wildcard_matches("www-*", "www-data"));
    assert!(!wildcard_matches("a*z", "abcxy"));
}
