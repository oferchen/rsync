//! # Overview
//!
//! Compatibility flags extend the rsync protocol by advertising optional
//! capabilities once both peers have agreed on a protocol version. Upstream
//! exchanges these flags using the variable-length integer codec defined in
//! [`crate::varint`]. This module mirrors that behaviour and exposes a typed
//! bitfield so higher layers can reason about individual compatibility bits
//! without manipulating integers directly.
//!
//! # Design
//!
//! [`CompatibilityFlags`] wraps a `u32` and provides associated constants for
//! every flag currently defined by rsync 3.4.1. The bitfield implements the
//! standard bit-operator traits (`BitOr`, `BitAnd`, `BitXor`) to keep usage
//! ergonomic, and reuses the varint codec for serialization. The module also
//! exposes [`KnownCompatibilityFlag`], an enumeration of the upstream flag
//! definitions, together with [`CompatibilityFlags::iter_known`] so higher
//! layers can iterate over individual capabilities without hand-rolling bit
//! scans.
//!
//! # Examples
//!
//! Encode and decode a set of compatibility flags from memory:
//!
//! ```
//! use rsync_protocol::{CompatibilityFlags, KnownCompatibilityFlag};
//!
//! let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SYMLINK_TIMES;
//! let mut bytes = Vec::new();
//! flags.encode_to_vec(&mut bytes).unwrap();
//! let (decoded, remainder) = CompatibilityFlags::decode_from_slice(&bytes).unwrap();
//! assert_eq!(decoded, flags);
//! assert!(remainder.is_empty());
//! ```
//!
//! Collect compatibility flags from an iterator of known flag variants:
//!
//! ```
//! use rsync_protocol::{CompatibilityFlags, KnownCompatibilityFlag};
//!
//! let flags: CompatibilityFlags = [
//!     KnownCompatibilityFlag::IncRecurse,
//!     KnownCompatibilityFlag::SafeFileList,
//! ]
//! .into_iter()
//! .collect();
//!
//! assert!(flags.contains(CompatibilityFlags::INC_RECURSE));
//! assert!(flags.contains(CompatibilityFlags::SAFE_FILE_LIST));
//! ```
//!
//! Iterate over the known compatibility flags set within a bitfield using the
//! standard iterator protocol:
//!
//! ```
//! use rsync_protocol::{CompatibilityFlags, KnownCompatibilityFlag};
//!
//! let flags = CompatibilityFlags::INC_RECURSE
//!     | CompatibilityFlags::CHECKSUM_SEED_FIX
//!     | CompatibilityFlags::VARINT_FLIST_FLAGS;
//!
//! let collected: Vec<_> = flags.into_iter().collect();
//! assert_eq!(
//!     collected,
//!     vec![
//!         KnownCompatibilityFlag::IncRecurse,
//!         KnownCompatibilityFlag::ChecksumSeedFix,
//!         KnownCompatibilityFlag::VarintFlistFlags,
//!     ]
//! );
//! ```
//!
//! # See also
//!
//! - [`crate::varint`] for the encoding and decoding primitives used by the
//!   bitfield.

use crate::varint::{decode_varint, encode_varint_to_vec, read_varint, write_varint};
use std::fmt;
use std::io::{self, Read, Write};
use std::iter::{Extend, FromIterator, FusedIterator};
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

/// Enumerates the compatibility flags defined by upstream rsync 3.4.1.
///
/// The variants mirror the canonical `CF_*` identifiers from the C
/// implementation. They serve as a strongly-typed view that avoids leaking raw
/// bit positions into higher layers while still supporting inexpensive
/// conversions back into [`CompatibilityFlags`]. The iterator returned by
/// [`CompatibilityFlags::iter_known`] yields values in ascending bit order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum KnownCompatibilityFlag {
    /// Sender and receiver support incremental recursion (`CF_INC_RECURSE`).
    #[doc(alias = "CF_INC_RECURSE")]
    IncRecurse,
    /// Symlink timestamps can be preserved (`CF_SYMLINK_TIMES`).
    #[doc(alias = "CF_SYMLINK_TIMES")]
    SymlinkTimes,
    /// Symlink payload requires iconv translation (`CF_SYMLINK_ICONV`).
    #[doc(alias = "CF_SYMLINK_ICONV")]
    SymlinkIconv,
    /// Receiver requests the "safe" file list (`CF_SAFE_FLIST`).
    #[doc(alias = "CF_SAFE_FLIST")]
    SafeFileList,
    /// Receiver cannot use the xattr optimization (`CF_AVOID_XATTR_OPTIM`).
    #[doc(alias = "CF_AVOID_XATTR_OPTIM")]
    AvoidXattrOptimization,
    /// Checksum seed handling follows the fixed ordering (`CF_CHKSUM_SEED_FIX`).
    #[doc(alias = "CF_CHKSUM_SEED_FIX")]
    ChecksumSeedFix,
    /// Partial directory should be used with `--inplace` (`CF_INPLACE_PARTIAL_DIR`).
    #[doc(alias = "CF_INPLACE_PARTIAL_DIR")]
    InplacePartialDir,
    /// File-list flags are encoded as varints (`CF_VARINT_FLIST_FLAGS`).
    #[doc(alias = "CF_VARINT_FLIST_FLAGS")]
    VarintFlistFlags,
    /// File-list entries support id0 names (`CF_ID0_NAMES`).
    #[doc(alias = "CF_ID0_NAMES")]
    Id0Names,
}

impl KnownCompatibilityFlag {
    /// Returns the [`CompatibilityFlags`] bit corresponding to the enum variant.
    #[must_use]
    pub const fn as_flag(self) -> CompatibilityFlags {
        match self {
            Self::IncRecurse => CompatibilityFlags::INC_RECURSE,
            Self::SymlinkTimes => CompatibilityFlags::SYMLINK_TIMES,
            Self::SymlinkIconv => CompatibilityFlags::SYMLINK_ICONV,
            Self::SafeFileList => CompatibilityFlags::SAFE_FILE_LIST,
            Self::AvoidXattrOptimization => CompatibilityFlags::AVOID_XATTR_OPTIMIZATION,
            Self::ChecksumSeedFix => CompatibilityFlags::CHECKSUM_SEED_FIX,
            Self::InplacePartialDir => CompatibilityFlags::INPLACE_PARTIAL_DIR,
            Self::VarintFlistFlags => CompatibilityFlags::VARINT_FLIST_FLAGS,
            Self::Id0Names => CompatibilityFlags::ID0_NAMES,
        }
    }

    /// Returns the canonical upstream identifier for the compatibility flag.
    ///
    /// The returned string mirrors the `CF_*` constant names used by the C
    /// implementation. Keeping the mapping centralised avoids repeating switch
    /// statements across the workspace when rendering diagnostics that need to
    /// match upstream wording.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::IncRecurse => "CF_INC_RECURSE",
            Self::SymlinkTimes => "CF_SYMLINK_TIMES",
            Self::SymlinkIconv => "CF_SYMLINK_ICONV",
            Self::SafeFileList => "CF_SAFE_FLIST",
            Self::AvoidXattrOptimization => "CF_AVOID_XATTR_OPTIM",
            Self::ChecksumSeedFix => "CF_CHKSUM_SEED_FIX",
            Self::InplacePartialDir => "CF_INPLACE_PARTIAL_DIR",
            Self::VarintFlistFlags => "CF_VARINT_FLIST_FLAGS",
            Self::Id0Names => "CF_ID0_NAMES",
        }
    }

    const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            _ if bits == CompatibilityFlags::INC_RECURSE.bits => Some(Self::IncRecurse),
            _ if bits == CompatibilityFlags::SYMLINK_TIMES.bits => Some(Self::SymlinkTimes),
            _ if bits == CompatibilityFlags::SYMLINK_ICONV.bits => Some(Self::SymlinkIconv),
            _ if bits == CompatibilityFlags::SAFE_FILE_LIST.bits => Some(Self::SafeFileList),
            _ if bits == CompatibilityFlags::AVOID_XATTR_OPTIMIZATION.bits => {
                Some(Self::AvoidXattrOptimization)
            }
            _ if bits == CompatibilityFlags::CHECKSUM_SEED_FIX.bits => Some(Self::ChecksumSeedFix),
            _ if bits == CompatibilityFlags::INPLACE_PARTIAL_DIR.bits => {
                Some(Self::InplacePartialDir)
            }
            _ if bits == CompatibilityFlags::VARINT_FLIST_FLAGS.bits => {
                Some(Self::VarintFlistFlags)
            }
            _ if bits == CompatibilityFlags::ID0_NAMES.bits => Some(Self::Id0Names),
            _ => None,
        }
    }
}

impl From<KnownCompatibilityFlag> for CompatibilityFlags {
    fn from(flag: KnownCompatibilityFlag) -> Self {
        flag.as_flag()
    }
}

impl fmt::Display for KnownCompatibilityFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Iterator over the known compatibility flags set within a [`CompatibilityFlags`] value.
///
/// [`Iterator::next`] yields flags in ascending bit order (lowest bit first) so
/// callers observe the same ordering exposed by [`CompatibilityFlags::iter_known`].
/// The iterator also implements [`DoubleEndedIterator`], allowing reverse
/// traversal via [`Iterator::next_back`] without allocating intermediate
/// collections.
#[derive(Clone, Debug)]
pub struct KnownCompatibilityFlagsIter {
    remaining: u32,
}

impl KnownCompatibilityFlagsIter {
    const fn new(flags: CompatibilityFlags) -> Self {
        Self {
            remaining: flags.bits & CompatibilityFlags::KNOWN_MASK,
        }
    }
}

impl Iterator for KnownCompatibilityFlagsIter {
    type Item = KnownCompatibilityFlag;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let bit_index = self.remaining.trailing_zeros();
        let bit_mask = 1u32 << bit_index;
        self.remaining &= !bit_mask;
        KnownCompatibilityFlag::from_bits(bit_mask)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.remaining.count_ones() as usize;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for KnownCompatibilityFlagsIter {
    fn len(&self) -> usize {
        self.remaining.count_ones() as usize
    }
}

impl FusedIterator for KnownCompatibilityFlagsIter {}

impl DoubleEndedIterator for KnownCompatibilityFlagsIter {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let bit_index = u32::BITS - 1 - self.remaining.leading_zeros();
        let bit_mask = 1u32 << bit_index;
        self.remaining &= !bit_mask;
        KnownCompatibilityFlag::from_bits(bit_mask)
    }
}

/// Bitfield that encodes rsync compatibility flags exchanged after protocol negotiation.
///
/// Upstream rsync uses the compatibility flag exchange to signal optional
/// behaviours that depend on both peers supporting protocol features introduced
/// after version 30. The flags are transmitted using the variable-length
/// integer codec implemented in [`crate::varint`], making the bitfield compact
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

    const KNOWN_MASK: u32 = Self::INC_RECURSE.bits
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

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(flags: CompatibilityFlags) -> Vec<u8> {
        let mut out = Vec::new();
        flags.encode_to_vec(&mut out).expect("encoding succeeds");
        out
    }

    #[test]
    fn bit_constants_match_expected_values() {
        assert_eq!(CompatibilityFlags::INC_RECURSE.bits(), 1);
        assert_eq!(CompatibilityFlags::SYMLINK_TIMES.bits(), 1 << 1);
        assert_eq!(CompatibilityFlags::SYMLINK_ICONV.bits(), 1 << 2);
        assert_eq!(CompatibilityFlags::SAFE_FILE_LIST.bits(), 1 << 3);
        assert_eq!(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION.bits(), 1 << 4);
        assert_eq!(CompatibilityFlags::CHECKSUM_SEED_FIX.bits(), 1 << 5);
        assert_eq!(CompatibilityFlags::INPLACE_PARTIAL_DIR.bits(), 1 << 6);
        assert_eq!(CompatibilityFlags::VARINT_FLIST_FLAGS.bits(), 1 << 7);
        assert_eq!(CompatibilityFlags::ID0_NAMES.bits(), 1 << 8);
    }

    #[test]
    fn encode_and_decode_round_trip_known_sets() {
        let sets = [
            CompatibilityFlags::EMPTY,
            CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SYMLINK_TIMES,
            CompatibilityFlags::SAFE_FILE_LIST
                | CompatibilityFlags::CHECKSUM_SEED_FIX
                | CompatibilityFlags::VARINT_FLIST_FLAGS,
            CompatibilityFlags::from_bits(1 << 30),
        ];

        for flags in sets {
            let encoded = encode(flags);
            let (decoded, remainder) =
                CompatibilityFlags::decode_from_slice(&encoded).expect("decoding succeeds");
            assert_eq!(decoded, flags);
            assert!(remainder.is_empty());
        }
    }

    #[test]
    fn iter_known_yields_flags_in_bit_order() {
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::CHECKSUM_SEED_FIX;

        let collected: Vec<_> = flags.iter_known().collect();
        assert_eq!(
            collected,
            vec![
                KnownCompatibilityFlag::IncRecurse,
                KnownCompatibilityFlag::ChecksumSeedFix,
                KnownCompatibilityFlag::VarintFlistFlags,
            ]
        );

        let mut iter = flags.iter_known();
        assert_eq!(iter.size_hint(), (3, Some(3)));
        assert_eq!(iter.len(), 3);
        assert_eq!(iter.next(), Some(KnownCompatibilityFlag::IncRecurse));
        assert_eq!(iter.size_hint(), (2, Some(2)));
        assert_eq!(iter.len(), 2);
    }

    #[test]
    fn iter_known_skips_unknown_bits() {
        let flags = CompatibilityFlags::from_bits(1 << 15)
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::ID0_NAMES;

        let collected: Vec<_> = flags.iter_known().collect();
        assert_eq!(
            collected,
            vec![
                KnownCompatibilityFlag::SafeFileList,
                KnownCompatibilityFlag::Id0Names,
            ]
        );
    }

    #[test]
    fn iter_known_supports_double_ended_iteration() {
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::ID0_NAMES;

        let mut iter = flags.iter_known();
        assert_eq!(iter.len(), 3);
        assert_eq!(iter.next_back(), Some(KnownCompatibilityFlag::Id0Names));
        assert_eq!(iter.len(), 2);
        assert_eq!(iter.next(), Some(KnownCompatibilityFlag::IncRecurse));
        assert_eq!(iter.len(), 1);
        assert_eq!(iter.next_back(), Some(KnownCompatibilityFlag::SafeFileList));
        assert_eq!(iter.len(), 0);
        assert_eq!(iter.next(), None);
        assert_eq!(iter.next_back(), None);
    }

    #[test]
    fn iter_known_rev_collects_descending_order() {
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS
            | CompatibilityFlags::ID0_NAMES;

        let collected: Vec<_> = flags.iter_known().rev().collect();
        assert_eq!(
            collected,
            vec![
                KnownCompatibilityFlag::Id0Names,
                KnownCompatibilityFlag::VarintFlistFlags,
                KnownCompatibilityFlag::ChecksumSeedFix,
                KnownCompatibilityFlag::IncRecurse,
            ]
        );
    }

    #[test]
    fn collecting_known_flags_produces_expected_bitfield() {
        let flags: CompatibilityFlags = [
            KnownCompatibilityFlag::IncRecurse,
            KnownCompatibilityFlag::ChecksumSeedFix,
            KnownCompatibilityFlag::IncRecurse,
        ]
        .into_iter()
        .collect();

        assert_eq!(
            flags,
            CompatibilityFlags::INC_RECURSE | CompatibilityFlags::CHECKSUM_SEED_FIX
        );
    }

    #[test]
    fn extend_adds_flags_without_clearing_existing_bits() {
        let mut flags = CompatibilityFlags::INC_RECURSE;
        flags.extend([
            KnownCompatibilityFlag::SafeFileList,
            KnownCompatibilityFlag::SafeFileList,
            KnownCompatibilityFlag::Id0Names,
        ]);

        assert!(flags.contains(CompatibilityFlags::INC_RECURSE));
        assert!(flags.contains(CompatibilityFlags::SAFE_FILE_LIST));
        assert!(flags.contains(CompatibilityFlags::ID0_NAMES));
        assert_eq!(flags.unknown_bits(), 0);
    }

    #[test]
    fn into_iter_collects_known_flags() {
        let flags = CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::VARINT_FLIST_FLAGS;

        let collected: Vec<_> = flags.into_iter().collect();
        assert_eq!(
            collected,
            vec![
                KnownCompatibilityFlag::IncRecurse,
                KnownCompatibilityFlag::ChecksumSeedFix,
                KnownCompatibilityFlag::VarintFlistFlags,
            ]
        );
    }

    #[test]
    fn into_iter_for_references_matches_owned_iteration() {
        let flags = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;

        let owned: Vec<_> = flags.into_iter().collect();
        let shared: Vec<_> = (&flags).into_iter().collect();
        let mut clone = flags;
        let mut shared_mut: Vec<_> = (&mut clone).into_iter().collect();

        assert_eq!(owned, shared);
        assert_eq!(shared, shared_mut);
        shared_mut.reverse();
        assert_eq!(
            shared_mut,
            vec![
                KnownCompatibilityFlag::SafeFileList,
                KnownCompatibilityFlag::IncRecurse
            ]
        );
    }

    #[test]
    fn read_from_preserves_high_bit_flags() {
        let flags = CompatibilityFlags::from_bits(0x8000_0001);
        let mut encoded = Vec::new();
        flags
            .encode_to_vec(&mut encoded)
            .expect("encoding succeeds");

        let mut cursor = io::Cursor::new(&encoded[..]);
        let decoded = CompatibilityFlags::read_from(&mut cursor)
            .expect("two's-complement values must round-trip");
        assert_eq!(decoded.bits(), flags.bits());

        let (from_slice, remainder) =
            CompatibilityFlags::decode_from_slice(&encoded).expect("slice decoding succeeds");
        assert_eq!(from_slice.bits(), flags.bits());
        assert!(remainder.is_empty());
    }

    #[test]
    fn unknown_bits_reports_future_flags() {
        let flags = CompatibilityFlags::from_bits(0x1FF | (1 << 12));
        assert_eq!(flags.unknown_bits(), 1 << 12);
    }

    #[test]
    fn bitwise_operators_behave_like_bitfields() {
        let mut flags = CompatibilityFlags::INC_RECURSE;
        flags |= CompatibilityFlags::SYMLINK_TIMES;
        assert!(flags.contains(CompatibilityFlags::SYMLINK_TIMES));

        flags &= CompatibilityFlags::SYMLINK_TIMES;
        assert_eq!(flags, CompatibilityFlags::SYMLINK_TIMES);

        flags ^= CompatibilityFlags::SYMLINK_TIMES;
        assert!(flags.is_empty());

        flags |= CompatibilityFlags::SYMLINK_ICONV;
        assert!(flags.contains(CompatibilityFlags::SYMLINK_ICONV));
        assert!(!flags.contains(CompatibilityFlags::SYMLINK_TIMES));
    }

    #[test]
    fn known_flag_name_matches_display_output() {
        for flag in [
            KnownCompatibilityFlag::IncRecurse,
            KnownCompatibilityFlag::SymlinkTimes,
            KnownCompatibilityFlag::SymlinkIconv,
            KnownCompatibilityFlag::SafeFileList,
            KnownCompatibilityFlag::AvoidXattrOptimization,
            KnownCompatibilityFlag::ChecksumSeedFix,
            KnownCompatibilityFlag::InplacePartialDir,
            KnownCompatibilityFlag::VarintFlistFlags,
            KnownCompatibilityFlag::Id0Names,
        ] {
            assert_eq!(flag.name(), flag.to_string());
        }
    }

    #[test]
    fn compatibility_flags_display_lists_known_and_unknown_bits() {
        let known = CompatibilityFlags::INC_RECURSE | CompatibilityFlags::SAFE_FILE_LIST;
        assert_eq!(known.to_string(), "CF_INC_RECURSE | CF_SAFE_FLIST");

        let unknown_only = CompatibilityFlags::from_bits(1 << 12);
        assert_eq!(unknown_only.to_string(), "unknown(0x1000)");

        let mixed = known | CompatibilityFlags::from_bits(1 << 20);
        assert_eq!(
            mixed.to_string(),
            "CF_INC_RECURSE | CF_SAFE_FLIST | unknown(0x100000)"
        );

        assert_eq!(CompatibilityFlags::EMPTY.to_string(), "CF_NONE");
    }
}
