use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;
use std::time::Duration;

use crate::{
    message::{Message, Role},
    rsync_error,
};
use rsync_compress::zlib::{CompressionLevel, CompressionLevelError};
use rsync_engine::signature::SignatureAlgorithm;

/// Describes the timeout configuration applied to network operations.
///
/// The variant captures whether the caller requested a custom timeout, disabled
/// socket timeouts entirely, or asked to rely on the default for the current
/// operation. Higher layers convert the setting into concrete [`Duration`]
/// values depending on the transport in use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferTimeout {
    /// Use the default timeout for the current operation.
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

impl Default for TransferTimeout {
    fn default() -> Self {
        Self::Default
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
                value: other.to_string(),
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HumanReadableModeParseError {
    /// The provided value was empty after trimming ASCII whitespace.
    Empty,
    /// The provided value did not match an accepted human-readable level.
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

impl fmt::Display for HumanReadableModeParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("human-readable level must not be empty"),
            Self::Invalid { value } => write!(
                f,
                "invalid human-readable level '{value}': expected 0, 1, or 2"
            ),
        }
    }
}

impl std::error::Error for HumanReadableModeParseError {}

/// Enumerates the strong checksum algorithms recognised by the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StrongChecksumAlgorithm {
    /// Automatically selects the negotiated algorithm (locally resolved to MD5).
    Auto,
    /// MD4 strong checksum.
    Md4,
    /// MD5 strong checksum.
    Md5,
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
        match self {
            StrongChecksumAlgorithm::Auto | StrongChecksumAlgorithm::Md5 => SignatureAlgorithm::Md5,
            StrongChecksumAlgorithm::Md4 => SignatureAlgorithm::Md4,
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
            transfer.to_string()
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
pub enum AddressMode {
    /// Allow the operating system to pick the address family.
    Default,
    /// Restrict resolution and connections to IPv4 addresses.
    Ipv4,
    /// Restrict resolution and connections to IPv6 addresses.
    Ipv6,
}

impl Default for AddressMode {
    fn default() -> Self {
        Self::Default
    }
}

/// Compression configuration propagated from the CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionSetting {
    /// Compression has been explicitly disabled (e.g. `--compress-level=0`).
    ///
    /// This is also the default when building a [`ClientConfig`](super::ClientConfig), matching
    /// upstream rsync's behaviour of leaving compression off unless the caller
    /// explicitly enables it.
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

impl Default for CompressionSetting {
    fn default() -> Self {
        Self::Disabled
    }
}

impl From<CompressionLevel> for CompressionSetting {
    fn from(level: CompressionLevel) -> Self {
        Self::Level(level)
    }
}

/// Deletion scheduling selected by the caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteMode {
    /// Do not remove extraneous destination entries.
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

impl Default for DeleteMode {
    fn default() -> Self {
        Self::Disabled
    }
}
