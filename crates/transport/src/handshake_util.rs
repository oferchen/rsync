#![allow(clippy::module_name_repetitions)]

//! Shared helpers for transport handshakes.
//!
//! This module centralises small pieces of logic used by both the binary and
//! legacy ASCII negotiation flows so they agree on how to interpret remote
//! protocol advertisements. Keeping the helpers in one place avoids subtle
//! drift between handshake wrappers and ensures that tests exercising one code
//! path also validate the other.

use rsync_protocol::ProtocolVersion;

/// Classification of the protocol version advertised by a remote peer.
///
/// Binary and legacy daemon negotiations both record the verbatim protocol
/// number announced by the peer alongside the clamped
/// [`ProtocolVersion`] that will be used for the remainder of the
/// session. When the advertisement exceeds the range supported by rsync
/// 3.4.1 the negotiated value is clamped to
/// [`ProtocolVersion::NEWEST`]. Higher layers frequently need to branch
/// on whether the peer ran a supported protocol or merely announced a
/// future release, so the classification is centralised here to ensure
/// the binary and legacy flows remain in lockstep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteProtocolAdvertisement {
    /// The peer advertised a protocol within the supported range.
    Supported(ProtocolVersion),
    /// The peer announced a future protocol that required clamping.
    Future(u32),
}

impl RemoteProtocolAdvertisement {
    /// Returns `true` when the peer advertised a protocol within the supported range.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported(_))
    }

    /// Returns the negotiated [`ProtocolVersion`] when the advertisement was supported.
    #[must_use]
    pub const fn supported(self) -> Option<ProtocolVersion> {
        match self {
            Self::Supported(version) => Some(version),
            Self::Future(_) => None,
        }
    }

    /// Returns the raw protocol number announced by the peer when it exceeded the supported range.
    #[must_use]
    pub const fn future(self) -> Option<u32> {
        match self {
            Self::Supported(_) => None,
            Self::Future(value) => Some(value),
        }
    }

    pub(crate) fn from_raw(advertised: u32, clamped: ProtocolVersion) -> Self {
        if remote_advertisement_was_clamped(advertised) {
            Self::Future(advertised)
        } else {
            Self::Supported(clamped)
        }
    }

    /// Returns the raw protocol number announced by the peer.
    ///
    /// The value matches the on-the-wire advertisement regardless of whether it fell within the
    /// supported range. When the peer selected a protocol known to rsync 3.4.1 the returned value
    /// equals the numeric form of [`ProtocolVersion`]; future advertisements yield the unclamped
    /// number so higher layers can surface diagnostics that mirror upstream rsync.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    /// use rsync_transport::RemoteProtocolAdvertisement;
    ///
    /// let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::from_supported(31).unwrap());
    /// assert_eq!(supported.advertised(), 31);
    ///
    /// let future = RemoteProtocolAdvertisement::Future(40);
    /// assert_eq!(future.advertised(), 40);
    /// ```
    #[must_use]
    pub const fn advertised(self) -> u32 {
        match self {
            Self::Supported(version) => version.as_u8() as u32,
            Self::Future(value) => value,
        }
    }
}

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
///
/// # Examples
///
/// Clamp the negotiated protocol to 29 even though the peer advertised 31, mirroring
/// `rsync --protocol=29` against a newer daemon.
///
/// ```rust,ignore
/// use rsync_protocol::ProtocolVersion;
/// use rsync_transport::handshake_util::local_cap_reduced_protocol;
///
/// let remote = ProtocolVersion::from_supported(31).unwrap();
/// let negotiated = ProtocolVersion::from_supported(29).unwrap();
///
/// assert!(local_cap_reduced_protocol(remote, negotiated));
/// ```
#[doc(alias = "--protocol")]
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

    #[test]
    fn classification_marks_supported_advertisements() {
        let version = ProtocolVersion::from_supported(31).expect("supported protocol");
        let advertised = u32::from(version.as_u8());
        let classification = RemoteProtocolAdvertisement::from_raw(advertised, version);

        assert!(classification.is_supported());
        assert_eq!(classification.supported(), Some(version));
        assert_eq!(classification.future(), None);
        assert_eq!(classification.advertised(), advertised);
    }

    #[test]
    fn classification_marks_future_advertisements() {
        let advertised = 40u32;
        let classification =
            RemoteProtocolAdvertisement::from_raw(advertised, ProtocolVersion::NEWEST);

        assert!(!classification.is_supported());
        assert_eq!(classification.supported(), None);
        assert_eq!(classification.future(), Some(advertised));
        assert_eq!(classification.advertised(), advertised);
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

        #[test]
        fn advertised_round_trips_to_raw_value(
            advertised in 0u32..=u16::MAX as u32,
        ) {
            let negotiated = if remote_advertisement_was_clamped(advertised) {
                ProtocolVersion::NEWEST
            } else {
                let byte = u8::try_from(advertised).unwrap_or(ProtocolVersion::NEWEST.as_u8());
                ProtocolVersion::from_supported(byte).unwrap_or(ProtocolVersion::OLDEST)
            };

            let classification = RemoteProtocolAdvertisement::from_raw(advertised, negotiated);
            let expected = if remote_advertisement_was_clamped(advertised) {
                advertised
            } else {
                negotiated.as_u8() as u32
            };

            prop_assert_eq!(classification.advertised(), expected);
        }
    }
}
