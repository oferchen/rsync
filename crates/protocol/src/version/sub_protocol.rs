//! Pre-release subprotocol reconciliation for the SSH/`setup_protocol` path.
//!
//! Upstream rsync tags a build that is not a final, official release with a
//! non-zero `SUBPROTOCOL_VERSION`. When such a build negotiates the newest
//! protocol, the server reconciles the peer's advertised `VER.SUB` against its
//! own so that the newest protocol is only used when both sides speak a
//! compatible pre-release; otherwise the protocol is downgraded by one. This
//! module mirrors that logic byte-for-byte.
//!
//! Upstream reference: `rsync.h:128` (`SUBPROTOCOL_VERSION`), `compat.c:885`
//! (`get_subprotocol_version`), and `compat.c:133` (`check_sub_protocol`).

use super::constants::NEWEST_SUPPORTED_PROTOCOL;

/// Newest protocol version this implementation speaks.
///
/// Mirrors `PROTOCOL_VERSION` from upstream `rsync.h:114`, which equals the
/// newest supported protocol.
pub const PROTOCOL_VERSION: u8 = NEWEST_SUPPORTED_PROTOCOL;

/// Subprotocol tag for this build.
///
/// upstream: rsync.h:128 `SUBPROTOCOL_VERSION`. It MUST be `0` for a final,
/// official release and non-zero only for a pre-release protocol tweak. This
/// crate ships as a release, so it mirrors upstream 3.4.4's value of `0`.
pub const SUBPROTOCOL_VERSION: u8 = 0;

/// Returns the subprotocol version this side advertises for `protocol_version`.
///
/// upstream: compat.c:885 `get_subprotocol_version()`. A release build
/// (`SUBPROTOCOL_VERSION == 0`) always advertises `0`; a pre-release build
/// advertises its subprotocol only while negotiating the newest protocol and
/// falls back to `0` (final version of the older protocol) otherwise.
#[must_use]
pub const fn get_subprotocol_version(protocol_version: u8) -> u8 {
    if SUBPROTOCOL_VERSION == 0 || protocol_version < PROTOCOL_VERSION {
        0
    } else {
        SUBPROTOCOL_VERSION
    }
}

/// Reconciles a pre-release peer's subprotocol against ours, returning the
/// protocol version to advertise.
///
/// upstream: compat.c:133 `check_sub_protocol()`. The server runs this before
/// the numeric version exchange so that the newest protocol is only advertised
/// when both sides speak a compatible (sub)protocol. `their_protocol` and
/// `their_sub` are the `VER.SUB` values the peer declared (via its `client_info`
/// string); a zero in either position means the peer is a final release (or too
/// old to declare a subprotocol), matching upstream's `atoi`/`strchr` parse
/// where a missing version, missing `.`, or zero subprotocol all fold into the
/// "no pre-release info" branch.
///
/// Outcomes (with `our_sub = get_subprotocol_version(protocol_version)`):
/// - both release (`their_sub == 0`): no change - the newest protocol stands.
/// - peer is a pre-release of an older protocol: downgrade to
///   `their_protocol - 1`.
/// - equal protocol, subprotocols differ (one or both pre-release): downgrade
///   by one.
/// - equal protocol, subprotocols match: no change.
/// - peer advertises a newer protocol: its subprotocol is treated as `0` (the
///   final version of that older protocol from our vantage point), so we only
///   downgrade when our own side is a pre-release.
#[must_use]
pub fn check_sub_protocol(
    protocol_version: u8,
    our_sub: u8,
    their_protocol: u8,
    their_sub: u8,
) -> u8 {
    // upstream: compat.c:139-148 - a peer that declares no VER.SUB (zero version,
    // no dot, or zero subprotocol) is a final release; only our own pre-release
    // status can force a downgrade here.
    if their_protocol == 0 || their_sub == 0 {
        if our_sub != 0 {
            return protocol_version - 1;
        }
        return protocol_version;
    }

    // upstream: compat.c:150-154 - a pre-release of an older protocol pins us to
    // the last release of that older protocol.
    if their_protocol < protocol_version {
        if their_sub != 0 {
            return their_protocol - 1;
        }
        return protocol_version;
    }

    // upstream: compat.c:156-159 - a newer protocol's subprotocol reads as 0
    // (its final release) from our side; then any subprotocol mismatch drops us
    // by one so both sides fall back to the last compatible protocol.
    let their_sub = if their_protocol > protocol_version {
        0
    } else {
        their_sub
    };
    if their_sub != our_sub {
        return protocol_version - 1;
    }
    protocol_version
}

#[cfg(test)]
mod tests {
    use super::{
        PROTOCOL_VERSION, SUBPROTOCOL_VERSION, check_sub_protocol, get_subprotocol_version,
    };

    // upstream: rsync.h:128 - a shipping release MUST carry SUBPROTOCOL_VERSION 0.
    #[test]
    fn subprotocol_version_matches_upstream_release() {
        assert_eq!(SUBPROTOCOL_VERSION, 0);
    }

    // upstream: rsync.h:114 - PROTOCOL_VERSION is the newest supported protocol.
    #[test]
    fn protocol_version_is_newest_supported() {
        assert_eq!(PROTOCOL_VERSION, 32);
    }

    // upstream: compat.c:885 - a release build advertises 0 at every version.
    #[test]
    fn get_subprotocol_version_is_zero_for_release() {
        assert_eq!(get_subprotocol_version(PROTOCOL_VERSION), 0);
        assert_eq!(get_subprotocol_version(PROTOCOL_VERSION - 1), 0);
        assert_eq!(get_subprotocol_version(30), 0);
    }

    /// Golden reconciliation table mirroring upstream `check_sub_protocol()`.
    ///
    /// Columns: `(our_proto, our_sub, their_proto, their_sub) -> negotiated`.
    /// `our_sub` is passed explicitly so the pre-release branches upstream
    /// guards behind `#if SUBPROTOCOL_VERSION != 0` are exercised even though
    /// this crate ships with `SUBPROTOCOL_VERSION == 0`.
    #[test]
    fn check_sub_protocol_matches_upstream_table() {
        // (our_proto, our_sub, their_proto, their_sub, expected)
        let cases: &[(u8, u8, u8, u8, u8)] = &[
            // Both release: peer declares no subprotocol -> newest stands.
            (32, 0, 32, 0, 32),
            // Release peer of an older protocol (their_sub == 0) -> no change here;
            // the later numeric min() clamps the version, not this function.
            (32, 0, 30, 0, 32),
            // Peer is a pre-release of an OLDER protocol -> pin to their_proto - 1.
            (32, 0, 30, 5, 29),
            (32, 0, 31, 7, 30),
            // Equal protocol, peer is a pre-release, we are a release -> downgrade.
            (32, 0, 32, 7, 31),
            // Equal protocol, both pre-release, subprotocols EQUAL -> no change.
            (32, 4, 32, 4, 32),
            // Equal protocol, both pre-release, subprotocols UNEQUAL -> downgrade.
            (32, 4, 32, 9, 31),
            // Equal protocol, we are a pre-release, peer is a release
            // (their_sub == 0 folds into the no-info branch) -> our_sub forces -1.
            (32, 4, 32, 0, 31),
            // Peer advertises a NEWER protocol: their_sub reads as 0. We are a
            // release (our_sub 0) -> no change.
            (32, 0, 33, 5, 32),
            // Same, but we are a pre-release -> our_sub != 0 forces a downgrade.
            (32, 4, 33, 5, 31),
            // No version info at all (their_proto == 0), release side -> no change.
            (32, 0, 0, 0, 32),
            // No version info, pre-release side -> downgrade by one.
            (32, 4, 0, 0, 31),
        ];

        for &(our_proto, our_sub, their_proto, their_sub, expected) in cases {
            assert_eq!(
                check_sub_protocol(our_proto, our_sub, their_proto, their_sub),
                expected,
                "check_sub_protocol({our_proto}, {our_sub}, {their_proto}, {their_sub})",
            );
        }
    }

    // upstream: compat.c:133 wired to get_subprotocol_version() - as an actual
    // release build, oc never downgrades against a plain release peer.
    #[test]
    fn release_build_never_downgrades_against_release_peer() {
        let our_sub = get_subprotocol_version(PROTOCOL_VERSION);
        assert_eq!(
            check_sub_protocol(PROTOCOL_VERSION, our_sub, PROTOCOL_VERSION, 0),
            PROTOCOL_VERSION,
        );
    }

    // upstream: compat.c:158-159 - as a release, an equal-protocol pre-release
    // peer still forces the one-step downgrade so both sides drop to protocol 31.
    #[test]
    fn release_build_downgrades_against_equal_prerelease_peer() {
        let our_sub = get_subprotocol_version(PROTOCOL_VERSION);
        assert_eq!(
            check_sub_protocol(PROTOCOL_VERSION, our_sub, PROTOCOL_VERSION, 3),
            PROTOCOL_VERSION - 1,
        );
    }
}
