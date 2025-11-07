use crate::varint::{decode_varint, encode_varint_to_vec, read_varint, write_varint};
use std::fmt;
use std::io::{self, Read, Write};
use std::iter::{Extend, FromIterator};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

use super::iter::KnownCompatibilityFlagsIter;
use super::known::KnownCompatibilityFlag;

/// Bitfield that encodes rsync compatibility flags exchanged after protocol negotiation.
///
/// Upstream rsync uses the compatibility flag exchange to signal optional
/// behaviours that depend on both peers supporting protocol features introduced
/// after version 30. The flags are transmitted using the variable-length
/// integer codec implemented in the `varint` module, making the bitfield compact
/// while retaining forward compatibility when new bits are defined.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash, Default)]
pub struct CompatibilityFlags {
    bits: u32,
}

impl CompatibilityFlags {
    const fn new(bits: u32) -> Self {
        Self { bits }
    }

    /// No compatibility flags.
    pub const EMPTY: Self = Self::new(0);
    /// Sender and receiver support incremental recursion (`CF_INC_RECURSE`).
    pub const INC_RECURSE: Self = Self::new(1 << 0);
    /// Symlink timestamps can be preserved (`CF_SYMLINK_TIMES`).
    pub const SYMLINK_TIMES: Self = Self::new(1 << 1);
    /// Symlink payload requires iconv translation (`CF_SYMLINK_ICONV`).
    pub const SYMLINK_ICONV: Self = Self::new(1 << 2);
    /// Receiver requests the "safe" file list (`CF_SAFE_FLIST`).
    pub const SAFE_FILE_LIST: Self = Self::new(1 << 3);
    /// Receiver cannot use the xattr optimization (`CF_AVOID_XATTR_OPTIM`).
    pub const AVOID_XATTR_OPTIMIZATION: Self = Self::new(1 << 4);
    /// Checksum seed handling follows the fixed ordering (`CF_CHKSUM_SEED_FIX`).
    pub const CHECKSUM_SEED_FIX: Self = Self::new(1 << 5);
    /// Partial directory should be used with `--inplace` (`CF_INPLACE_PARTIAL_DIR`).
    pub const INPLACE_PARTIAL_DIR: Self = Self::new(1 << 6);
    /// File-list flags are encoded as varints (`CF_VARINT_FLIST_FLAGS`).
    pub const VARINT_FLIST_FLAGS: Self = Self::new(1 << 7);
    /// File-list entries support id0 names (`CF_ID0_NAMES`).
    pub const ID0_NAMES: Self = Self::new(1 << 8);

    /// Bitfield containing every compatibility flag recognised by this crate.
    pub const ALL_KNOWN: Self = Self::new(Self::KNOWN_MASK);

    pub(super) const KNOWN_MASK: u32 = Self::INC_RECURSE.bits
        | Self::SYMLINK_TIMES.bits
        | Self::SYMLINK_ICONV.bits
        | Self::SAFE_FILE_LIST.bits
        | Self::AVOID_XATTR_OPTIMIZATION.bits
        | Self::CHECKSUM_SEED_FIX.bits
        | Self::INPLACE_PARTIAL_DIR.bits
        | Self::VARINT_FLIST_FLAGS.bits
        | Self::ID0_NAMES.bits;

    /// Returns a bitfield constructed from the raw `bits` without masking.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self { bits }
    }

    /// Returns the raw bit representation of the compatibility flags.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.bits
    }

    /// Returns `true` when no compatibility flags are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Reports the subset of bits that are not yet defined in this crate.
    #[must_use]
    pub const fn unknown_bits(self) -> u32 {
        self.bits & !Self::KNOWN_MASK
    }

    /// Returns `true` when the bitfield contains flags unknown to this release.
    ///
    /// Upstream rsync tolerates future compatibility bits by leaving them set
    /// while ignoring their semantics. Higher layers in the Rust implementation
    /// often need to detect that situation so they can log downgraded
    /// diagnostics or gate behaviour that depends on mutually understood
    /// capabilities. This helper mirrors the upstream check performed after the
    /// bitfield is received over the wire by simply testing whether any
    /// off-range bits are present.
    #[must_use]
    pub const fn has_unknown_bits(self) -> bool {
        self.unknown_bits() != 0
    }

    /// Returns a new bitfield with all unknown compatibility flags cleared.
    ///
    /// Older upstream daemons mask future bits when propagating their own flag
    /// set so peers without knowledge of a capability do not accidentally
    /// expose it further downstream. Reproducing that behaviour keeps the Rust
    /// implementation compatible with mixed-version deployments while still
    /// allowing callers to retain the original value when they need to surface
    /// the full bit pattern in diagnostics.
    #[must_use]
    pub const fn without_unknown_bits(self) -> Self {
        Self::new(self.bits & Self::KNOWN_MASK)
    }

    /// Checks whether all flags in `other` are set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.bits & other.bits) == other.bits
    }

    /// Returns a new bitfield containing the union of both operands.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self::new(self.bits | other.bits)
    }

    /// Returns a new bitfield containing only the bits common to both operands.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self::new(self.bits & other.bits)
    }

    /// Returns a new bitfield containing the bits present in `self` but not in `other`.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self::new(self.bits & !other.bits)
    }

    /// Returns an iterator over the known compatibility flags set in the bitfield.
    ///
    /// The iterator yields [`KnownCompatibilityFlag`] values in ascending bit
    /// order. Unknown bits that are outside the upstream-defined set are
    /// skipped, mirroring rsync's behaviour where future flags are tolerated but
    /// ignored by older implementations.
    #[must_use]
    pub fn iter_known(self) -> KnownCompatibilityFlagsIter {
        KnownCompatibilityFlagsIter::new(self)
    }

    /// Encodes the bitfield using rsync's variable-length integer format and writes it to `writer`.
    ///
    /// # Errors
    ///
    /// Propagates any error produced by the underlying writer. The encoding mirrors upstream rsync
    /// by reinterpreting the raw bitfield as a signed 32-bit integer, ensuring bits that extend into
    /// the sign position are preserved.
    pub fn write_to<W: Write>(self, writer: &mut W) -> io::Result<()> {
        write_varint(writer, self.bits as i32)
    }

    /// Appends the encoded flags to `out` using the varint wire format.
    pub fn encode_to_vec(self, out: &mut Vec<u8>) -> io::Result<()> {
        encode_varint_to_vec(self.bits as i32, out);
        Ok(())
    }

    /// Decodes a compatibility flag bitfield from `reader`.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors reported by [`read_varint`]. The helper mirrors upstream rsync by
    /// reinterpreting negative `i32` values using two's-complement semantics so future
    /// compatibility bits that extend into the sign position are preserved.
    pub fn read_from<R: Read>(reader: &mut R) -> io::Result<Self> {
        let value = read_varint(reader)?;
        Ok(Self::new(value as u32))
    }

    /// Decodes a compatibility flag bitfield from the beginning of `bytes` and returns the remaining slice.
    ///
    /// Negative integers are interpreted using two's-complement semantics so that encoded bitfields
    /// with sign-bit extensions round-trip exactly like the upstream C implementation.
    pub fn decode_from_slice(bytes: &[u8]) -> io::Result<(Self, &[u8])> {
        let (value, remainder) = decode_varint(bytes)?;
        Ok((Self::new(value as u32), remainder))
    }

    /// Decodes a compatibility flag bitfield from `bytes`, advancing the slice on success.
    ///
    /// The helper mirrors [`decode_from_slice`](Self::decode_from_slice) but updates `bytes` to point to
    /// the remainder when decoding succeeds. Callers that process wire payloads sequentially can therefore
    /// advance their input cursor without manually threading the remainder through intermediate variables.
    /// If decoding fails the original slice is left untouched so transports can retry or surface the error
    /// while still pointing at the offending data.
    ///
    /// # Examples
    ///
    /// ```
    /// use oc_rsync_protocol::CompatibilityFlags;
    ///
    /// let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
    /// let mut encoded = Vec::new();
    /// flags.encode_to_vec(&mut encoded).unwrap();
    /// encoded.extend_from_slice(&[0x7F]);
    /// let mut slice: &[u8] = &encoded;
    /// let decoded = CompatibilityFlags::decode_from_slice_mut(&mut slice).unwrap();
    ///
    /// assert_eq!(decoded, flags);
    /// assert_eq!(slice, &[0x7F]);
    /// ```
    pub fn decode_from_slice_mut(bytes: &mut &[u8]) -> io::Result<Self> {
        match Self::decode_from_slice(bytes) {
            Ok((flags, remainder)) => {
                *bytes = remainder;
                Ok(flags)
            }
            Err(err) => Err(err),
        }
    }
}

impl FromIterator<KnownCompatibilityFlag> for CompatibilityFlags {
    /// Builds a [`CompatibilityFlags`] value from an iterator of known flag variants.
    ///
    /// The implementation mirrors upstream rsync's practice of folding optional
    /// capabilities into a bitfield by OR-ing the corresponding bits. Duplicate
    /// flags are ignored because they do not affect the resulting bit mask.
    fn from_iter<I: IntoIterator<Item = KnownCompatibilityFlag>>(iter: I) -> Self {
        let mut bits = 0u32;
        for flag in iter {
            bits |= flag.as_flag().bits();
        }
        Self::from_bits(bits)
    }
}

impl Extend<KnownCompatibilityFlag> for CompatibilityFlags {
    /// Adds each known flag yielded by the iterator to the bitfield.
    fn extend<I: IntoIterator<Item = KnownCompatibilityFlag>>(&mut self, iter: I) {
        for flag in iter {
            *self |= flag.as_flag();
        }
    }
}

impl IntoIterator for CompatibilityFlags {
    type Item = KnownCompatibilityFlag;
    type IntoIter = KnownCompatibilityFlagsIter;

    fn into_iter(self) -> Self::IntoIter {
        self.iter_known()
    }
}

impl IntoIterator for &CompatibilityFlags {
    type Item = KnownCompatibilityFlag;
    type IntoIter = KnownCompatibilityFlagsIter;

    fn into_iter(self) -> Self::IntoIter {
        (*self).iter_known()
    }
}

impl IntoIterator for &mut CompatibilityFlags {
    type Item = KnownCompatibilityFlag;
    type IntoIter = KnownCompatibilityFlagsIter;

    fn into_iter(self) -> Self::IntoIter {
        (*self).iter_known()
    }
}

impl fmt::Debug for CompatibilityFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompatibilityFlags")
            .field("bits", &format_args!("0x{:x}", self.bits))
            .finish()
    }
}

impl fmt::Display for CompatibilityFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("CF_NONE");
        }

        let mut first = true;
        for flag in self.iter_known() {
            if !first {
                f.write_str(" | ")?;
            }
            first = false;
            fmt::Display::fmt(&flag, f)?;
        }

        let unknown = self.unknown_bits();
        if unknown != 0 {
            if !first {
                f.write_str(" | ")?;
            }
            write!(f, "unknown(0x{unknown:x})")?;
        }

        Ok(())
    }
}

impl Not for CompatibilityFlags {
    type Output = Self;

    fn not(self) -> Self::Output {
        Self::new(!self.bits)
    }
}

impl BitOr for CompatibilityFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl BitOrAssign for CompatibilityFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.bits |= rhs.bits;
    }
}

impl BitAnd for CompatibilityFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        self.intersection(rhs)
    }
}

impl BitAndAssign for CompatibilityFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.bits &= rhs.bits;
    }
}

impl BitXor for CompatibilityFlags {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self::Output {
        Self::new(self.bits ^ rhs.bits)
    }
}

impl BitXorAssign for CompatibilityFlags {
    fn bitxor_assign(&mut self, rhs: Self) {
        self.bits ^= rhs.bits;
    }
}

impl From<CompatibilityFlags> for u32 {
    fn from(flags: CompatibilityFlags) -> Self {
        flags.bits
    }
}

impl From<u32> for CompatibilityFlags {
    fn from(bits: u32) -> Self {
        Self::from_bits(bits)
    }
}

impl From<KnownCompatibilityFlag> for CompatibilityFlags {
    /// Converts a single known compatibility flag into the corresponding bitfield value.
    ///
    /// The helper mirrors [`KnownCompatibilityFlag::as_flag`] and allows
    /// callers to promote a variant into [`CompatibilityFlags`] without
    /// repeating the mapping logic or match statements.
    fn from(flag: KnownCompatibilityFlag) -> Self {
        flag.as_flag()
    }
}
