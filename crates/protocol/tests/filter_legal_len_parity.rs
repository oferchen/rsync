//! Parity assertions for upstream rsync's `legal_len=1` rule at protocol < 29.
//!
//! Upstream `exclude.c:1530` sets `legal_len=1` when serializing filter rules
//! for a peer that speaks protocol < 29, so only the single-character prefixes
//! `+ `, `- ` (and the bare clear `!`) are allowed on the wire. If the rule
//! list contains anything else - the dir-merge `:` prefix or any modifier
//! flag - upstream's `send_rules()` at `exclude.c:1623-1627` exits
//! `RERR_PROTOCOL` with "filter rules are too modern for remote rsync"
//! before any bytes leave the sender.
//!
//! These tests pin oc-rsync's serializer and parser to the same semantics so
//! the `up:merge-filter` interop scenario stays classified as an upstream
//! limitation. See `tools/ci/known_failures.conf` and the BR-1a/b audit at
//! `docs/audits/br-1-merge-filter-repro.md`.

use protocol::ProtocolVersion;
use protocol::filters::{FilterRuleWireFormat, RuleType, build_rule_prefix, write_filter_list};

const PROTO_28: u8 = 28;

fn proto(v: u8) -> ProtocolVersion {
    ProtocolVersion::from_supported(v).expect("supported protocol version")
}

fn dir_merge_rule(pattern: &str) -> FilterRuleWireFormat {
    FilterRuleWireFormat {
        rule_type: RuleType::DirMerge,
        pattern: pattern.to_owned(),
        ..FilterRuleWireFormat::default()
    }
}

/// Dir-merge `:` rules cannot be encoded at protocol 28.
///
/// Mirrors upstream `exclude.c:1530-1534`: `legal_len=1` rejects any prefix
/// longer than `"+ "`/`"- "`, and the `:` rule type unconditionally exceeds
/// that budget.
#[test]
fn dir_merge_prefix_unsendable_at_protocol_28() {
    let rule = dir_merge_rule(".rsync-filter");

    let prefix = build_rule_prefix(&rule, proto(PROTO_28));

    assert!(
        prefix.is_none(),
        "dir-merge rules must be unsendable at protocol 28 (upstream exclude.c:1530 legal_len=1); got {prefix:?}",
    );
}

/// `write_filter_list` at protocol 28 errors with the upstream "too modern"
/// message when a dir-merge rule is included.
///
/// `serialize_rule` (`crates/protocol/src/filters/wire.rs:441-446`) is private,
/// but it is reachable through `write_filter_list`. The error message is the
/// verbatim string upstream prints in `send_rules:1623-1627` before exiting
/// `RERR_PROTOCOL`.
#[test]
fn serialize_rule_errors_with_too_modern_for_dir_merge_at_protocol_28() {
    let rule = dir_merge_rule(".rsync-filter");
    let mut buf = Vec::new();

    let err = write_filter_list(&mut buf, std::slice::from_ref(&rule), proto(PROTO_28))
        .expect_err("dir-merge serialization must fail at protocol 28");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.get_ref()
            .is_some_and(|e| e.is::<protocol::ProtocolViolation>()),
        "too-modern filter must be tagged RERR_PROTOCOL (exclude.c:1627, exit 2)",
    );
    assert!(
        err.to_string()
            .contains("filter rules are too modern for remote rsync"),
        "error message must match upstream send_rules:1623-1627 wording; got {err}",
    );
    assert!(
        buf.is_empty(),
        "no bytes must be emitted before the protocol error; got {} bytes",
        buf.len(),
    );
}

/// Every modifier flag is independently unsendable at protocol 28.
///
/// Upstream `exclude.c:1530` budgets one prefix byte, so each modifier
/// (anchored, negate, cvs_exclude, no_inherit, word_split, exclude_from_merge,
/// xattr_only, sender_side, receiver_side, perishable) pushes the prefix
/// length past `legal_len=1` and triggers the `RERR_PROTOCOL` exit at
/// `send_rules:1623-1627`. The parameterised loop pins all ten flags.
#[test]
fn each_modifier_unsendable_at_protocol_28() {
    type Setter = fn(&mut FilterRuleWireFormat);
    let cases: &[(&str, Setter)] = &[
        ("anchored", |r| r.anchored = true),
        ("negate", |r| r.negate = true),
        ("cvs_exclude", |r| r.cvs_exclude = true),
        ("no_inherit", |r| r.no_inherit = true),
        ("word_split", |r| r.word_split = true),
        ("exclude_from_merge", |r| r.exclude_from_merge = true),
        ("xattr_only", |r| r.xattr_only = true),
        ("sender_side", |r| r.sender_side = true),
        ("receiver_side", |r| r.receiver_side = true),
        ("perishable", |r| r.perishable = true),
    ];

    for (name, setter) in cases {
        let mut rule = FilterRuleWireFormat::exclude("pattern".to_owned());
        setter(&mut rule);

        let prefix = build_rule_prefix(&rule, proto(PROTO_28));
        assert!(
            prefix.is_none(),
            "modifier '{name}' must be unsendable at protocol 28 (upstream exclude.c:1530 legal_len=1)",
        );

        let mut buf = Vec::new();
        let result = write_filter_list(&mut buf, std::slice::from_ref(&rule), proto(PROTO_28));
        let err = match result {
            Ok(()) => panic!("rule with '{name}' modifier must fail at protocol 28"),
            Err(e) => e,
        };
        assert!(
            err.to_string()
                .contains("filter rules are too modern for remote rsync"),
            "modifier '{name}': error must match upstream send_rules:1623-1627 wording; got {err}",
        );
    }
}

/// The wire parser rejects `:`-prefixed rules at protocol 28.
///
/// Upstream `exclude.c:1119-1133` runs the `XFLG_OLD_PREFIXES` branch of
/// `parse_rule_tok()` at protocol < 29, which only accepts `"+ pattern"`,
/// `"- pattern"`, or the bare `"!"` clear marker. A `:`-prefixed payload
/// must be rejected with `InvalidData`. Construct the wire frame manually
/// (length-prefixed payload + zero terminator) since `write_filter_list`
/// already refuses to emit it.
#[test]
fn wire_parser_rejects_dir_merge_at_protocol_28() {
    use protocol::filters::read_filter_list;

    let payload = b": .rsync-filter";
    let mut buf = Vec::with_capacity(4 + payload.len() + 4);
    buf.extend_from_slice(&(payload.len() as i32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf.extend_from_slice(&0i32.to_le_bytes());

    let err = read_filter_list(&mut &buf[..], proto(PROTO_28))
        .expect_err("parser must reject ':' prefix at protocol 28");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("invalid old-style filter prefix"),
        "parser must reject ':' under XFLG_OLD_PREFIXES (upstream exclude.c:1119-1133); got {err}",
    );
}

/// `is_known_failure_from_conf` in `tools/ci/known_failures.conf` returns 0
/// for `up:merge-filter` only when the forced protocol is <= 28.
///
/// Validates the BR-1e classification: the entry must live inside the
/// `forced_proto <= 28` block, not in the unconditional `KNOWN_FAILURES`
/// array (which would mask the test at modern protocols where upstream's
/// `legal_len` budget no longer applies).
///
/// Runs only on Unix because the shell function lives in a bash conf and
/// Windows CI does not ship bash.
#[cfg(unix)]
#[test]
fn known_failures_conf_marks_merge_filter_only_up_to_proto_28() {
    use std::path::PathBuf;
    use std::process::Command;

    let conf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("tools/ci/known_failures.conf");
    assert!(conf.exists(), "missing conf at {}", conf.display());

    let bash = which_bash().unwrap_or_else(|| "/bin/bash".to_owned());
    let conf_str = conf.to_string_lossy().into_owned();

    let check = |proto: &str| -> bool {
        let script = format!(
            "set -u; source '{conf_str}'; is_known_failure_from_conf up merge-filter '{proto}'",
        );
        let status = Command::new(&bash)
            .args(["-c", &script])
            .status()
            .expect("bash invocation must succeed");
        status.success()
    };

    for proto in ["28", "27"] {
        assert!(
            check(proto),
            "merge-filter must be a known failure at proto {proto}"
        );
    }
    for proto in ["29", "30", "31", "32", ""] {
        assert!(
            !check(proto),
            "merge-filter must NOT be a known failure at proto '{proto}' (upstream-only limitation)",
        );
    }
}

#[cfg(unix)]
fn which_bash() -> Option<String> {
    for candidate in [
        "/bin/bash",
        "/usr/bin/bash",
        "/usr/local/bin/bash",
        "/opt/homebrew/bin/bash",
    ] {
        if std::path::Path::new(candidate).exists() {
            return Some(candidate.to_owned());
        }
    }
    None
}
