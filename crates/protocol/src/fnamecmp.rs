//! Basis file comparison type constants for alternate basis selection.
//!
//! When the generator selects a basis file for delta transfer, it records
//! the type of basis used via an `FNAMECMP_*` value. This byte is sent on
//! the wire when `ITEM_BASIS_TYPE_FOLLOWS` is set in the item flags, telling
//! the receiver which kind of basis file was chosen.
//!
//! # Wire Format
//!
//! The basis type is a single byte following the item flags. Values 0x00-0x7F
//! are indices into the `basis_dir[]` array (populated by `--copy-dest`,
//! `--link-dest`, or `--compare-dest` options). Values 0x80-0x83 identify
//! special basis sources.
//!
//! # Upstream Reference
//!
//! - `rsync.h` - `FNAMECMP_*` constant definitions
//! - `generator.c` - `recv_generator()` sets `fnamecmp_type` based on basis selection
//! - `generator.c` - `try_dests_reg()` returns basis dir index (0x00-0x7F range)

use std::fmt;

/// Basis file comparison type sent on the wire.
///
/// Identifies which basis file the generator selected for delta transfer.
/// The receiver uses this to understand the provenance of the basis file
/// and potentially locate it for delta application.
///
/// # Wire Encoding
///
/// Encoded as a single `u8` on the wire when `ITEM_BASIS_TYPE_FOLLOWS`
/// (bit 11) is set in the item flags.
///
/// # Upstream Reference
///
/// - `rsync.h` - `FNAMECMP_*` definitions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FnameCmpType {
    /// Basis file from a `--copy-dest`, `--link-dest`, or `--compare-dest` directory.
    ///
    /// The index (0x00-0x7F) identifies which entry in the `basis_dir[]` array
    /// was used. Multiple `--copy-dest` or `--link-dest` options create multiple
    /// entries, and `try_dests_reg()` / `try_dests_non()` return the matching index.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h` - `FNAMECMP_BASIS_DIR_LOW` (0x00) through `FNAMECMP_BASIS_DIR_HIGH` (0x7F)
    /// - `generator.c` - `try_dests_reg()` returns the matching basis dir index
    BasisDir(u8),

    /// The main destination file (the "fname" itself).
    ///
    /// This is the default comparison type when no alternate basis is used.
    /// The generator compares against the existing destination file.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h` - `FNAMECMP_FNAME` (0x80)
    /// - `generator.c` - `fnamecmp_type = FNAMECMP_FNAME` is the initial assignment
    Fname,

    /// A partially transferred file from an incomplete previous transfer.
    ///
    /// When `--partial-dir` is configured and a partial file exists from a
    /// prior interrupted transfer, the generator uses it as a delta basis
    /// to resume the transfer efficiently.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h` - `FNAMECMP_PARTIAL_DIR` (0x81)
    /// - `generator.c` - Set when `partialptr` is found
    PartialDir,

    /// A backup copy of the destination file.
    ///
    /// When `--inplace` and `--backup` are both active, the generator uses
    /// the backup file as the basis. This avoids corrupting the backup if
    /// the transfer fails mid-write.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h` - `FNAMECMP_BACKUP` (0x82)
    /// - `generator.c` - Set when `inplace && make_backups > 0`
    Backup,

    /// A fuzzy-matched file used as an approximate basis.
    ///
    /// When `--fuzzy` is enabled and no exact basis exists, the generator
    /// searches for a similarly-named file to use as a delta basis. The
    /// wire value encodes `FNAMECMP_FUZZY + fuzzy_basis_index` for multi-level
    /// fuzzy matching.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h` - `FNAMECMP_FUZZY` (0x83)
    /// - `generator.c` - Set by `find_fuzzy()` when a match is found
    Fuzzy,
}

impl FnameCmpType {
    // Wire constants matching upstream rsync.h

    /// Lower bound of the basis directory index range (inclusive).
    ///
    /// Upstream: `FNAMECMP_BASIS_DIR_LOW` = 0x00. Must remain 0.
    pub const BASIS_DIR_LOW: u8 = 0x00;

    /// Upper bound of the basis directory index range (inclusive).
    ///
    /// Upstream: `FNAMECMP_BASIS_DIR_HIGH` = 0x7F.
    pub const BASIS_DIR_HIGH: u8 = 0x7F;

    /// Wire value for the main destination file.
    ///
    /// Upstream: `FNAMECMP_FNAME` = 0x80.
    pub const FNAME: u8 = 0x80;

    /// Wire value for a partial directory file.
    ///
    /// Upstream: `FNAMECMP_PARTIAL_DIR` = 0x81.
    pub const PARTIAL_DIR: u8 = 0x81;

    /// Wire value for a backup file.
    ///
    /// Upstream: `FNAMECMP_BACKUP` = 0x82.
    pub const BACKUP: u8 = 0x82;

    /// Wire value for a fuzzy-matched file.
    ///
    /// Upstream: `FNAMECMP_FUZZY` = 0x83.
    pub const FUZZY: u8 = 0x83;

    /// Decodes a wire byte into a typed `FnameCmpType`.
    ///
    /// Values 0x00-0x7F map to `BasisDir(index)`. Values 0x80-0x83 map
    /// to the corresponding named variants. Values 0x84-0xFF are invalid
    /// per the upstream protocol.
    ///
    /// # Errors
    ///
    /// Returns `None` for wire values 0x84-0xFF, which are undefined in
    /// upstream rsync.
    #[must_use]
    pub const fn from_wire(byte: u8) -> Option<Self> {
        match byte {
            Self::BASIS_DIR_LOW..=Self::BASIS_DIR_HIGH => Some(Self::BasisDir(byte)),
            Self::FNAME => Some(Self::Fname),
            Self::PARTIAL_DIR => Some(Self::PartialDir),
            Self::BACKUP => Some(Self::Backup),
            Self::FUZZY => Some(Self::Fuzzy),
            _ => None,
        }
    }

    /// Encodes this type as its wire byte value.
    #[must_use]
    pub const fn to_wire(self) -> u8 {
        match self {
            Self::BasisDir(index) => index,
            Self::Fname => Self::FNAME,
            Self::PartialDir => Self::PARTIAL_DIR,
            Self::Backup => Self::BACKUP,
            Self::Fuzzy => Self::FUZZY,
        }
    }

    /// Returns true if this is a basis directory reference.
    #[must_use]
    pub const fn is_basis_dir(&self) -> bool {
        matches!(self, Self::BasisDir(_))
    }

    /// Returns the basis directory index, if this is a `BasisDir` variant.
    #[must_use]
    pub const fn basis_dir_index(&self) -> Option<u8> {
        match self {
            Self::BasisDir(idx) => Some(*idx),
            _ => None,
        }
    }
}

impl fmt::Display for FnameCmpType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BasisDir(idx) => write!(f, "basis_dir[{idx}]"),
            Self::Fname => write!(f, "fname"),
            Self::PartialDir => write!(f, "partial_dir"),
            Self::Backup => write!(f, "backup"),
            Self::Fuzzy => write!(f, "fuzzy"),
        }
    }
}

impl From<FnameCmpType> for u8 {
    fn from(t: FnameCmpType) -> Self {
        t.to_wire()
    }
}

impl TryFrom<u8> for FnameCmpType {
    type Error = InvalidFnameCmpType;

    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        Self::from_wire(byte).ok_or(InvalidFnameCmpType(byte))
    }
}

/// Error returned when a wire byte does not correspond to a valid `FnameCmpType`.
///
/// Values 0x84-0xFF are undefined in the upstream protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidFnameCmpType(
    /// The invalid wire byte.
    pub u8,
);

impl fmt::Display for InvalidFnameCmpType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid fnamecmp type: 0x{:02X}", self.0)
    }
}

impl std::error::Error for InvalidFnameCmpType {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_upstream_rsync_h() {
        // upstream: rsync.h
        // #define FNAMECMP_BASIS_DIR_LOW  0x00
        // #define FNAMECMP_BASIS_DIR_HIGH 0x7F
        // #define FNAMECMP_FNAME          0x80
        // #define FNAMECMP_PARTIAL_DIR    0x81
        // #define FNAMECMP_BACKUP         0x82
        // #define FNAMECMP_FUZZY          0x83
        assert_eq!(FnameCmpType::BASIS_DIR_LOW, 0x00);
        assert_eq!(FnameCmpType::BASIS_DIR_HIGH, 0x7F);
        assert_eq!(FnameCmpType::FNAME, 0x80);
        assert_eq!(FnameCmpType::PARTIAL_DIR, 0x81);
        assert_eq!(FnameCmpType::BACKUP, 0x82);
        assert_eq!(FnameCmpType::FUZZY, 0x83);
    }

    #[test]
    fn wire_roundtrip_named_variants() {
        let cases = [
            (FnameCmpType::Fname, 0x80),
            (FnameCmpType::PartialDir, 0x81),
            (FnameCmpType::Backup, 0x82),
            (FnameCmpType::Fuzzy, 0x83),
        ];
        for (variant, expected_wire) in cases {
            assert_eq!(variant.to_wire(), expected_wire);
            assert_eq!(FnameCmpType::from_wire(expected_wire), Some(variant));
        }
    }

    #[test]
    fn wire_roundtrip_basis_dir() {
        for idx in 0..=0x7F_u8 {
            let t = FnameCmpType::BasisDir(idx);
            assert_eq!(t.to_wire(), idx);
            assert_eq!(FnameCmpType::from_wire(idx), Some(t));
        }
    }

    #[test]
    fn from_wire_rejects_undefined_values() {
        for byte in 0x84..=0xFF_u8 {
            assert_eq!(FnameCmpType::from_wire(byte), None);
        }
    }

    #[test]
    fn try_from_u8_valid() {
        assert_eq!(FnameCmpType::try_from(0x80), Ok(FnameCmpType::Fname));
        assert_eq!(FnameCmpType::try_from(0x00), Ok(FnameCmpType::BasisDir(0)));
        assert_eq!(
            FnameCmpType::try_from(0x7F),
            Ok(FnameCmpType::BasisDir(0x7F))
        );
    }

    #[test]
    fn try_from_u8_invalid() {
        assert_eq!(FnameCmpType::try_from(0x84), Err(InvalidFnameCmpType(0x84)));
        assert_eq!(FnameCmpType::try_from(0xFF), Err(InvalidFnameCmpType(0xFF)));
    }

    #[test]
    fn into_u8() {
        let byte: u8 = FnameCmpType::Fname.into();
        assert_eq!(byte, 0x80);
        let byte: u8 = FnameCmpType::BasisDir(5).into();
        assert_eq!(byte, 5);
    }

    #[test]
    fn is_basis_dir() {
        assert!(FnameCmpType::BasisDir(0).is_basis_dir());
        assert!(FnameCmpType::BasisDir(127).is_basis_dir());
        assert!(!FnameCmpType::Fname.is_basis_dir());
        assert!(!FnameCmpType::PartialDir.is_basis_dir());
        assert!(!FnameCmpType::Backup.is_basis_dir());
        assert!(!FnameCmpType::Fuzzy.is_basis_dir());
    }

    #[test]
    fn basis_dir_index() {
        assert_eq!(FnameCmpType::BasisDir(42).basis_dir_index(), Some(42));
        assert_eq!(FnameCmpType::Fname.basis_dir_index(), None);
    }

    #[test]
    fn display_formatting() {
        assert_eq!(FnameCmpType::BasisDir(0).to_string(), "basis_dir[0]");
        assert_eq!(FnameCmpType::BasisDir(5).to_string(), "basis_dir[5]");
        assert_eq!(FnameCmpType::Fname.to_string(), "fname");
        assert_eq!(FnameCmpType::PartialDir.to_string(), "partial_dir");
        assert_eq!(FnameCmpType::Backup.to_string(), "backup");
        assert_eq!(FnameCmpType::Fuzzy.to_string(), "fuzzy");
    }

    #[test]
    fn invalid_fnamecmp_type_display() {
        let err = InvalidFnameCmpType(0x84);
        assert_eq!(err.to_string(), "invalid fnamecmp type: 0x84");
    }

    #[test]
    fn basis_dir_low_must_be_zero() {
        // upstream: rsync.h comment "Must remain 0!"
        assert_eq!(FnameCmpType::BASIS_DIR_LOW, 0);
        assert_eq!(FnameCmpType::BasisDir(0).to_wire(), 0);
    }

    #[test]
    fn all_128_basis_dir_indices_valid() {
        // Upstream supports 128 basis directories (0x00-0x7F)
        for i in 0..128_u8 {
            let t = FnameCmpType::BasisDir(i);
            assert!(t.is_basis_dir());
            assert_eq!(t.basis_dir_index(), Some(i));
            assert_eq!(t.to_wire(), i);
        }
    }
}
