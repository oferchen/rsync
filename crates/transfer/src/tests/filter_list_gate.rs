//! Tests for the filter-list wire gate and per-rule send elision.
//!
//! These encode upstream's `send_filter_list()` / `recv_filter_list()` contract
//! from `exclude.c`: the `receiver_wants_list` predicate that decides whether a
//! filter list crosses the wire at all, and the `send_rules()` elision that
//! drops rules the peer must never see. The value of matching upstream here is
//! wire fidelity - a mismatched gate makes one end send (or read) a list the
//! other end does not, corrupting the stream, and a leaked rule wrongly protects
//! or drops files during `--delete`.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType};

use crate::{
    TOO_MODERN_FILTER_RULES_MSG, perishable_rules_too_modern, receiver_wants_filter_list,
    wire_rule_crosses_wire,
};

// -- receiver_wants_list truth table (exclude.c:1647-1648, 1676-1677) --

/// `--prune-empty-dirs` always needs the list, independent of delete flags and
/// protocol, because the sender cannot prune empty dirs without the receiver's
/// rules. upstream: `prune_empty_dirs ||` short-circuits the whole expression.
#[test]
fn prune_empty_dirs_always_wants_list() {
    for proto in [ProtocolVersion::V28, ProtocolVersion::V32] {
        assert!(receiver_wants_filter_list(true, false, false, proto));
        assert!(receiver_wants_filter_list(true, true, true, proto));
    }
}

/// With neither `--delete` nor `--prune-empty-dirs`, no list is wanted: push
/// mode applies excludes locally in the generator.
#[test]
fn no_delete_no_prune_wants_no_list() {
    assert!(!receiver_wants_filter_list(
        false,
        false,
        false,
        ProtocolVersion::V32
    ));
}

/// `--delete` without `--delete-excluded` always wants the list so the receiver
/// can honour the same excludes during its deletion pass.
#[test]
fn delete_without_excluded_wants_list_every_protocol() {
    for proto in [
        ProtocolVersion::V28,
        ProtocolVersion::V29,
        ProtocolVersion::V32,
    ] {
        assert!(receiver_wants_filter_list(false, true, false, proto));
    }
}

/// The regression this restores: `--delete --delete-excluded` on protocol >= 29
/// still wants the list (the sender-side modifier is encodable), but on a legacy
/// protocol < 29 peer it must NOT - the pre-29 wire cannot carry the `s`
/// modifier, so upstream neither sends nor reads the list. Dropping the
/// `(!delete_excluded || protocol_version >= 29)` gate made oc send/read an extra
/// list against a legacy peer, desyncing the stream.
#[test]
fn delete_excluded_gates_on_protocol_29() {
    // protocol >= 29: list still wanted.
    assert!(receiver_wants_filter_list(
        false,
        true,
        true,
        ProtocolVersion::V29
    ));
    assert!(receiver_wants_filter_list(
        false,
        true,
        true,
        ProtocolVersion::V32
    ));
    // protocol < 29: list suppressed.
    assert!(!receiver_wants_filter_list(
        false,
        true,
        true,
        ProtocolVersion::V28
    ));
}

// -- send_rules per-rule elision (exclude.c:1605-1612) --

fn dir_merge(no_prefixes: bool) -> FilterRuleWireFormat {
    FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        no_prefixes,
        ..FilterRuleWireFormat::default()
    }
}

/// A both-sided no-prefix per-directory merge (`:-`) is elided from a PUSH under
/// `--delete-excluded` (upstream `elide = am_sender ? LOCAL_RULE`), because the
/// sender applies it locally. Before the fix, oc converted the implicit
/// sender-side flag only for include/exclude rules, so this merge still crossed
/// the wire on a push.
#[test]
fn no_prefix_dir_merge_elided_on_push_under_delete_excluded() {
    let rule = dir_merge(true);
    assert!(!wire_rule_crosses_wire(
        &rule,
        true,
        true,
        ProtocolVersion::V32
    ));
}

/// On a PULL the same rule is REMOTE_RULE and still crosses the wire, WITHOUT a
/// side modifier - the remote sender needs it. upstream `add_rule` spares
/// per-directory merges from the implicit sender-side flip, so the pull encoding
/// must stay both-sided (this is why the fix elides rather than marking `s`).
#[test]
fn no_prefix_dir_merge_transmitted_on_pull_under_delete_excluded() {
    let rule = dir_merge(true);
    assert!(wire_rule_crosses_wire(
        &rule,
        false,
        true,
        ProtocolVersion::V32
    ));
}

/// A prefixed `:` merge (has prefixes) is never elided - only no-prefix merges
/// can be reduced to bare include/excludes and applied locally.
#[test]
fn prefixed_dir_merge_not_elided_under_delete_excluded() {
    let rule = dir_merge(false);
    assert!(wire_rule_crosses_wire(
        &rule,
        true,
        true,
        ProtocolVersion::V32
    ));
}

/// Without `--delete-excluded`, a no-prefix dir-merge is transmitted normally on
/// both directions.
#[test]
fn no_prefix_dir_merge_transmitted_without_delete_excluded() {
    let rule = dir_merge(true);
    assert!(wire_rule_crosses_wire(
        &rule,
        true,
        false,
        ProtocolVersion::V32
    ));
    assert!(wire_rule_crosses_wire(
        &rule,
        false,
        false,
        ProtocolVersion::V32
    ));
}

/// Side-local rules are always elided toward the peer regardless of
/// `--delete-excluded`: a sender-side rule is dropped on a push, a receiver-side
/// rule on a pull (upstream `elide == LOCAL_RULE -> continue`).
#[test]
fn side_local_rules_are_elided() {
    let sender_side = FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        sender_side: true,
        ..FilterRuleWireFormat::default()
    };
    assert!(!wire_rule_crosses_wire(
        &sender_side,
        true,
        false,
        ProtocolVersion::V32
    ));
    assert!(wire_rule_crosses_wire(
        &sender_side,
        false,
        false,
        ProtocolVersion::V32
    ));

    let receiver_side = FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        receiver_side: true,
        ..FilterRuleWireFormat::default()
    };
    assert!(wire_rule_crosses_wire(
        &receiver_side,
        true,
        false,
        ProtocolVersion::V32
    ));
    assert!(!wire_rule_crosses_wire(
        &receiver_side,
        false,
        false,
        ProtocolVersion::V32
    ));
}

/// An explicitly sender-sided no-prefix merge (`:s-`) is handled by the
/// side-local check on a push, and the no-prefix branch must not also fire (it
/// requires neither side flag), so no spurious `s` doubling occurs.
#[test]
fn explicitly_sided_no_prefix_merge_uses_side_check() {
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        no_prefixes: true,
        sender_side: true,
        ..FilterRuleWireFormat::default()
    };
    // Push: elided via the side-local check.
    assert!(!wire_rule_crosses_wire(
        &rule,
        true,
        true,
        ProtocolVersion::V32
    ));
    // Pull: sender-side rule still crosses to the remote sender.
    assert!(wire_rule_crosses_wire(
        &rule,
        false,
        true,
        ProtocolVersion::V32
    ));
}

// -- CVS-origin send gate (exclude.c:1652-1668 send_filter_list) --

/// A built-in `-C` exclude (e.g. `- *.o`) carries no side flag, so before the
/// CVS gate it crossed the wire on both directions. Upstream only adds the `-C`
/// rules to the transmitted list on a sending client (`am_sender`); a receiving
/// client appends them after `send_rules()` (exclude.c:1663-1668), so they must
/// stay local on a pull. The `-C` flag is forwarded to the peer in argv, which
/// regenerates the excludes itself, so transmitting them would be redundant and
/// non-upstream bytes on the wire.
#[test]
fn cvs_origin_exclude_local_on_pull_transmitted_on_push() {
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::Exclude,
        pattern: "*.o".to_owned(),
        cvs_origin: true,
        ..FilterRuleWireFormat::default()
    };
    // Pull (receiving client): kept local, never transmitted.
    assert!(!wire_rule_crosses_wire(
        &rule,
        false,
        false,
        ProtocolVersion::V32
    ));
    // Push (sending client): the built-in list crosses at any protocol.
    assert!(wire_rule_crosses_wire(
        &rule,
        true,
        false,
        ProtocolVersion::V32
    ));
    assert!(wire_rule_crosses_wire(
        &rule,
        true,
        false,
        ProtocolVersion::V28
    ));
}

/// The `:C` per-directory merge is a protocol >= 29 wire feature: upstream adds
/// it to the transmitted list only when `protocol_version >= 29`
/// (exclude.c:1653), and keeps it local on a legacy peer (exclude.c:1664).
/// Emitting it on a pre-29 peer would abort with "filter rules are too modern"
/// because `get_rule_prefix()` cannot encode a per-directory merge there.
#[test]
fn cvs_origin_dir_merge_gated_on_protocol_29() {
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        pattern: ".cvsignore".to_owned(),
        cvs_exclude: true,
        cvs_origin: true,
        ..FilterRuleWireFormat::default()
    };
    // Push, protocol >= 29: `:C` crosses the wire.
    assert!(wire_rule_crosses_wire(
        &rule,
        true,
        false,
        ProtocolVersion::V29
    ));
    assert!(wire_rule_crosses_wire(
        &rule,
        true,
        false,
        ProtocolVersion::V32
    ));
    // Push, protocol < 29: `:C` is unsendable and kept local instead.
    assert!(!wire_rule_crosses_wire(
        &rule,
        true,
        false,
        ProtocolVersion::V28
    ));
    // Pull: kept local at every protocol (receiving client).
    assert!(!wire_rule_crosses_wire(
        &rule,
        false,
        false,
        ProtocolVersion::V32
    ));
    assert!(!wire_rule_crosses_wire(
        &rule,
        false,
        false,
        ProtocolVersion::V28
    ));
}

/// A manually specified `:C` filter directive (NOT from the `-C` flag) has no
/// CVS origin marker, so the pull-local / pre-29 gate does not fire for it: it
/// follows the ordinary send path, matching upstream where such a rule is a
/// literal filter-list entry that errors on a legacy peer rather than being
/// silently kept local.
#[test]
fn manual_dir_merge_without_cvs_origin_not_gated() {
    let rule = FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        pattern: ".cvsignore".to_owned(),
        cvs_exclude: true,
        cvs_origin: false,
        ..FilterRuleWireFormat::default()
    };
    // No CVS origin: crosses on a pull (ordinary send path).
    assert!(wire_rule_crosses_wire(
        &rule,
        false,
        false,
        ProtocolVersion::V32
    ));
}

// -- perishable "too modern" abort (exclude.c:1573-1577 / 1624-1628) --

fn perishable_exclude() -> FilterRuleWireFormat {
    FilterRuleWireFormat::exclude("*.tmp".to_owned()).with_perishable(true)
}

/// (a) A sending client at a pre-30 protocol with a perishable rule must abort.
/// upstream `get_rule_prefix()` returns NULL for the rule (the pre-30 wire has no
/// `p` modifier) and `send_rules()` turns that into a fatal RERR_PROTOCOL rather
/// than silently dropping it - oc previously accepted the transfer, an
/// observable-fidelity divergence (exit code + stderr), so this asserts the
/// predicate fires at both legacy protocols the sender can negotiate.
#[test]
fn perishable_rule_aborts_on_push_pre_protocol_30() {
    let rules = [perishable_exclude()];
    assert!(perishable_rules_too_modern(
        &rules,
        true,
        ProtocolVersion::V28
    ));
    assert!(perishable_rules_too_modern(
        &rules,
        true,
        ProtocolVersion::V29
    ));
}

/// (b) At protocol >= 30 the `p` modifier is encodable, so the same rule is sent
/// (with `p`, verified by the prefix builder tests) and never aborts.
#[test]
fn perishable_rule_sent_at_protocol_30_and_above() {
    let rules = [perishable_exclude()];
    assert!(!perishable_rules_too_modern(
        &rules,
        true,
        ProtocolVersion::V30
    ));
    assert!(!perishable_rules_too_modern(
        &rules,
        true,
        ProtocolVersion::V32
    ));
}

/// (c) A receiver (pull) at a pre-30 protocol keeps the rule and merely omits the
/// `p` char (upstream's `else if (am_sender)` guard does not fire), so it must
/// NOT abort - only the sender direction is fatal.
#[test]
fn perishable_rule_kept_on_pull_pre_protocol_30() {
    let rules = [perishable_exclude()];
    assert!(!perishable_rules_too_modern(
        &rules,
        false,
        ProtocolVersion::V28
    ));
    assert!(!perishable_rules_too_modern(
        &rules,
        false,
        ProtocolVersion::V29
    ));
}

/// (d) A non-perishable rule is representable on every wire, so a pre-30 push is
/// unaffected - the abort is scoped strictly to the perishable flag.
#[test]
fn non_perishable_rule_unaffected_pre_protocol_30() {
    let rules = [FilterRuleWireFormat::exclude("*.tmp".to_owned())];
    assert!(!perishable_rules_too_modern(
        &rules,
        true,
        ProtocolVersion::V28
    ));
}

/// The abort maps to upstream's exact observable outcome: the stderr diagnostic
/// text is byte-for-byte `filter rules are too modern for remote rsync.`
/// (exclude.c:1625) and it is tagged as a protocol violation, which the core
/// exit-code mapper renders as RERR_PROTOCOL (exit code 2) - the same class as
/// upstream's `exit_cleanup(RERR_PROTOCOL)`.
#[test]
fn too_modern_abort_message_and_exit_class() {
    assert_eq!(
        TOO_MODERN_FILTER_RULES_MSG,
        "filter rules are too modern for remote rsync."
    );
    let err = protocol::protocol_violation(TOO_MODERN_FILTER_RULES_MSG);
    assert_eq!(err.to_string(), TOO_MODERN_FILTER_RULES_MSG);
    assert!(
        err.get_ref()
            .and_then(|inner| inner.downcast_ref::<protocol::ProtocolViolation>())
            .is_some(),
        "abort must be tagged ProtocolViolation so it maps to RERR_PROTOCOL (2)"
    );
}
