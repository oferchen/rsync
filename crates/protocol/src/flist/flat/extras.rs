//! Length-prefixed codec for the flat file-list extras tail.
//!
//! An [`ExtrasRef`] held in a [`FileEntryHeader`](super::FileEntryHeader) is a
//! 4-byte offset into a contiguous `Vec<u8>` blob arena. This module owns the
//! arena ([`ExtrasArena`]) and the encode/decode of the variable-length extras
//! record it stores: a self-describing, length-prefixed tail laid out exactly
//! as the design specifies (`docs/design/flat-flist-representation.md`,
//! "Offset-based indexing scheme" and RSS-A.5.f).
//!
//! The tail mirrors upstream's `union file_extras` model (upstream:
//! `rsync.h:786-792`, allocated in `flist.c:make_file()`), with one structural
//! difference: upstream derives which extras a node carries from *global*
//! config indices, whereas the flat store makes the selection *self-describing
//! per entry* with a 2-byte presence mask. The semantic field set is the same
//! (symlink target, rdev, hardlink data, ACL/xattr indices, checksum,
//! user/group names, atime/crtime/atime_nsec) - compare field-for-field
//! against the upstream `F_*` accessors cited in the design.
//!
//! The arena follows a build-then-freeze lifecycle - append records while
//! building, read them back by offset once frozen - and offsets are stable
//! for the life of the arena because tails are written once and never
//! mutated. [`FlatFileList`](super::FlatFileList) owns an `ExtrasArena`
//! and encodes extras through
//! [`push_with_extras`](super::FlatFileList::push_with_extras).

/// Presence bit: the [`FlatExtras::link_target`] field is present.
pub const EXTRA_LINK_TARGET: u16 = 1 << 0;
/// Presence bit: the [`FlatExtras::rdev_major`]/[`FlatExtras::rdev_minor`] pair
/// is present (block/char device numbers).
pub const EXTRA_RDEV: u16 = 1 << 1;
/// Presence bit: the [`FlatExtras::hardlink_idx`] field is present
/// (protocol 30+ hardlink group number).
pub const EXTRA_HARDLINK: u16 = 1 << 2;
/// Presence bit: the [`FlatExtras::acl_ndx`] field is present.
pub const EXTRA_ACL: u16 = 1 << 3;
/// Presence bit: the [`FlatExtras::def_acl_ndx`] field is present
/// (default ACL index, directories only).
pub const EXTRA_DEF_ACL: u16 = 1 << 4;
/// Presence bit: the [`FlatExtras::xattr_ndx`] field is present.
pub const EXTRA_XATTR: u16 = 1 << 5;
/// Presence bit: the [`FlatExtras::checksum`] field is present (--checksum).
pub const EXTRA_CHECKSUM: u16 = 1 << 6;
/// Presence bit: the [`FlatExtras::user_name`] field is present (protocol 30+).
pub const EXTRA_USER_NAME: u16 = 1 << 7;
/// Presence bit: the [`FlatExtras::group_name`] field is present (protocol 30+).
pub const EXTRA_GROUP_NAME: u16 = 1 << 8;
/// Presence bit: the [`FlatExtras::atime`] field is present (--atimes).
pub const EXTRA_ATIME: u16 = 1 << 9;
/// Presence bit: the [`FlatExtras::crtime`] field is present (--crtimes).
pub const EXTRA_CRTIME: u16 = 1 << 10;
/// Presence bit: the [`FlatExtras::atime_nsec`] field is present (protocol 32+).
pub const EXTRA_ATIME_NSEC: u16 = 1 << 11;

/// Upstream caps a strong checksum at 32 bytes (`MAX_DIGEST_LEN`); the tail
/// encodes checksum length in a single byte, which this bound stays within.
const MAX_CHECKSUM_LEN: usize = 32;

use super::ExtrasRef;

/// Decoded view of one packed extras tail.
///
/// Carries the optional metadata an [`ExtrasRef`] tail can hold. Field names
/// mirror the legacy `FileEntryExtras`
/// (`crates/protocol/src/flist/entry/extras.rs`) for parity. A field is
/// `Some`/non-zero only when its corresponding `EXTRA_*` presence bit was set
/// in the encoded record; absent fields decode to `None` (or `0` for the
/// always-`i64`/`u32` time fields, which are gated by their own presence bits).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlatExtras {
    /// Symlink target path bytes (for symlinks).
    pub link_target: Option<Vec<u8>>,
    /// Device major number (for block/char devices); paired with `rdev_minor`.
    pub rdev_major: Option<u32>,
    /// Device minor number (for block/char devices); paired with `rdev_major`.
    pub rdev_minor: Option<u32>,
    /// Hardlink group number (protocol 30+).
    pub hardlink_idx: Option<u32>,
    /// Access ACL index (--acls, protocol 30+).
    pub acl_ndx: Option<u32>,
    /// Default ACL index for directories (--acls).
    pub def_acl_ndx: Option<u32>,
    /// Extended attribute index (--xattrs).
    pub xattr_ndx: Option<u32>,
    /// File checksum bytes for --checksum mode (up to 32 bytes).
    pub checksum: Option<Vec<u8>>,
    /// User name for cross-system ownership mapping (protocol 30+).
    pub user_name: Option<Vec<u8>>,
    /// Group name for cross-system ownership mapping (protocol 30+).
    pub group_name: Option<Vec<u8>>,
    /// Access time, seconds since the Unix epoch (--atimes).
    pub atime: Option<i64>,
    /// Creation time, seconds since the Unix epoch (--crtimes).
    pub crtime: Option<i64>,
    /// Access time nanoseconds (protocol 32+, --atimes).
    pub atime_nsec: Option<u32>,
}

impl FlatExtras {
    /// Returns `true` when no optional field is present.
    ///
    /// An empty record needs no tail; [`ExtrasArena::append`] returns
    /// [`ExtrasRef::NO_EXTRAS`] for it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.presence_mask() == 0
    }

    /// Computes the 2-byte presence mask describing which fields are set.
    fn presence_mask(&self) -> u16 {
        let mut mask = 0u16;
        if self.link_target.is_some() {
            mask |= EXTRA_LINK_TARGET;
        }
        if self.rdev_major.is_some() && self.rdev_minor.is_some() {
            mask |= EXTRA_RDEV;
        }
        if self.hardlink_idx.is_some() {
            mask |= EXTRA_HARDLINK;
        }
        if self.acl_ndx.is_some() {
            mask |= EXTRA_ACL;
        }
        if self.def_acl_ndx.is_some() {
            mask |= EXTRA_DEF_ACL;
        }
        if self.xattr_ndx.is_some() {
            mask |= EXTRA_XATTR;
        }
        if self.checksum.is_some() {
            mask |= EXTRA_CHECKSUM;
        }
        if self.user_name.is_some() {
            mask |= EXTRA_USER_NAME;
        }
        if self.group_name.is_some() {
            mask |= EXTRA_GROUP_NAME;
        }
        if self.atime.is_some() {
            mask |= EXTRA_ATIME;
        }
        if self.crtime.is_some() {
            mask |= EXTRA_CRTIME;
        }
        if self.atime_nsec.is_some() {
            mask |= EXTRA_ATIME_NSEC;
        }
        mask
    }
}

/// Error returned when a tail cannot be decoded.
///
/// A well-formed arena built by [`ExtrasArena::append`] never produces these;
/// they guard against a malformed offset, a truncated tail, or an over-long
/// checksum, so the decoder fails loud rather than panicking on a bad slice.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtrasError {
    /// The [`ExtrasRef`] offset lies past the end of the arena.
    #[error("extras offset out of range")]
    OffsetOutOfRange,
    /// The tail ended before all fields the presence mask promised were read.
    #[error("extras tail truncated")]
    Truncated,
    /// A checksum field declared a length above the 32-byte maximum.
    #[error("extras checksum length exceeds maximum")]
    ChecksumTooLong,
}

/// Append-only blob arena for packed extras tails.
///
/// Records are encoded by [`append`](Self::append), which returns the
/// [`ExtrasRef`] offset where the tail begins, and read back by
/// [`decode`](Self::decode). Build-then-freeze: append while constructing the
/// file list, then treat the arena as immutable and resolve offsets on demand.
#[derive(Debug, Clone, Default)]
pub struct ExtrasArena {
    blobs: Vec<u8>,
}

impl ExtrasArena {
    /// Creates an empty arena.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of bytes currently held in the arena.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    /// Returns `true` when the arena holds no encoded bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }

    /// Encodes `extras` as a length-prefixed tail, appending it to the arena.
    ///
    /// Returns the [`ExtrasRef`] offset where the tail begins, or
    /// [`ExtrasRef::NO_EXTRAS`] when `extras` has no present field (the common
    /// case) - an empty record consumes no arena bytes. The encoding writes a
    /// 2-byte presence mask followed by the present fields in canonical order;
    /// see the module docs for the exact layout.
    ///
    /// Offsets never exceed `u32::MAX - 1`: the arena would have to hold ~4 GiB
    /// of tails to approach the [`ExtrasRef::NO_EXTRAS`] sentinel, far beyond
    /// any realistic file list.
    #[must_use]
    pub fn append(&mut self, extras: &FlatExtras) -> ExtrasRef {
        let mask = extras.presence_mask();
        if mask == 0 {
            return ExtrasRef::NO_EXTRAS;
        }

        let offset = u32::try_from(self.blobs.len()).expect("ExtrasArena exceeded 4 GiB");
        self.blobs.extend_from_slice(&mask.to_le_bytes());

        if let Some(target) = &extras.link_target {
            self.put_bytes_u16(target);
        }
        if mask & EXTRA_RDEV != 0 {
            // The mask bit is set only when `presence_mask` saw both halves of
            // the pair as `Some`, but treat divergent state as a soft failure
            // instead of a panic: emit zeros and debug_assert so a future
            // refactor that decouples the mask from the fields cannot crash a
            // long-running daemon. upstream: rsync.h:786-792 `union file_extras`
            // ties the two halves together by layout, which the type system
            // here does not yet enforce.
            debug_assert!(
                extras.rdev_major.is_some() && extras.rdev_minor.is_some(),
                "EXTRA_RDEV mask set without rdev_major/rdev_minor",
            );
            self.put_u32(extras.rdev_major.unwrap_or(0));
            self.put_u32(extras.rdev_minor.unwrap_or(0));
        }
        if let Some(idx) = extras.hardlink_idx {
            self.put_u32(idx);
        }
        if let Some(ndx) = extras.acl_ndx {
            self.put_u32(ndx);
        }
        if let Some(ndx) = extras.def_acl_ndx {
            self.put_u32(ndx);
        }
        if let Some(ndx) = extras.xattr_ndx {
            self.put_u32(ndx);
        }
        if let Some(sum) = &extras.checksum {
            // upstream caps a strong checksum at MAX_DIGEST_LEN (32); the tail
            // stores the length in one byte, so clamp defensively.
            let len = sum.len().min(MAX_CHECKSUM_LEN);
            self.blobs.push(len as u8);
            self.blobs.extend_from_slice(&sum[..len]);
        }
        if let Some(name) = &extras.user_name {
            self.put_bytes_u16(name);
        }
        if let Some(name) = &extras.group_name {
            self.put_bytes_u16(name);
        }
        if let Some(atime) = extras.atime {
            self.put_i64(atime);
        }
        if let Some(crtime) = extras.crtime {
            self.put_i64(crtime);
        }
        if let Some(nsec) = extras.atime_nsec {
            self.put_u32(nsec);
        }

        ExtrasRef(offset)
    }

    /// Decodes the tail at `reference`, reconstructing a [`FlatExtras`].
    ///
    /// Returns `Ok(None)` for [`ExtrasRef::NO_EXTRAS`] (no record). Otherwise
    /// reads the presence mask at the offset and walks the present fields in
    /// canonical order. Returns [`ExtrasError`] for a malformed offset or a
    /// truncated tail rather than panicking.
    pub fn decode(&self, reference: ExtrasRef) -> Result<Option<FlatExtras>, ExtrasError> {
        if reference == ExtrasRef::NO_EXTRAS {
            return Ok(None);
        }

        let mut cursor = Cursor {
            blob: &self.blobs,
            pos: reference.0 as usize,
        };
        if cursor.pos > self.blobs.len() {
            return Err(ExtrasError::OffsetOutOfRange);
        }

        let mask = cursor.read_u16()?;
        let mut extras = FlatExtras::default();

        if mask & EXTRA_LINK_TARGET != 0 {
            extras.link_target = Some(cursor.read_bytes_u16()?);
        }
        if mask & EXTRA_RDEV != 0 {
            extras.rdev_major = Some(cursor.read_u32()?);
            extras.rdev_minor = Some(cursor.read_u32()?);
        }
        if mask & EXTRA_HARDLINK != 0 {
            extras.hardlink_idx = Some(cursor.read_u32()?);
        }
        if mask & EXTRA_ACL != 0 {
            extras.acl_ndx = Some(cursor.read_u32()?);
        }
        if mask & EXTRA_DEF_ACL != 0 {
            extras.def_acl_ndx = Some(cursor.read_u32()?);
        }
        if mask & EXTRA_XATTR != 0 {
            extras.xattr_ndx = Some(cursor.read_u32()?);
        }
        if mask & EXTRA_CHECKSUM != 0 {
            let len = cursor.read_u8()? as usize;
            if len > MAX_CHECKSUM_LEN {
                return Err(ExtrasError::ChecksumTooLong);
            }
            extras.checksum = Some(cursor.read_n(len)?);
        }
        if mask & EXTRA_USER_NAME != 0 {
            extras.user_name = Some(cursor.read_bytes_u16()?);
        }
        if mask & EXTRA_GROUP_NAME != 0 {
            extras.group_name = Some(cursor.read_bytes_u16()?);
        }
        if mask & EXTRA_ATIME != 0 {
            extras.atime = Some(cursor.read_i64()?);
        }
        if mask & EXTRA_CRTIME != 0 {
            extras.crtime = Some(cursor.read_i64()?);
        }
        if mask & EXTRA_ATIME_NSEC != 0 {
            extras.atime_nsec = Some(cursor.read_u32()?);
        }

        Ok(Some(extras))
    }

    fn put_u32(&mut self, value: u32) {
        self.blobs.extend_from_slice(&value.to_le_bytes());
    }

    fn put_i64(&mut self, value: i64) {
        self.blobs.extend_from_slice(&value.to_le_bytes());
    }

    /// Writes a `u16` length prefix followed by the raw bytes.
    /// Inputs longer than `u16::MAX` bytes are clamped; in practice symlink
    /// targets and name strings never approach this limit.
    fn put_bytes_u16(&mut self, bytes: &[u8]) {
        let len = bytes.len().min(u16::MAX as usize);
        self.blobs.extend_from_slice(&(len as u16).to_le_bytes());
        self.blobs.extend_from_slice(&bytes[..len]);
    }
}

/// Forward-only reader over a byte slice, failing loud on truncation.
struct Cursor<'a> {
    blob: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn read_n(&mut self, len: usize) -> Result<Vec<u8>, ExtrasError> {
        let end = self.pos.checked_add(len).ok_or(ExtrasError::Truncated)?;
        let slice = self.blob.get(self.pos..end).ok_or(ExtrasError::Truncated)?;
        self.pos = end;
        Ok(slice.to_vec())
    }

    fn read_u8(&mut self) -> Result<u8, ExtrasError> {
        let byte = *self.blob.get(self.pos).ok_or(ExtrasError::Truncated)?;
        self.pos += 1;
        Ok(byte)
    }

    fn read_u16(&mut self) -> Result<u16, ExtrasError> {
        let bytes = self.read_array::<2>()?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_u32(&mut self) -> Result<u32, ExtrasError> {
        let bytes = self.read_array::<4>()?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_i64(&mut self) -> Result<i64, ExtrasError> {
        let bytes = self.read_array::<8>()?;
        Ok(i64::from_le_bytes(bytes))
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], ExtrasError> {
        let end = self.pos.checked_add(N).ok_or(ExtrasError::Truncated)?;
        let slice = self.blob.get(self.pos..end).ok_or(ExtrasError::Truncated)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        self.pos = end;
        Ok(out)
    }

    fn read_bytes_u16(&mut self) -> Result<Vec<u8>, ExtrasError> {
        let len = self.read_u16()? as usize;
        self.read_n(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_extras_yields_no_extras_sentinel() {
        let mut arena = ExtrasArena::new();
        let extras = FlatExtras::default();
        assert!(extras.is_empty());

        let reference = arena.append(&extras);
        // An empty record must not consume arena bytes and must round-trip to
        // the NO_EXTRAS sentinel decoding to None.
        assert_eq!(reference, ExtrasRef::NO_EXTRAS);
        assert!(arena.is_empty());
        assert_eq!(arena.decode(reference).unwrap(), None);
    }

    #[test]
    fn symlink_target_round_trips() {
        let mut arena = ExtrasArena::new();
        let extras = FlatExtras {
            link_target: Some(b"../some/where/target".to_vec()),
            ..FlatExtras::default()
        };

        let reference = arena.append(&extras);
        assert_ne!(reference, ExtrasRef::NO_EXTRAS);
        let decoded = arena.decode(reference).unwrap().unwrap();
        // Only the symlink target is present; every other field stays absent.
        assert_eq!(decoded, extras);
        assert_eq!(
            decoded.link_target.as_deref(),
            Some(&b"../some/where/target"[..])
        );
        assert_eq!(decoded.rdev_major, None);
    }

    #[test]
    fn device_numbers_round_trip() {
        let mut arena = ExtrasArena::new();
        let extras = FlatExtras {
            rdev_major: Some(8),
            rdev_minor: Some(17),
            ..FlatExtras::default()
        };

        let reference = arena.append(&extras);
        let decoded = arena.decode(reference).unwrap().unwrap();
        // The rdev pair is encoded together under one presence bit.
        assert_eq!(decoded.rdev_major, Some(8));
        assert_eq!(decoded.rdev_minor, Some(17));
        assert_eq!(decoded.link_target, None);
    }

    #[test]
    fn all_fields_round_trip() {
        let mut arena = ExtrasArena::new();
        let extras = FlatExtras {
            link_target: Some(b"link".to_vec()),
            rdev_major: Some(1),
            rdev_minor: Some(2),
            hardlink_idx: Some(42),
            acl_ndx: Some(3),
            def_acl_ndx: Some(4),
            xattr_ndx: Some(5),
            checksum: Some(vec![0xAB; 16]),
            user_name: Some(b"alice".to_vec()),
            group_name: Some(b"staff".to_vec()),
            atime: Some(-12345),
            crtime: Some(987_654_321),
            atime_nsec: Some(500),
        };

        let reference = arena.append(&extras);
        let decoded = arena.decode(reference).unwrap().unwrap();
        // Every field must survive a full encode/decode cycle unchanged.
        assert_eq!(decoded, extras);
    }

    #[test]
    fn multiple_records_at_distinct_offsets() {
        let mut arena = ExtrasArena::new();
        let first = FlatExtras {
            link_target: Some(b"first".to_vec()),
            ..FlatExtras::default()
        };
        let second = FlatExtras {
            rdev_major: Some(10),
            rdev_minor: Some(20),
            ..FlatExtras::default()
        };
        let third = FlatExtras {
            checksum: Some(vec![1, 2, 3, 4]),
            user_name: Some(b"bob".to_vec()),
            ..FlatExtras::default()
        };

        let r1 = arena.append(&first);
        let r2 = arena.append(&second);
        let r3 = arena.append(&third);

        // Distinct records land at distinct offsets and stay independently
        // addressable after later appends.
        assert_ne!(r1, r2);
        assert_ne!(r2, r3);
        assert_ne!(r1, r3);
        assert_eq!(arena.decode(r1).unwrap().unwrap(), first);
        assert_eq!(arena.decode(r2).unwrap().unwrap(), second);
        assert_eq!(arena.decode(r3).unwrap().unwrap(), third);
    }

    #[test]
    fn out_of_range_offset_is_rejected() {
        let arena = ExtrasArena::new();
        // An offset past the (empty) arena is a malformed reference, not a panic.
        assert_eq!(
            arena.decode(ExtrasRef(8)),
            Err(ExtrasError::OffsetOutOfRange)
        );
    }

    #[test]
    fn truncated_tail_is_rejected() {
        let mut arena = ExtrasArena::new();
        let extras = FlatExtras {
            rdev_major: Some(1),
            rdev_minor: Some(2),
            ..FlatExtras::default()
        };
        let reference = arena.append(&extras);
        // Lop off the trailing bytes so the promised rdev pair is incomplete.
        arena.blobs.truncate(arena.blobs.len() - 3);
        assert_eq!(arena.decode(reference), Err(ExtrasError::Truncated));
    }

    /// EDG-PANIC.5 regression: the `EXTRA_RDEV` mask is set only by
    /// `presence_mask` when both halves are `Some`, so the inner write must
    /// never see a divergent state in normal flow. The encode path now
    /// debug-asserts the invariant and falls back to zeros instead of
    /// panicking, so a future refactor that decouples the mask from the
    /// fields downgrades to a soft failure under release builds.
    #[test]
    fn rdev_mask_implies_both_halves_present() {
        let mut extras = FlatExtras::default();
        // Setting only one half must leave the mask bit cleared so the
        // unwrap/expect path in `append` is never reached.
        extras.rdev_major = Some(8);
        assert_eq!(extras.presence_mask() & EXTRA_RDEV, 0);

        extras.rdev_major = None;
        extras.rdev_minor = Some(17);
        assert_eq!(extras.presence_mask() & EXTRA_RDEV, 0);

        extras.rdev_major = Some(8);
        extras.rdev_minor = Some(17);
        assert_ne!(extras.presence_mask() & EXTRA_RDEV, 0);

        // Round-trip the well-formed pair to confirm the happy path still
        // produces the expected encoded bytes after the defensive change.
        let mut arena = ExtrasArena::new();
        let reference = arena.append(&extras);
        let decoded = arena.decode(reference).unwrap().unwrap();
        assert_eq!(decoded.rdev_major, Some(8));
        assert_eq!(decoded.rdev_minor, Some(17));
    }

    #[test]
    fn checksum_clamped_to_maximum() {
        let mut arena = ExtrasArena::new();
        let extras = FlatExtras {
            checksum: Some(vec![0xFF; 64]),
            ..FlatExtras::default()
        };
        let reference = arena.append(&extras);
        let decoded = arena.decode(reference).unwrap().unwrap();
        // Over-long checksums are clamped to the 32-byte upstream maximum.
        assert_eq!(
            decoded.checksum.as_ref().map(Vec::len),
            Some(MAX_CHECKSUM_LEN)
        );
    }
}
