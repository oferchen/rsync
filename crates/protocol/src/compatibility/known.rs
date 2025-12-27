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
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unrecognized compatibility flag identifier: {identifier}")]
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

impl fmt::Display for KnownCompatibilityFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_variants_count() {
        assert_eq!(KnownCompatibilityFlag::ALL.len(), 9);
    }

    #[test]
    fn as_flag_inc_recurse() {
        let flag = KnownCompatibilityFlag::IncRecurse;
        assert_eq!(flag.as_flag(), CompatibilityFlags::INC_RECURSE);
    }

    #[test]
    fn as_flag_symlink_times() {
        let flag = KnownCompatibilityFlag::SymlinkTimes;
        assert_eq!(flag.as_flag(), CompatibilityFlags::SYMLINK_TIMES);
    }

    #[test]
    fn as_flag_symlink_iconv() {
        let flag = KnownCompatibilityFlag::SymlinkIconv;
        assert_eq!(flag.as_flag(), CompatibilityFlags::SYMLINK_ICONV);
    }

    #[test]
    fn as_flag_safe_file_list() {
        let flag = KnownCompatibilityFlag::SafeFileList;
        assert_eq!(flag.as_flag(), CompatibilityFlags::SAFE_FILE_LIST);
    }

    #[test]
    fn as_flag_avoid_xattr_optimization() {
        let flag = KnownCompatibilityFlag::AvoidXattrOptimization;
        assert_eq!(flag.as_flag(), CompatibilityFlags::AVOID_XATTR_OPTIMIZATION);
    }

    #[test]
    fn as_flag_checksum_seed_fix() {
        let flag = KnownCompatibilityFlag::ChecksumSeedFix;
        assert_eq!(flag.as_flag(), CompatibilityFlags::CHECKSUM_SEED_FIX);
    }

    #[test]
    fn as_flag_inplace_partial_dir() {
        let flag = KnownCompatibilityFlag::InplacePartialDir;
        assert_eq!(flag.as_flag(), CompatibilityFlags::INPLACE_PARTIAL_DIR);
    }

    #[test]
    fn as_flag_varint_flist_flags() {
        let flag = KnownCompatibilityFlag::VarintFlistFlags;
        assert_eq!(flag.as_flag(), CompatibilityFlags::VARINT_FLIST_FLAGS);
    }

    #[test]
    fn as_flag_id0_names() {
        let flag = KnownCompatibilityFlag::Id0Names;
        assert_eq!(flag.as_flag(), CompatibilityFlags::ID0_NAMES);
    }

    #[test]
    fn name_inc_recurse() {
        assert_eq!(KnownCompatibilityFlag::IncRecurse.name(), "CF_INC_RECURSE");
    }

    #[test]
    fn name_symlink_times() {
        assert_eq!(
            KnownCompatibilityFlag::SymlinkTimes.name(),
            "CF_SYMLINK_TIMES"
        );
    }

    #[test]
    fn name_symlink_iconv() {
        assert_eq!(
            KnownCompatibilityFlag::SymlinkIconv.name(),
            "CF_SYMLINK_ICONV"
        );
    }

    #[test]
    fn name_safe_file_list() {
        assert_eq!(KnownCompatibilityFlag::SafeFileList.name(), "CF_SAFE_FLIST");
    }

    #[test]
    fn name_avoid_xattr_optimization() {
        assert_eq!(
            KnownCompatibilityFlag::AvoidXattrOptimization.name(),
            "CF_AVOID_XATTR_OPTIM"
        );
    }

    #[test]
    fn name_checksum_seed_fix() {
        assert_eq!(
            KnownCompatibilityFlag::ChecksumSeedFix.name(),
            "CF_CHKSUM_SEED_FIX"
        );
    }

    #[test]
    fn name_inplace_partial_dir() {
        assert_eq!(
            KnownCompatibilityFlag::InplacePartialDir.name(),
            "CF_INPLACE_PARTIAL_DIR"
        );
    }

    #[test]
    fn name_varint_flist_flags() {
        assert_eq!(
            KnownCompatibilityFlag::VarintFlistFlags.name(),
            "CF_VARINT_FLIST_FLAGS"
        );
    }

    #[test]
    fn name_id0_names() {
        assert_eq!(KnownCompatibilityFlag::Id0Names.name(), "CF_ID0_NAMES");
    }

    #[test]
    fn from_str_inc_recurse() {
        let parsed = KnownCompatibilityFlag::from_str("CF_INC_RECURSE").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::IncRecurse);
    }

    #[test]
    fn from_str_symlink_times() {
        let parsed = KnownCompatibilityFlag::from_str("CF_SYMLINK_TIMES").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::SymlinkTimes);
    }

    #[test]
    fn from_str_symlink_iconv() {
        let parsed = KnownCompatibilityFlag::from_str("CF_SYMLINK_ICONV").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::SymlinkIconv);
    }

    #[test]
    fn from_str_safe_flist() {
        let parsed = KnownCompatibilityFlag::from_str("CF_SAFE_FLIST").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::SafeFileList);
    }

    #[test]
    fn from_str_avoid_xattr_optim() {
        let parsed = KnownCompatibilityFlag::from_str("CF_AVOID_XATTR_OPTIM").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::AvoidXattrOptimization);
    }

    #[test]
    fn from_str_chksum_seed_fix() {
        let parsed = KnownCompatibilityFlag::from_str("CF_CHKSUM_SEED_FIX").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::ChecksumSeedFix);
    }

    #[test]
    fn from_str_inplace_partial_dir() {
        let parsed = KnownCompatibilityFlag::from_str("CF_INPLACE_PARTIAL_DIR").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::InplacePartialDir);
    }

    #[test]
    fn from_str_varint_flist_flags() {
        let parsed = KnownCompatibilityFlag::from_str("CF_VARINT_FLIST_FLAGS").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::VarintFlistFlags);
    }

    #[test]
    fn from_str_id0_names() {
        let parsed = KnownCompatibilityFlag::from_str("CF_ID0_NAMES").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::Id0Names);
    }

    #[test]
    fn from_str_unknown() {
        let result = KnownCompatibilityFlag::from_str("CF_UNKNOWN");
        assert!(result.is_err());
    }

    #[test]
    fn from_str_empty() {
        let result = KnownCompatibilityFlag::from_str("");
        assert!(result.is_err());
    }

    #[test]
    fn from_str_lowercase() {
        let result = KnownCompatibilityFlag::from_str("cf_inc_recurse");
        assert!(result.is_err());
    }

    #[test]
    fn from_name_valid() {
        let parsed = KnownCompatibilityFlag::from_name("CF_INC_RECURSE").unwrap();
        assert_eq!(parsed, KnownCompatibilityFlag::IncRecurse);
    }

    #[test]
    fn from_name_invalid() {
        let result = KnownCompatibilityFlag::from_name("INVALID");
        assert!(result.is_err());
    }

    #[test]
    fn parse_error_identifier() {
        let error = KnownCompatibilityFlag::from_str("UNKNOWN_FLAG").unwrap_err();
        assert_eq!(error.identifier(), "UNKNOWN_FLAG");
    }

    #[test]
    fn parse_error_display() {
        let error = KnownCompatibilityFlag::from_str("BAD_FLAG").unwrap_err();
        let display = format!("{}", error);
        assert!(display.contains("BAD_FLAG"));
        assert!(display.contains("unrecognized"));
    }

    #[test]
    fn display_trait() {
        let flag = KnownCompatibilityFlag::IncRecurse;
        assert_eq!(format!("{}", flag), "CF_INC_RECURSE");
    }

    #[test]
    fn display_all_variants() {
        for flag in KnownCompatibilityFlag::ALL {
            let display = format!("{}", flag);
            assert!(display.starts_with("CF_"));
        }
    }

    #[test]
    fn from_bits_inc_recurse() {
        let result = KnownCompatibilityFlag::from_bits(CompatibilityFlags::INC_RECURSE.bits());
        assert_eq!(result, Some(KnownCompatibilityFlag::IncRecurse));
    }

    #[test]
    fn from_bits_symlink_times() {
        let result = KnownCompatibilityFlag::from_bits(CompatibilityFlags::SYMLINK_TIMES.bits());
        assert_eq!(result, Some(KnownCompatibilityFlag::SymlinkTimes));
    }

    #[test]
    fn from_bits_unknown() {
        let result = KnownCompatibilityFlag::from_bits(0xFFFF_FFFF);
        assert_eq!(result, None);
    }

    #[test]
    fn from_bits_zero() {
        let result = KnownCompatibilityFlag::from_bits(0);
        assert_eq!(result, None);
    }

    #[test]
    fn clone_and_eq() {
        let flag = KnownCompatibilityFlag::IncRecurse;
        let cloned = flag.clone();
        assert_eq!(flag, cloned);
    }

    #[test]
    fn copy_trait() {
        let flag = KnownCompatibilityFlag::SymlinkTimes;
        let copied = flag;
        assert_eq!(flag, copied);
    }

    #[test]
    fn hash_trait() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(KnownCompatibilityFlag::IncRecurse);
        set.insert(KnownCompatibilityFlag::SymlinkTimes);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&KnownCompatibilityFlag::IncRecurse));
    }

    #[test]
    fn all_flags_unique() {
        use std::collections::HashSet;
        let set: HashSet<_> = KnownCompatibilityFlag::ALL.iter().collect();
        assert_eq!(set.len(), KnownCompatibilityFlag::ALL.len());
    }

    #[test]
    fn roundtrip_all_flags() {
        for flag in KnownCompatibilityFlag::ALL {
            let name = flag.name();
            let parsed = KnownCompatibilityFlag::from_str(name).unwrap();
            assert_eq!(flag, parsed);
        }
    }
}
