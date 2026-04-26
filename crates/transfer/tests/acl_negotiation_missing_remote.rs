//! Integration tests for ACL capability negotiation when the remote peer
//! lacks ACL support.
//!
//! Upstream rsync gates ACL transfer on two layers:
//!
//! 1. **Protocol-version gate** (`compat.c:655-661`): if the negotiated
//!    protocol is `< 30`, the sender errors out with
//!    `"--acls requires protocol 30 or higher (negotiated %d)."`.
//! 2. **Local-binary feature gate** (`options.c:1842-1857`): when the
//!    server-side rsync was compiled without `--enable-acl-support`
//!    (`#ifdef SUPPORT_ACLS`), the entire `acls.c` compilation unit
//!    is empty (acls.c:25,1141) and the resulting binary will never
//!    advertise `-A` in the compact server flag string emitted from
//!    `options.c`. There is no dedicated capability bit for ACLs on
//!    the wire - the absence of `-A` in the server flags is the
//!    "remote lacks ACL support" signal.
//!
//! oc-rsync mirrors layer (1) verbatim in
//! `transfer::setup::restrictions::apply_protocol_restrictions` and
//! covers layer (2) in `ParsedServerFlags::clear_unsupported_features`
//! plus the warning emission in `transfer::lib`. These tests exercise
//! the cross-layer behaviour end-to-end so that future refactors do
//! not silently regress to either:
//! - panicking when the peer advertises no ACL support, or
//! - sending ACL bytes on the wire when the peer cannot decode them.
//!
//! Upstream references:
//! - `target/interop/upstream-src/rsync-3.4.1/compat.c:655-661`
//! - `target/interop/upstream-src/rsync-3.4.1/options.c:1842-1857`
//! - `target/interop/upstream-src/rsync-3.4.1/acls.c:25,1141`

#![deny(unsafe_code)]

use protocol::ProtocolVersion;
use transfer::flags::ParsedServerFlags;
use transfer::setup::{ProtocolRestrictionFlags, apply_protocol_restrictions};

/// Compact flag string from a modern (protocol 30+) peer that was built
/// WITHOUT `--enable-acl-support`. Note the absence of `A` and `X`.
///
/// Mirrors the format produced by upstream `options.c:server_options`
/// when neither `SUPPORT_ACLS` nor `SUPPORT_XATTRS` is defined.
const SERVER_FLAGS_NO_ACL_NO_XATTR: &str = "-logDtpre.iLsfxC";

/// Compact flag string from a modern peer that DOES support ACLs and xattrs.
const SERVER_FLAGS_WITH_ACL_AND_XATTR: &str = "-logDtpreAX.iLsfxC";

/// When the remote peer's server flag string lacks `A`, parsing must report
/// `acls = false` regardless of what the local side requested. This is the
/// wire-level signal that the remote was built without `SUPPORT_ACLS`.
///
/// upstream: options.c omits the `A` byte from `argstr` when SUPPORT_ACLS
/// is not defined at compile time.
#[test]
fn server_flag_string_without_a_flag_yields_acls_false() {
    let flags = ParsedServerFlags::parse(SERVER_FLAGS_NO_ACL_NO_XATTR).unwrap();
    assert!(
        !flags.acls,
        "remote without ACL support must not advertise -A on the wire",
    );
    assert!(
        !flags.xattrs,
        "remote without xattr support must not advertise -X on the wire",
    );
    // Sanity check: the rest of the typical archive flags are still parsed.
    assert!(flags.links);
    assert!(flags.owner);
    assert!(flags.group);
    assert!(flags.times);
    assert!(flags.perms);
    assert!(flags.recursive);
}

/// Confirm the positive control: a peer that does support ACLs/xattrs sets
/// the corresponding parsed flags.
#[test]
fn server_flag_string_with_a_and_x_yields_acls_and_xattrs_true() {
    let flags = ParsedServerFlags::parse(SERVER_FLAGS_WITH_ACL_AND_XATTR).unwrap();
    assert!(flags.acls, "remote with ACL support advertises -A");
    assert!(flags.xattrs, "remote with xattr support advertises -X");
}

/// Negotiated protocol 30+ with a remote that lacks ACL support: the
/// protocol-version gate must NOT reject the transfer (it only fires
/// for `protocol < 30`). The acls flag remains true at this stage and
/// only gets cleared by the wire-flag layer when parsing the server's
/// reply.
///
/// This mirrors upstream `compat.c:655-661`, which is conditional on
/// `protocol_version < 30` and entirely silent for newer protocols.
#[test]
fn protocol_30_plus_does_not_reject_acls_at_restriction_layer() {
    for version in [30u8, 31, 32] {
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            ..Default::default()
        };
        let proto = ProtocolVersion::try_from(version).unwrap();
        let result = apply_protocol_restrictions(proto, &flags);
        assert!(
            result.is_ok(),
            "protocol {version} must not reject --acls at the restriction layer; \
             ACL incompatibility with the remote build is signalled via the absent \
             -A flag in the server flag string, not via the protocol-version gate",
        );
    }
}

/// Cross-layer scenario: client requested `--acls`, negotiated protocol is
/// 32 (so the restriction layer is silent), but the remote peer's server
/// flag string lacks `-A` (remote built without `SUPPORT_ACLS`). The
/// expected outcome is the graceful path - no panic, no error, parsed
/// `acls` ends up `false`, and any local "I want ACLs" flag clears
/// itself when reconciled against the parsed peer flags.
///
/// This is the integration scenario for Task #1392: "Test ACL capability
/// negotiation when remote lacks ACL support".
#[test]
fn end_to_end_remote_without_acl_support_at_protocol_32() {
    // Step 1: protocol-version gate - protocol 32 lets ACLs through.
    let proto = ProtocolVersion::try_from(32u8).unwrap();
    let restriction_flags = ProtocolRestrictionFlags {
        preserve_acls: true,
        ..Default::default()
    };
    let adjustments = apply_protocol_restrictions(proto, &restriction_flags)
        .expect("protocol 32 with --acls must pass the restriction layer");
    // The restriction layer should make no append-mode or delete-phase
    // adjustments for this scenario.
    assert_eq!(adjustments.append_mode, None);
    assert_eq!(adjustments.delete_before, None);

    // Step 2: parse the server flag string the remote sent. A modern peer
    // built without SUPPORT_ACLS will NOT include 'A' in the flag string.
    let parsed = ParsedServerFlags::parse(SERVER_FLAGS_NO_ACL_NO_XATTR)
        .expect("server flag string from a healthy peer must parse");
    assert!(
        !parsed.acls,
        "wire-level negotiation: remote without SUPPORT_ACLS must not set -A, \
         so parsed.acls must be false even though local requested --acls",
    );

    // Step 3: the receiver uses parsed.acls (NOT the local request) to decide
    // whether to expect ACL bytes on the wire. With parsed.acls == false the
    // wire path will not attempt to read or write ACL frames - matching the
    // peer's (non-)behaviour.
    let mut effective = parsed.clone();
    let cleared = effective.clear_unsupported_features();
    // Already-false flags must remain false and must not be reported as cleared.
    assert!(!effective.acls);
    assert!(
        !cleared.contains(&"ACLs"),
        "ACL flag was already false from the wire; clear_unsupported_features \
         must not double-report it: {cleared:?}",
    );
}

/// Cross-layer scenario for the failing path: client requested `--acls`,
/// but the negotiated protocol is `< 30`. The restriction layer must
/// reject this with the exact upstream error message format.
///
/// upstream: compat.c:655-661
/// ```c
/// if (preserve_acls && !local_server) {
///     rprintf(FERROR, "--acls requires protocol 30 or higher "
///                     "(negotiated %d).\n", protocol_version);
///     exit_cleanup(RERR_PROTOCOL);
/// }
/// ```
#[test]
fn end_to_end_protocol_below_30_rejects_acls_with_upstream_message() {
    for version in [27u8, 28, 29] {
        let proto = ProtocolVersion::try_from(version).unwrap();
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            ..Default::default()
        };
        let err = apply_protocol_restrictions(proto, &flags)
            .expect_err("protocol < 30 with --acls must be rejected");
        let message = err.to_string();
        let expected = format!("--acls requires protocol 30 or higher (negotiated {version}).");
        assert_eq!(
            message, expected,
            "error message must match upstream compat.c:657-659 format byte-for-byte",
        );
    }
}

/// Local-server transfers (no remote at all) bypass the protocol-version
/// gate for ACLs. This matches upstream `compat.c:655` which guards the
/// error with `!local_server`.
///
/// Even at protocol 27, a `--acls` local copy proceeds because there is
/// no peer to be incompatible with.
#[test]
fn local_server_bypasses_acl_protocol_gate() {
    for version in [27u8, 28, 29, 30, 31, 32] {
        let proto = ProtocolVersion::try_from(version).unwrap();
        let flags = ProtocolRestrictionFlags {
            preserve_acls: true,
            local_server: true,
            ..Default::default()
        };
        assert!(
            apply_protocol_restrictions(proto, &flags).is_ok(),
            "local_server at protocol {version} must bypass the ACL gate",
        );
    }
}

/// When BOTH the protocol-version gate fires AND the remote lacks ACL
/// support on the wire, the version error wins because it is checked
/// first. The user sees the upstream-compatible message rather than a
/// silent skip - matching upstream's error-then-exit ordering in
/// `compat.c`.
#[test]
fn protocol_gate_fires_before_wire_negotiation_at_old_protocol() {
    let proto = ProtocolVersion::try_from(29u8).unwrap();
    let restriction_flags = ProtocolRestrictionFlags {
        preserve_acls: true,
        ..Default::default()
    };
    let err = apply_protocol_restrictions(proto, &restriction_flags)
        .expect_err("protocol 29 must reject --acls regardless of peer flag string");
    assert!(
        err.to_string().contains("--acls requires protocol 30"),
        "expected upstream-format error, got: {err}",
    );
    // Belt-and-suspenders: even if a caller ignored the error and went on
    // to parse the (hypothetical) peer flag string, they would still see
    // acls = false on the wire and skip ACL transfer.
    let parsed = ParsedServerFlags::parse(SERVER_FLAGS_NO_ACL_NO_XATTR).unwrap();
    assert!(!parsed.acls);
}

/// Idempotency / no-op safety: parsing a flag string with no `-A` and
/// then calling `clear_unsupported_features` must never mutate the flag
/// or report ACLs as cleared, regardless of the local build features.
/// This guards against the bug class where a peer-driven false gets
/// double-cleared and reported as a spurious warning to the user.
#[test]
fn clear_unsupported_features_is_silent_when_remote_already_omits_acls() {
    let mut flags = ParsedServerFlags::parse(SERVER_FLAGS_NO_ACL_NO_XATTR).unwrap();
    assert!(!flags.acls);
    let cleared = flags.clear_unsupported_features();
    assert!(
        !cleared.contains(&"ACLs"),
        "must not warn about ACLs being cleared when the remote never asked for them: {cleared:?}",
    );
    assert!(!flags.acls, "ACL flag must remain false after clear");
}
