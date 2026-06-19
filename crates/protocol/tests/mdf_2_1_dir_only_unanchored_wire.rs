//! MDF-2.1 gap-cell coverage: a directory-only unanchored exclude such as
//! `- foo/*/` must emit exactly one wire rule, not a synthetic
//! `- foo/*/**` companion. Closes the row .3 x MDF-2 cell from FIL-AUD-2
//! (`docs/design/fil-aud-exclude-vs-mdf-matrix.md`).
//!
//! UTS-DD-exclude.3 root cause: oc-rsync previously promoted directory-only
//! unanchored rules into descendant `pattern/**` companions on the wire, so
//! a remote sender receiving a `- foo/*/` from the local client also saw a
//! `- foo/*/**` it would otherwise have to interpret. Upstream
//! `exclude.c:rule_matches()` (3.4.1 lines 903-960) returns "no match" for a
//! non-directory candidate when `FILTRULE_DIRECTORY` is set, so the sender
//! never emits a descendant companion. The fix that landed via UTS-DD now
//! limits descendant synthesis to the single-path API; the wire path must
//! stay clean.
//!
//! In-tree references: `crates/filters/src/compiled.rs` (descendant
//! suppression for dir-only unanchored wildcards), and the parallel
//! UTS-DD regression at
//! `crates/filters/tests/uts_dd_exclude_3_dir_only_unanchored.rs` which
//! covers the decision API. This test pins the wire-byte side of the same
//! contract so a refactor of the wire emitter cannot silently re-introduce
//! the descendant companion.
//!
//! FIL-AUD-3 spec section 2.3.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list, write_filter_list};

/// Build the exact wire-format rule list a sender must emit for the
/// canonical sibling-include corpus
/// `- foo/*/`, `+ foo/s?b/`. Two rules, no synthetic descendant.
///
/// upstream: `exclude.c::send_filter_list()` (3.4.1) writes one wire record
/// per parsed rule; `parse_rule_tok` does not duplicate dir-only rules into
/// `pattern/**` siblings.
fn dir_only_corpus() -> [FilterRuleWireFormat; 2] {
    [
        FilterRuleWireFormat {
            rule_type: RuleType::Exclude,
            pattern: "foo/*".to_owned(),
            directory_only: true,
            ..FilterRuleWireFormat::default()
        },
        FilterRuleWireFormat {
            rule_type: RuleType::Include,
            pattern: "foo/s?b".to_owned(),
            directory_only: true,
            ..FilterRuleWireFormat::default()
        },
    ]
}

/// Exactly two wire records appear: `- foo/*/` and `+ foo/s?b/`. No
/// synthetic `- foo/*/**` companion. The frame is length-prefixed and
/// terminated by a 4-byte zero record, mirroring upstream's
/// `send_filter_list()` shape.
///
/// upstream: `exclude.c::send_filter_list()` and `get_rule_prefix()` for the
/// per-rule encoding; `exclude.c:rule_matches() FILTRULE_DIRECTORY` guard
/// for the upstream rationale that no descendant companion is emitted.
#[test]
fn dir_only_unanchored_emits_no_descendant_companion() {
    let protocol = ProtocolVersion::from_supported(32).unwrap();
    let rules = dir_only_corpus();

    let mut buf = Vec::new();
    write_filter_list(&mut buf, &rules, protocol).unwrap();

    // Two length-prefixed records + zero terminator. Records:
    //   - "- foo/*/"   = 3-byte prefix "- " + "foo/*" + "/"  = 8 bytes
    //   - "+ foo/s?b/" = 3-byte prefix "+ " + "foo/s?b" + "/" = 10 bytes
    let expected: Vec<u8> = {
        let mut v = Vec::new();
        v.extend_from_slice(&(8u32).to_le_bytes());
        v.extend_from_slice(b"- foo/*/");
        v.extend_from_slice(&(10u32).to_le_bytes());
        v.extend_from_slice(b"+ foo/s?b/");
        v.extend_from_slice(&[0u8; 4]);
        v
    };
    assert_eq!(
        buf, expected,
        "wire bytes for dir-only sibling-include corpus must contain exactly two rules",
    );

    // Round-trip parity: decode and confirm count and shape; no synthetic
    // `- foo/*/**` ever materialises.
    let parsed = read_filter_list(&mut &buf[..], protocol).unwrap();
    assert_eq!(parsed.len(), 2, "decoder must observe exactly two rules");
    for rule in &parsed {
        assert!(
            rule.directory_only,
            "every rule must carry FILTRULE_DIRECTORY, not be a descendant companion",
        );
        assert!(
            !rule.pattern.contains("**"),
            "no rule pattern should carry a `/**` descendant suffix",
        );
    }
}

/// At protocol 29 (the minimum modern-prefix version) the same two rules
/// must serialise without a descendant companion. Wire payload shape stays
/// constant across the modern prefix range; only the modifier set differs
/// per protocol.
///
/// upstream: `exclude.c::get_rule_prefix()` emits identical prefixes for
/// these two rule shapes at v29 and v32 (no side modifiers active).
#[test]
fn dir_only_unanchored_wire_format_stable_at_v29() {
    let protocol = ProtocolVersion::from_supported(29).unwrap();
    let rules = dir_only_corpus();

    let mut buf = Vec::new();
    write_filter_list(&mut buf, &rules, protocol).unwrap();

    let mut expected = Vec::new();
    expected.extend_from_slice(&(8u32).to_le_bytes());
    expected.extend_from_slice(b"- foo/*/");
    expected.extend_from_slice(&(10u32).to_le_bytes());
    expected.extend_from_slice(b"+ foo/s?b/");
    expected.extend_from_slice(&[0u8; 4]);

    assert_eq!(buf, expected);
}
