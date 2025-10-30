#![deny(unsafe_code)]

//! Client configuration data structures and helpers.
//!
//! This module isolates the data types used to describe transfer requests so
//! that they remain accessible to both the CLI front-end and daemon entry
//! points without keeping the primary orchestration module monolithic. All
//! definitions are re-exported from [`crate::client`] to preserve the existing
//! public API.

use std::env;
use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{
    bandwidth::{self, BandwidthLimiter, BandwidthParseError},
    message::{Message, Role},
    rsync_error,
};
use rsync_compress::zlib::{CompressionLevel, CompressionLevelError};
use rsync_engine::{SkipCompressList, local_copy::DirMergeOptions, signature::SignatureAlgorithm};
use rsync_meta::ChmodModifiers;

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
    /// This is also the default when building a [`ClientConfig`], matching
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

/// Identifies the strategy applied to a reference directory entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceDirectoryKind {
    /// Skip creating the destination when the referenced file matches.
    Compare,
    /// Copy data from the reference directory when the file matches.
    Copy,
    /// Create a hard link to the reference directory when the file matches.
    Link,
}

/// Describes a reference directory consulted during local copy execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceDirectory {
    kind: ReferenceDirectoryKind,
    path: PathBuf,
}

impl ReferenceDirectory {
    /// Creates a new reference directory entry.
    #[must_use]
    pub fn new(kind: ReferenceDirectoryKind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
        }
    }

    /// Returns the kind associated with the reference directory entry.
    #[must_use]
    pub const fn kind(&self) -> ReferenceDirectoryKind {
        self.kind
    }

    /// Returns the base path of the reference directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Configuration describing the requested client operation.
/// Describes a bind address specified via `--address`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindAddress {
    raw: OsString,
    socket: SocketAddr,
}

impl BindAddress {
    /// Creates a new bind address from the caller-provided specification.
    #[must_use]
    pub const fn new(raw: OsString, socket: SocketAddr) -> Self {
        Self { raw, socket }
    }

    /// Returns the raw command-line representation forwarded to the fallback binary.
    #[must_use]
    pub fn raw(&self) -> &OsStr {
        self.raw.as_os_str()
    }

    /// Returns the socket address (with port zero) used when binding local connections.
    #[must_use]
    pub const fn socket(&self) -> SocketAddr {
        self.socket
    }
}

/// Configuration describing the requested client operation.
pub struct ClientConfig {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete_mode: DeleteMode,
    delete_excluded: bool,
    max_delete: Option<u64>,
    min_file_size: Option<u64>,
    max_file_size: Option<u64>,
    modify_window: Option<u64>,
    remove_source_files: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    chmod: Option<ChmodModifiers>,
    omit_dir_times: bool,
    omit_link_times: bool,
    compress: bool,
    compression_level: Option<CompressionLevel>,
    compression_setting: CompressionSetting,
    skip_compress: SkipCompressList,
    whole_file: bool,
    checksum: bool,
    checksum_choice: StrongChecksumChoice,
    checksum_seed: Option<u32>,
    size_only: bool,
    ignore_existing: bool,
    ignore_missing_args: bool,
    update: bool,
    numeric_ids: bool,
    preallocate: bool,
    preserve_hard_links: bool,
    filter_rules: Vec<FilterRuleSpec>,
    debug_flags: Vec<OsString>,
    sparse: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    copy_unsafe_links: bool,
    keep_dirlinks: bool,
    safe_links: bool,
    relative_paths: bool,
    one_file_system: bool,
    implied_dirs: bool,
    mkpath: bool,
    prune_empty_dirs: bool,
    verbosity: u8,
    progress: bool,
    stats: bool,
    human_readable: bool,
    partial: bool,
    partial_dir: Option<PathBuf>,
    temp_directory: Option<PathBuf>,
    backup: bool,
    backup_dir: Option<PathBuf>,
    backup_suffix: Option<OsString>,
    delay_updates: bool,
    inplace: bool,
    append: bool,
    append_verify: bool,
    force_event_collection: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    list_only: bool,
    address_mode: AddressMode,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
    link_dest_paths: Vec<PathBuf>,
    reference_directories: Vec<ReferenceDirectory>,
    /// Optional command executed to reach rsync:// daemons.
    #[doc(alias = "--connect-program")]
    connect_program: Option<OsString>,
    /// Optional local bind address forwarded to network transports.
    #[doc(alias = "--address")]
    bind_address: Option<BindAddress>,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            transfer_args: Vec::new(),
            dry_run: false,
            delete_mode: DeleteMode::Disabled,
            delete_excluded: false,
            max_delete: None,
            min_file_size: None,
            max_file_size: None,
            modify_window: None,
            remove_source_files: false,
            bandwidth_limit: None,
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: false,
            preserve_times: false,
            owner_override: None,
            group_override: None,
            chmod: None,
            omit_dir_times: false,
            omit_link_times: false,
            compress: false,
            compression_level: None,
            compression_setting: CompressionSetting::default(),
            skip_compress: SkipCompressList::default(),
            whole_file: true,
            checksum: false,
            checksum_choice: StrongChecksumChoice::default(),
            checksum_seed: None,
            size_only: false,
            ignore_existing: false,
            ignore_missing_args: false,
            update: false,
            numeric_ids: false,
            preallocate: false,
            preserve_hard_links: false,
            filter_rules: Vec::new(),
            debug_flags: Vec::new(),
            sparse: false,
            copy_links: false,
            copy_dirlinks: false,
            copy_unsafe_links: false,
            keep_dirlinks: false,
            safe_links: false,
            relative_paths: false,
            one_file_system: false,
            implied_dirs: true,
            mkpath: false,
            prune_empty_dirs: false,
            verbosity: 0,
            progress: false,
            stats: false,
            human_readable: false,
            partial: false,
            partial_dir: None,
            temp_directory: None,
            backup: false,
            backup_dir: None,
            backup_suffix: None,
            delay_updates: false,
            inplace: false,
            append: false,
            append_verify: false,
            force_event_collection: false,
            preserve_devices: false,
            preserve_specials: false,
            list_only: false,
            address_mode: AddressMode::Default,
            timeout: TransferTimeout::Default,
            connect_timeout: TransferTimeout::Default,
            link_dest_paths: Vec::new(),
            reference_directories: Vec::new(),
            connect_program: None,
            bind_address: None,
            #[cfg(feature = "acl")]
            preserve_acls: false,
            #[cfg(feature = "xattr")]
            preserve_xattrs: false,
        }
    }
}

impl ClientConfig {
    /// Creates a new [`ClientConfigBuilder`].
    #[must_use]
    pub fn builder() -> ClientConfigBuilder {
        ClientConfigBuilder::default()
    }

    /// Returns the raw transfer arguments provided by the caller.
    #[must_use]
    pub fn transfer_args(&self) -> &[OsString] {
        &self.transfer_args
    }

    /// Returns the ordered reference directories supplied via `--compare-dest`,
    /// `--copy-dest`, or `--link-dest`.
    #[must_use]
    #[doc(alias = "--compare-dest")]
    #[doc(alias = "--copy-dest")]
    #[doc(alias = "--link-dest")]
    pub fn reference_directories(&self) -> &[ReferenceDirectory] {
        &self.reference_directories
    }

    /// Reports whether transfers should be listed without mutating the destination.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only(&self) -> bool {
        self.list_only
    }

    /// Returns the preferred address family used for daemon or remote-shell connections.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn address_mode(&self) -> AddressMode {
        self.address_mode
    }

    /// Returns the configured connect program, if any.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn connect_program(&self) -> Option<&OsStr> {
        self.connect_program.as_deref()
    }

    /// Returns the optional bind address configured via `--address`.
    #[must_use]
    #[doc(alias = "--address")]
    pub fn bind_address(&self) -> Option<&BindAddress> {
        self.bind_address.as_ref()
    }

    /// Reports whether a transfer was explicitly requested.
    #[must_use]
    pub fn has_transfer_request(&self) -> bool {
        !self.transfer_args.is_empty()
    }

    /// Returns whether symlinks should be materialised as their referents.
    #[must_use]
    #[doc(alias = "--copy-links")]
    #[doc(alias = "-L")]
    pub const fn copy_links(&self) -> bool {
        self.copy_links
    }

    /// Returns whether symlinks that target directories should be traversed as directories.
    #[must_use]
    #[doc(alias = "--copy-dirlinks")]
    #[doc(alias = "-k")]
    pub const fn copy_dirlinks(&self) -> bool {
        self.copy_dirlinks
    }

    /// Returns whether unsafe symlinks should be materialised as their referents.
    #[must_use]
    #[doc(alias = "--copy-unsafe-links")]
    pub const fn copy_unsafe_links(&self) -> bool {
        self.copy_unsafe_links
    }

    /// Returns whether existing destination directory symlinks should be preserved.
    #[must_use]
    #[doc(alias = "--keep-dirlinks")]
    pub const fn keep_dirlinks(&self) -> bool {
        self.keep_dirlinks
    }

    /// Reports whether unsafe symlinks should be ignored (`--safe-links`).
    #[must_use]
    #[doc(alias = "--safe-links")]
    pub const fn safe_links(&self) -> bool {
        self.safe_links
    }

    /// Returns the ordered list of filter rules supplied by the caller.
    #[must_use]
    pub fn filter_rules(&self) -> &[FilterRuleSpec] {
        &self.filter_rules
    }

    /// Returns the debug categories requested via `--debug`.
    #[must_use]
    #[doc(alias = "--debug")]
    pub fn debug_flags(&self) -> &[OsString] {
        &self.debug_flags
    }

    /// Returns the configured transfer timeout.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub const fn timeout(&self) -> TransferTimeout {
        self.timeout
    }

    /// Returns the configured connection timeout.
    #[must_use]
    #[doc(alias = "--contimeout")]
    pub const fn connect_timeout(&self) -> TransferTimeout {
        self.connect_timeout
    }

    /// Returns whether the run should avoid mutating the destination filesystem.
    #[must_use]
    #[doc(alias = "--dry-run")]
    #[doc(alias = "-n")]
    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Returns the configured deletion mode.
    #[must_use]
    pub const fn delete_mode(&self) -> DeleteMode {
        self.delete_mode
    }

    /// Returns whether extraneous destination files should be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(&self) -> bool {
        self.delete_mode.is_enabled()
    }

    /// Returns whether extraneous entries should be removed before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(&self) -> bool {
        matches!(self.delete_mode, DeleteMode::Before)
    }

    /// Returns whether extraneous entries should be removed after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(&self) -> bool {
        matches!(self.delete_mode, DeleteMode::After)
    }

    /// Returns whether extraneous entries should be removed after transfers using delayed sweeps.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(&self) -> bool {
        matches!(self.delete_mode, DeleteMode::Delay)
    }

    /// Returns whether excluded destination entries should also be deleted.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(&self) -> bool {
        self.delete_excluded
    }

    /// Returns the configured maximum number of deletions, if any.
    #[must_use]
    #[doc(alias = "--max-delete")]
    pub const fn max_delete(&self) -> Option<u64> {
        self.max_delete
    }

    /// Returns the minimum file size filter, if configured.
    #[must_use]
    #[doc(alias = "--min-size")]
    pub const fn min_file_size(&self) -> Option<u64> {
        self.min_file_size
    }

    /// Returns the maximum file size filter, if configured.
    #[must_use]
    #[doc(alias = "--max-size")]
    pub const fn max_file_size(&self) -> Option<u64> {
        self.max_file_size
    }

    /// Returns the modification time tolerance, if configured.
    #[must_use]
    #[doc(alias = "--modify-window")]
    pub const fn modify_window(&self) -> Option<u64> {
        self.modify_window
    }

    /// Returns the modification time tolerance as a [`Duration`].
    #[must_use]
    pub fn modify_window_duration(&self) -> Duration {
        self.modify_window
            .map(Duration::from_secs)
            .unwrap_or(Duration::ZERO)
    }

    /// Returns whether the sender should remove source files after transfer.
    #[must_use]
    #[doc(alias = "--remove-source-files")]
    #[doc(alias = "--remove-sent-files")]
    pub const fn remove_source_files(&self) -> bool {
        self.remove_source_files
    }

    /// Returns the requested bandwidth limit, if any.
    #[must_use]
    pub fn bandwidth_limit(&self) -> Option<BandwidthLimit> {
        self.bandwidth_limit
    }

    /// Reports whether ownership preservation was requested.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn preserve_owner(&self) -> bool {
        self.preserve_owner
    }

    /// Returns the configured ownership override, if any.
    #[must_use]
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports whether group preservation was requested.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn preserve_group(&self) -> bool {
        self.preserve_group
    }

    /// Returns the configured group override, if any.
    #[must_use]
    pub const fn group_override(&self) -> Option<u32> {
        self.group_override
    }

    /// Returns the configured chmod modifiers, if any.
    #[must_use]
    #[doc(alias = "--chmod")]
    pub fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn preserve_permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn preserve_times(&self) -> bool {
        self.preserve_times
    }

    /// Reports whether directory timestamps should be skipped when preserving times.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(&self) -> bool {
        self.omit_dir_times
    }

    /// Indicates whether symbolic link modification times should be skipped.
    #[must_use]
    #[doc(alias = "--omit-link-times")]
    pub const fn omit_link_times(&self) -> bool {
        self.omit_link_times
    }

    /// Reports whether POSIX ACLs should be preserved.
    #[cfg(feature = "acl")]
    #[must_use]
    #[doc(alias = "--acls")]
    #[doc(alias = "-A")]
    pub const fn preserve_acls(&self) -> bool {
        self.preserve_acls
    }

    /// Reports whether compression was requested for transfers.
    #[must_use]
    #[doc(alias = "--compress")]
    #[doc(alias = "-z")]
    pub const fn compress(&self) -> bool {
        self.compress
    }

    /// Returns the configured compression level override, if any.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_level(&self) -> Option<CompressionLevel> {
        self.compression_level
    }

    /// Returns the compression setting that should apply when compression is enabled.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_setting(&self) -> CompressionSetting {
        self.compression_setting
    }

    /// Returns the suffix list that disables compression for matching files.
    pub fn skip_compress(&self) -> &SkipCompressList {
        &self.skip_compress
    }

    /// Reports whether whole-file transfers should be used.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "-W")]
    #[doc(alias = "--no-whole-file")]
    pub const fn whole_file(&self) -> bool {
        self.whole_file
    }

    /// Reports whether extended attributes should be preserved.
    #[cfg(feature = "xattr")]
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
    }

    /// Reports whether strong checksum comparison should be used when evaluating updates.
    #[must_use]
    #[doc(alias = "--checksum")]
    pub const fn checksum(&self) -> bool {
        self.checksum
    }

    /// Returns the negotiated strong checksum choice.
    #[must_use]
    #[doc(alias = "--checksum-choice")]
    pub const fn checksum_choice(&self) -> StrongChecksumChoice {
        self.checksum_choice
    }

    /// Returns the strong checksum algorithm applied during local validation.
    #[must_use]
    pub const fn checksum_signature_algorithm(&self) -> SignatureAlgorithm {
        let algorithm = self.checksum_choice.file_signature_algorithm();
        match (algorithm, self.checksum_seed) {
            (SignatureAlgorithm::Xxh64 { .. }, Some(seed)) => {
                SignatureAlgorithm::Xxh64 { seed: seed as u64 }
            }
            (SignatureAlgorithm::Xxh3 { .. }, Some(seed)) => {
                SignatureAlgorithm::Xxh3 { seed: seed as u64 }
            }
            (SignatureAlgorithm::Xxh3_128 { .. }, Some(seed)) => {
                SignatureAlgorithm::Xxh3_128 { seed: seed as u64 }
            }
            (other, _) => other,
        }
    }

    /// Returns the checksum seed configured via `--checksum-seed`, if any.
    #[must_use]
    #[doc(alias = "--checksum-seed")]
    pub const fn checksum_seed(&self) -> Option<u32> {
        self.checksum_seed
    }

    /// Reports whether size-only change detection should be used when evaluating updates.
    #[must_use]
    #[doc(alias = "--size-only")]
    pub const fn size_only(&self) -> bool {
        self.size_only
    }

    /// Returns whether existing destination files should be skipped.
    #[must_use]
    pub const fn ignore_existing(&self) -> bool {
        self.ignore_existing
    }

    /// Returns whether missing source arguments should be ignored.
    #[must_use]
    #[doc(alias = "--ignore-missing-args")]
    pub const fn ignore_missing_args(&self) -> bool {
        self.ignore_missing_args
    }

    /// Reports whether files newer on the destination should be preserved.
    #[must_use]
    #[doc(alias = "--update")]
    #[doc(alias = "-u")]
    pub const fn update(&self) -> bool {
        self.update
    }

    /// Reports whether numeric UID/GID values should be preserved.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(&self) -> bool {
        self.numeric_ids
    }

    /// Returns whether hard links should be preserved when copying files.
    #[must_use]
    #[doc(alias = "--hard-links")]
    pub const fn preserve_hard_links(&self) -> bool {
        self.preserve_hard_links
    }

    /// Reports whether destination files should be preallocated before writing.
    #[doc(alias = "--preallocate")]
    pub const fn preallocate(&self) -> bool {
        self.preallocate
    }

    /// Reports whether sparse file handling has been requested.
    #[must_use]
    #[doc(alias = "--sparse")]
    pub const fn sparse(&self) -> bool {
        self.sparse
    }

    /// Reports whether device nodes should be preserved during the transfer.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn preserve_devices(&self) -> bool {
        self.preserve_devices
    }

    /// Reports whether special files such as FIFOs should be preserved.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn preserve_specials(&self) -> bool {
        self.preserve_specials
    }

    /// Reports whether relative path preservation was requested.
    #[must_use]
    #[doc(alias = "--relative")]
    #[doc(alias = "-R")]
    pub const fn relative_paths(&self) -> bool {
        self.relative_paths
    }

    /// Reports whether traversal should remain on a single filesystem.
    #[must_use]
    #[doc(alias = "--one-file-system")]
    #[doc(alias = "-x")]
    pub const fn one_file_system(&self) -> bool {
        self.one_file_system
    }

    /// Returns whether parent directories implied by the source path should be created.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub const fn implied_dirs(&self) -> bool {
        self.implied_dirs
    }

    /// Returns whether destination path components should be created when missing.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath(&self) -> bool {
        self.mkpath
    }

    /// Returns whether empty directories should be pruned after filtering.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(&self) -> bool {
        self.prune_empty_dirs
    }

    /// Returns the requested verbosity level.
    #[must_use]
    #[doc(alias = "--verbose")]
    #[doc(alias = "-v")]
    pub const fn verbosity(&self) -> u8 {
        self.verbosity
    }

    /// Reports whether progress output was requested.
    #[must_use]
    #[doc(alias = "--progress")]
    pub const fn progress(&self) -> bool {
        self.progress
    }

    /// Reports whether a statistics summary should be emitted after the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub const fn stats(&self) -> bool {
        self.stats
    }

    /// Reports whether human-readable formatting should be applied to byte counts.
    #[must_use]
    #[doc(alias = "--human-readable")]
    pub const fn human_readable(&self) -> bool {
        self.human_readable
    }

    /// Reports whether partial transfers were requested.
    #[must_use]
    #[doc(alias = "--partial")]
    #[doc(alias = "-P")]
    pub const fn partial(&self) -> bool {
        self.partial
    }

    /// Reports whether updates should be delayed until after the transfer completes.
    #[must_use]
    #[doc(alias = "--delay-updates")]
    pub const fn delay_updates(&self) -> bool {
        self.delay_updates
    }

    /// Returns the optional directory used to store partial files.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn partial_directory(&self) -> Option<&Path> {
        self.partial_dir.as_deref()
    }

    /// Returns the configured temporary directory used for staged updates.
    #[doc(alias = "--temp-dir")]
    #[doc(alias = "--tmp-dir")]
    pub fn temp_directory(&self) -> Option<&Path> {
        self.temp_directory.as_deref()
    }

    /// Returns the ordered list of link-destination directories supplied by the caller.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn link_dest_paths(&self) -> &[PathBuf] {
        &self.link_dest_paths
    }

    /// Reports whether destination updates should be performed in place.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(&self) -> bool {
        self.inplace
    }

    /// Reports whether appended transfers are enabled.
    #[must_use]
    #[doc(alias = "--append")]
    pub const fn append(&self) -> bool {
        self.append
    }

    /// Reports whether append verification is enabled.
    #[must_use]
    #[doc(alias = "--append-verify")]
    pub const fn append_verify(&self) -> bool {
        self.append_verify
    }

    /// Reports whether event collection has been explicitly requested by the caller.
    #[must_use]
    pub const fn force_event_collection(&self) -> bool {
        self.force_event_collection
    }

    /// Returns whether the configuration requires collection of transfer events.
    #[must_use]
    pub const fn collect_events(&self) -> bool {
        self.force_event_collection || self.verbosity > 0 || self.progress || self.list_only
    }

    /// Reports whether backups should be created before overwriting or deleting entries.
    #[must_use]
    #[doc(alias = "--backup")]
    pub const fn backup(&self) -> bool {
        self.backup
    }

    /// Returns the configured backup directory when `--backup-dir` is supplied.
    #[must_use]
    #[doc(alias = "--backup-dir")]
    pub fn backup_directory(&self) -> Option<&Path> {
        self.backup_dir.as_deref()
    }

    /// Returns the suffix appended to backup entries when specified.
    #[must_use]
    #[doc(alias = "--suffix")]
    pub fn backup_suffix(&self) -> Option<&OsStr> {
        self.backup_suffix.as_deref()
    }
}

/// Builder used to assemble a [`ClientConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfigBuilder {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete_mode: DeleteMode,
    delete_excluded: bool,
    max_delete: Option<u64>,
    min_file_size: Option<u64>,
    max_file_size: Option<u64>,
    modify_window: Option<u64>,
    remove_source_files: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    chmod: Option<ChmodModifiers>,
    omit_dir_times: bool,
    omit_link_times: bool,
    compress: bool,
    compression_level: Option<CompressionLevel>,
    compression_setting: CompressionSetting,
    skip_compress: SkipCompressList,
    whole_file: Option<bool>,
    checksum: bool,
    checksum_choice: StrongChecksumChoice,
    checksum_seed: Option<u32>,
    size_only: bool,
    ignore_existing: bool,
    ignore_missing_args: bool,
    update: bool,
    numeric_ids: bool,
    preallocate: bool,
    preserve_hard_links: bool,
    filter_rules: Vec<FilterRuleSpec>,
    debug_flags: Vec<OsString>,
    sparse: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    copy_unsafe_links: bool,
    keep_dirlinks: bool,
    safe_links: bool,
    relative_paths: bool,
    one_file_system: bool,
    implied_dirs: Option<bool>,
    mkpath: bool,
    prune_empty_dirs: bool,
    verbosity: u8,
    progress: bool,
    stats: bool,
    human_readable: bool,
    partial: bool,
    partial_dir: Option<PathBuf>,
    temp_directory: Option<PathBuf>,
    backup: bool,
    backup_dir: Option<PathBuf>,
    backup_suffix: Option<OsString>,
    delay_updates: bool,
    inplace: bool,
    append: bool,
    append_verify: bool,
    force_event_collection: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    list_only: bool,
    address_mode: AddressMode,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
    link_dest_paths: Vec<PathBuf>,
    reference_directories: Vec<ReferenceDirectory>,
    connect_program: Option<OsString>,
    bind_address: Option<BindAddress>,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
}

impl ClientConfigBuilder {
    /// Sets the transfer arguments that should be propagated to the engine.
    #[must_use]
    pub fn transfer_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.transfer_args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Enables or disables dry-run mode.
    #[must_use]
    #[doc(alias = "--dry-run")]
    #[doc(alias = "-n")]
    pub const fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Enables or disables list-only mode, mirroring `--list-only`.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only(mut self, list_only: bool) -> Self {
        self.list_only = list_only;
        self
    }

    /// Configures the local bind address applied to network transports.
    #[must_use]
    #[doc(alias = "--address")]
    pub fn bind_address(mut self, address: Option<BindAddress>) -> Self {
        self.bind_address = address;
        self
    }

    /// Enables or disables deletion of extraneous destination files.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete_mode = if delete {
            DeleteMode::During
        } else {
            DeleteMode::Disabled
        };
        self
    }

    /// Requests deletion of extraneous entries before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(mut self, delete_before: bool) -> Self {
        if delete_before {
            self.delete_mode = DeleteMode::Before;
        } else if matches!(self.delete_mode, DeleteMode::Before) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Requests deletion of extraneous entries while directories are processed.
    #[must_use]
    #[doc(alias = "--delete-during")]
    pub const fn delete_during(mut self) -> Self {
        self.delete_mode = DeleteMode::During;
        self
    }

    /// Enables deletion of extraneous entries after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(mut self, delete_after: bool) -> Self {
        if delete_after {
            self.delete_mode = DeleteMode::After;
        } else if matches!(self.delete_mode, DeleteMode::After) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Requests delayed deletion sweeps that run after transfers complete.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(mut self, delete_delay: bool) -> Self {
        if delete_delay {
            self.delete_mode = DeleteMode::Delay;
        } else if matches!(self.delete_mode, DeleteMode::Delay) {
            self.delete_mode = DeleteMode::Disabled;
        }
        self
    }

    /// Enables or disables deletion of excluded destination entries.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(mut self, delete: bool) -> Self {
        self.delete_excluded = delete;
        self
    }

    /// Sets the maximum number of deletions permitted during execution.
    #[must_use]
    #[doc(alias = "--max-delete")]
    pub const fn max_delete(mut self, limit: Option<u64>) -> Self {
        self.max_delete = limit;
        self
    }

    /// Sets the minimum file size to transfer.
    #[must_use]
    #[doc(alias = "--min-size")]
    pub const fn min_file_size(mut self, limit: Option<u64>) -> Self {
        self.min_file_size = limit;
        self
    }

    /// Sets the maximum file size to transfer.
    #[must_use]
    #[doc(alias = "--max-size")]
    pub const fn max_file_size(mut self, limit: Option<u64>) -> Self {
        self.max_file_size = limit;
        self
    }

    /// Sets the modification time tolerance used when comparing files.
    #[must_use]
    #[doc(alias = "--modify-window")]
    pub const fn modify_window(mut self, window: Option<u64>) -> Self {
        self.modify_window = window;
        self
    }

    /// Adds a `--compare-dest` reference directory consulted during execution.
    #[must_use]
    #[doc(alias = "--compare-dest")]
    pub fn compare_destination<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.reference_directories.push(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            path,
        ));
        self
    }

    /// Adds a `--copy-dest` reference directory consulted during execution.
    #[must_use]
    #[doc(alias = "--copy-dest")]
    pub fn copy_destination<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.reference_directories
            .push(ReferenceDirectory::new(ReferenceDirectoryKind::Copy, path));
        self
    }

    /// Adds a `--link-dest` reference directory consulted during execution.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn link_destination<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.reference_directories
            .push(ReferenceDirectory::new(ReferenceDirectoryKind::Link, path));
        self
    }

    /// Enables or disables removal of source files after a successful transfer.
    #[must_use]
    #[doc(alias = "--remove-source-files")]
    #[doc(alias = "--remove-sent-files")]
    pub const fn remove_source_files(mut self, remove: bool) -> Self {
        self.remove_source_files = remove;
        self
    }

    /// Requests that destination files be preallocated before writing begins.
    #[must_use]
    #[doc(alias = "--preallocate")]
    pub const fn preallocate(mut self, preallocate: bool) -> Self {
        self.preallocate = preallocate;
        self
    }

    /// Enables or disables preservation of hard links between files.
    #[must_use]
    #[doc(alias = "--hard-links")]
    pub const fn hard_links(mut self, preserve: bool) -> Self {
        self.preserve_hard_links = preserve;
        self
    }

    /// Configures the optional bandwidth limit to apply during transfers.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub fn bandwidth_limit(mut self, limit: Option<BandwidthLimit>) -> Self {
        self.bandwidth_limit = limit;
        self
    }

    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    /// Applies an explicit ownership override using numeric identifiers.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Requests that group metadata be preserved.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Applies an explicit group override using numeric identifiers.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
        self
    }

    /// Applies chmod modifiers that should be evaluated after metadata preservation.
    #[must_use]
    #[doc(alias = "--chmod")]
    pub fn chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Requests that permissions be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Requests that timestamps be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn times(mut self, preserve: bool) -> Self {
        self.preserve_times = preserve;
        self
    }

    /// Requests that directory timestamps be skipped when preserving times.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(mut self, omit: bool) -> Self {
        self.omit_dir_times = omit;
        self
    }

    /// Controls whether symbolic link modification times should be preserved.
    #[must_use]
    #[doc(alias = "--omit-link-times")]
    pub const fn omit_link_times(mut self, omit: bool) -> Self {
        self.omit_link_times = omit;
        self
    }

    #[cfg(feature = "acl")]
    /// Enables or disables POSIX ACL preservation when applying metadata.
    #[must_use]
    #[doc(alias = "--acls")]
    #[doc(alias = "-A")]
    pub const fn acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Enables or disables compression for the transfer.
    #[must_use]
    #[doc(alias = "--compress")]
    #[doc(alias = "--no-compress")]
    #[doc(alias = "-z")]
    pub const fn compress(mut self, compress: bool) -> Self {
        self.compress = compress;
        if compress && self.compression_setting.is_disabled() {
            self.compression_setting = CompressionSetting::level(CompressionLevel::Default);
        } else {
            self.compression_setting = CompressionSetting::disabled();
            self.compression_level = None;
        }
        self
    }

    /// Applies an explicit compression level override when building the configuration.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_level(mut self, level: Option<CompressionLevel>) -> Self {
        self.compression_level = level;
        if let Some(level) = level {
            self.compression_setting = CompressionSetting::level(level);
            self.compress = true;
        }
        self
    }

    /// Sets the compression level that should apply when compression is enabled.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn compression_setting(mut self, setting: CompressionSetting) -> Self {
        self.compression_setting = setting;
        self.compress = setting.is_enabled();
        if !self.compress {
            self.compression_level = None;
        }
        self
    }

    /// Overrides the suffix list used to disable compression for specific extensions.
    #[must_use]
    #[doc(alias = "--skip-compress")]
    pub fn skip_compress(mut self, list: SkipCompressList) -> Self {
        self.skip_compress = list;
        self
    }

    /// Requests that whole-file transfers be used instead of the delta algorithm.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "-W")]
    #[doc(alias = "--no-whole-file")]
    pub fn whole_file(mut self, whole_file: bool) -> Self {
        self.whole_file = Some(whole_file);
        self
    }

    /// Enables or disables checksum-based change detection.
    #[must_use]
    #[doc(alias = "--checksum")]
    #[doc(alias = "-c")]
    pub const fn checksum(mut self, checksum: bool) -> Self {
        self.checksum = checksum;
        self
    }

    /// Overrides the strong checksum selection used during validation.
    #[must_use]
    #[doc(alias = "--checksum-choice")]
    pub const fn checksum_choice(mut self, choice: StrongChecksumChoice) -> Self {
        self.checksum_choice = choice;
        self
    }

    /// Configures the checksum seed forwarded to the engine and fallback binary.
    #[must_use]
    #[doc(alias = "--checksum-seed")]
    pub const fn checksum_seed(mut self, seed: Option<u32>) -> Self {
        self.checksum_seed = seed;
        self
    }

    /// Enables or disables size-only change detection.
    #[must_use]
    #[doc(alias = "--size-only")]
    pub const fn size_only(mut self, size_only: bool) -> Self {
        self.size_only = size_only;
        self
    }

    /// Enables or disables skipping of existing destination files.
    #[must_use]
    #[doc(alias = "--ignore-existing")]
    pub const fn ignore_existing(mut self, ignore_existing: bool) -> Self {
        self.ignore_existing = ignore_existing;
        self
    }

    /// Enables or disables ignoring missing source arguments.
    #[must_use]
    #[doc(alias = "--ignore-missing-args")]
    pub const fn ignore_missing_args(mut self, ignore: bool) -> Self {
        self.ignore_missing_args = ignore;
        self
    }

    /// Enables or disables preservation of newer destination files.
    #[must_use]
    #[doc(alias = "--update")]
    #[doc(alias = "-u")]
    pub const fn update(mut self, update: bool) -> Self {
        self.update = update;
        self
    }

    /// Requests that numeric UID/GID values be preserved instead of names.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric_ids: bool) -> Self {
        self.numeric_ids = numeric_ids;
        self
    }

    /// Enables or disables sparse file handling for the transfer.
    #[must_use]
    #[doc(alias = "--sparse")]
    #[doc(alias = "-S")]
    pub const fn sparse(mut self, sparse: bool) -> Self {
        self.sparse = sparse;
        self
    }

    /// Enables or disables copying symlink referents.
    #[must_use]
    #[doc(alias = "--copy-links")]
    #[doc(alias = "-L")]
    pub const fn copy_links(mut self, copy_links: bool) -> Self {
        self.copy_links = copy_links;
        self
    }

    /// Enables or disables copying unsafe symlink referents.
    #[must_use]
    #[doc(alias = "--copy-unsafe-links")]
    pub const fn copy_unsafe_links(mut self, copy_unsafe_links: bool) -> Self {
        self.copy_unsafe_links = copy_unsafe_links;
        self
    }

    /// Enables treating symlinks that target directories as directories during traversal.
    #[must_use]
    #[doc(alias = "--copy-dirlinks")]
    #[doc(alias = "-k")]
    pub const fn copy_dirlinks(mut self, copy_dirlinks: bool) -> Self {
        self.copy_dirlinks = copy_dirlinks;
        self
    }

    /// Preserves existing destination symlinks that refer to directories.
    #[must_use]
    #[doc(alias = "--keep-dirlinks")]
    pub const fn keep_dirlinks(mut self, keep_dirlinks: bool) -> Self {
        self.keep_dirlinks = keep_dirlinks;
        self
    }

    /// Enables or disables skipping unsafe symlinks.
    #[must_use]
    #[doc(alias = "--safe-links")]
    pub const fn safe_links(mut self, safe_links: bool) -> Self {
        self.safe_links = safe_links;
        self
    }

    /// Enables or disables copying of device nodes during the transfer.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn devices(mut self, preserve: bool) -> Self {
        self.preserve_devices = preserve;
        self
    }

    /// Enables or disables copying of special files during the transfer.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn specials(mut self, preserve: bool) -> Self {
        self.preserve_specials = preserve;
        self
    }

    /// Enables or disables preservation of source-relative path components.
    #[must_use]
    #[doc(alias = "--relative")]
    #[doc(alias = "-R")]
    pub const fn relative_paths(mut self, relative: bool) -> Self {
        self.relative_paths = relative;
        self
    }

    /// Enables or disables traversal across filesystem boundaries.
    #[must_use]
    #[doc(alias = "--one-file-system")]
    #[doc(alias = "-x")]
    pub const fn one_file_system(mut self, enabled: bool) -> Self {
        self.one_file_system = enabled;
        self
    }

    /// Enables or disables creation of parent directories implied by the source path.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub fn implied_dirs(mut self, implied: bool) -> Self {
        self.implied_dirs = Some(implied);
        self
    }

    /// Enables destination path creation prior to copying.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath(mut self, mkpath: bool) -> Self {
        self.mkpath = mkpath;
        self
    }

    /// Enables or disables pruning of empty directories after filters apply.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(mut self, prune: bool) -> Self {
        self.prune_empty_dirs = prune;
        self
    }

    /// Sets the verbosity level requested by the caller.
    #[must_use]
    #[doc(alias = "--verbose")]
    #[doc(alias = "-v")]
    pub const fn verbosity(mut self, verbosity: u8) -> Self {
        self.verbosity = verbosity;
        self
    }

    /// Enables or disables progress reporting for the transfer.
    #[must_use]
    #[doc(alias = "--progress")]
    #[doc(alias = "--no-progress")]
    pub const fn progress(mut self, progress: bool) -> Self {
        self.progress = progress;
        self
    }

    /// Enables or disables statistics reporting for the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub const fn stats(mut self, stats: bool) -> Self {
        self.stats = stats;
        self
    }

    /// Enables or disables human-readable output formatting.
    #[must_use]
    #[doc(alias = "--human-readable")]
    pub const fn human_readable(mut self, enabled: bool) -> Self {
        self.human_readable = enabled;
        self
    }

    /// Enables or disables retention of partial files on failure.
    #[must_use]
    #[doc(alias = "--partial")]
    #[doc(alias = "--no-partial")]
    #[doc(alias = "-P")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// Enables or disables delayed update commits, mirroring `--delay-updates`.
    #[must_use]
    #[doc(alias = "--delay-updates")]
    pub const fn delay_updates(mut self, delay: bool) -> Self {
        self.delay_updates = delay;
        self
    }

    /// Enables or disables creation of backups before overwriting or deleting entries.
    #[must_use]
    #[doc(alias = "--backup")]
    #[doc(alias = "-b")]
    pub const fn backup(mut self, backup: bool) -> Self {
        self.backup = backup;
        self
    }

    /// Configures the optional directory that should receive backup entries.
    #[must_use]
    #[doc(alias = "--backup-dir")]
    pub fn backup_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.backup_dir = directory.map(Into::into);
        if self.backup_dir.is_some() {
            self.backup = true;
        }
        self
    }

    /// Overrides the suffix appended to backup file names.
    #[must_use]
    #[doc(alias = "--suffix")]
    pub fn backup_suffix<S: Into<OsString>>(mut self, suffix: Option<S>) -> Self {
        self.backup_suffix = suffix.map(Into::into);
        if self.backup_suffix.is_some() {
            self.backup = true;
        }
        self
    }

    /// Configures the directory used to store partial files when transfers fail.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn partial_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.partial_dir = directory.map(Into::into);
        if self.partial_dir.is_some() {
            self.partial = true;
        }
        self
    }

    /// Configures the directory used for temporary files when staging updates.
    #[must_use]
    #[doc(alias = "--temp-dir")]
    #[doc(alias = "--tmp-dir")]
    pub fn temp_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.temp_directory = directory.map(Into::into);
        self
    }

    /// Extends the link-destination list used when creating hard links for unchanged files.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn extend_link_dests<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.link_dest_paths.extend(
            paths
                .into_iter()
                .map(Into::into)
                .filter(|path| !path.as_os_str().is_empty()),
        );
        self
    }

    /// Enables or disables in-place updates for destination files.
    #[must_use]
    #[doc(alias = "--inplace")]
    #[doc(alias = "--no-inplace")]
    pub const fn inplace(mut self, inplace: bool) -> Self {
        self.inplace = inplace;
        self
    }

    /// Enables append-only transfers for existing destination files.
    #[must_use]
    #[doc(alias = "--append")]
    pub const fn append(mut self, append: bool) -> Self {
        self.append = append;
        if !append {
            self.append_verify = false;
        }
        self
    }

    /// Enables append verification for existing destination files.
    #[must_use]
    #[doc(alias = "--append-verify")]
    pub const fn append_verify(mut self, verify: bool) -> Self {
        if verify {
            self.append = true;
            self.append_verify = true;
        } else {
            self.append_verify = false;
        }
        self
    }

    /// Forces collection of transfer events regardless of verbosity.
    #[must_use]
    pub const fn force_event_collection(mut self, force: bool) -> Self {
        self.force_event_collection = force;
        self
    }

    /// Enables or disables extended attribute preservation for the transfer.
    #[cfg(feature = "xattr")]
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
        self
    }

    /// Replaces the collected debug flags with the provided list.
    #[must_use]
    #[doc(alias = "--debug")]
    pub fn debug_flags<I, S>(mut self, flags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.debug_flags = flags.into_iter().map(Into::into).collect();
        self
    }

    /// Appends a filter rule to the configuration being constructed.
    #[must_use]
    pub fn add_filter_rule(mut self, rule: FilterRuleSpec) -> Self {
        self.filter_rules.push(rule);
        self
    }

    /// Extends the builder with a collection of filter rules.
    #[must_use]
    pub fn extend_filter_rules<I>(mut self, rules: I) -> Self
    where
        I: IntoIterator<Item = FilterRuleSpec>,
    {
        self.filter_rules.extend(rules);
        self
    }

    /// Sets the timeout configuration that should apply to network transfers.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub const fn timeout(mut self, timeout: TransferTimeout) -> Self {
        self.timeout = timeout;
        self
    }

    /// Configures the connection timeout applied to network handshakes.
    #[must_use]
    #[doc(alias = "--contimeout")]
    pub const fn connect_timeout(mut self, timeout: TransferTimeout) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Configures the command used to reach rsync:// daemons.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn connect_program(mut self, program: Option<OsString>) -> Self {
        self.connect_program = program;
        self
    }

    /// Selects the preferred address family for network operations.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn address_mode(mut self, mode: AddressMode) -> Self {
        self.address_mode = mode;
        self
    }

    /// Finalises the builder and constructs a [`ClientConfig`].
    #[must_use]
    pub fn build(self) -> ClientConfig {
        ClientConfig {
            transfer_args: self.transfer_args,
            dry_run: self.dry_run,
            delete_mode: self.delete_mode,
            delete_excluded: self.delete_excluded,
            max_delete: self.max_delete,
            min_file_size: self.min_file_size,
            max_file_size: self.max_file_size,
            modify_window: self.modify_window,
            remove_source_files: self.remove_source_files,
            bandwidth_limit: self.bandwidth_limit,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
            owner_override: self.owner_override,
            group_override: self.group_override,
            chmod: self.chmod,
            omit_dir_times: self.omit_dir_times,
            omit_link_times: self.omit_link_times,
            compress: self.compress,
            compression_level: self.compression_level,
            compression_setting: self.compression_setting,
            skip_compress: self.skip_compress,
            whole_file: self.whole_file.unwrap_or(true),
            checksum: self.checksum,
            checksum_choice: self.checksum_choice,
            checksum_seed: self.checksum_seed,
            size_only: self.size_only,
            ignore_existing: self.ignore_existing,
            ignore_missing_args: self.ignore_missing_args,
            update: self.update,
            numeric_ids: self.numeric_ids,
            preallocate: self.preallocate,
            preserve_hard_links: self.preserve_hard_links,
            filter_rules: self.filter_rules,
            debug_flags: self.debug_flags,
            sparse: self.sparse,
            copy_links: self.copy_links,
            copy_dirlinks: self.copy_dirlinks,
            copy_unsafe_links: self.copy_unsafe_links,
            keep_dirlinks: self.keep_dirlinks,
            safe_links: self.safe_links,
            relative_paths: self.relative_paths,
            one_file_system: self.one_file_system,
            implied_dirs: self.implied_dirs.unwrap_or(true),
            mkpath: self.mkpath,
            prune_empty_dirs: self.prune_empty_dirs,
            verbosity: self.verbosity,
            progress: self.progress,
            stats: self.stats,
            human_readable: self.human_readable,
            partial: self.partial,
            partial_dir: self.partial_dir,
            temp_directory: self.temp_directory,
            backup: self.backup,
            backup_dir: self.backup_dir,
            backup_suffix: self.backup_suffix,
            delay_updates: self.delay_updates,
            inplace: self.inplace,
            append: self.append,
            append_verify: self.append_verify,
            force_event_collection: self.force_event_collection,
            preserve_devices: self.preserve_devices,
            preserve_specials: self.preserve_specials,
            list_only: self.list_only,
            address_mode: self.address_mode,
            timeout: self.timeout,
            connect_timeout: self.connect_timeout,
            link_dest_paths: self.link_dest_paths,
            reference_directories: self.reference_directories,
            connect_program: self.connect_program,
            bind_address: self.bind_address,
            #[cfg(feature = "acl")]
            preserve_acls: self.preserve_acls,
            #[cfg(feature = "xattr")]
            preserve_xattrs: self.preserve_xattrs,
        }
    }
}

/// Parses a `--skip-compress` specification into a [`SkipCompressList`].
pub fn parse_skip_compress_list(value: &OsStr) -> Result<SkipCompressList, Message> {
    let text = value.to_str().ok_or_else(|| {
        rsync_error!(
            1,
            "--skip-compress accepts only UTF-8 patterns in this build"
        )
        .with_role(Role::Client)
    })?;

    SkipCompressList::parse(text).map_err(|error| {
        rsync_error!(1, format!("invalid --skip-compress specification: {error}"))
            .with_role(Role::Client)
    })
}

/// Parses the `RSYNC_SKIP_COMPRESS` environment variable into a
/// [`SkipCompressList`].
///
/// Returning [`Ok(None)`] indicates that the variable was unset, allowing
/// callers to retain their default skip-compress configuration. When the
/// variable is present but empty the function returns an empty list, matching
/// upstream rsync's semantics where an explicitly empty list disables the
/// optimisation altogether.
pub fn skip_compress_from_env(variable: &str) -> Result<Option<SkipCompressList>, Message> {
    let Some(value) = env::var_os(variable) else {
        return Ok(None);
    };

    let text = value.to_str().ok_or_else(|| {
        rsync_error!(
            1,
            format!("{variable} accepts only UTF-8 patterns in this build")
        )
        .with_role(Role::Client)
    })?;

    SkipCompressList::parse(text).map(Some).map_err(|error| {
        rsync_error!(1, format!("invalid {variable} specification: {error}"))
            .with_role(Role::Client)
    })
}

/// Classifies a filter rule as inclusive or exclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterRuleKind {
    /// Include matching paths.
    Include,
    /// Exclude matching paths.
    Exclude,
    /// Clear all previously defined filter rules.
    Clear,
    /// Protect matching destination paths from deletion.
    Protect,
    /// Remove protection for matching destination paths.
    Risk,
    /// Merge per-directory filter rules from `.rsync-filter` style files.
    DirMerge,
    /// Exclude directories containing a specific marker file.
    ExcludeIfPresent,
}

/// Filter rule supplied by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRuleSpec {
    kind: FilterRuleKind,
    pattern: String,
    dir_merge_options: Option<DirMergeOptions>,
    applies_to_sender: bool,
    applies_to_receiver: bool,
}

impl FilterRuleSpec {
    /// Creates an include rule for the given pattern text.
    #[must_use]
    #[doc(alias = "show")]
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Include,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates an exclude rule for the given pattern text.
    #[must_use]
    #[doc(alias = "hide")]
    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Exclude,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Clears all previously configured filter rules.
    #[must_use]
    #[doc(alias = "!")]
    pub fn clear() -> Self {
        Self {
            kind: FilterRuleKind::Clear,
            pattern: String::new(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates a protect rule for the given pattern text.
    #[must_use]
    pub fn protect(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Protect,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: false,
            applies_to_receiver: true,
        }
    }

    /// Creates a receiver-only risk rule equivalent to `risk PATTERN` or `R PATTERN`.
    #[must_use]
    #[doc(alias = "R")]
    pub fn risk(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Risk,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: false,
            applies_to_receiver: true,
        }
    }

    /// Creates a per-directory merge rule for the provided filter file pattern.
    #[must_use]
    pub fn dir_merge(pattern: impl Into<String>, options: DirMergeOptions) -> Self {
        Self {
            kind: FilterRuleKind::DirMerge,
            pattern: pattern.into(),
            dir_merge_options: Some(options),
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Excludes directories that contain the named marker file.
    #[must_use]
    #[doc(alias = "exclude-if-present")]
    pub fn exclude_if_present(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::ExcludeIfPresent,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: true,
        }
    }

    /// Creates an include rule that only affects the sending side.
    #[must_use]
    pub fn show(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Include,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: false,
        }
    }

    /// Creates an exclude rule that only affects the sending side.
    #[must_use]
    pub fn hide(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Exclude,
            pattern: pattern.into(),
            dir_merge_options: None,
            applies_to_sender: true,
            applies_to_receiver: false,
        }
    }

    /// Returns the rule kind.
    #[must_use]
    pub const fn kind(&self) -> FilterRuleKind {
        self.kind
    }

    /// Returns the pattern associated with this rule.
    ///
    /// For list-clearing rules created via [`FilterRuleSpec::clear`] the
    /// returned string is empty because the variant does not carry a pattern.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Returns the options associated with a dir-merge rule, if any.
    #[must_use]
    pub fn dir_merge_options(&self) -> Option<&DirMergeOptions> {
        self.dir_merge_options.as_ref()
    }

    /// Reports whether the rule applies to the sending side.
    #[must_use]
    pub const fn applies_to_sender(&self) -> bool {
        self.applies_to_sender
    }

    /// Reports whether the rule applies to the receiving side.
    #[must_use]
    pub const fn applies_to_receiver(&self) -> bool {
        self.applies_to_receiver
    }

    /// Applies dir-merge style overrides (anchor, side modifiers) to the rule.
    pub fn apply_dir_merge_overrides(&mut self, options: &DirMergeOptions) {
        if matches!(self.kind, FilterRuleKind::Clear) {
            return;
        }

        if options.anchor_root_enabled() && !self.pattern.starts_with('/') {
            self.pattern.insert(0, '/');
        }

        if let Some(sender) = options.sender_side_override() {
            self.applies_to_sender = sender;
        }

        if let Some(receiver) = options.receiver_side_override() {
            self.applies_to_receiver = receiver;
        }
    }
}

/// Bandwidth limit expressed in bytes per second.
///
/// # Examples
/// ```
/// use rsync_core::client::BandwidthLimit;
/// use std::num::NonZeroU64;
///
/// let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(1024).unwrap());
/// assert_eq!(limit.bytes_per_second().get(), 1024);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BandwidthLimit {
    bytes_per_second: NonZeroU64,
    burst_bytes: Option<NonZeroU64>,
    burst_specified: bool,
}

impl BandwidthLimit {
    const fn new_internal(
        bytes_per_second: NonZeroU64,
        burst: Option<NonZeroU64>,
        burst_specified: bool,
    ) -> Self {
        Self {
            bytes_per_second,
            burst_bytes: burst,
            burst_specified,
        }
    }

    /// Creates a new [`BandwidthLimit`] from the supplied byte-per-second value.
    #[must_use]
    pub const fn from_bytes_per_second(bytes_per_second: NonZeroU64) -> Self {
        Self::new_internal(bytes_per_second, None, false)
    }

    /// Creates a new [`BandwidthLimit`] from a rate and optional burst size.
    #[must_use]
    pub const fn from_rate_and_burst(
        bytes_per_second: NonZeroU64,
        burst: Option<NonZeroU64>,
    ) -> Self {
        Self::new_internal(bytes_per_second, burst, burst.is_some())
    }

    /// Converts parsed [`bandwidth::BandwidthLimitComponents`] into a
    /// [`BandwidthLimit`].
    ///
    /// Returning `None` mirrors upstream rsync's interpretation of `0` as an
    /// unlimited rate. Callers that parse `--bwlimit` arguments can therefore
    /// reuse the shared decoding logic and only materialise a [`BandwidthLimit`]
    /// when throttling is active.
    #[must_use]
    pub const fn from_components(components: bandwidth::BandwidthLimitComponents) -> Option<Self> {
        match components.rate() {
            Some(rate) => Some(Self::new_internal(
                rate,
                components.burst(),
                components.burst_specified(),
            )),
            None => None,
        }
    }

    /// Parses a textual `--bwlimit` value into an optional [`BandwidthLimit`].
    pub fn parse(text: &str) -> Result<Option<Self>, BandwidthParseError> {
        let components = bandwidth::parse_bandwidth_limit(text)?;
        Ok(Self::from_components(components))
    }

    /// Returns the configured rate in bytes per second.
    #[must_use]
    pub const fn bytes_per_second(self) -> NonZeroU64 {
        self.bytes_per_second
    }

    /// Returns the configured burst size in bytes, if any.
    #[must_use]
    pub const fn burst_bytes(self) -> Option<NonZeroU64> {
        self.burst_bytes
    }

    /// Indicates whether a burst component was explicitly specified.
    #[must_use]
    pub const fn burst_specified(self) -> bool {
        self.burst_specified
    }

    /// Produces the shared [`bandwidth::BandwidthLimitComponents`] representation
    /// for this limit.
    ///
    /// The conversion retains both the byte-per-second rate and the optional burst
    /// component so higher layers can forward the configuration to helpers that
    /// operate on the shared parsing type. Returning a dedicated value keeps the
    /// conversion explicit while avoiding the need for callers to reach into the
    /// `bandwidth` crate directly when they already hold a [`BandwidthLimit`].
    #[must_use]
    pub const fn components(&self) -> bandwidth::BandwidthLimitComponents {
        bandwidth::BandwidthLimitComponents::new_with_specified(
            Some(self.bytes_per_second),
            self.burst_bytes,
            self.burst_specified,
        )
    }

    /// Consumes the limit and returns the
    /// [`bandwidth::BandwidthLimitComponents`] representation.
    ///
    /// This by-value variant mirrors [`Self::components`] for callers that want
    /// to forward the components without keeping the original [`BandwidthLimit`]
    /// instance alive.
    #[must_use]
    pub const fn into_components(self) -> bandwidth::BandwidthLimitComponents {
        self.components()
    }

    /// Constructs a [`BandwidthLimiter`] that enforces this configuration.
    ///
    /// The limiter mirrors upstream rsync's token bucket by combining the
    /// configured rate with the optional burst component. Returning a concrete
    /// limiter keeps higher layers from re-encoding the rate/burst tuple when
    /// they need to apply throttling to local copies or daemon transfers.
    #[must_use]
    pub fn to_limiter(&self) -> BandwidthLimiter {
        BandwidthLimiter::with_burst(self.bytes_per_second, self.burst_bytes)
    }

    /// Consumes the limit and produces a [`BandwidthLimiter`].
    ///
    /// This by-value variant mirrors [`Self::to_limiter`] while avoiding the
    /// additional copy of the [`BandwidthLimit`] structure when the caller no
    /// longer needs it.
    #[must_use]
    pub fn into_limiter(self) -> BandwidthLimiter {
        self.to_limiter()
    }

    /// Returns the sanitised `--bwlimit` argument expected by legacy fallbacks.
    ///
    /// When delegating remote transfers to the system `rsync` binary we must
    /// forward the throttling setting using the byte-per-second form accepted by
    /// upstream releases. This helper mirrors the formatting performed by
    /// upstream `rsync` when normalising parsed limits, ensuring fallback
    /// invocations receive identical values.
    #[must_use]
    pub fn fallback_argument(&self) -> OsString {
        let mut value = self.bytes_per_second.get().to_string();
        if self.burst_specified {
            value.push(':');
            value.push_str(
                &self
                    .burst_bytes
                    .map(|burst| burst.get().to_string())
                    .unwrap_or_else(|| "0".to_string()),
            );
        }

        OsString::from(value)
    }

    /// Returns the argument that disables bandwidth limiting for fallbacks.
    #[must_use]
    pub fn fallback_unlimited_argument() -> OsString {
        OsString::from("0")
    }
}

impl From<BandwidthLimit> for bandwidth::BandwidthLimitComponents {
    fn from(limit: BandwidthLimit) -> Self {
        limit.into_components()
    }
}

impl From<&BandwidthLimit> for bandwidth::BandwidthLimitComponents {
    fn from(limit: &BandwidthLimit) -> Self {
        limit.components()
    }
}
