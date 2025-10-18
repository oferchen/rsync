#![allow(clippy::module_name_repetitions)]

//! Shared helpers for transport handshakes.
//!
//! This module centralises small pieces of logic used by both the binary and
//! legacy ASCII negotiation flows so they agree on how to interpret remote
//! protocol advertisements. Keeping the helpers in one place avoids subtle
//! drift between handshake wrappers and ensures that tests exercising one code
//! path also validate the other.

use core::convert::TryFrom;
use rsync_protocol::ProtocolVersion;

/// Reports whether a remote protocol advertisement was clamped to a supported value.
///
/// Upstream rsync accepts peers that announce protocol numbers newer than it
/// understands by clamping the negotiated value to its newest implementation.
/// Handshake wrappers rely on this helper to detect that condition so they can
/// surface diagnostics that match the C implementation. Values outside the
/// byte range are treated as future protocols and therefore considered clamped.
#[must_use]
pub(crate) fn remote_advertisement_was_clamped(
    advertised: u32,
    negotiated: ProtocolVersion,
) -> bool {
    let advertised_byte = u8::try_from(advertised).unwrap_or(u8::MAX);
    advertised_byte > negotiated.as_u8()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn negotiated_version_strategy() -> impl Strategy<Value = ProtocolVersion> {
        let versions: Vec<ProtocolVersion> = ProtocolVersion::supported_versions_array()
            .iter()
            .copied()
            .collect();
        prop::sample::select(versions)
    }

    #[test]
    fn detects_future_versions_encoded_in_u32() {
        let negotiated = ProtocolVersion::NEWEST;
        assert!(remote_advertisement_was_clamped(40, negotiated));
        assert!(remote_advertisement_was_clamped(0x0001_0200, negotiated));
    }

    #[test]
    fn ignores_advertisements_within_supported_range() {
        let negotiated = ProtocolVersion::from_supported(30).expect("protocol 30 supported");
        assert!(!remote_advertisement_was_clamped(30, negotiated));
        assert!(!remote_advertisement_was_clamped(
            ProtocolVersion::OLDEST.as_u8().into(),
            negotiated
        ));
    }

    proptest! {
        #[test]
        fn within_byte_range_matches_direct_comparison(
            advertised in 0u32..=u8::MAX as u32,
            negotiated in negotiated_version_strategy(),
        ) {
            let expected = (advertised as u8) > negotiated.as_u8();
            prop_assert_eq!(
                remote_advertisement_was_clamped(advertised, negotiated),
                expected
            );
        }

        #[test]
        fn out_of_range_values_always_report_clamp(
            advertised in (u8::MAX as u32 + 1)..=u32::MAX,
            negotiated in negotiated_version_strategy(),
        ) {
            prop_assert!(remote_advertisement_was_clamped(advertised, negotiated));
        }
    }
}
