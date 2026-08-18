//! Conformance of the filter-rule wire decoder against upstream `parse_rule_tok`.
//!
//! `recv_filter_list()` (exclude.c:1971-1984) feeds peer-supplied bytes through
//! the same `parse_rule_tok()` that parses a command-line rule, with
//! `xflags = 0` at protocol >= 29. Every guard upstream applies to a filter rule
//! therefore applies to a rule arriving over the wire.
//!
//! Each rejection below is paired with the byte-adjacent acceptance it must NOT
//! swallow. Without that companion a row would also pass against a decoder that
//! refused everything, which is the failure mode these tests exist to exclude.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list};
use std::io;

/// Frames one rule the way `recv_filter_list()` reads it: 4-byte LE length,
/// body, 4-byte LE zero terminator (upstream: exclude.c:1979-1984).
fn decode(payload: &[u8], version: u8) -> io::Result<Vec<FilterRuleWireFormat>> {
    let protocol = ProtocolVersion::from_supported(version).expect("supported version");
    let mut buf = Vec::new();
    buf.extend_from_slice(&(payload.len() as i32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf.extend_from_slice(&0i32.to_le_bytes());
    read_filter_list(&mut &buf[..], protocol)
}

fn one(payload: &[u8], version: u8) -> FilterRuleWireFormat {
    let mut rules = decode(payload, version).expect("payload must decode");
    assert_eq!(rules.len(), 1, "expected exactly one rule from {payload:?}");
    rules.pop().expect("one rule")
}

/// Asserts the payload is refused AND that the diagnostic names the offending
/// byte at its offset. Checking the position is what distinguishes "the guard
/// fired on the byte I meant" from "something else failed first".
fn err_at(payload: &[u8], position: usize, byte: char) {
    let err = decode(payload, 32).expect_err("payload must be refused");
    let msg = err.to_string();
    assert!(
        msg.contains(&format!("'{byte}' at position {position}")),
        "expected modifier '{byte}' at position {position}, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Unknown byte. upstream: exclude.c:1371-1379 `default: goto invalid`.
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_modifier_byte_is_refused() {
    err_at(b"-q foo", 1, 'q');
}

#[test]
fn a_known_modifier_in_the_same_slot_is_accepted() {
    let rule = one(b"-p foo", 32);
    assert!(rule.perishable);
    assert_eq!(rule.pattern, "foo");
}

// ---------------------------------------------------------------------------
// Comma form. upstream: exclude.c:1325-1329 `if (s[1] == ',') s++;`
// ---------------------------------------------------------------------------

#[test]
fn a_comma_may_separate_the_prefix_from_its_modifiers() {
    let rule = one(b"-,p foo", 32);
    assert!(rule.perishable);
    assert_eq!(rule.pattern, "foo");
}

#[test]
fn the_comma_is_skipped_rather_than_consumed_as_a_modifier() {
    // Discriminator: if the comma were eaten AS a modifier, the bad byte would
    // be reported at position 1. Reporting position 2 proves it was skipped.
    err_at(b"-,q foo", 2, 'q');
}

#[test]
fn the_comma_form_and_the_plain_form_decode_identically() {
    assert_eq!(one(b":,C f", 32), one(b":C f", 32));
}

// ---------------------------------------------------------------------------
// Underscore separator. upstream: exclude.c:1365 terminator + :1444 consume.
// Documented in rsync.1.md: "You can use an underscore instead of a space".
// ---------------------------------------------------------------------------

#[test]
fn an_underscore_terminates_the_modifier_run_and_is_consumed() {
    // Before the fix oc had no `_` arm at all: the byte hit the catch-all,
    // which did NOT advance the pattern start, so the underscore survived into
    // the pattern body. oc excluded `_foo` where upstream excludes `foo` -
    // a different file set, at exit 0 on both sides.
    let rule = one(b"-p_foo", 32);
    assert!(rule.perishable);
    assert_eq!(
        rule.pattern, "foo",
        "the '_' separator must not survive into the pattern"
    );
}

#[test]
fn an_underscore_and_a_space_are_the_same_separator() {
    assert_eq!(one(b"-p_foo", 32), one(b"-p foo", 32));
}

// ---------------------------------------------------------------------------
// `/` has no position gate. upstream: exclude.c:1392-1394.
// ---------------------------------------------------------------------------

#[test]
fn the_anchor_modifier_is_accepted_after_another_modifier() {
    let rule = one(b":n/ .f", 32);
    assert!(rule.no_inherit && rule.anchored);
    assert_eq!(rule.pattern, ".f");
}

#[test]
fn modifier_order_does_not_change_the_decoded_rule() {
    assert_eq!(one(b":n/ .f", 32), one(b":/n .f", 32));
}

// ---------------------------------------------------------------------------
// Merge-file guards. upstream: exclude.c:1395-1400 / :1410-1419 / :1433-1437.
// ---------------------------------------------------------------------------

#[test]
fn negation_is_refused_on_a_merge_rule() {
    err_at(b":! .f", 1, '!');
}

#[test]
fn negation_is_accepted_on_a_plain_exclude() {
    assert!(one(b"-! core", 32).negate);
}

#[test]
fn merge_only_modifiers_are_refused_on_an_exclude() {
    for (payload, byte) in [
        (b"-e f".as_slice(), 'e'),
        (b"-n f".as_slice(), 'n'),
        (b"-w f".as_slice(), 'w'),
    ] {
        err_at(payload, 1, byte);
    }
}

#[test]
fn merge_only_modifiers_are_accepted_on_a_dir_merge() {
    assert!(one(b":e .f", 32).exclude_from_merge);
    assert!(one(b":n .f", 32).no_inherit);
    assert!(one(b":w .f", 32).word_split);
}

// ---------------------------------------------------------------------------
// `-`/`+` need MERGE_FILE set AND NO_PREFIXES unset.
// upstream: exclude.c:1381-1390 BITS_SETnUNSET.
// ---------------------------------------------------------------------------

#[test]
fn the_no_prefixes_modifier_is_refused_on_a_plain_exclude() {
    err_at(b"-- foo", 1, '-');
}

#[test]
fn the_no_prefixes_modifier_is_refused_twice_on_the_same_rule() {
    err_at(b":-- .f", 2, '-');
}

#[test]
fn the_no_prefixes_modifier_is_accepted_once_on_a_dir_merge() {
    let minus = one(b":- .excl", 32);
    assert!(minus.no_prefixes && !minus.no_prefixes_include);
    let plus = one(b":+ .incl", 32);
    assert!(plus.no_prefixes && plus.no_prefixes_include);
}

// ---------------------------------------------------------------------------
// `C` guards. upstream: exclude.c:1402-1409.
// ---------------------------------------------------------------------------

#[test]
fn the_cvs_modifier_is_refused_after_no_prefixes() {
    err_at(b":-C .f", 2, 'C');
}

#[test]
fn the_cvs_modifier_is_refused_on_a_side_specifying_prefix() {
    err_at(b"PC f", 1, 'C');
}

#[test]
fn the_cvs_modifier_is_accepted_on_a_dir_merge_and_implies_four_flags() {
    let rule = one(b":C f", 32);
    assert_eq!(rule.rule_type, RuleType::DirMerge);
    assert!(rule.cvs_exclude && rule.no_inherit && rule.word_split && rule.no_prefixes);
}

// ---------------------------------------------------------------------------
// Side modifiers vs a side-specifying prefix. upstream: exclude.c:1423-1432.
//
// Only `P`/`R` can reach this guard: oc's RuleType has no Hide/Show, and
// from_prefix_char refuses `H`/`S` outright, so `Hs`/`Ss` are unspellable.
// ---------------------------------------------------------------------------

#[test]
fn side_modifiers_are_refused_after_a_side_specifying_prefix() {
    err_at(b"Pr f", 1, 'r');
    err_at(b"Ps f", 1, 's');
    err_at(b"Rr f", 1, 'r');
}

#[test]
fn side_modifiers_are_accepted_on_a_plain_exclude() {
    let rule = one(b"-sr shared", 32);
    assert!(rule.sender_side && rule.receiver_side);
}

// ---------------------------------------------------------------------------
// Empty pattern. upstream: exclude.c:1474-1475, with the CVS_IGNORE carve-out.
// ---------------------------------------------------------------------------

#[test]
fn an_empty_pattern_is_refused() {
    let err = decode(b"- ", 32).expect_err("an empty pattern must be refused");
    assert!(
        err.to_string().contains("unexpected end of filter rule"),
        "got: {err}"
    );
}

#[test]
fn a_cvs_rule_may_carry_an_empty_pattern() {
    // The `!CVS_IGNORE` half of upstream's condition. Without it the check
    // would break `:C `, whose pattern upstream fills in downstream.
    let rule = one(b":C ", 32);
    assert!(rule.cvs_exclude);
    assert!(rule.pattern.is_empty());
    // ...and the non-CVS sibling of the same shape is still refused, so the
    // carve-out is specific rather than a hole.
    assert!(decode(b":n ", 32).is_err());
}

#[test]
fn a_bare_slash_is_a_legal_one_byte_pattern() {
    // Pins the empty-pattern check ABOVE the trailing-`/` strip. Upstream
    // computes its length on the raw remainder (exclude.c:1462-1465), so `- /`
    // has length 1 and is legal. If the check moved below the strip this would
    // error while serialize_rule still emits `- /` - an encoder/decoder
    // asymmetry the fuzz round-trip oracle asserts against.
    let rule = one(b"- /", 32);
    assert!(rule.directory_only);
    assert!(rule.pattern.is_empty());
}

// ---------------------------------------------------------------------------
// Clear rule. upstream: exclude.c:1365 (loop skipped) + :1467-1471.
// ---------------------------------------------------------------------------

#[test]
fn a_clear_rule_refuses_trailing_text() {
    let err = decode(b"!keep", 32).expect_err("trailing text after `!` must be refused");
    assert!(
        err.to_string().contains("'!' rule has trailing characters"),
        "got: {err}"
    );
}

#[test]
fn a_bare_clear_rule_is_accepted() {
    assert_eq!(one(b"!", 32).rule_type, RuleType::Clear);
}

#[test]
fn a_clear_rule_takes_no_modifiers_at_all() {
    // Discriminator: the message must be "trailing characters", NOT "invalid
    // modifier". Upstream's `ch != '!'` term short-circuits before the first
    // `*++s`, so `x` is never examined as a modifier byte.
    let err = decode(b"!x", 32).expect_err("`!x` must be refused");
    let msg = err.to_string();
    assert!(msg.contains("trailing characters"), "got: {msg}");
    assert!(
        !msg.contains("invalid modifier"),
        "the modifier loop must be skipped entirely, got: {msg}"
    );
}

#[test]
fn the_same_byte_is_a_valid_modifier_on_a_non_clear_rule() {
    assert!(one(b"-x user.*", 32).xattr_only);
}

// ---------------------------------------------------------------------------
// Protocol gates removed from the DECODER.
//
// Upstream has no protocol test in the modifier switch; the `p`-requires-30
// and `s`/`r`-require-29 rules live in the SENDER (get_rule_prefix,
// exclude.c:1865-1877).
// ---------------------------------------------------------------------------

#[test]
fn perishable_is_accepted_at_protocol_29() {
    // Before the fix this decoded to the pattern "p foo" - a live silent
    // misparse, not a pedantic gate.
    let rule = one(b"-p foo", 29);
    assert!(rule.perishable);
    assert_eq!(rule.pattern, "foo");
}

#[test]
fn side_modifiers_are_accepted_at_protocol_29() {
    let rule = one(b"-sr shared", 29);
    assert!(rule.sender_side && rule.receiver_side);
}

#[test]
fn protocol_29_and_32_decode_the_same_bytes_identically() {
    assert_eq!(one(b"-p foo", 29), one(b"-p foo", 32));
    assert_eq!(one(b"-sr shared", 29), one(b"-sr shared", 32));
}
