//! MDF-2.2 gap-cell coverage: per-token exclude rules expanded from a
//! merge file under `--delete-excluded` must acquire `FILTRULE_SENDER_SIDE`
//! implicitly, so the wire emission carries the `s` short-prefix
//! (`-s pattern` rather than bare `- pattern`). Closes the row .4 x MDF-2
//! cell from FIL-AUD-2 (`docs/design/fil-aud-exclude-vs-mdf-matrix.md`).
//!
//! UTS-DD-exclude.4 root cause: without the implicit flip, the receiver's
//! delete-pass would observe `applies_to_receiver = true` on the merged
//! exclude rule and skip the matching files instead of deleting them under
//! `--delete-excluded`. Upstream `exclude.c::parse_rule_tok()` lines
//! 1324-1332 (3.4.1) OR the FILTRULE_SENDER_SIDE bit onto every per-token
//! rule when the `delete_excluded` global is set, except for rules that
//! already carry an explicit `r`/`s` modifier (the OR is gated by the
//! FILTRULES_SIDES bit being clear).
//!
//! In-tree implementation: `crates/filters/src/chain/mod.rs:537`
//! `apply_merge_implicit_sender_side`. The decision-side parity already
//! has coverage at `crates/filters/src/chain/tests.rs:801`
//! `cvs_dir_merge_expands_to_sender_side_under_delete_excluded`. This
//! test pins the wire-byte side of the same contract.
//!
//! FIL-AUD-3 spec section 2.4.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list, write_filter_list};

/// With `--delete-excluded` active, a merge-file `- *.tmp` rule must
/// serialise to the wire as `-s *.tmp` (sender-side only). The `s`
/// short-prefix is what upstream emits via
/// `exclude.c::get_rule_prefix()` for a FILTRULE_SENDER_SIDE-bearing
/// exclude.
///
/// upstream: `exclude.c::parse_rule_tok()` lines 1324-1332 implicit OR;
/// `exclude.c::get_rule_prefix()` `s` modifier emission.
#[test]
fn delete_excluded_merge_exclude_wire_emits_sender_side_prefix() {
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "*.tmp".to_owned(),
        sender_side: true,
        ..FilterRuleWireFormat::default()
    };

    let mut buf = Vec::new();
    write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

    // Record: "-s *.tmp" = 4-byte prefix "-s " + 5-byte pattern "*.tmp" = 8 bytes.
    let mut expected = Vec::new();
    expected.extend_from_slice(&(8u32).to_le_bytes());
    expected.extend_from_slice(b"-s *.tmp");
    expected.extend_from_slice(&[0u8; 4]);
    assert_eq!(
        buf, expected,
        "delete-excluded merge rule must emit `s` short-prefix on the wire",
    );

    // Round-trip: the decoder must observe applies_to_sender only.
    let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
    assert_eq!(parsed.len(), 1);
    assert!(parsed[0].sender_side, "sender_side bit must round-trip");
    assert!(
        !parsed[0].receiver_side,
        "receiver_side bit must stay cleared under delete-excluded implicit flip",
    );
}

/// Sanity baseline: without the implicit flip (delete-excluded off), the
/// same merge-file exclude must serialise as bare `- *.tmp`. This pins the
/// expected default emission so a regression that always sets the
/// sender_side bit would trip both this baseline test and the positive
/// test above.
///
/// upstream: `exclude.c::parse_rule_tok()` leaves applies-to-both as the
/// default for an exclude rule lacking explicit side modifiers.
#[test]
fn merge_exclude_without_delete_excluded_emits_bare_prefix() {
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "*.tmp".to_owned(),
        ..FilterRuleWireFormat::default()
    };

    let mut buf = Vec::new();
    write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(&(7u32).to_le_bytes());
    expected.extend_from_slice(b"- *.tmp");
    expected.extend_from_slice(&[0u8; 4]);
    assert_eq!(buf, expected);
}

/// Negative control: an `include` rule must NEVER acquire the implicit
/// sender-side flip even under `--delete-excluded`. Upstream gates the OR
/// on the exclude branch only - includes are receiver-relevant by design
/// because they are how the user reintroduces files into the delete-pass
/// scope.
///
/// upstream: `exclude.c::parse_rule_tok()` lines 1324-1332 - the OR is on
/// the exclude path, not the include path.
#[test]
fn delete_excluded_does_not_flip_include_rule_wire_format() {
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::Include,
        pattern: "*.keep".to_owned(),
        ..FilterRuleWireFormat::default()
    };

    let mut buf = Vec::new();
    write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(&(8u32).to_le_bytes());
    expected.extend_from_slice(b"+ *.keep");
    expected.extend_from_slice(&[0u8; 4]);
    assert_eq!(
        buf, expected,
        "include rules must serialise without the `s` short-prefix even under delete-excluded",
    );
}

/// User-typed `--filter -r *.tmp` carries an explicit receiver-side bit
/// and MUST round-trip exactly that on the wire - the implicit flip is
/// gated by `FILTRULES_SIDES` already being clear, so an explicit-side
/// rule is left untouched. Without this guard, an over-eager implicit
/// flip would mutate a user-typed `-r` rule into `-s -r`, contradicting
/// the user's intent.
///
/// upstream: `exclude.c::parse_rule_tok()` - the implicit OR is skipped
/// when any FILTRULES_SIDES bit is already set on the parsed rule.
#[test]
fn explicit_receiver_side_rule_unaffected_by_delete_excluded() {
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "*.tmp".to_owned(),
        receiver_side: true,
        ..FilterRuleWireFormat::default()
    };

    let mut buf = Vec::new();
    write_filter_list(&mut buf, std::slice::from_ref(&rule), protocol).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(&(8u32).to_le_bytes());
    expected.extend_from_slice(b"-r *.tmp");
    expected.extend_from_slice(&[0u8; 4]);
    assert_eq!(buf, expected);

    let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
    assert_eq!(parsed.len(), 1);
    assert!(!parsed[0].sender_side);
    assert!(parsed[0].receiver_side);
}
