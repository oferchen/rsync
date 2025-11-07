#![allow(clippy::module_name_repetitions)]

//! Shared helpers for transport handshakes.
//!
//! This module centralises small pieces of logic used by both the binary and
//! legacy ASCII negotiation flows so they agree on how to interpret remote
//! protocol advertisements. Keeping the helpers in one place avoids subtle
//! drift between handshake wrappers and ensures that tests exercising one code
//! path also validate the other.

use oc_rsync_protocol::ProtocolVersion;
use std::fmt;

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
    Future {
        /// Raw protocol number announced by the peer.
        advertised: u32,
        /// [`ProtocolVersion`] obtained after applying upstream clamps.
        clamped: ProtocolVersion,
    },
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
            Self::Future { .. } => None,
        }
    }

    /// Returns the raw protocol number announced by the peer when it exceeded the supported range.
    #[must_use]
    pub const fn future(self) -> Option<u32> {
        match self {
            Self::Supported(_) => None,
            Self::Future { advertised, .. } => Some(advertised),
        }
    }

    /// Returns the [`ProtocolVersion`] used after clamping a future advertisement.
    ///
    /// When the peer announces a protocol newer than rsync 3.4.1 understands the
    /// negotiated session is downgraded to [`ProtocolVersion::NEWEST`]. This
    /// helper surfaces the clamped value only for those future advertisements so
    /// higher layers can reference it in diagnostics without repeating the enum
    /// matching boilerplate. Supported advertisements return [`None`], mirroring
    /// the [`Self::future`] accessor.
    ///
    /// # Examples
    ///
    /// ```
    /// use oc_rsync_protocol::ProtocolVersion;
    /// use oc_rsync_transport::RemoteProtocolAdvertisement;
    ///
    /// let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::V31);
    /// assert_eq!(supported.clamped(), None);
    ///
    /// let future = RemoteProtocolAdvertisement::Future {
    ///     advertised: 40,
    ///     clamped: ProtocolVersion::NEWEST,
    /// };
    /// assert_eq!(future.clamped(), Some(ProtocolVersion::NEWEST));
    /// ```
    #[must_use]
    pub const fn clamped(self) -> Option<ProtocolVersion> {
        match self {
            Self::Supported(_) => None,
            Self::Future { clamped, .. } => Some(clamped),
        }
    }

    pub(crate) const fn from_raw(advertised: u32, clamped: ProtocolVersion) -> Self {
        if remote_advertisement_was_clamped(advertised) {
            Self::Future {
                advertised,
                clamped,
            }
        } else {
            Self::Supported(clamped)
        }
    }

    /// Reports whether the advertised protocol exceeded the supported range.
    ///
    /// Upstream rsync clamps peers that announce future protocol versions to its newest
    /// implementation. Sessions where that occurred are surfaced via
    /// [`RemoteProtocolAdvertisement::Future`]. This helper exposes the same
    /// classification as a boolean flag so higher layers can branch on the condition without
    /// manually matching on the enum variants.
    ///
    /// # Examples
    ///
    /// ```
    /// use oc_rsync_protocol::ProtocolVersion;
    /// use oc_rsync_transport::RemoteProtocolAdvertisement;
    ///
    /// let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::V31);
    /// assert!(!supported.was_clamped());
    ///
    /// let future = RemoteProtocolAdvertisement::Future {
    ///     advertised: 40,
    ///     clamped: ProtocolVersion::NEWEST,
    /// };
    /// assert!(future.was_clamped());
    /// ```
    #[must_use]
    pub const fn was_clamped(self) -> bool {
        matches!(self, Self::Future { .. })
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
    /// use oc_rsync_protocol::ProtocolVersion;
    /// use oc_rsync_transport::RemoteProtocolAdvertisement;
    ///
    /// let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::from_supported(31).unwrap());
    /// assert_eq!(supported.advertised(), 31);
    ///
    /// let future = RemoteProtocolAdvertisement::Future {
    ///     advertised: 40,
    ///     clamped: ProtocolVersion::NEWEST,
    /// };
    /// assert_eq!(future.advertised(), 40);
    /// ```
    #[must_use]
    pub const fn advertised(self) -> u32 {
        match self {
            Self::Supported(version) => version.as_u8() as u32,
            Self::Future { advertised, .. } => advertised,
        }
    }

    /// Returns the [`ProtocolVersion`] used for the session after applying upstream clamps.
    ///
    /// Upstream rsync downgrades peers that advertise future protocol versions to
    /// [`ProtocolVersion::NEWEST`]. Sessions where the peer announced a supported
    /// protocol negotiate that exact version. The helper exposes the
    /// [`ProtocolVersion`] selected after upstream applies its clamps. Callers
    /// that also track a user-specified protocol cap (for example, via
    /// `--protocol`) should combine this value with
    /// [`local_cap_reduced_protocol`] or the negotiated protocol reported by the
    /// handshake structures to derive the final session version.
    ///
    /// # Examples
    ///
    /// ```
    /// use oc_rsync_protocol::ProtocolVersion;
    /// use oc_rsync_transport::RemoteProtocolAdvertisement;
    ///
    /// let supported = RemoteProtocolAdvertisement::Supported(
    ///     ProtocolVersion::from_supported(30).unwrap()
    /// );
    /// assert_eq!(supported.negotiated(), ProtocolVersion::from_supported(30).unwrap());
    ///
    /// let future = RemoteProtocolAdvertisement::Future {
    ///     advertised: 40,
    ///     clamped: ProtocolVersion::NEWEST,
    /// };
    /// assert_eq!(future.negotiated(), ProtocolVersion::NEWEST);
    /// ```
    #[must_use]
    pub const fn negotiated(self) -> ProtocolVersion {
        match self {
            Self::Supported(version) => version,
            Self::Future { clamped, .. } => clamped,
        }
    }
}

impl From<RemoteProtocolAdvertisement> for ProtocolVersion {
    /// Converts the classification into the negotiated [`ProtocolVersion`].
    ///
    /// Future protocol advertisements are represented as
    /// [`RemoteProtocolAdvertisement::Future`] and therefore clamp to
    /// [`ProtocolVersion::NEWEST`], mirroring the behaviour used by upstream
    /// rsync when a peer announces a newer release. Supported advertisements
    /// return their negotiated counterpart unchanged. The conversion keeps
    /// higher-level helpers ergonomic when they only need the active protocol
    /// value without branching on the classification.
    ///
    /// # Examples
    ///
    /// ```
    /// use oc_rsync_protocol::ProtocolVersion;
    /// use oc_rsync_transport::RemoteProtocolAdvertisement;
    ///
    /// let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::V31);
    /// let negotiated: ProtocolVersion = supported.into();
    /// assert_eq!(negotiated, ProtocolVersion::V31);
    ///
    /// let future = RemoteProtocolAdvertisement::Future {
    ///     advertised: 40,
    ///     clamped: ProtocolVersion::NEWEST,
    /// };
    /// let negotiated: ProtocolVersion = future.into();
    /// assert_eq!(negotiated, ProtocolVersion::NEWEST);
    /// ```
    fn from(classification: RemoteProtocolAdvertisement) -> Self {
        classification.negotiated()
    }
}

impl fmt::Display for RemoteProtocolAdvertisement {
    /// Formats the remote protocol announcement for diagnostics and logging.
    ///
    /// Supported advertisements render as `protocol <version>` whereas future
    /// announcements indicate the raw value together with the clamped
    /// [`ProtocolVersion`]. The format is intentionally concise so it can be
    /// embedded within higher-level messages without further allocation or
    /// string manipulation. The output is covered by unit tests to guarantee
    /// stability for downstream consumers.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Supported(version) => write!(f, "protocol {version}"),
            Self::Future {
                advertised,
                clamped,
            } => write!(f, "future protocol {advertised} (clamped to {clamped})"),
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
pub(crate) const fn remote_advertisement_was_clamped(advertised: u32) -> bool {
    let newest_supported = ProtocolVersion::NEWEST.as_u8() as u32;
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
/// ```
/// use oc_rsync_protocol::ProtocolVersion;
/// use oc_rsync_transport::local_cap_reduced_protocol;
///
/// let remote = ProtocolVersion::from_supported(31).unwrap();
/// let negotiated = ProtocolVersion::from_supported(29).unwrap();
///
/// assert!(local_cap_reduced_protocol(remote, negotiated));
/// ```
#[doc(alias = "--protocol")]
#[must_use]
pub const fn local_cap_reduced_protocol(
    remote: ProtocolVersion,
    negotiated: ProtocolVersion,
) -> bool {
    negotiated.as_u8() < remote.as_u8()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    macro_rules! const_assert {
        ($condition:expr $(,)?) => {
            const_assert!($condition, stringify!($condition));
        };
        ($condition:expr, $message:expr $(,)?) => {
            const _: () = {
                if !$condition {
                    panic!("{}", $message);
                }
            };
        };
    }

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
    fn remote_advertisement_helpers_are_const_evaluable() {
        const CLAMPED: bool =
            remote_advertisement_was_clamped(ProtocolVersion::NEWEST.as_u8() as u32 + 1);
        const NOT_CLAMPED: bool =
            remote_advertisement_was_clamped(ProtocolVersion::NEWEST.as_u8() as u32);
        const FUTURE: RemoteProtocolAdvertisement =
            RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST);
        const SUPPORTED: RemoteProtocolAdvertisement = RemoteProtocolAdvertisement::from_raw(
            ProtocolVersion::V30.as_u8() as u32,
            ProtocolVersion::V30,
        );
        const_assert!(CLAMPED, "remote advertisements must be clamped");
        const_assert!(!NOT_CLAMPED, "remote advertisement unexpectedly clamped");
        const_assert!(
            FUTURE.was_clamped(),
            "future advertisement should be clamped",
        );
        const_assert!(
            !SUPPORTED.was_clamped(),
            "supported advertisement should not be clamped",
        );

        let clamped = CLAMPED;
        let not_clamped = NOT_CLAMPED;
        let future = FUTURE;
        let supported = SUPPORTED;

        assert!(clamped);
        assert!(!not_clamped);
        assert!(matches!(
            future,
            RemoteProtocolAdvertisement::Future {
                advertised: 40,
                clamped
            } if clamped == ProtocolVersion::NEWEST
        ));
        assert!(matches!(
            supported,
            RemoteProtocolAdvertisement::Supported(ProtocolVersion::V30)
        ));
        assert_eq!(future.negotiated(), ProtocolVersion::NEWEST);
        assert_eq!(supported.negotiated(), ProtocolVersion::V30);
        assert!(future.was_clamped());
        assert!(!supported.was_clamped());
    }

    #[test]
    fn local_cap_detection_is_const_evaluable() {
        const WAS_CAPPED: bool =
            local_cap_reduced_protocol(ProtocolVersion::V31, ProtocolVersion::V29);
        const NOT_CAPPED: bool =
            local_cap_reduced_protocol(ProtocolVersion::V29, ProtocolVersion::V29);

        const_assert!(WAS_CAPPED, "local cap reduction must be detected");
        const_assert!(!NOT_CAPPED, "local cap should not be detected");

        let was_capped = WAS_CAPPED;
        let not_capped = NOT_CAPPED;

        assert!(was_capped);
        assert!(!not_capped);
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
        assert_eq!(classification.clamped(), None);
        assert_eq!(classification.advertised(), advertised);
        assert_eq!(classification.negotiated(), version);
        assert!(!classification.was_clamped());
    }

    #[test]
    fn classification_marks_future_advertisements() {
        let advertised = 40u32;
        let classification =
            RemoteProtocolAdvertisement::from_raw(advertised, ProtocolVersion::NEWEST);

        assert!(!classification.is_supported());
        assert_eq!(classification.supported(), None);
        assert_eq!(classification.future(), Some(advertised));
        assert_eq!(classification.clamped(), Some(ProtocolVersion::NEWEST));
        assert_eq!(classification.advertised(), advertised);
        assert_eq!(classification.negotiated(), ProtocolVersion::NEWEST);
        assert!(classification.was_clamped());
    }

    #[test]
    fn classification_converts_into_protocol_version() {
        let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::V31);
        let future = RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST);

        let supported_version: ProtocolVersion = supported.into();
        let future_version: ProtocolVersion = future.into();

        assert_eq!(supported_version, ProtocolVersion::V31);
        assert_eq!(future_version, ProtocolVersion::NEWEST);
        assert_eq!(future.clamped(), Some(ProtocolVersion::NEWEST));
    }

    #[test]
    fn classification_display_is_stable() {
        let supported = RemoteProtocolAdvertisement::Supported(ProtocolVersion::V31);
        let future = RemoteProtocolAdvertisement::from_raw(40, ProtocolVersion::NEWEST);

        assert_eq!(supported.to_string(), "protocol 31");
        assert_eq!(future.to_string(), "future protocol 40 (clamped to 32)");
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
            prop_assert_eq!(classification.negotiated(), negotiated);
            prop_assert_eq!(
                classification.was_clamped(),
                remote_advertisement_was_clamped(advertised)
            );
        }
    }
}
