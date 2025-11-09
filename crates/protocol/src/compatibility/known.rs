use std::fmt;
use std::str::FromStr;

use super::flags::CompatibilityFlags;

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
    /// Canonical ordering of compatibility flags defined by upstream rsync 3.4.1.
    ///
    /// The array lists variants in ascending bit order so it can be used to
    /// populate [`CompatibilityFlags::ALL_KNOWN`] or iterate over every
    /// capability without duplicating match statements. The ordering matches the
    /// iteration semantics of [`CompatibilityFlags::iter_known`].
    pub const ALL: [Self; 9] = [
        Self::IncRecurse,
        Self::SymlinkTimes,
        Self::SymlinkIconv,
        Self::SafeFileList,
        Self::AvoidXattrOptimization,
        Self::ChecksumSeedFix,
        Self::InplacePartialDir,
        Self::VarintFlistFlags,
        Self::Id0Names,
    ];

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

    pub(super) const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            _ if bits == CompatibilityFlags::INC_RECURSE.bits() => Some(Self::IncRecurse),
            _ if bits == CompatibilityFlags::SYMLINK_TIMES.bits() => Some(Self::SymlinkTimes),
            _ if bits == CompatibilityFlags::SYMLINK_ICONV.bits() => Some(Self::SymlinkIconv),
            _ if bits == CompatibilityFlags::SAFE_FILE_LIST.bits() => Some(Self::SafeFileList),
            _ if bits == CompatibilityFlags::AVOID_XATTR_OPTIMIZATION.bits() => {
                Some(Self::AvoidXattrOptimization)
            }
            _ if bits == CompatibilityFlags::CHECKSUM_SEED_FIX.bits() => {
                Some(Self::ChecksumSeedFix)
            }
            _ if bits == CompatibilityFlags::INPLACE_PARTIAL_DIR.bits() => {
                Some(Self::InplacePartialDir)
            }
            _ if bits == CompatibilityFlags::VARINT_FLIST_FLAGS.bits() => {
                Some(Self::VarintFlistFlags)
            }
            _ if bits == CompatibilityFlags::ID0_NAMES.bits() => Some(Self::Id0Names),
            _ => None,
        }
    }

    /// Attempts to parse a canonical upstream identifier into a compatibility flag variant.
    ///
    /// The parser accepts the exact `CF_*` names emitted by upstream rsync and returned by
    /// [`Self::name`]. Any other input is rejected, ensuring higher layers avoid silently mapping
    /// unknown identifiers to a default value. When parsing fails, the returned
    /// `ParseKnownCompatibilityFlagError` exposes the offending identifier via
    /// [`ParseKnownCompatibilityFlagError::identifier`], making it trivial for
    /// callers to surface actionable diagnostics or log messages that mirror
    /// upstream wording.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::str::FromStr;
    /// use protocol::KnownCompatibilityFlag;
    ///
    /// let parsed = KnownCompatibilityFlag::from_str("CF_INC_RECURSE").expect("known flag");
    /// assert_eq!(parsed, KnownCompatibilityFlag::IncRecurse);
    /// assert!(KnownCompatibilityFlag::from_str("CF_UNKNOWN").is_err());
    /// ```
    #[must_use = "discarding the parsed flag would drop potential parse errors"]
    pub fn from_name(name: &str) -> Result<Self, ParseKnownCompatibilityFlagError> {
        Self::from_str(name)
    }
}

impl FromStr for KnownCompatibilityFlag {
    type Err = ParseKnownCompatibilityFlagError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "CF_INC_RECURSE" => Ok(Self::IncRecurse),
            "CF_SYMLINK_TIMES" => Ok(Self::SymlinkTimes),
            "CF_SYMLINK_ICONV" => Ok(Self::SymlinkIconv),
            "CF_SAFE_FLIST" => Ok(Self::SafeFileList),
            "CF_AVOID_XATTR_OPTIM" => Ok(Self::AvoidXattrOptimization),
            "CF_CHKSUM_SEED_FIX" => Ok(Self::ChecksumSeedFix),
            "CF_INPLACE_PARTIAL_DIR" => Ok(Self::InplacePartialDir),
            "CF_VARINT_FLIST_FLAGS" => Ok(Self::VarintFlistFlags),
            "CF_ID0_NAMES" => Ok(Self::Id0Names),
            _ => Err(ParseKnownCompatibilityFlagError::new(s)),
        }
    }
}

/// Error returned when parsing a [`KnownCompatibilityFlag`] from an invalid identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseKnownCompatibilityFlagError {
    identifier: Box<str>,
}

impl ParseKnownCompatibilityFlagError {
    pub(crate) fn new(identifier: impl Into<Box<str>>) -> Self {
        Self {
            identifier: identifier.into(),
        }
    }

    /// Returns the identifier that failed to parse.
    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }
}

impl fmt::Display for ParseKnownCompatibilityFlagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unrecognized compatibility flag identifier: {}",
            self.identifier()
        )
    }
}

impl std::error::Error for ParseKnownCompatibilityFlagError {}

impl fmt::Display for KnownCompatibilityFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
