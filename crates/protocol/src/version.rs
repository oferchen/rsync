use core::array::IntoIter;
use core::convert::TryFrom;
use core::fmt;
use core::num::{
    NonZeroI8, NonZeroI16, NonZeroI32, NonZeroI64, NonZeroI128, NonZeroIsize, NonZeroU8,
    NonZeroU16, NonZeroU32, NonZeroU64, NonZeroU128, NonZeroUsize, Wrapping,
};
use core::ops::RangeInclusive;
use core::str::FromStr;

use crate::error::NegotiationError;

const OLDEST_SUPPORTED_PROTOCOL: u8 = 28;
const NEWEST_SUPPORTED_PROTOCOL: u8 = 32;

/// Inclusive range of protocol versions that upstream rsync 3.4.1 understands.
const UPSTREAM_PROTOCOL_RANGE: RangeInclusive<u8> =
    OLDEST_SUPPORTED_PROTOCOL..=NEWEST_SUPPORTED_PROTOCOL;

/// Inclusive range of protocol versions supported by the Rust implementation.
///
/// Upstream rsync communicates the supported span in several diagnostics and
/// negotiation helpers. Exposing the same range ensures higher layers can
/// render parity messages without hard-coding protocol boundaries. The value is
/// kept in sync with [`ProtocolVersion::OLDEST`] and
/// [`ProtocolVersion::NEWEST`], so the compile-time guards later in this file
/// will fail if the literals ever drift.
pub const SUPPORTED_PROTOCOL_RANGE: RangeInclusive<u8> =
    OLDEST_SUPPORTED_PROTOCOL..=NEWEST_SUPPORTED_PROTOCOL;

/// Inclusive `(oldest, newest)` tuple describing the protocol span supported by the Rust
/// implementation.
///
/// Upstream rsync surfaces the lowest and highest negotiated versions in many diagnostics. Exporting
/// the tuple keeps higher layers from duplicating the literal bounds while guaranteeing parity with
/// [`SUPPORTED_PROTOCOL_RANGE`]. The helper mirrors the information exposed by
/// [`ProtocolVersion::supported_range_bounds`] without requiring callers to depend on the
/// [`ProtocolVersion`] type.
pub const SUPPORTED_PROTOCOL_BOUNDS: (u8, u8) =
    (OLDEST_SUPPORTED_PROTOCOL, NEWEST_SUPPORTED_PROTOCOL);

/// Errors that can occur while parsing a protocol version from a string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseProtocolVersionErrorKind {
    /// The provided string was empty after trimming ASCII whitespace.
    Empty,
    /// The provided string contained non-digit characters.
    InvalidDigit,
    /// The provided string encoded a negative value.
    Negative,
    /// The provided string encoded an integer larger than `u8::MAX`.
    Overflow,
    /// The parsed integer fell outside upstream rsync's supported range.
    UnsupportedRange(u8),
}

/// Error type returned when parsing a [`ProtocolVersion`] from text fails.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseProtocolVersionError {
    kind: ParseProtocolVersionErrorKind,
}

impl ParseProtocolVersionError {
    const fn new(kind: ParseProtocolVersionErrorKind) -> Self {
        Self { kind }
    }

    /// Returns the classification describing why parsing failed.
    #[must_use]
    pub const fn kind(self) -> ParseProtocolVersionErrorKind {
        self.kind
    }

    /// Returns the unsupported protocol byte that triggered
    /// [`ParseProtocolVersionErrorKind::UnsupportedRange`], if any.
    #[must_use]
    pub const fn unsupported_value(self) -> Option<u8> {
        match self.kind {
            ParseProtocolVersionErrorKind::UnsupportedRange(value) => Some(value),
            _ => None,
        }
    }
}

impl fmt::Display for ParseProtocolVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ParseProtocolVersionErrorKind::Empty => f.write_str("protocol version string is empty"),
            ParseProtocolVersionErrorKind::InvalidDigit => {
                f.write_str("protocol version must be an unsigned integer")
            }
            ParseProtocolVersionErrorKind::Negative => {
                f.write_str("protocol version cannot be negative")
            }
            ParseProtocolVersionErrorKind::Overflow => {
                f.write_str("protocol version value exceeds u8::MAX")
            }
            ParseProtocolVersionErrorKind::UnsupportedRange(value) => {
                let (oldest, newest) = ProtocolVersion::supported_range_bounds();
                write!(
                    f,
                    "protocol version {} is outside the supported range {}-{}",
                    value, oldest, newest
                )
            }
        }
    }
}

impl std::error::Error for ParseProtocolVersionError {}

/// A single negotiated rsync protocol version.
///
/// # Examples
///
/// Parse a version string using the `FromStr` implementation. The helper trims leading and trailing
/// ASCII whitespace and accepts an optional leading `+`, mirroring the tolerance found in upstream
/// rsync's option parser.
///
/// ```
/// use std::str::FromStr;
/// use rsync_protocol::ProtocolVersion;
///
/// let version = ProtocolVersion::from_str(" 31 ")?;
/// assert_eq!(version.as_u8(), 31);
/// # Ok::<_, rsync_protocol::ParseProtocolVersionError>(())
/// ```
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
    Wrapping<u8> => |value: Wrapping<u8>| value.0,
    Wrapping<u16> => |value: Wrapping<u16>| value.0.min(u16::from(u8::MAX)) as u8,
    Wrapping<u32> => |value: Wrapping<u32>| value.0.min(u32::from(u8::MAX)) as u8,
    Wrapping<u64> => |value: Wrapping<u64>| value.0.min(u64::from(u8::MAX)) as u8,
    Wrapping<u128> => |value: Wrapping<u128>| value.0.min(u128::from(u8::MAX)) as u8,
    Wrapping<usize> => |value: Wrapping<usize>| value.0.min(usize::from(u8::MAX)) as u8,
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
    Wrapping<i8> => |value: Wrapping<i8>| value.0.clamp(0, i8::MAX) as u8,
    Wrapping<i16> => |value: Wrapping<i16>| value.0.clamp(0, i16::from(u8::MAX)) as u8,
    Wrapping<i32> => |value: Wrapping<i32>| value.0.clamp(0, i32::from(u8::MAX)) as u8,
    Wrapping<i64> => |value: Wrapping<i64>| value.0.clamp(0, i64::from(u8::MAX)) as u8,
    Wrapping<i128> => |value: Wrapping<i128>| value.0.clamp(0, i128::from(u8::MAX)) as u8,
    Wrapping<isize> => |value: Wrapping<isize>| value.0.clamp(0, isize::from(u8::MAX)) as u8,
);

macro_rules! declare_supported_protocols {
    ($($ver:literal),+ $(,)?) => {
        #[doc = "Number of protocol versions supported by the Rust implementation."]
        pub const SUPPORTED_PROTOCOL_COUNT: usize = declare_supported_protocols!(@len $($ver),+);

        #[doc = "Protocol versions supported by the Rust implementation, ordered from"]
        #[doc = "newest to oldest as required by upstream rsync's negotiation logic."]
        pub const SUPPORTED_PROTOCOLS: [u8; SUPPORTED_PROTOCOL_COUNT] = [
            $($ver),+
        ];
        const SUPPORTED_PROTOCOL_VERSIONS: [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT] = [
            $(ProtocolVersion::new_const($ver)),+
        ];
    };
    (@len $($ver:literal),+) => {
        <[()]>::len(&[$(declare_supported_protocols!(@unit $ver)),+])
    };
    (@unit $ver:literal) => { () };
}

declare_supported_protocols!(32, 31, 30, 29, 28);

/// Bitmask describing the protocol versions supported by the Rust implementation.
///
/// Each bit position corresponds to the numeric protocol identifier understood by upstream
/// rsync 3.4.1. For example, bit `32` (counting from zero) represents protocol 32. The mask is
/// used by helpers that need to perform constant-time membership checks or preallocate lookup
/// tables keyed by the negotiated protocol value without iterating over the canonical version
/// list. Exposing the bitmap keeps those call sites in sync with [`SUPPORTED_PROTOCOLS`] while
/// avoiding duplicate literals that could drift as new protocols are added.
pub const SUPPORTED_PROTOCOL_BITMAP: u64 = {
    let mut bitmap = 0u64;
    let mut index = 0usize;

    while index < SUPPORTED_PROTOCOL_COUNT {
        let protocol = SUPPORTED_PROTOCOLS[index];
        bitmap |= 1u64 << protocol;
        index += 1;
    }

    bitmap
};

// Compile-time guard that mirrors the runtime assertions covered by the unit
// tests. Keeping the invariants in a `const` block ensures the workspace cannot
// accidentally reorder, duplicate, or drift the advertised protocol list even
// when tests are skipped. The loop bodies intentionally avoid iterator adapters
// to remain usable in const evaluation.
const _: () = {
    let protocols = SUPPORTED_PROTOCOLS;
    assert!(
        !protocols.is_empty(),
        "supported protocol list must not be empty"
    );
    assert!(
        protocols.len() == SUPPORTED_PROTOCOL_COUNT,
        "supported protocol count must match list length",
    );
    assert!(
        protocols[0] == ProtocolVersion::NEWEST.as_u8(),
        "newest supported protocol must lead the list",
    );
    assert!(
        protocols[protocols.len() - 1] == ProtocolVersion::OLDEST.as_u8(),
        "oldest supported protocol must terminate the list",
    );

    let newest = ProtocolVersion::NEWEST.as_u8() as u32;
    assert!(
        newest < u64::BITS,
        "supported protocol bitmap must accommodate newest protocol",
    );

    let mut index = 1usize;
    while index < SUPPORTED_PROTOCOL_COUNT {
        assert!(
            protocols[index - 1] > protocols[index],
            "supported protocols must be strictly descending",
        );
        assert!(
            ProtocolVersion::OLDEST.as_u8() <= protocols[index]
                && protocols[index] <= ProtocolVersion::NEWEST.as_u8(),
            "each supported protocol must fall within the upstream range",
        );
        index += 1;
    }

    let versions = ProtocolVersion::SUPPORTED_VERSIONS;
    assert!(
        versions.len() == SUPPORTED_PROTOCOL_COUNT,
        "cached ProtocolVersion list must mirror numeric protocols",
    );

    let mut index = 0usize;
    while index < versions.len() {
        assert!(
            versions[index].as_u8() == protocols[index],
            "cached ProtocolVersion must match numeric protocol at each index",
        );
        index += 1;
    }

    let mut bitmap = 0u64;
    index = 0usize;
    while index < SUPPORTED_PROTOCOL_COUNT {
        bitmap |= 1u64 << protocols[index];
        index += 1;
    }
    assert!(
        bitmap == SUPPORTED_PROTOCOL_BITMAP,
        "supported protocol bitmap must mirror numeric protocol list",
    );
    assert!(
        SUPPORTED_PROTOCOL_BITMAP.count_ones() as usize == SUPPORTED_PROTOCOL_COUNT,
        "supported protocol bitmap must contain one bit per protocol version",
    );
    assert!(
        SUPPORTED_PROTOCOL_BITMAP >> (ProtocolVersion::NEWEST.as_u8() as usize + 1) == 0,
        "supported protocol bitmap must not include bits above the newest supported version",
    );
    assert!(
        SUPPORTED_PROTOCOL_BITMAP & ((1u64 << ProtocolVersion::OLDEST.as_u8()) - 1) == 0,
        "supported protocol bitmap must not include bits below the oldest supported version",
    );

    let range_oldest = *SUPPORTED_PROTOCOL_RANGE.start();
    let range_newest = *SUPPORTED_PROTOCOL_RANGE.end();
    assert!(
        range_oldest == ProtocolVersion::OLDEST.as_u8(),
        "supported protocol range must begin at the oldest supported version",
    );
    assert!(
        range_newest == ProtocolVersion::NEWEST.as_u8(),
        "supported protocol range must end at the newest supported version",
    );

    let (bounds_oldest, bounds_newest) = SUPPORTED_PROTOCOL_BOUNDS;
    assert!(
        bounds_oldest == range_oldest,
        "supported protocol bounds tuple must begin at the oldest supported version",
    );
    assert!(
        bounds_newest == range_newest,
        "supported protocol bounds tuple must end at the newest supported version",
    );

    let upstream_oldest = *UPSTREAM_PROTOCOL_RANGE.start();
    let upstream_newest = *UPSTREAM_PROTOCOL_RANGE.end();
    assert!(
        range_oldest == upstream_oldest && range_newest == upstream_newest,
        "supported protocol range must match upstream rsync's protocol span",
    );
};

impl ProtocolVersion {
    pub(crate) const fn new_const(value: u8) -> Self {
        match NonZeroU8::new(value) {
            Some(v) => Self(v),
            None => panic!("protocol version must be non-zero"),
        }
    }

    /// The newest protocol version supported by upstream rsync 3.4.1.
    pub const NEWEST: ProtocolVersion = ProtocolVersion::new_const(NEWEST_SUPPORTED_PROTOCOL);

    /// The oldest protocol version supported by upstream rsync 3.4.1.
    pub const OLDEST: ProtocolVersion = ProtocolVersion::new_const(OLDEST_SUPPORTED_PROTOCOL);

    /// Array of protocol versions supported by the Rust implementation,
    /// ordered from newest to oldest.
    pub const SUPPORTED_VERSIONS: [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT] =
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

    /// Returns the cached list of supported protocol versions as a fixed-size array reference.
    ///
    /// Some compile-time contexts require access to the concrete array type instead of a slice,
    /// mirroring helpers such as [`ProtocolVersion::supported_protocol_numbers_array`]. Exposing
    /// the array keeps those call sites in sync with the canonical
    /// [`ProtocolVersion::SUPPORTED_VERSIONS`] cache while avoiding duplicate literals.
    #[must_use]
    pub const fn supported_versions_array() -> &'static [ProtocolVersion; SUPPORTED_PROTOCOL_COUNT]
    {
        &Self::SUPPORTED_VERSIONS
    }

    /// Reports whether the provided numeric protocol identifier is supported
    /// by this implementation.
    ///
    /// The helper mirrors the [`ProtocolVersion::is_supported`] guard but
    /// operates on the raw byte without attempting to construct a
    /// [`ProtocolVersion`]. Callers that only need a membership check can rely
    /// on this helper to obtain the answer in constant time using the
    /// precomputed [`SUPPORTED_PROTOCOL_BITMAP`] instead of scanning the
    /// canonical list. This is particularly useful in const contexts where
    /// callers validate table entries that embed protocol numbers directly
    /// without triggering a runtime negotiation.
    #[must_use]
    pub const fn is_supported_protocol_number(value: u8) -> bool {
        if value < Self::OLDEST.as_u8() || value > Self::NEWEST.as_u8() {
            return false;
        }

        // Guard against shifts that would exceed the width of the bitmap when
        // future protocol versions extend beyond the `u64` range. The runtime
        // check mirrors the compile-time assertion found in the static guard
        // below, but keeping the condition here ensures callers receive a
        // deterministic `false` instead of triggering a panic should the
        // invariant ever be violated in a release build.
        if value as u32 >= u64::BITS {
            return false;
        }

        (SUPPORTED_PROTOCOL_BITMAP & (1u64 << value)) != 0
    }

    /// Returns the numeric protocol identifiers supported by this
    /// implementation in newest-to-oldest order.
    ///
    /// Upstream rsync frequently passes around the raw `u8` identifiers when
    /// negotiating with a peer. Providing a slice view avoids forcing callers
    /// to depend on the exported [`SUPPORTED_PROTOCOLS`] array directly while
    /// still guaranteeing byte-for-byte parity with upstream's ordering. The
    /// iterator borrows the canonical list so repeated calls reuse the same
    /// backing storage instead of copying the table for every traversal.
    #[must_use]
    pub const fn supported_protocol_numbers() -> &'static [u8] {
        &SUPPORTED_PROTOCOLS
    }

    /// Returns the numeric protocol identifiers as a fixed-size array reference.
    ///
    /// Some compile-time contexts require access to the concrete array type
    /// instead of a slice—mirroring upstream code that embeds protocol tables in
    /// static data structures. Providing this helper keeps those call sites in
    /// sync with the canonical [`SUPPORTED_PROTOCOLS`] constant without
    /// re-exporting the array in multiple locations.
    #[must_use]
    pub const fn supported_protocol_numbers_array() -> &'static [u8; SUPPORTED_PROTOCOL_COUNT] {
        &SUPPORTED_PROTOCOLS
    }

    /// Returns a bitmap describing the protocol versions supported by this implementation.
    ///
    /// Each set bit corresponds to the numeric identifier advertised to peers, making it useful
    /// for constant-time membership tests and pre-sizing data structures indexed by protocol
    /// version. For example, bit `32` (counting from zero) represents protocol 32. The bitmap is
    /// derived from the canonical [`SUPPORTED_PROTOCOLS`] list to guarantee it stays in sync with
    /// the negotiated versions.
    #[must_use]
    pub const fn supported_protocol_bitmap() -> u64 {
        SUPPORTED_PROTOCOL_BITMAP
    }

    /// Returns an iterator over the numeric protocol identifiers supported by this implementation.
    ///
    /// Upstream rsync often iterates over the protocol list while negotiating with peers,
    /// especially when emitting diagnostics that mention every supported version. Exposing an
    /// iterator keeps those call sites allocation-free and mirrors the semantics provided by
    /// [`ProtocolVersion::supported_versions_iter`] without requiring callers to convert the
    /// exported slice into an owned vector.
    #[must_use]
    pub fn supported_protocol_numbers_iter() -> IntoIter<u8, { SUPPORTED_PROTOCOL_COUNT }> {
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
        SUPPORTED_PROTOCOL_RANGE
    }

    /// Returns the inclusive supported range as a tuple of `(oldest, newest)`.
    ///
    /// Higher layers frequently need to mention both bounds in diagnostics
    /// without necessarily constructing a [`RangeInclusive`]. Centralizing the
    /// numeric pair keeps those call sites in sync with the canonical
    /// [`ProtocolVersion::OLDEST`] and [`ProtocolVersion::NEWEST`] constants,
    /// avoiding duplicate literals that could drift if upstream changes the
    /// supported span.
    #[must_use]
    pub const fn supported_range_bounds() -> (u8, u8) {
        SUPPORTED_PROTOCOL_BOUNDS
    }

    /// Returns the oldest and newest supported protocol versions as strongly typed values.
    ///
    /// Higher layers that operate on [`ProtocolVersion`] values instead of raw bytes can
    /// use this helper to remain in sync with the canonical bounds without re-encoding the
    /// numeric range. The pair mirrors the information exposed by
    /// [`ProtocolVersion::supported_range_bounds`] while preserving type safety for code that
    /// stores negotiated versions in strongly typed structures.
    #[must_use]
    pub const fn supported_version_bounds() -> (ProtocolVersion, ProtocolVersion) {
        (Self::OLDEST, Self::NEWEST)
    }

    /// Returns the inclusive range of supported protocol versions using strongly typed values.
    ///
    /// The range mirrors [`ProtocolVersion::supported_range`] but yields
    /// [`ProtocolVersion`] instances so callers can iterate without converting between the raw
    /// byte representation and the wrapper type. This is particularly useful when constructing
    /// lookup tables keyed by [`ProtocolVersion`] or when rendering diagnostics that already work
    /// with the strongly typed representation.
    #[must_use]
    pub fn supported_version_range() -> RangeInclusive<ProtocolVersion> {
        Self::OLDEST..=Self::NEWEST
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
    pub fn supported_versions_iter() -> IntoIter<ProtocolVersion, { SUPPORTED_PROTOCOL_COUNT }> {
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

    /// Returns the zero-based offset from [`ProtocolVersion::OLDEST`] when iterating
    /// protocol versions in ascending order.
    ///
    /// Upstream rsync often indexes lookup tables by subtracting the oldest supported
    /// protocol value from the negotiated byte. Exposing the same arithmetic keeps the
    /// Rust implementation in sync while avoiding ad-hoc calculations (and the risk of
    /// off-by-one mistakes) in higher layers. The helper can be used in const contexts,
    /// making it suitable for compile-time table initialisation.
    #[must_use]
    #[inline]
    pub const fn offset_from_oldest(self) -> usize {
        (self.as_u8() - Self::OLDEST.as_u8()) as usize
    }

    /// Returns the zero-based offset from [`ProtocolVersion::NEWEST`] when iterating
    /// protocol versions in the descending order used by [`SUPPORTED_PROTOCOLS`].
    ///
    /// This index matches the position of the version within [`SUPPORTED_PROTOCOLS`]
    /// and [`ProtocolVersion::SUPPORTED_VERSIONS`], allowing lookup tables keyed by the
    /// canonical newest-to-oldest order to perform constant-time indexing without
    /// recomputing differences at every call site.
    #[must_use]
    #[inline]
    pub const fn offset_from_newest(self) -> usize {
        (Self::NEWEST.as_u8() - self.as_u8()) as usize
    }

    /// Returns the next newer protocol version within the supported range, if any.
    ///
    /// Upstream rsync frequently iterates across protocol numbers while
    /// comparing capabilities. Providing a strongly typed successor keeps those
    /// loops ergonomic without forcing callers to perform manual bounds checks
    /// or convert the version back into a raw integer. When the current value
    /// already equals [`ProtocolVersion::NEWEST`], the method yields `None` to
    /// mirror the behavior of reaching the end of the range.
    #[must_use]
    pub const fn next_newer(self) -> Option<Self> {
        if self.as_u8() >= Self::NEWEST.as_u8() {
            None
        } else {
            Some(Self::new_const(self.as_u8() + 1))
        }
    }

    /// Returns the next older protocol version within the supported range, if any.
    ///
    /// The helper mirrors [`ProtocolVersion::next_newer`] but walks towards the
    /// lower bound. Callers that need to inspect predecessor versions can rely
    /// on the function to remain inside the negotiated span without manually
    /// checking for underflow. When invoked on [`ProtocolVersion::OLDEST`] the
    /// method returns `None`, signalling that there is no older supported
    /// protocol.
    #[must_use]
    pub const fn next_older(self) -> Option<Self> {
        if self.as_u8() <= Self::OLDEST.as_u8() {
            None
        } else {
            Some(Self::new_const(self.as_u8() - 1))
        }
    }

    /// Returns the raw numeric value represented by this version.
    #[must_use]
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self.0.get()
    }

    /// Returns the non-zero byte representation used in protocol negotiation.
    ///
    /// Upstream rsync frequently stores negotiated protocol versions in
    /// `uint8` fields that rely on the non-zero invariant to distinguish
    /// between "no version negotiated" (`0`) and a real protocol value. The
    /// Rust implementation mirrors that convention by wrapping the negotiated
    /// byte in [`core::num::NonZeroU8`]. Exposing the pre-validated wrapper lets
    /// higher layers avoid reconstructing it manually when interacting with
    /// caches, lookup tables, or serialization helpers that expect the invariant
    /// to hold.
    #[must_use]
    #[inline]
    pub const fn as_non_zero(self) -> NonZeroU8 {
        self.0
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
        version.as_non_zero()
    }
}

macro_rules! impl_from_protocol_version_for_unsigned {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty as From<u8>>::from(version.as_u8())
                }
            }
        )+
    };
}

impl_from_protocol_version_for_unsigned!(u16, u32, u64, u128, usize);

impl From<ProtocolVersion> for i8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        i8::try_from(version.as_u8()).expect("protocol versions fit within i8")
    }
}

macro_rules! impl_from_protocol_version_for_signed {
    ($($ty:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty as From<u8>>::from(version.as_u8())
                }
            }
        )+
    };
}

impl_from_protocol_version_for_signed!(i16, i32, i64, i128, isize);

macro_rules! impl_from_protocol_version_for_nonzero_unsigned {
    ($($ty:ty => $base:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty>::new(<$base as From<u8>>::from(version.as_u8()))
                        .expect("protocol versions are always non-zero")
                }
            }
        )+
    };
}

impl_from_protocol_version_for_nonzero_unsigned!(
    NonZeroU16 => u16,
    NonZeroU32 => u32,
    NonZeroU64 => u64,
    NonZeroU128 => u128,
    NonZeroUsize => usize,
);

impl From<ProtocolVersion> for NonZeroI8 {
    #[inline]
    fn from(version: ProtocolVersion) -> Self {
        NonZeroI8::new(i8::try_from(version.as_u8()).expect("protocol versions fit within i8"))
            .expect("protocol versions are always non-zero")
    }
}

macro_rules! impl_from_protocol_version_for_nonzero_signed {
    ($($ty:ty => $base:ty),+ $(,)?) => {
        $(
            impl From<ProtocolVersion> for $ty {
                #[inline]
                fn from(version: ProtocolVersion) -> Self {
                    <$ty>::new(<$base as From<u8>>::from(version.as_u8()))
                        .expect("protocol versions are always non-zero")
                }
            }
        )+
    };
}

impl_from_protocol_version_for_nonzero_signed!(
    NonZeroI16 => i16,
    NonZeroI32 => i32,
    NonZeroI64 => i64,
    NonZeroI128 => i128,
    NonZeroIsize => isize,
);

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

impl FromStr for ProtocolVersion {
    type Err = ParseProtocolVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim_matches(|c: char| c.is_ascii_whitespace());
        if trimmed.is_empty() {
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::Empty,
            ));
        }

        let mut digits = trimmed;
        if let Some(rest) = digits.strip_prefix('+') {
            digits = rest;
        }

        if let Some(rest) = digits.strip_prefix('-') {
            if rest.chars().all(|ch| ch.is_ascii_digit()) {
                return Err(ParseProtocolVersionError::new(
                    ParseProtocolVersionErrorKind::Negative,
                ));
            }
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::InvalidDigit,
            ));
        }

        if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::InvalidDigit,
            ));
        }

        let value: u16 = digits
            .parse()
            .map_err(|_| ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::Overflow))?;

        if value > u16::from(u8::MAX) {
            return Err(ParseProtocolVersionError::new(
                ParseProtocolVersionErrorKind::Overflow,
            ));
        }

        let byte = value as u8;
        ProtocolVersion::from_supported(byte).ok_or_else(|| {
            ParseProtocolVersionError::new(ParseProtocolVersionErrorKind::UnsupportedRange(byte))
        })
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

impl PartialEq<NonZeroU8> for ProtocolVersion {
    fn eq(&self, other: &NonZeroU8) -> bool {
        self.as_non_zero() == *other
    }
}

impl PartialEq<ProtocolVersion> for NonZeroU8 {
    fn eq(&self, other: &ProtocolVersion) -> bool {
        *self == other.as_non_zero()
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
///
/// # Examples
///
/// ```
/// use rsync_protocol::{select_highest_mutual, ProtocolVersion};
///
/// let negotiated = select_highest_mutual([
///     ProtocolVersion::NEWEST,
///     ProtocolVersion::from_supported(31).expect("31 is within the supported range"),
/// ])
/// .expect("newest protocol should be accepted");
/// assert_eq!(negotiated, ProtocolVersion::NEWEST);
///
/// let err = select_highest_mutual([27u8]).unwrap_err();
/// assert!(matches!(err, rsync_protocol::NegotiationError::UnsupportedVersion(27)));
/// ```
#[must_use = "the negotiation outcome must be checked"]
pub fn select_highest_mutual<I, T>(peer_versions: I) -> Result<ProtocolVersion, NegotiationError>
where
    I: IntoIterator<Item = T>,
    T: ProtocolVersionAdvertisement,
{
    let mut seen = [false; ProtocolVersion::NEWEST.as_u8() as usize + 1];
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
