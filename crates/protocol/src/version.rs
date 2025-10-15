use core::array::IntoIter;
use core::convert::TryFrom;
use core::fmt;
use core::num::NonZeroU8;
use core::ops::RangeInclusive;

use crate::error::NegotiationError;

/// Inclusive range of protocol versions that upstream rsync 3.4.1 understands.
const UPSTREAM_PROTOCOL_RANGE: RangeInclusive<u8> = 28..=32;

/// A single negotiated rsync protocol version.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProtocolVersion(NonZeroU8);

/// Types that can be interpreted as peer-advertised protocol versions.
///
/// The negotiation helpers in this module frequently operate on raw numeric
/// protocol identifiers transmitted by the peer. However, higher layers may
/// already work with fully validated [`ProtocolVersion`] values or iterate over
/// references when forwarding buffers. Exposing a small conversion trait keeps
/// the public helper flexible without forcing callers to allocate temporary
/// vectors or clone data solely to satisfy the type signature.
#[doc(hidden)]
pub trait ProtocolVersionAdvertisement: Copy {
    /// Returns the numeric representation expected by the negotiation logic.
    fn into_advertised_version(self) -> u8;
}

impl ProtocolVersionAdvertisement for u8 {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        self
    }
}

impl ProtocolVersionAdvertisement for NonZeroU8 {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        self.get()
    }
}

impl ProtocolVersionAdvertisement for ProtocolVersion {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        self.as_u8()
    }
}

impl ProtocolVersionAdvertisement for &u8 {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        *self
    }
}

impl ProtocolVersionAdvertisement for &NonZeroU8 {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        self.get()
    }
}

impl ProtocolVersionAdvertisement for &ProtocolVersion {
    #[inline]
    fn into_advertised_version(self) -> u8 {
        self.as_u8()
    }
}

macro_rules! declare_supported_protocols {
    ($($ver:literal),+ $(,)?) => {
        #[doc = "Protocol versions supported by the Rust implementation, ordered from"]
        #[doc = "newest to oldest as required by upstream rsync's negotiation logic."]
        pub const SUPPORTED_PROTOCOLS: [u8; declare_supported_protocols!(@len $($ver),+)] = [
            $($ver),+
        ];
        const SUPPORTED_PROTOCOL_VERSIONS: [ProtocolVersion;
            declare_supported_protocols!(@len $($ver),+)
        ] = [
            $(ProtocolVersion::new_const($ver)),+
        ];
    };
    (@len $($ver:literal),+) => {
        <[()]>::len(&[$(declare_supported_protocols!(@unit $ver)),+])
    };
    (@unit $ver:literal) => { () };
}

declare_supported_protocols!(32, 31, 30, 29, 28);

impl ProtocolVersion {
    pub(crate) const fn new_const(value: u8) -> Self {
        match NonZeroU8::new(value) {
            Some(v) => Self(v),
            None => panic!("protocol version must be non-zero"),
        }
    }

    /// The newest protocol version supported by upstream rsync 3.4.1.
    pub const NEWEST: ProtocolVersion = ProtocolVersion::new_const(32);

    /// The oldest protocol version supported by upstream rsync 3.4.1.
    pub const OLDEST: ProtocolVersion = ProtocolVersion::new_const(28);

    /// Array of protocol versions supported by the Rust implementation,
    /// ordered from newest to oldest.
    pub const SUPPORTED_VERSIONS: [ProtocolVersion; SUPPORTED_PROTOCOLS.len()] =
        SUPPORTED_PROTOCOL_VERSIONS;

    /// Returns a reference to the list of supported protocol versions in
    /// newest-to-oldest order.
    ///
    /// Exposing the slice instead of the fixed-size array mirrors the API
    /// shape found in upstream rsync's C helpers where callers operate on
    /// spans rather than arrays with baked-in lengths. This keeps parity while
    /// allowing downstream crates to consume the list without depending on the
    /// const-generic length used by the internal cache.
    #[must_use]
    pub const fn supported_versions() -> &'static [ProtocolVersion] {
        &Self::SUPPORTED_VERSIONS
    }

    /// Returns an iterator over the supported protocol versions in
    /// newest-to-oldest order.
    ///
    /// The iterator yields copies of the cached [`ProtocolVersion`]
    /// constants, mirroring the ordering exposed by
    /// [`SUPPORTED_PROTOCOLS`]. Higher layers that only need to iterate
    /// without borrowing the underlying array can rely on this helper to
    /// avoid manual slice handling while still matching upstream parity.
    #[must_use]
    pub fn supported_versions_iter() -> IntoIter<ProtocolVersion, { SUPPORTED_PROTOCOLS.len() }> {
        Self::SUPPORTED_VERSIONS.into_iter()
    }

    /// Reports whether the provided version is supported by this
    /// implementation. This helper mirrors the upstream negotiation guard and
    /// allows callers to perform quick validation before attempting a
    /// handshake.
    #[must_use]
    #[inline]
    pub const fn is_supported(value: u8) -> bool {
        let mut index = 0;
        while index < SUPPORTED_PROTOCOLS.len() {
            if SUPPORTED_PROTOCOLS[index] == value {
                return true;
            }
            index += 1;
        }
        false
    }

    /// Returns the raw numeric value represented by this version.
    #[must_use]
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self.0.get()
    }

    /// Converts a peer-advertised version into the negotiated protocol version.
    ///
    /// Upstream rsync tolerates peers that advertise a protocol newer than it
    /// understands by clamping the negotiated value to its newest supported
    /// protocol. Versions older than [`ProtocolVersion::OLDEST`] remain
    /// unsupported.
    #[must_use = "the negotiated protocol version must be handled"]
    pub fn from_peer_advertisement(value: u8) -> Result<Self, NegotiationError> {
        if value < Self::OLDEST.as_u8() {
            return Err(NegotiationError::UnsupportedVersion(value));
        }

        let clamped = if value > Self::NEWEST.as_u8() {
            Self::NEWEST.as_u8()
        } else {
            value
        };

        match NonZeroU8::new(clamped) {
            Some(non_zero) => Ok(Self(non_zero)),
            None => Err(NegotiationError::UnsupportedVersion(value)),
        }
    }
}

impl From<ProtocolVersion> for u8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        version.as_u8()
    }
}

impl From<ProtocolVersion> for NonZeroU8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        version.0
    }
}

impl TryFrom<u8> for ProtocolVersion {
    type Error = NegotiationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if UPSTREAM_PROTOCOL_RANGE.contains(&value) {
            match NonZeroU8::new(value) {
                Some(non_zero) => Ok(Self(non_zero)),
                None => Err(NegotiationError::UnsupportedVersion(value)),
            }
        } else {
            Err(NegotiationError::UnsupportedVersion(value))
        }
    }
}

impl TryFrom<NonZeroU8> for ProtocolVersion {
    type Error = NegotiationError;

    fn try_from(value: NonZeroU8) -> Result<Self, Self::Error> {
        <ProtocolVersion as TryFrom<u8>>::try_from(value.get())
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

impl PartialEq<u8> for ProtocolVersion {
    fn eq(&self, other: &u8) -> bool {
        self.as_u8() == *other
    }
}

impl PartialEq<ProtocolVersion> for u8 {
    fn eq(&self, other: &ProtocolVersion) -> bool {
        *self == other.as_u8()
    }
}

/// Selects the highest mutual protocol version between the Rust implementation and a peer.
///
/// The caller provides the list of protocol versions advertised by the peer in any order.
/// The function filters the peer list to versions that upstream rsync 3.4.1 recognizes and
/// clamps versions newer than [`ProtocolVersion::NEWEST`] down to the newest supported
/// value, matching upstream tolerance for future releases. Duplicate peer entries and
/// out-of-order announcements are tolerated. If no mutual protocol exists,
/// [`NegotiationError::NoMutualProtocol`] is returned with the filtered peer list for context.
#[must_use = "the negotiation outcome must be checked"]
pub fn select_highest_mutual<I, T>(peer_versions: I) -> Result<ProtocolVersion, NegotiationError>
where
    I: IntoIterator<Item = T>,
    T: ProtocolVersionAdvertisement,
{
    let mut seen_mask: u64 = 0;
    let mut seen_any = false;
    let mut seen_max = ProtocolVersion::OLDEST.as_u8();
    let mut oldest_rejection: Option<u8> = None;

    for version in peer_versions {
        let advertised = version.into_advertised_version();

        match ProtocolVersion::from_peer_advertisement(advertised) {
            Ok(proto) => {
                let value = proto.as_u8();
                let bit = 1u64 << value;
                if seen_mask & bit == 0 {
                    seen_mask |= bit;
                    seen_any = true;
                    if value > seen_max {
                        seen_max = value;
                    }
                }
            }
            Err(NegotiationError::UnsupportedVersion(value))
                if value < ProtocolVersion::OLDEST.as_u8() =>
            {
                if oldest_rejection.is_none_or(|current| value < current) {
                    oldest_rejection = Some(value);
                }
            }
            Err(err) => return Err(err),
        }
    }

    for ours in SUPPORTED_PROTOCOLS {
        if seen_mask & (1u64 << ours) != 0 {
            return Ok(ProtocolVersion::new_const(ours));
        }
    }

    if let Some(value) = oldest_rejection {
        return Err(NegotiationError::UnsupportedVersion(value));
    }

    let peer_versions = if seen_any {
        let start = ProtocolVersion::OLDEST.as_u8();
        let span = usize::from(seen_max.saturating_sub(start)) + 1;
        let mut versions = Vec::with_capacity(span);

        for version in start..=seen_max {
            if seen_mask & (1u64 << version) != 0 {
                versions.push(version);
            }
        }

        versions
    } else {
        Vec::new()
    };

    Err(NegotiationError::NoMutualProtocol { peer_versions })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_protocol_is_preferred() {
        let result = select_highest_mutual([32, 31, 30]).expect("must succeed");
        assert_eq!(result, ProtocolVersion::NEWEST);
    }

    #[test]
    fn downgrades_when_peer_lacks_newest() {
        let result = select_highest_mutual([31]).expect("must succeed");
        assert_eq!(result.as_u8(), 31);
    }

    #[test]
    fn reports_no_mutual_protocol() {
        let err = select_highest_mutual(core::iter::empty::<u8>()).unwrap_err();
        assert_eq!(
            err,
            NegotiationError::NoMutualProtocol {
                peer_versions: vec![]
            }
        );
    }

    #[test]
    fn select_highest_mutual_deduplicates_peer_versions() {
        let negotiated = select_highest_mutual([32, 32, 31, 31]).expect("must select 32");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_handles_unsorted_peer_versions() {
        let negotiated = select_highest_mutual([29, 32, 30, 31]).expect("must select newest");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_accepts_slice_iterators() {
        let peers = [31u8, 29, 32];
        let negotiated = select_highest_mutual(peers.iter()).expect("slice iter works");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_accepts_protocol_version_references() {
        let peers = ProtocolVersion::supported_versions();
        let negotiated = select_highest_mutual(peers.iter()).expect("refs work");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_accepts_non_zero_u8_advertisements() {
        let peers = [
            NonZeroU8::new(32).expect("non-zero"),
            NonZeroU8::new(31).expect("non-zero"),
        ];
        let negotiated = select_highest_mutual(peers).expect("non-zero values work");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn display_for_no_mutual_protocol_mentions_filtered_list() {
        let err = NegotiationError::NoMutualProtocol {
            peer_versions: vec![29, 30],
        };
        let rendered = err.to_string();
        assert!(rendered.contains("peer offered [29, 30]"));
        assert!(rendered.contains("we support"));
    }

    #[test]
    fn rejects_zero_protocol_version() {
        let err = select_highest_mutual([0]).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(0));
    }

    #[test]
    fn clamps_future_versions_in_peer_advertisements_directly() {
        let negotiated =
            ProtocolVersion::from_peer_advertisement(40).expect("future versions clamp");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn rejects_peer_advertisements_older_than_supported_range() {
        let err = ProtocolVersion::from_peer_advertisement(27).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(27));
    }

    #[test]
    fn clamps_future_peer_versions_in_selection() {
        let negotiated = select_highest_mutual([35, 31]).expect("must clamp to newest");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn ignores_versions_older_than_supported_when_newer_exists() {
        let negotiated = select_highest_mutual([27, 29, 27]).expect("29 should be selected");
        assert_eq!(negotiated.as_u8(), 29);
    }

    #[test]
    fn reports_unsupported_when_only_too_old_versions_are_offered() {
        let err = select_highest_mutual([27, 26]).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(26));
    }

    #[test]
    fn supported_versions_constant_matches_u8_list() {
        let expected: Vec<u8> = ProtocolVersion::SUPPORTED_VERSIONS
            .into_iter()
            .map(ProtocolVersion::as_u8)
            .collect();
        assert_eq!(expected, SUPPORTED_PROTOCOLS);
    }

    #[test]
    fn supported_versions_method_matches_constant_slice() {
        assert_eq!(
            ProtocolVersion::supported_versions(),
            ProtocolVersion::SUPPORTED_VERSIONS.as_slice()
        );
    }

    #[test]
    fn supported_versions_iterator_matches_constants() {
        let via_iterator: Vec<u8> = ProtocolVersion::supported_versions_iter()
            .map(ProtocolVersion::as_u8)
            .collect();
        assert_eq!(via_iterator, SUPPORTED_PROTOCOLS);
    }

    #[test]
    fn detects_supported_versions() {
        for version in SUPPORTED_PROTOCOLS {
            assert!(ProtocolVersion::is_supported(version));
        }
    }

    #[test]
    fn rejects_unsupported_versions_in_helper() {
        assert!(!ProtocolVersion::is_supported(0));
        assert!(!ProtocolVersion::is_supported(27));
        assert!(!ProtocolVersion::is_supported(33));
    }

    #[test]
    fn rejects_out_of_range_non_zero_u8() {
        let value = NonZeroU8::new(27).expect("non-zero");
        let err = ProtocolVersion::try_from(value).unwrap_err();
        assert_eq!(err, NegotiationError::UnsupportedVersion(27));
    }

    #[test]
    fn converts_from_non_zero_u8() {
        let value = NonZeroU8::new(31).expect("non-zero");
        let version = ProtocolVersion::try_from(value).expect("valid");
        assert_eq!(version.as_u8(), 31);
    }

    #[test]
    fn select_highest_mutual_accepts_only_future_versions() {
        let negotiated = select_highest_mutual([40]).expect("future-only handshake clamps");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn select_highest_mutual_prefers_newest_when_future_and_too_old_mix() {
        let negotiated = select_highest_mutual([0, 40])
            .expect("unsupported low versions must not mask clamped future ones");
        assert_eq!(negotiated, ProtocolVersion::NEWEST);
    }

    #[test]
    fn converts_protocol_version_to_non_zero_u8() {
        let value = NonZeroU8::new(28).expect("non-zero");
        let version = ProtocolVersion::try_from(value).expect("valid");
        let round_trip: NonZeroU8 = version.into();
        assert_eq!(round_trip, value);
    }

    #[test]
    fn converts_protocol_version_to_u8() {
        let version = ProtocolVersion::try_from(32).expect("valid");
        let value: u8 = version.into();
        assert_eq!(value, 32);
    }

    #[test]
    fn compares_directly_with_u8() {
        let version = ProtocolVersion::try_from(30).expect("valid");
        assert_eq!(version, 30);
        assert_eq!(30, version);
    }

    #[test]
    fn supported_versions_are_sorted_descending() {
        let mut sorted = SUPPORTED_PROTOCOLS;
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(sorted, SUPPORTED_PROTOCOLS);
    }

    #[test]
    fn protocol_version_display_matches_numeric_value() {
        let version = ProtocolVersion::try_from(32).expect("valid");
        assert_eq!(version.to_string(), "32");
    }
}
