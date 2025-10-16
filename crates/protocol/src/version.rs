use core::array::IntoIter;
use core::convert::TryFrom;
use core::fmt;
use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize,
};
use core::ops::RangeInclusive;

use crate::error::NegotiationError;

/// Inclusive range of protocol versions that upstream rsync 3.4.1 understands.
const UPSTREAM_PROTOCOL_RANGE: RangeInclusive<u8> = 28..=32;

/// A single negotiated rsync protocol version.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProtocolVersion(NonZeroU8);

/// Types that can be interpreted as peer-advertised protocol versions.
///
/// The negotiation helpers in this module frequently operate on raw numeric
/// protocol identifiers transmitted by the peer. However, higher layers may
/// already work with fully validated [`ProtocolVersion`] values, store the
/// negotiated byte in non-zero wrappers, or iterate over references when
/// forwarding buffers. Exposing a small conversion trait keeps the public
/// helper flexible without forcing callers to allocate temporary vectors,
/// normalize wrappers, or clone data solely to satisfy the type signature.
/// Implementations are provided for primitive integers, [`ProtocolVersion`],
/// and both shared and mutable references so iterator adapters such as
/// [`core::slice::iter`](core::slice::iter) and
/// [`core::slice::iter_mut`](core::slice::iter_mut) can be forwarded directly.
#[doc(hidden)]
pub trait ProtocolVersionAdvertisement {
    /// Returns the numeric representation expected by the negotiation logic.
    ///
    /// Implementations for integer types wider than `u8` saturate to
    /// `u8::MAX` to mirror upstream rsync's tolerance for future protocol
    /// revisions. Values above the byte range are therefore treated as
    /// "newer than supported" and subsequently clamped to
    /// [`ProtocolVersion::NEWEST`].
    fn into_advertised_version(self) -> u8;
}

macro_rules! impl_protocol_version_advertisement {
    ($($ty:ty => $into:expr),+ $(,)?) => {
        $(
            impl ProtocolVersionAdvertisement for $ty {
                #[inline]
                fn into_advertised_version(self) -> u8 {
                    let convert = $into;
                    convert(self)
                }
            }

            impl ProtocolVersionAdvertisement for &$ty {
                #[inline]
                fn into_advertised_version(self) -> u8 {
                    let convert = $into;
                    convert(*self)
                }
            }

            impl ProtocolVersionAdvertisement for &mut $ty {
                #[inline]
                fn into_advertised_version(self) -> u8 {
                    let convert = $into;
                    convert(*self)
                }
            }
        )+
    };
}

impl_protocol_version_advertisement!(
    u8 => |value: u8| value,
    NonZeroU8 => NonZeroU8::get,
    ProtocolVersion => ProtocolVersion::as_u8,
    u16 => |value: u16| value.min(u16::from(u8::MAX)) as u8,
    u32 => |value: u32| value.min(u32::from(u8::MAX)) as u8,
    u64 => |value: u64| value.min(u64::from(u8::MAX)) as u8,
    u128 => |value: u128| value.min(u128::from(u8::MAX)) as u8,
    usize => |value: usize| value.min(usize::from(u8::MAX)) as u8,
    NonZeroU16 => |value: NonZeroU16| value.get().min(u16::from(u8::MAX)) as u8,
    NonZeroU32 => |value: NonZeroU32| value.get().min(u32::from(u8::MAX)) as u8,
    NonZeroU64 => |value: NonZeroU64| value.get().min(u64::from(u8::MAX)) as u8,
    NonZeroU128 => |value: NonZeroU128| value.get().min(u128::from(u8::MAX)) as u8,
    NonZeroUsize => |value: NonZeroUsize| value.get().min(usize::from(u8::MAX)) as u8,
    i8 => |value: i8| value.clamp(0, i8::MAX) as u8,
    i16 => |value: i16| value.clamp(0, i16::from(u8::MAX)) as u8,
    i32 => |value: i32| value.clamp(0, i32::from(u8::MAX)) as u8,
    i64 => |value: i64| value.clamp(0, i64::from(u8::MAX)) as u8,
    i128 => |value: i128| value.clamp(0, i128::from(u8::MAX)) as u8,
    isize => |value: isize| value.clamp(0, isize::from(u8::MAX)) as u8,
    NonZeroI8 => |value: NonZeroI8| value.get().clamp(0, i8::MAX) as u8,
    NonZeroI16 => |value: NonZeroI16| value.get().clamp(0, i16::from(u8::MAX)) as u8,
    NonZeroI32 => |value: NonZeroI32| value.get().clamp(0, i32::from(u8::MAX)) as u8,
    NonZeroI64 => |value: NonZeroI64| value.get().clamp(0, i64::from(u8::MAX)) as u8,
    NonZeroI128 => |value: NonZeroI128| value.get().clamp(0, i128::from(u8::MAX)) as u8,
    NonZeroIsize => |value: NonZeroIsize| value.get().clamp(0, isize::from(u8::MAX)) as u8,
);

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

    /// Returns the numeric protocol identifiers supported by this
    /// implementation in newest-to-oldest order.
    ///
    /// Upstream rsync frequently passes around the raw `u8` identifiers when
    /// negotiating with a peer. Providing a slice view avoids forcing callers
    /// to depend on the exported [`SUPPORTED_PROTOCOLS`] array directly while
    /// still guaranteeing byte-for-byte parity with upstream's ordering.
    #[must_use]
    pub const fn supported_protocol_numbers() -> &'static [u8] {
        &SUPPORTED_PROTOCOLS
    }

    /// Returns an iterator over the numeric protocol identifiers supported by this implementation.
    ///
    /// Upstream rsync often iterates over the protocol list while negotiating with peers,
    /// especially when emitting diagnostics that mention every supported version. Exposing an
    /// iterator keeps those call sites allocation-free and mirrors the semantics provided by
    /// [`ProtocolVersion::supported_versions_iter`] without requiring callers to convert the
    /// exported slice into an owned vector.
    #[must_use]
    pub fn supported_protocol_numbers_iter() -> IntoIter<u8, { SUPPORTED_PROTOCOLS.len() }> {
        SUPPORTED_PROTOCOLS.into_iter()
    }

    /// Returns the inclusive range of protocol versions supported by this implementation.
    ///
    /// Higher layers frequently render diagnostics that mention the supported protocol span.
    /// Exposing the range directly keeps those call-sites in sync with the
    /// [`ProtocolVersion::OLDEST`] and [`ProtocolVersion::NEWEST`] bounds without duplicating the
    /// numeric literals.
    #[must_use]
    pub const fn supported_range() -> RangeInclusive<u8> {
        Self::OLDEST.as_u8()..=Self::NEWEST.as_u8()
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

    /// Attempts to construct a [`ProtocolVersion`] from a byte that is known to be within the
    /// range supported by upstream rsync 3.4.1.
    ///
    /// The helper accepts the raw numeric value emitted on the wire and returns `Some` when the
    /// version falls inside the inclusive range [`ProtocolVersion::OLDEST`]..=[`ProtocolVersion::NEWEST`].
    /// Values outside that span yield `None`. Unlike [`TryFrom<u8>`], the function is `const`, making
    /// it suitable for compile-time validation in tables that embed protocol numbers directly.
    ///
    /// ```
    /// use rsync_protocol::ProtocolVersion;
    ///
    /// const MAYBE_NEWEST: Option<ProtocolVersion> = ProtocolVersion::from_supported(32);
    /// assert_eq!(MAYBE_NEWEST, Some(ProtocolVersion::NEWEST));
    ///
    /// const UNKNOWN: Option<ProtocolVersion> = ProtocolVersion::from_supported(27);
    /// assert!(UNKNOWN.is_none());
    /// ```
    #[must_use]
    pub const fn from_supported(value: u8) -> Option<Self> {
        if value >= Self::OLDEST.as_u8() && value <= Self::NEWEST.as_u8() {
            Some(Self::new_const(value))
        } else {
            None
        }
    }

    /// Reports whether the provided version is supported by this
    /// implementation. This helper mirrors the upstream negotiation guard and
    /// allows callers to perform quick validation before attempting a
    /// handshake.
    #[must_use]
    #[inline]
    pub const fn is_supported(value: u8) -> bool {
        Self::from_supported(value).is_some()
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
        if !UPSTREAM_PROTOCOL_RANGE.contains(&value) {
            return Err(NegotiationError::UnsupportedVersion(value));
        }

        // The upstream-supported range excludes zero, ensuring the constructor cannot fail here.
        Ok(Self::from_supported(value).expect("values within the upstream range are supported"))
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
    let mut seen = [false; u8::MAX as usize + 1];
    let mut seen_any = false;
    let mut seen_max = ProtocolVersion::OLDEST.as_u8();
    let mut oldest_rejection: Option<u8> = None;

    for version in peer_versions {
        let advertised = version.into_advertised_version();

        match ProtocolVersion::from_peer_advertisement(advertised) {
            Ok(proto) => {
                let value = proto.as_u8();
                let index = usize::from(value);
                if !seen[index] {
                    seen[index] = true;
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
        if seen[usize::from(ours)] {
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
            if seen[usize::from(version)] {
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
mod tests;
