//! MDF-2.3 gap-cell coverage: a pattern that already contains `**` must
//! not acquire a synthetic `**/` prefix companion on the wire. Closes the
//! row .5 x MDF-2 cell from FIL-AUD-2
//! (`docs/design/fil-aud-exclude-vs-mdf-matrix.md`).
//!
//! UTS-DD-exclude.5 root cause: oc-rsync historically prepended `**/` to
//! every unanchored pattern to mimic upstream's
//! `wildmatch_array(..., slash_handling = -1)` cross-segment match
//! (`lib/wildmatch.c:316`). When the source pattern already contained
//! `**`, the prefix compounded into `**/foo/**/bar`, polluting the rule
//! list with companions upstream never emits and breaking byte-for-byte
//! wire parity with `exclude.c::send_filter_list()` (3.4.1).
//!
//! Upstream `exclude.c:rule_matches()` lines 903-960 handle unanchored
//! `**`-bearing patterns without rewriting; the matcher handles
//! cross-segment matching at evaluation time. The wire format mirrors the
//! user-typed rule exactly.
//!
//! In-tree implementation: `crates/filters/src/compiled.rs`
//! `normalise_recursive_wildcards` (PR #5751). Decision-side parity at
//! `crates/filters/tests/uts_dd_exclude_5_no_double_recursive_wildcard.rs`.
//! This test pins the wire-byte side of the same contract.
//!
//! FIL-AUD-3 spec section 2.5.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list, write_filter_list};

/// Build the wire rule list a sender must emit for the canonical
/// double-star corpus from `testsuite/exclude.test` and the UTS-DD
/// regression. Three rules, no synthetic `**/`-prefix companions.
///
/// upstream: `exclude.c::send_filter_list()` (3.4.1) writes one wire
/// record per user-typed rule with no rewriting of the pattern string.
fn double_star_corpus() -> [FilterRuleWireFormat; 3] {
    [
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: "foo/**/bar".to_owned(),
            ..FilterRuleWireFormat::default()
        },
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: "**/baz".to_owned(),
            ..FilterRuleWireFormat::default()
        },
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: "bar".to_owned(),
            ..FilterRuleWireFormat::default()
        },
    ]
}

/// The wire emission carries exactly the three user-typed rules with no
/// `**/`-prefix mirror. Pinning the byte sequence guards against a
/// regression that re-introduces the implicit-prefix rewrite at
/// pre-emission time.
///
/// upstream: `exclude.c::get_rule_prefix()` and `send_filter_list()`
/// emit the pattern byte-for-byte as the user typed it.
#[test]
fn double_star_patterns_emit_no_prefix_companion() {
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let rules = double_star_corpus();

    let mut buf = Vec::new();
    write_filter_list(&mut buf, &rules, protocol).unwrap();

    // Records:
    //   - "- foo/**/bar" = 3-byte prefix "- " + 10-byte pattern = 12 bytes
    //   - "- **/baz"     = 3-byte prefix "- " + 6-byte pattern  = 8 bytes
    //   - "- bar"        = 3-byte prefix "- " + 3-byte pattern  = 5 bytes
    let mut expected = Vec::new();
    expected.extend_from_slice(&(12u32).to_le_bytes());
    expected.extend_from_slice(b"- foo/**/bar");
    expected.extend_from_slice(&(8u32).to_le_bytes());
    expected.extend_from_slice(b"- **/baz");
    expected.extend_from_slice(&(5u32).to_le_bytes());
    expected.extend_from_slice(b"- bar");
    expected.extend_from_slice(&[0u8; 4]);
    assert_eq!(
        buf, expected,
        "wire bytes must contain exactly three rules with no `**/`-prefix companion",
    );

    // Round-trip: the decoder observes the same three rules and no
    // additional `**/`-prefix companion materialises.
    let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
    assert_eq!(parsed.len(), 3);

    for rule in &parsed {
        // A `**/foo/**/bar` companion would be detectable as a rule that
        // both starts with `**/` AND contains an interior `**`. No such
        // rule must appear in the parsed list.
        let starts_with_double_star_slash = rule.pattern.starts_with("**/");
        let has_interior_double_star = rule
            .pattern
            .strip_prefix("**/")
            .map(|rest| rest.contains("**"))
            .unwrap_or(false);
        assert!(
            !(starts_with_double_star_slash && has_interior_double_star),
            "no rule must combine a leading `**/` prefix with an interior `**`: got {:?}",
            rule.pattern,
        );
    }
}

/// Wire-format stability at protocol 29: the same three rules must
/// serialise identically because no side modifiers fire and the prefix
/// builder emits identical bytes across v29 and v32 for plain excludes.
///
/// upstream: `exclude.c::get_rule_prefix()` modifier set is empty here.
#[test]
fn double_star_patterns_wire_format_stable_at_v29() {
    let protocol = ProtocolVersion::from_supported(29).unwrap();
    let rules = double_star_corpus();

    let mut buf = Vec::new();
    write_filter_list(&mut buf, &rules, protocol).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(&(12u32).to_le_bytes());
    expected.extend_from_slice(b"- foo/**/bar");
    expected.extend_from_slice(&(8u32).to_le_bytes());
    expected.extend_from_slice(b"- **/baz");
    expected.extend_from_slice(&(5u32).to_le_bytes());
    expected.extend_from_slice(b"- bar");
    expected.extend_from_slice(&[0u8; 4]);
    assert_eq!(buf, expected);
}
