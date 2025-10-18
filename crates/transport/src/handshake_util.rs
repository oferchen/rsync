#![allow(clippy::module_name_repetitions)]

//! Shared helpers for transport handshakes.
//!
//! This module centralises small pieces of logic used by both the binary and
//! legacy ASCII negotiation flows so they agree on how to interpret remote
//! protocol advertisements. Keeping the helpers in one place avoids subtle
//! drift between handshake wrappers and ensures that tests exercising one code
//! path also validate the other.

use rsync_protocol::ProtocolVersion;

/// Reports whether a remote protocol advertisement was clamped to the newest supported value.
///
/// Upstream rsync accepts peers that announce protocol numbers newer than it
/// understands by clamping the negotiated value to its newest implementation.
/// Handshake wrappers rely on this helper to detect that condition so they can
/// surface diagnostics that match the C implementation. The detection only
/// fires when the peer announced a protocol strictly newer than
/// [`ProtocolVersion::NEWEST`]; locally capping the negotiation via
/// `--protocol` never triggers this path because the peer still spoke a
/// supported version. Values outside the byte range are treated as future
/// protocols and therefore considered clamped.
#[must_use]
pub(crate) fn remote_advertisement_was_clamped(advertised: u32) -> bool {
    let newest_supported = u32::from(ProtocolVersion::NEWEST.as_u8());
    advertised > newest_supported
}

/// Reports whether the caller capped the negotiated protocol below the value announced by the peer.
///
/// Upstream rsync allows users to limit the negotiated protocol via `--protocol`. When the limit is
/// lower than the peer's selected version the final handshake runs at the caller's cap. Centralising
/// the comparison keeps the binary and legacy negotiation code paths in sync so diagnostics describing
/// locally capped sessions remain identical regardless of transport style.
#[must_use]
pub(crate) fn local_cap_reduced_protocol(
    remote: ProtocolVersion,
    negotiated: ProtocolVersion,
) -> bool {
    negotiated < remote
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn negotiated_version_strategy() -> impl Strategy<Value = ProtocolVersion> {
        let versions: Vec<ProtocolVersion> = ProtocolVersion::supported_versions_array().to_vec();
        prop::sample::select(versions)
    }

    #[test]
    fn detects_future_versions_encoded_in_u32() {
        assert!(remote_advertisement_was_clamped(40));
        assert!(remote_advertisement_was_clamped(0x0001_0200));
    }

    #[test]
    fn ignores_advertisements_within_supported_range() {
        assert!(!remote_advertisement_was_clamped(30));
        assert!(!remote_advertisement_was_clamped(
            ProtocolVersion::OLDEST.as_u8().into()
        ));
    }

    #[test]
    fn local_cap_reductions_do_not_appear_as_remote_clamps() {
        let remote = ProtocolVersion::NEWEST.as_u8();

        assert!(!remote_advertisement_was_clamped(u32::from(remote)));
    }

    #[test]
    fn future_remote_versions_are_detected_even_with_local_caps() {
        assert!(remote_advertisement_was_clamped(40));
    }

    proptest! {
        #[test]
        fn within_byte_range_matches_direct_comparison(
            advertised in 0u32..=u8::MAX as u32,
        ) {
            let newest = u32::from(ProtocolVersion::NEWEST.as_u8());
            let expected = advertised > newest;
            prop_assert_eq!(
                remote_advertisement_was_clamped(advertised),
                expected
            );
        }

        #[test]
        fn out_of_range_values_always_report_clamp(
            advertised in (u8::MAX as u32 + 1)..=u32::MAX,
        ) {
            prop_assert!(remote_advertisement_was_clamped(advertised));
        }

        #[test]
        fn local_cap_detection_matches_direct_comparison(
            remote in negotiated_version_strategy(),
            negotiated in negotiated_version_strategy(),
        ) {
            let expected = negotiated < remote;
            prop_assert_eq!(local_cap_reduced_protocol(remote, negotiated), expected);
        }
    }
}
