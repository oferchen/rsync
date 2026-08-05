//! Round-trip proof that the filter-rule wire serializer and parser are true
//! inverses.
//!
//! The filter-list boundary has two halves that must agree byte-for-byte:
//!
//! - the serializer `write_filter_list()` -> `serialize_rule()` ->
//!   `build_rule_prefix()`, which turns a [`FilterRuleWireFormat`] into the
//!   length-prefixed `<prefix><pattern>[/]` record upstream's
//!   `send_rules()`/`get_rule_prefix()` emit (exclude.c:1589-1642, 1525-1587);
//! - the parser `read_filter_list()` -> `parse_wire_rule()`, which decodes that
//!   record back into a [`FilterRuleWireFormat`], mirroring upstream's
//!   `recv_filter_list()`/`parse_rule_tok()` (exclude.c:1672-1698, 1092-1338).
//!
//! Existing unit tests in `crates/protocol/src/filters/` exercise the two
//! halves one direction at a time (build a rule, write it, then assert a few
//! decoded fields), so a serializer change that silently stopped being the
//! parser's inverse - e.g. dropping a modifier char, reordering the prefix, or
//! folding the anchor differently - could pass every existing assertion while
//! corrupting an oc<->oc transfer's filter list. These tests close that gap:
//! for a representative rule of every wire-representable kind and modifier they
//! pin the exact on-wire payload (the golden bytes an upstream peer would
//! receive) AND assert that decoding it reproduces the original rule, so the
//! serializer and parser are proven mutual inverses, not merely
//! independently plausible.
//!
//! # Upstream wire format (exclude.c:1633-1640 `send_rules()`)
//!
//! Each rule is `write_int(plen + len + dlen)` followed by the prefix bytes,
//! the pattern bytes, and - when the rule is directory-only - a trailing `/`.
//! `write_int()` is a 4-byte little-endian `int32` (io.c), NOT a varint. The
//! list ends with a bare `write_int(0)`. The prefix is the compact modifier
//! string `get_rule_prefix()` builds: a rule-type character, the active
//! modifier letters in a fixed order, then a single separating space.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, read_filter_list, write_filter_list};

fn proto(v: u8) -> ProtocolVersion {
    ProtocolVersion::from_supported(v).expect("supported protocol version")
}

/// Serializes a single rule with [`write_filter_list`], asserts the framing
/// (4-byte LE length prefix + payload + 4-byte LE zero terminator), returns the
/// raw payload bytes, and decodes the buffer back with [`read_filter_list`].
///
/// Returning both the payload and the decoded rule lets each case pin the exact
/// upstream-format bytes (the golden) and prove the decode is the encode's
/// inverse in one shot.
fn encode_payload_and_decode(
    rule: &FilterRuleWireFormat,
    protocol: ProtocolVersion,
) -> (Vec<u8>, FilterRuleWireFormat) {
    let mut buf = Vec::new();
    write_filter_list(&mut buf, std::slice::from_ref(rule), protocol)
        .expect("rule serializes for this protocol");

    assert!(
        buf.len() >= 8,
        "framing is at least a length prefix plus terminator, got {} bytes",
        buf.len(),
    );
    let len = i32::from_le_bytes(buf[..4].try_into().unwrap());
    assert!(len > 0, "single non-empty rule must have a positive length");
    let payload_end = 4 + len as usize;
    assert_eq!(
        buf.len(),
        payload_end + 4,
        "buffer is length(4) + payload + terminator(4)",
    );
    assert_eq!(
        &buf[payload_end..],
        &0i32.to_le_bytes(),
        "list is terminated by a 4-byte LE zero (upstream write_int(0))",
    );
    let payload = buf[4..payload_end].to_vec();

    let mut decoded = read_filter_list(&mut &buf[..], protocol).expect("wire record decodes");
    assert_eq!(decoded.len(), 1, "exactly one rule was written");
    (payload, decoded.pop().unwrap())
}

/// Asserts the rule serializes to exactly `expected_payload` AND decodes back to
/// itself: the serializer and parser are inverses for this rule.
fn assert_roundtrips_to_self(
    rule: FilterRuleWireFormat,
    protocol: ProtocolVersion,
    expected_payload: &[u8],
) {
    let (payload, decoded) = encode_payload_and_decode(&rule, protocol);
    assert_eq!(
        payload, expected_payload,
        "wire payload must match upstream get_rule_prefix() format for {rule:?}",
    );
    assert_eq!(
        decoded, rule,
        "decode(encode(rule)) must equal the original rule (true inverse)",
    );
}

// ---------------------------------------------------------------------------
// Include / exclude - the `+`/`-` rule types.
// ---------------------------------------------------------------------------

#[test]
fn exclude_bare_pattern_roundtrips() {
    // upstream: exclude.c:1536-1540 - a plain exclude serializes as `- <pat>`.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("*.log".to_owned()),
        proto(32),
        b"- *.log",
    );
}

#[test]
fn include_bare_pattern_roundtrips() {
    // upstream: exclude.c:1536-1537 - FILTRULE_INCLUDE serializes as `+ <pat>`.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::include("*.txt".to_owned()),
        proto(32),
        b"+ *.txt",
    );
}

#[test]
fn exclude_with_spaces_and_glob_metachars_roundtrips() {
    // The single space after the prefix is the modifier terminator; every byte
    // after it - embedded spaces, glob classes, a leading `!`, internal slashes
    // - is pattern payload and must survive verbatim. This guards the parser's
    // "first space ends the prefix, rest is the pattern" contract
    // (exclude.c:1215 loop breaks on ' ').
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("!keep foo/*.[ch]?".to_owned()),
        proto(32),
        b"- !keep foo/*.[ch]?",
    );
}

// ---------------------------------------------------------------------------
// Anchor (`/`) and directory-only (trailing `/`) - carried in the pattern body
// for non-merge rules, per upstream (FILTRULE_ABS_PATH stays unset).
// ---------------------------------------------------------------------------

#[test]
fn anchored_exclude_roundtrips() {
    // upstream: exclude.c:200-208 / 941-944 - a command-line anchored rule keeps
    // its leading `/` in ent->pattern (FILTRULE_ABS_PATH unset), so the anchor
    // rides in the pattern body as `- /tmp`, and the parser folds the leading
    // `/` back into the `anchored` bit to recover the canonical bare pattern.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("tmp".to_owned()).with_anchored(true),
        proto(32),
        b"- /tmp",
    );
}

#[test]
fn directory_only_exclude_roundtrips() {
    // upstream: exclude.c:1632,1639-1640 - FILTRULE_DIRECTORY appends a trailing
    // `/` (dlen=1) after the pattern.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("cache".to_owned()).with_directory_only(true),
        proto(32),
        b"- cache/",
    );
}

#[test]
fn directory_only_wildcard_include_roundtrips() {
    // The `--include='*/' --exclude='*'` directory-descent idiom: the trailing
    // `/` is the directory-only flag, re-appended once by the serializer, and
    // the stored pattern is the bare `*`.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::include("*".to_owned()).with_directory_only(true),
        proto(32),
        b"+ */",
    );
}

#[test]
fn anchored_directory_only_exclude_roundtrips() {
    // Anchor (leading `/` in the body) and directory-only (trailing `/`) compose
    // on one rule: `- /logs/`. The parser folds the leading `/` into `anchored`
    // and strips the trailing `/` into `directory_only`, recovering bare `logs`.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("logs".to_owned())
            .with_anchored(true)
            .with_directory_only(true),
        proto(32),
        b"- /logs/",
    );
}

#[test]
fn negated_exclude_roundtrips() {
    // upstream: exclude.c:1546 / 1241-1246 - FILTRULE_NEGATE emits `!` right
    // after the type char (`-!`), decoded back by the modifier loop.
    let mut rule = FilterRuleWireFormat::exclude("core".to_owned());
    rule.negate = true;
    assert_roundtrips_to_self(rule, proto(32), b"-! core");
}

#[test]
fn xattr_only_exclude_roundtrips() {
    // upstream: exclude.c:1564 / 1284-1286 - FILTRULE_XATTR emits `x`.
    let mut rule = FilterRuleWireFormat::exclude("user.*".to_owned());
    rule.xattr_only = true;
    assert_roundtrips_to_self(rule, proto(32), b"-x user.*");
}

// ---------------------------------------------------------------------------
// Sender-/receiver-side modifiers (`s`/`r`, protocol >= 29) and perishable
// (`p`, protocol >= 30). Sender-side exclude == `hide`, sender-side include ==
// `show` at the wire layer (there is no distinct `H`/`S` wire char - upstream
// parse_rule_tok maps `H`->exclude|SENDER_SIDE and `S`->include|SENDER_SIDE,
// exclude.c:1194-1200, then serializes them via the `s` modifier).
// ---------------------------------------------------------------------------

#[test]
fn sender_side_exclude_is_hide_and_roundtrips() {
    // `hide` == sender-side exclude (exclude.c:1197-1199 `H`).
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("*.tmp".to_owned()).with_sides(true, false),
        proto(29),
        b"-s *.tmp",
    );
}

#[test]
fn sender_side_include_is_show_and_roundtrips() {
    // `show` == sender-side include (exclude.c:1194-1199 `S`).
    assert_roundtrips_to_self(
        FilterRuleWireFormat::include("keep".to_owned()).with_sides(true, false),
        proto(29),
        b"+s keep",
    );
}

#[test]
fn receiver_side_exclude_roundtrips() {
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("*.bak".to_owned()).with_sides(false, true),
        proto(29),
        b"-r *.bak",
    );
}

#[test]
fn both_sides_exclude_roundtrips() {
    // upstream: exclude.c:1566-1572 - `s` precedes `r` in the emitted order.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("shared".to_owned()).with_sides(true, true),
        proto(29),
        b"-sr shared",
    );
}

#[test]
fn perishable_exclude_roundtrips() {
    // upstream: exclude.c:1573-1578 - FILTRULE_PERISHABLE emits `p` at proto >= 30.
    assert_roundtrips_to_self(
        FilterRuleWireFormat::exclude("*.swp".to_owned()).with_perishable(true),
        proto(30),
        b"-p *.swp",
    );
}

#[test]
fn all_scalar_modifiers_on_exclude_roundtrip() {
    // Kitchen-sink exclude exercising every modifier legal on a non-merge rule,
    // in the exact order get_rule_prefix() emits them: `!` `x` `s` `r` `p`
    // (exclude.c:1546-1578). Proves the serializer's emission order and the
    // parser's accept order agree across the whole modifier set at once.
    let mut rule = FilterRuleWireFormat::exclude("blob".to_owned());
    rule.negate = true;
    rule.xattr_only = true;
    rule.sender_side = true;
    rule.receiver_side = true;
    rule.perishable = true;
    assert_roundtrips_to_self(rule, proto(32), b"-!xsrp blob");
}

// ---------------------------------------------------------------------------
// Merge (`.`) and dir-merge (`:`) rules and their merge-only modifiers.
// ---------------------------------------------------------------------------

#[test]
fn merge_rule_roundtrips() {
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::Merge,
            pattern: "merge.rules".to_owned(),
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b". merge.rules",
    );
}

#[test]
fn dir_merge_rule_roundtrips() {
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".rsync-filter".to_owned(),
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b": .rsync-filter",
    );
}

#[test]
fn dir_merge_anchored_emits_slash_modifier_and_roundtrips() {
    // upstream: exclude.c:1238-1239 / 1543-1544 - on a merge/dir-merge rule the
    // anchor is FILTRULE_ABS_PATH, emitted as the `/` PREFIX modifier (`:/`),
    // NOT as a leading slash in the pattern body (contrast the non-merge case).
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".rules".to_owned(),
            anchored: true,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b":/ .rules",
    );
}

#[test]
fn dir_merge_no_inherit_roundtrips() {
    // upstream: exclude.c:1551-1552 / 1261-1264 - `n` == FILTRULE_NO_INHERIT.
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".f".to_owned(),
            no_inherit: true,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b":n .f",
    );
}

#[test]
fn dir_merge_word_split_roundtrips() {
    // upstream: exclude.c:1553-1554 / 1279-1282 - `w` == FILTRULE_WORD_SPLIT.
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".f".to_owned(),
            word_split: true,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b":w .f",
    );
}

#[test]
fn dir_merge_exclude_self_roundtrips() {
    // upstream: exclude.c:1562-1563 / 1256-1259 - `e` == FILTRULE_EXCLUDE_SELF.
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".f".to_owned(),
            exclude_from_merge: true,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b":e .f",
    );
}

#[test]
fn dir_merge_no_prefixes_minus_roundtrips() {
    // upstream: exclude.c:1555-1560 / 1227-1231 - `-` on a merge rule sets
    // FILTRULE_NO_PREFIXES (exclude-only per-dir rules), emitted between `w`
    // and `e`.
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".excl".to_owned(),
            no_prefixes: true,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b":- .excl",
    );
}

#[test]
fn dir_merge_no_prefixes_plus_roundtrips() {
    // upstream: exclude.c:1556-1557 / 1232-1236 - `+` sets NO_PREFIXES|INCLUDE
    // (include-only per-dir rules).
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".incl".to_owned(),
            no_prefixes: true,
            no_prefixes_include: true,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b":+ .incl",
    );
}

#[test]
fn dir_merge_all_merge_modifiers_roundtrip() {
    // Kitchen-sink dir-merge in get_rule_prefix() emission order: `/` `n` `w`
    // `e` `s` `r` `p` (exclude.c:1543-1578). `-`/`+` (no-prefixes) is mutually
    // exclusive with a plain merge body and is covered separately above.
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".rules".to_owned(),
            anchored: true,
            no_inherit: true,
            word_split: true,
            exclude_from_merge: true,
            sender_side: true,
            receiver_side: true,
            perishable: true,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b":/nwesrp .rules",
    );
}

// ---------------------------------------------------------------------------
// Clear (`!`) - resets the accumulated list.
// ---------------------------------------------------------------------------

#[test]
fn clear_rule_roundtrips() {
    // upstream: exclude.c:1208-1210 / 1315-1323 - a bare `!` (with the trailing
    // separator space) is FILTRULE_CLEAR_LIST and carries no pattern.
    assert_roundtrips_to_self(
        FilterRuleWireFormat {
            rule_type: RuleType::Clear,
            ..FilterRuleWireFormat::default()
        },
        proto(32),
        b"! ",
    );
}

// ---------------------------------------------------------------------------
// A whole heterogeneous list round-trips in order through one write/read pair,
// proving the framing (per-rule length prefix + terminator) composes.
// ---------------------------------------------------------------------------

#[test]
fn heterogeneous_rule_list_roundtrips_in_order() {
    let protocol = proto(32);
    let rules = vec![
        FilterRuleWireFormat::exclude("*.log".to_owned()),
        FilterRuleWireFormat::include("*.txt".to_owned()).with_directory_only(true),
        FilterRuleWireFormat::exclude("secret".to_owned()).with_anchored(true),
        FilterRuleWireFormat {
            rule_type: RuleType::DirMerge,
            pattern: ".rsync-filter".to_owned(),
            ..FilterRuleWireFormat::default()
        },
        FilterRuleWireFormat {
            rule_type: RuleType::Clear,
            ..FilterRuleWireFormat::default()
        },
        FilterRuleWireFormat::exclude("*.bak".to_owned()).with_sides(false, true),
    ];

    let mut buf = Vec::new();
    write_filter_list(&mut buf, &rules, protocol).expect("list serializes");
    let decoded = read_filter_list(&mut &buf[..], protocol).expect("list decodes");

    assert_eq!(
        decoded, rules,
        "the decoded list must equal the original list, rule-for-rule in order",
    );
}

// ---------------------------------------------------------------------------
// Documented NON-identity normalizations. These are NOT round-trip bugs: they
// mirror upstream, which has no `P`/`R`/`H`/`S` character on the wire. The
// receiver-/sender-side semantics ride on the `r`/`s` modifier of a plain
// `-`/`+` rule, so the recovered rule is the semantically-equivalent
// exclude/include, not the original enum tag. Pinning the exact recovered form
// documents the intended asymmetry so a future change cannot quietly turn it
// into data loss.
// ---------------------------------------------------------------------------

#[test]
fn protect_normalizes_to_receiver_side_exclude() {
    // upstream: exclude.c:1204-1206 - `P`/protect parses as an exclude carrying
    // FILTRULE_RECEIVER_SIDE; exclude.c:1536-1540,1569-1572 serialize it as
    // `-r`. There is no `P` byte on the wire, so decode yields a receiver-side
    // exclude, and re-encoding THAT is stable (`-r protected`).
    let protocol = proto(32);
    let protect = FilterRuleWireFormat {
        rule_type: RuleType::Protect,
        pattern: "protected".to_owned(),
        receiver_side: true,
        ..FilterRuleWireFormat::default()
    };

    let (payload, decoded) = encode_payload_and_decode(&protect, protocol);
    assert_eq!(
        payload, b"-r protected",
        "protect serializes as `-r`, not `P`"
    );
    assert_eq!(decoded.rule_type, RuleType::Exclude);
    assert!(
        decoded.receiver_side,
        "receiver-side semantics preserved via `r`"
    );
    assert_eq!(decoded.pattern, "protected");

    // The normalized form is a genuine fixpoint: re-encoding it reproduces the
    // same bytes, so no information is lost on subsequent hops.
    let (payload2, decoded2) = encode_payload_and_decode(&decoded, protocol);
    assert_eq!(payload2, payload);
    assert_eq!(decoded2, decoded);
}

#[test]
fn risk_normalizes_to_receiver_side_include() {
    // upstream: exclude.c:1201-1206 - `R`/risk parses as an include carrying
    // FILTRULE_RECEIVER_SIDE, serialized as `+r`.
    let protocol = proto(32);
    let risk = FilterRuleWireFormat {
        rule_type: RuleType::Risk,
        pattern: "scratch".to_owned(),
        receiver_side: true,
        ..FilterRuleWireFormat::default()
    };

    let (payload, decoded) = encode_payload_and_decode(&risk, protocol);
    assert_eq!(payload, b"+r scratch", "risk serializes as `+r`, not `R`");
    assert_eq!(decoded.rule_type, RuleType::Include);
    assert!(decoded.receiver_side);
    assert_eq!(decoded.pattern, "scratch");

    let (payload2, decoded2) = encode_payload_and_decode(&decoded, protocol);
    assert_eq!(payload2, payload);
    assert_eq!(decoded2, decoded);
}

// ---------------------------------------------------------------------------
// Boundary: the wire pattern is UTF-8 only. `FilterRuleWireFormat::pattern` is
// a `String`, and the reader validates each record with `str::from_utf8`
// before decoding, so a non-UTF-8 pattern is not representable and a non-UTF-8
// record is rejected rather than silently truncated. Pinning this documents
// the limitation relative to upstream, which carries filter patterns as raw
// bytes.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// CVS `C` modifier. The serializer emits ONLY `C` and suppresses the flags it
// implies (no-inherit/word-split/no-prefixes), matching upstream
// get_rule_prefix() (exclude.c:1548-1561). The PARSER re-derives those implied
// flags from `C`, mirroring upstream parse_rule_tok case 'C'
// (exclude.c:1248-1255 sets NO_PREFIXES|WORD_SPLIT|NO_INHERIT|CVS_IGNORE). The
// two halves compose into a stable WIRE fixpoint: `:C` decodes to the full
// implied flag set and re-encodes back to `:C`.
// ---------------------------------------------------------------------------

#[test]
fn cvs_dir_merge_c_modifier_re_derives_implied_flags_matching_upstream() {
    let protocol = proto(32);
    // A realistic CVS dir-merge carries the flags `C` implies on the send side.
    let cvs = FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        pattern: ".cvsignore".to_owned(),
        cvs_exclude: true,
        no_inherit: true,
        word_split: true,
        no_prefixes: true,
        ..FilterRuleWireFormat::default()
    };

    // Serializer collapses the implied flags to a single `C`, and the parser
    // re-derives them, so this full-implied rule is a true struct fixpoint.
    assert_roundtrips_to_self(cvs, protocol, b":C .cvsignore");

    // A rule carrying ONLY the `C` bit still serializes to `:C`, and decoding it
    // re-derives the flags `C` implies - exactly what an UPSTREAM peer's `:C`
    // means. So the struct gains the implied flags (it is NOT a struct fixpoint),
    // but the WIRE bytes are a fixpoint: `:C` -> full-implied -> `:C`.
    let bare_cvs = FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        pattern: ".cvsignore".to_owned(),
        cvs_exclude: true,
        ..FilterRuleWireFormat::default()
    };
    let (payload, decoded) = encode_payload_and_decode(&bare_cvs, protocol);
    assert_eq!(payload, b":C .cvsignore");
    assert!(decoded.cvs_exclude, "the `C` bit survives");
    assert!(decoded.no_inherit, "no-inherit is re-derived from `C`");
    assert!(decoded.word_split, "word-split is re-derived from `C`");
    assert!(decoded.no_prefixes, "no-prefixes is re-derived from `C`");

    // Re-encoding the decoded rule reproduces the same `:C` bytes (the
    // serializer suppresses the implied flags again - no double-application).
    let (payload2, decoded2) = encode_payload_and_decode(&decoded, protocol);
    assert_eq!(payload2, b":C .cvsignore");
    assert_eq!(decoded2, decoded, "`:C` is a wire fixpoint");
}

#[test]
fn non_utf8_wire_record_is_rejected_not_silently_decoded() {
    let protocol = proto(32);

    // Hand-build a well-framed record whose payload (`- \xff`) is a valid
    // prefix followed by an invalid UTF-8 byte, then the zero terminator.
    let mut buf = Vec::new();
    let payload: &[u8] = b"- \xff";
    buf.extend_from_slice(&(payload.len() as i32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf.extend_from_slice(&0i32.to_le_bytes());

    let err = read_filter_list(&mut &buf[..], protocol)
        .expect_err("a non-UTF-8 filter record must be rejected, not decoded lossily");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}
