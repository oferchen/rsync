use std::num::NonZeroU64;
use std::str::FromStr;
use std::time::Duration;

use thiserror::Error;

use crate::{
    message::{Message, Role},
    rsync_error,
};
use compress::zlib::{CompressionLevel, CompressionLevelError};
use engine::signature::SignatureAlgorithm;

/// Describes the timeout configuration applied to network operations.
///
/// The variant captures whether the caller requested a custom timeout, disabled
/// socket timeouts entirely, or asked to rely on the default for the current
/// operation. Higher layers convert the setting into concrete [`Duration`]
/// values depending on the transport in use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum TransferTimeout {
    /// Use the default timeout for the current operation.
    #[default]
    Default,
    /// Disable socket timeouts entirely.
    Disabled,
    /// Apply a caller-provided timeout expressed in seconds.
    Seconds(NonZeroU64),
}

impl TransferTimeout {
    /// Returns the timeout expressed as a [`Duration`] using the provided
    /// default when the setting is [`TransferTimeout::Default`].
    #[must_use]
    pub fn effective(self, default: Duration) -> Option<Duration> {
        match self {
            TransferTimeout::Default => Some(default),
            TransferTimeout::Disabled => None,
            TransferTimeout::Seconds(seconds) => Some(Duration::from_secs(seconds.get())),
        }
    }

    /// Convenience helper returning the raw seconds value when specified.
    #[must_use]
    pub const fn as_seconds(self) -> Option<NonZeroU64> {
        match self {
            TransferTimeout::Seconds(value) => Some(value),
            TransferTimeout::Default | TransferTimeout::Disabled => None,
        }
    }
}

/// Controls how byte counters are rendered for user-facing output.
///
/// Upstream `rsync` accepts optional levels for `--human-readable` that either
/// disable humanisation entirely, enable suffix-based formatting, or emit both
/// the humanised and exact decimal value.  The enum mirrors those levels so the
/// CLI can propagate the caller's preference to both the local renderer and any
/// fallback `rsync` invocations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(alias = "--human-readable")]
pub enum HumanReadableMode {
    /// Disable human-readable formatting and display exact decimal values.
    Disabled,
    /// Enable suffix-based formatting (e.g. `1.23K`, `4.56M`).
    Enabled,
    /// Display both the human-readable value and the exact decimal value.
    Combined,
}

impl HumanReadableMode {
    /// Parses a human-readable level from textual input.
    ///
    /// The parser trims ASCII whitespace before interpreting the value and
    /// accepts the numeric levels used by upstream `rsync`. A dedicated error
    /// type captures empty inputs and out-of-range values so callers can emit
    /// diagnostics that match the original CLI.
    pub fn parse(text: &str) -> Result<Self, HumanReadableModeParseError> {
        let trimmed = text.trim_matches(|ch: char| ch.is_ascii_whitespace());
        if trimmed.is_empty() {
            return Err(HumanReadableModeParseError::Empty);
        }

        match trimmed {
            "0" => Ok(Self::Disabled),
            "1" => Ok(Self::Enabled),
            "2" => Ok(Self::Combined),
            other => Err(HumanReadableModeParseError::Invalid {
                value: other.to_owned(),
            }),
        }
    }

    /// Reports whether human-readable formatting should be used.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Reports whether the exact decimal value should be included alongside the
    /// human-readable representation.
    #[must_use]
    pub const fn includes_exact(self) -> bool {
        matches!(self, Self::Combined)
    }
}

impl FromStr for HumanReadableMode {
    type Err = HumanReadableModeParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Errors produced when parsing a [`HumanReadableMode`] from text.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum HumanReadableModeParseError {
    /// The provided value was empty after trimming ASCII whitespace.
    #[error("human-readable level must not be empty")]
    Empty,
    /// The provided value did not match an accepted human-readable level.
    #[error("invalid human-readable level '{value}': expected 0, 1, or 2")]
    Invalid {
        /// The invalid value supplied by the caller after trimming ASCII whitespace.
        value: String,
    },
}

impl HumanReadableModeParseError {
    /// Returns the invalid value supplied by the caller when available.
    #[must_use]
    pub fn invalid_value(&self) -> Option<&str> {
        match self {
            Self::Invalid { value } => Some(value.as_str()),
            Self::Empty => None,
        }
    }
}

/// Enumerates the strong checksum algorithms recognised by the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrongChecksumAlgorithm {
    /// Automatically selects the negotiated algorithm (locally resolved to MD5).
    Auto,
    /// MD4 strong checksum.
    Md4,
    /// MD5 strong checksum.
    Md5,
    /// SHA-1 strong checksum.
    Sha1,
    /// XXH64 strong checksum.
    Xxh64,
    /// XXH3/64 strong checksum.
    Xxh3,
    /// XXH3/128 strong checksum.
    Xxh128,
}

impl StrongChecksumAlgorithm {
    /// Converts the selection into the [`SignatureAlgorithm`] used by the transfer engine.
    #[must_use]
    pub const fn to_signature_algorithm(self) -> SignatureAlgorithm {
        use checksums::strong::Md5Seed;
        match self {
            StrongChecksumAlgorithm::Auto | StrongChecksumAlgorithm::Md5 => {
                SignatureAlgorithm::Md5 {
                    seed_config: Md5Seed::none(),
                }
            }
            StrongChecksumAlgorithm::Md4 => SignatureAlgorithm::Md4,
            StrongChecksumAlgorithm::Sha1 => SignatureAlgorithm::Sha1,
            StrongChecksumAlgorithm::Xxh64 => SignatureAlgorithm::Xxh64 { seed: 0 },
            StrongChecksumAlgorithm::Xxh3 => SignatureAlgorithm::Xxh3 { seed: 0 },
            StrongChecksumAlgorithm::Xxh128 => SignatureAlgorithm::Xxh3_128 { seed: 0 },
        }
    }

    /// Returns the canonical flag spelling for the algorithm.
    #[must_use]
    pub const fn canonical_name(self) -> &'static str {
        match self {
            StrongChecksumAlgorithm::Auto => "auto",
            StrongChecksumAlgorithm::Md4 => "md4",
            StrongChecksumAlgorithm::Md5 => "md5",
            StrongChecksumAlgorithm::Sha1 => "sha1",
            StrongChecksumAlgorithm::Xxh64 => "xxh64",
            StrongChecksumAlgorithm::Xxh3 => "xxh3",
            StrongChecksumAlgorithm::Xxh128 => "xxh128",
        }
    }
}

/// Resolved checksum-choice configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StrongChecksumChoice {
    transfer: StrongChecksumAlgorithm,
    file: StrongChecksumAlgorithm,
}

impl StrongChecksumChoice {
    /// Parses a `--checksum-choice` argument and resolves the negotiated algorithms.
    pub fn parse(text: &str) -> Result<Self, Message> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(rsync_error!(
                1,
                "invalid --checksum-choice value '': value must name a checksum algorithm"
            )
            .with_role(Role::Client));
        }

        let mut parts = trimmed.splitn(2, ',');
        let transfer = Self::parse_single(parts.next().unwrap())?;
        let file = match parts.next() {
            Some(part) => Self::parse_single(part)?,
            None => transfer,
        };

        Ok(Self { transfer, file })
    }

    fn parse_single(label: &str) -> Result<StrongChecksumAlgorithm, Message> {
        let normalized = label.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "auto" => Ok(StrongChecksumAlgorithm::Auto),
            "md4" => Ok(StrongChecksumAlgorithm::Md4),
            "md5" => Ok(StrongChecksumAlgorithm::Md5),
            "sha1" => Ok(StrongChecksumAlgorithm::Sha1),
            "xxh64" | "xxhash" => Ok(StrongChecksumAlgorithm::Xxh64),
            "xxh3" | "xxh3-64" => Ok(StrongChecksumAlgorithm::Xxh3),
            "xxh128" | "xxh3-128" => Ok(StrongChecksumAlgorithm::Xxh128),
            _ => Err(rsync_error!(
                1,
                format!("invalid --checksum-choice value '{normalized}': unsupported checksum")
            )
            .with_role(Role::Client)),
        }
    }

    /// Returns the transfer-algorithm selection (first component).
    #[must_use]
    pub const fn transfer(self) -> StrongChecksumAlgorithm {
        self.transfer
    }

    /// Returns the checksum used for `--checksum` validation (second component).
    #[must_use]
    #[doc(alias = "--checksum-choice")]
    pub const fn file(self) -> StrongChecksumAlgorithm {
        self.file
    }

    /// Resolves the file checksum algorithm into a [`SignatureAlgorithm`].
    #[must_use]
    pub const fn file_signature_algorithm(self) -> SignatureAlgorithm {
        self.file.to_signature_algorithm()
    }

    /// Renders the selection into the canonical argument form accepted by `--checksum-choice`.
    #[must_use]
    pub fn to_argument(self) -> String {
        let transfer = self.transfer.canonical_name();
        let file = self.file.canonical_name();
        if self.transfer == self.file {
            transfer.to_owned()
        } else {
            format!("{transfer},{file}")
        }
    }
}

impl Default for StrongChecksumChoice {
    fn default() -> Self {
        Self {
            transfer: StrongChecksumAlgorithm::Auto,
            file: StrongChecksumAlgorithm::Auto,
        }
    }
}

/// Selects the preferred address family for daemon and remote-shell connections.
///
/// When [`AddressMode::Ipv4`] or [`AddressMode::Ipv6`] is selected, network
/// operations restrict socket resolution to the requested family, mirroring
/// upstream rsync's `--ipv4` and `--ipv6` flags. The default mode allows the
/// operating system to pick whichever address family resolves first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(alias = "--ipv4")]
#[doc(alias = "--ipv6")]
#[derive(Default)]
pub enum AddressMode {
    /// Allow the operating system to pick the address family.
    #[default]
    Default,
    /// Restrict resolution and connections to IPv4 addresses.
    Ipv4,
    /// Restrict resolution and connections to IPv6 addresses.
    Ipv6,
}

/// Compression configuration propagated from the CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum CompressionSetting {
    /// Compression has been explicitly disabled (e.g. `--compress-level=0`).
    ///
    /// This is also the default when building a [`ClientConfig`](super::ClientConfig), matching
    /// upstream rsync's behaviour of leaving compression off unless the caller
    /// explicitly enables it.
    #[default]
    Disabled,
    /// Compression is enabled with the provided [`CompressionLevel`].
    Level(CompressionLevel),
}

impl CompressionSetting {
    /// Returns a setting that disables compression.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    /// Returns a setting that enables compression using `level`.
    #[must_use]
    pub const fn level(level: CompressionLevel) -> Self {
        Self::Level(level)
    }

    /// Parses a numeric compression level into a [`CompressionSetting`].
    ///
    /// Values `1` through `9` map to [`CompressionLevel::Precise`]. A value of
    /// `0` disables compression, mirroring upstream rsync's interpretation of
    /// `--compress-level=0`. Values outside the supported range return
    /// [`CompressionLevelError`].
    pub fn try_from_numeric(level: u32) -> Result<Self, CompressionLevelError> {
        if level == 0 {
            Ok(Self::Disabled)
        } else {
            CompressionLevel::from_numeric(level).map(Self::Level)
        }
    }

    /// Reports whether compression should be enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Level(_))
    }

    /// Reports whether compression has been explicitly disabled.
    #[must_use]
    pub const fn is_disabled(self) -> bool {
        !self.is_enabled()
    }

    /// Returns the compression level that should be used when compression is
    /// enabled. When compression is disabled the default zlib level is
    /// returned, mirroring upstream rsync's behaviour when the caller toggles
    /// compression without specifying an explicit level.
    #[must_use]
    pub const fn level_or_default(self) -> CompressionLevel {
        match self {
            Self::Level(level) => level,
            Self::Disabled => CompressionLevel::Default,
        }
    }
}

impl From<CompressionLevel> for CompressionSetting {
    fn from(level: CompressionLevel) -> Self {
        Self::Level(level)
    }
}

/// Deletion scheduling selected by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum DeleteMode {
    /// Do not remove extraneous destination entries.
    #[default]
    Disabled,
    /// Remove extraneous entries before transferring file data.
    Before,
    /// Remove extraneous entries while processing directory contents (upstream default).
    During,
    /// Record deletions during the walk and prune entries after transfers finish.
    Delay,
    /// Remove extraneous entries after the transfer completes.
    After,
}

impl DeleteMode {
    /// Returns `true` when deletion sweeps are enabled.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod transfer_timeout_tests {
        use super::*;

        #[test]
        fn default_is_default_variant() {
            let timeout = TransferTimeout::default();
            assert_eq!(timeout, TransferTimeout::Default);
        }

        #[test]
        fn effective_with_default() {
            let timeout = TransferTimeout::Default;
            let default = Duration::from_secs(30);
            assert_eq!(timeout.effective(default), Some(default));
        }

        #[test]
        fn effective_with_disabled() {
            let timeout = TransferTimeout::Disabled;
            let default = Duration::from_secs(30);
            assert_eq!(timeout.effective(default), None);
        }

        #[test]
        fn effective_with_seconds() {
            let timeout = TransferTimeout::Seconds(NonZeroU64::new(60).unwrap());
            let default = Duration::from_secs(30);
            assert_eq!(timeout.effective(default), Some(Duration::from_secs(60)));
        }

        #[test]
        fn as_seconds_with_seconds() {
            let timeout = TransferTimeout::Seconds(NonZeroU64::new(45).unwrap());
            assert_eq!(timeout.as_seconds(), Some(NonZeroU64::new(45).unwrap()));
        }

        #[test]
        fn as_seconds_with_default() {
            let timeout = TransferTimeout::Default;
            assert_eq!(timeout.as_seconds(), None);
        }

        #[test]
        fn as_seconds_with_disabled() {
            let timeout = TransferTimeout::Disabled;
            assert_eq!(timeout.as_seconds(), None);
        }

        #[test]
        fn clone_and_copy() {
            let timeout = TransferTimeout::Seconds(NonZeroU64::new(10).unwrap());
            let cloned = timeout;
            let copied = timeout;
            assert_eq!(timeout, cloned);
            assert_eq!(timeout, copied);
        }

        #[test]
        fn debug_format() {
            let timeout = TransferTimeout::Default;
            assert_eq!(format!("{timeout:?}"), "Default");

            let timeout = TransferTimeout::Disabled;
            assert_eq!(format!("{timeout:?}"), "Disabled");

            let timeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
            assert!(format!("{timeout:?}").contains("Seconds"));
        }
    }

    mod human_readable_mode_tests {
        use super::*;

        #[test]
        fn parse_level_0() {
            assert_eq!(
                HumanReadableMode::parse("0").unwrap(),
                HumanReadableMode::Disabled
            );
        }

        #[test]
        fn parse_level_1() {
            assert_eq!(
                HumanReadableMode::parse("1").unwrap(),
                HumanReadableMode::Enabled
            );
        }

        #[test]
        fn parse_level_2() {
            assert_eq!(
                HumanReadableMode::parse("2").unwrap(),
                HumanReadableMode::Combined
            );
        }

        #[test]
        fn parse_with_whitespace() {
            assert_eq!(
                HumanReadableMode::parse("  1  ").unwrap(),
                HumanReadableMode::Enabled
            );
        }

        #[test]
        fn parse_empty_returns_error() {
            let result = HumanReadableMode::parse("");
            assert!(matches!(result, Err(HumanReadableModeParseError::Empty)));
        }

        #[test]
        fn parse_invalid_returns_error() {
            let result = HumanReadableMode::parse("3");
            assert!(matches!(
                result,
                Err(HumanReadableModeParseError::Invalid { .. })
            ));
        }

        #[test]
        fn from_str_works() {
            use std::str::FromStr;
            assert_eq!(
                HumanReadableMode::from_str("1").unwrap(),
                HumanReadableMode::Enabled
            );
        }

        #[test]
        fn is_enabled_disabled() {
            assert!(!HumanReadableMode::Disabled.is_enabled());
        }

        #[test]
        fn is_enabled_enabled() {
            assert!(HumanReadableMode::Enabled.is_enabled());
        }

        #[test]
        fn is_enabled_combined() {
            assert!(HumanReadableMode::Combined.is_enabled());
        }

        #[test]
        fn includes_exact_disabled() {
            assert!(!HumanReadableMode::Disabled.includes_exact());
        }

        #[test]
        fn includes_exact_enabled() {
            assert!(!HumanReadableMode::Enabled.includes_exact());
        }

        #[test]
        fn includes_exact_combined() {
            assert!(HumanReadableMode::Combined.includes_exact());
        }

        #[test]
        fn parse_error_invalid_value() {
            let err = HumanReadableMode::parse("foo").unwrap_err();
            assert_eq!(err.invalid_value(), Some("foo"));
        }

        #[test]
        fn parse_error_empty_value() {
            let err = HumanReadableMode::parse("").unwrap_err();
            assert_eq!(err.invalid_value(), None);
        }

        #[test]
        fn error_display() {
            let empty_err = HumanReadableModeParseError::Empty;
            assert!(empty_err.to_string().contains("must not be empty"));

            let invalid_err = HumanReadableModeParseError::Invalid {
                value: "3".to_owned(),
            };
            assert!(invalid_err.to_string().contains("invalid"));
        }
    }

    mod strong_checksum_algorithm_tests {
        use super::*;

        #[test]
        fn canonical_names() {
            assert_eq!(StrongChecksumAlgorithm::Auto.canonical_name(), "auto");
            assert_eq!(StrongChecksumAlgorithm::Md4.canonical_name(), "md4");
            assert_eq!(StrongChecksumAlgorithm::Md5.canonical_name(), "md5");
            assert_eq!(StrongChecksumAlgorithm::Sha1.canonical_name(), "sha1");
            assert_eq!(StrongChecksumAlgorithm::Xxh64.canonical_name(), "xxh64");
            assert_eq!(StrongChecksumAlgorithm::Xxh3.canonical_name(), "xxh3");
            assert_eq!(StrongChecksumAlgorithm::Xxh128.canonical_name(), "xxh128");
        }

        #[test]
        fn to_signature_algorithm() {
            // Just ensure they don't panic
            let _ = StrongChecksumAlgorithm::Auto.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Md4.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Md5.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Sha1.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh64.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh3.to_signature_algorithm();
            let _ = StrongChecksumAlgorithm::Xxh128.to_signature_algorithm();
        }

        #[test]
        fn clone_and_copy() {
            let alg = StrongChecksumAlgorithm::Md5;
            let cloned = alg;
            let copied = alg;
            assert_eq!(alg, cloned);
            assert_eq!(alg, copied);
        }

        #[test]
        fn debug_format() {
            assert_eq!(format!("{:?}", StrongChecksumAlgorithm::Auto), "Auto");
            assert_eq!(format!("{:?}", StrongChecksumAlgorithm::Xxh128), "Xxh128");
        }
    }

    mod strong_checksum_choice_tests {
        use super::*;

        #[test]
        fn parse_single_algorithm() {
            let choice = StrongChecksumChoice::parse("md5").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Md5);
            assert_eq!(choice.file(), StrongChecksumAlgorithm::Md5);
        }

        #[test]
        fn parse_two_algorithms() {
            let choice = StrongChecksumChoice::parse("xxh3,md5").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh3);
            assert_eq!(choice.file(), StrongChecksumAlgorithm::Md5);
        }

        #[test]
        fn parse_with_whitespace() {
            let choice = StrongChecksumChoice::parse("  sha1  ").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Sha1);
        }

        #[test]
        fn parse_xxhash_alias() {
            let choice = StrongChecksumChoice::parse("xxhash").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh64);
        }

        #[test]
        fn parse_xxh3_64_alias() {
            let choice = StrongChecksumChoice::parse("xxh3-64").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh3);
        }

        #[test]
        fn parse_xxh3_128_alias() {
            let choice = StrongChecksumChoice::parse("xxh3-128").unwrap();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Xxh128);
        }

        #[test]
        fn parse_empty_returns_error() {
            assert!(StrongChecksumChoice::parse("").is_err());
        }

        #[test]
        fn parse_invalid_returns_error() {
            assert!(StrongChecksumChoice::parse("invalid").is_err());
        }

        #[test]
        fn default_is_auto() {
            let choice = StrongChecksumChoice::default();
            assert_eq!(choice.transfer(), StrongChecksumAlgorithm::Auto);
            assert_eq!(choice.file(), StrongChecksumAlgorithm::Auto);
        }

        #[test]
        fn to_argument_same_algorithm() {
            let choice = StrongChecksumChoice::parse("md5").unwrap();
            assert_eq!(choice.to_argument(), "md5");
        }

        #[test]
        fn to_argument_different_algorithms() {
            let choice = StrongChecksumChoice::parse("xxh3,md5").unwrap();
            assert_eq!(choice.to_argument(), "xxh3,md5");
        }

        #[test]
        fn file_signature_algorithm() {
            let choice = StrongChecksumChoice::parse("md5").unwrap();
            let _ = choice.file_signature_algorithm(); // Just ensure no panic
        }
    }

    mod address_mode_tests {
        use super::*;

        #[test]
        fn default_is_default_variant() {
            assert_eq!(AddressMode::default(), AddressMode::Default);
        }

        #[test]
        fn clone_and_copy() {
            let mode = AddressMode::Ipv4;
            let cloned = mode;
            let copied = mode;
            assert_eq!(mode, cloned);
            assert_eq!(mode, copied);
        }

        #[test]
        fn debug_format() {
            assert_eq!(format!("{:?}", AddressMode::Default), "Default");
            assert_eq!(format!("{:?}", AddressMode::Ipv4), "Ipv4");
            assert_eq!(format!("{:?}", AddressMode::Ipv6), "Ipv6");
        }
    }

    mod compression_setting_tests {
        use super::*;

        #[test]
        fn default_is_disabled() {
            assert_eq!(CompressionSetting::default(), CompressionSetting::Disabled);
        }

        #[test]
        fn disabled_constructor() {
            assert_eq!(CompressionSetting::disabled(), CompressionSetting::Disabled);
        }

        #[test]
        fn level_constructor() {
            let level = CompressionLevel::Default;
            let setting = CompressionSetting::level(level);
            assert!(matches!(setting, CompressionSetting::Level(_)));
        }

        #[test]
        fn try_from_numeric_zero_disables() {
            let result = CompressionSetting::try_from_numeric(0).unwrap();
            assert_eq!(result, CompressionSetting::Disabled);
        }

        #[test]
        fn try_from_numeric_valid_levels() {
            for level in 1..=9 {
                let result = CompressionSetting::try_from_numeric(level);
                assert!(result.is_ok());
                assert!(result.unwrap().is_enabled());
            }
        }

        #[test]
        fn try_from_numeric_invalid_level() {
            let result = CompressionSetting::try_from_numeric(10);
            assert!(result.is_err());
        }

        #[test]
        fn is_enabled_disabled() {
            assert!(!CompressionSetting::Disabled.is_enabled());
        }

        #[test]
        fn is_enabled_level() {
            let setting = CompressionSetting::level(CompressionLevel::Default);
            assert!(setting.is_enabled());
        }

        #[test]
        fn is_disabled() {
            assert!(CompressionSetting::Disabled.is_disabled());
            assert!(!CompressionSetting::level(CompressionLevel::Default).is_disabled());
        }

        #[test]
        fn level_or_default_with_disabled() {
            let setting = CompressionSetting::Disabled;
            assert_eq!(setting.level_or_default(), CompressionLevel::Default);
        }

        #[test]
        fn from_compression_level() {
            let level = CompressionLevel::Default;
            let setting: CompressionSetting = level.into();
            assert!(matches!(setting, CompressionSetting::Level(_)));
        }
    }

    mod delete_mode_tests {
        use super::*;

        #[test]
        fn default_is_disabled() {
            assert_eq!(DeleteMode::default(), DeleteMode::Disabled);
        }

        #[test]
        fn is_enabled_disabled() {
            assert!(!DeleteMode::Disabled.is_enabled());
        }

        #[test]
        fn is_enabled_before() {
            assert!(DeleteMode::Before.is_enabled());
        }

        #[test]
        fn is_enabled_during() {
            assert!(DeleteMode::During.is_enabled());
        }

        #[test]
        fn is_enabled_delay() {
            assert!(DeleteMode::Delay.is_enabled());
        }

        #[test]
        fn is_enabled_after() {
            assert!(DeleteMode::After.is_enabled());
        }

        #[test]
        fn clone_and_copy() {
            let mode = DeleteMode::Before;
            let cloned = mode;
            let copied = mode;
            assert_eq!(mode, cloned);
            assert_eq!(mode, copied);
        }

        #[test]
        fn debug_format() {
            assert_eq!(format!("{:?}", DeleteMode::Disabled), "Disabled");
            assert_eq!(format!("{:?}", DeleteMode::Before), "Before");
            assert_eq!(format!("{:?}", DeleteMode::During), "During");
            assert_eq!(format!("{:?}", DeleteMode::Delay), "Delay");
            assert_eq!(format!("{:?}", DeleteMode::After), "After");
        }
    }
}
