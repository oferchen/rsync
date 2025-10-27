#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `client` module exposes the orchestration entry points consumed by the
//! `oc-rsync` CLI binary. The current implementation focuses on providing a
//! deterministic, synchronous local copy engine that mirrors the high-level
//! behaviour of `rsync SOURCE DEST` when no remote shells or daemons are
//! involved. The API models the configuration and error structures that higher
//! layers will reuse once network transports and the full delta-transfer engine
//! land.
//!
//! # Design
//!
//! - [`ClientConfig`] encapsulates the caller-provided transfer arguments. A
//!   builder is offered so future options (e.g. logging verbosity) can be wired
//!   through without breaking call sites. Today it exposes toggles for dry-run
//!   validation (`--dry-run`) and extraneous-destination cleanup (`--delete`).
//! - [`run_client`] executes the client flow. The helper delegates to
//!   [`rsync_engine::local_copy`] to mirror a simplified subset of upstream
//!   behaviour by copying files, directories, and symbolic links on the local
//!   filesystem while preserving permissions, timestamps, optional
//!   ownership/group metadata, and sparse regions when requested. Delta
//!   compression and advanced metadata such as ACLs or extended attributes
//!   remain out of scope for this snapshot. When remote operands are detected,
//!   the client delegates to the system `rsync` binary so network transfers are
//!   available while the native engine is completed. When
//!   deletion is requested (including [`--delete-excluded`](crate::client::ClientConfig::delete_excluded)),
//!   the helper removes destination entries that are absent from the source tree
//!   before applying metadata and prunes excluded entries when explicitly
//!   requested.
//! - [`ModuleListRequest`] parses daemon-style operands (`rsync://host/` or
//!   `host::`) and [`run_module_list`] connects to the remote daemon using the
//!   legacy `@RSYNCD:` negotiation to retrieve the advertised module table.
//! - [`ClientError`] carries the exit code and fully formatted
//!   [`Message`](crate::message::Message) so binaries can surface diagnostics via
//!   the central rendering helpers.
//!
//! # Invariants
//!
//! - `ClientError::exit_code` always matches the exit code embedded in the
//!   [`Message`].
//! - `run_client` never panics and preserves the provided configuration even
//!   when reporting unsupported functionality.
//!
//! # Errors
//!
//! All failures are routed through [`ClientError`]. The structure implements
//! [`std::error::Error`], allowing integration with higher-level error handling
//! stacks without losing access to the formatted diagnostic.
//!
//! # Examples
//!
//! Running the client with a single source copies the file into the destination
//! path. The helper currently operates entirely on the local filesystem.
//!
//! ```
//! use rsync_core::client::{run_client, ClientConfig};
//! use std::fs;
//! use tempfile::tempdir;
//!
//! let temp = tempdir().unwrap();
//! let source = temp.path().join("source.txt");
//! let destination = temp.path().join("dest.txt");
//! fs::write(&source, b"example").unwrap();
//!
//! let config = ClientConfig::builder()
//!     .transfer_args([source.clone(), destination.clone()])
//!     .build();
//!
//! let summary = run_client(config).expect("local copy succeeds");
//! assert_eq!(summary.files_copied(), 1);
//! assert_eq!(fs::read(&destination).unwrap(), b"example");
//! ```
//!
//! # See also
//!
//! - [`crate::message`] for the formatting utilities reused by the client
//!   orchestration.
//! - [`crate::version`] for the canonical version banner shared with the CLI.
//! - [`rsync_engine::local_copy`] for the transfer plan executed by this module.

use std::collections::HashMap;
use std::env::VarError;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::ErrorKind;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::ToSocketAddrs;
use std::net::{SocketAddr, TcpStream};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{
    Arc,
    mpsc::{self, Sender},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use std::{env, error::Error};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use rsync_checksums::strong::Md5;
use rsync_compress::zlib::{CompressionLevel, CompressionLevelError};
use rsync_engine::SkipCompressList;
pub use rsync_engine::local_copy::{DirMergeEnforcedKind, DirMergeOptions};
use rsync_engine::local_copy::{
    DirMergeRule, ExcludeIfPresentRule, FilterProgram, FilterProgramEntry, LocalCopyAction,
    LocalCopyArgumentError, LocalCopyError, LocalCopyErrorKind, LocalCopyExecution,
    LocalCopyFileKind, LocalCopyMetadata, LocalCopyOptions, LocalCopyPlan, LocalCopyProgress,
    LocalCopyRecord, LocalCopyRecordHandler, LocalCopyReport, LocalCopySummary,
    ReferenceDirectory as EngineReferenceDirectory,
    ReferenceDirectoryKind as EngineReferenceDirectoryKind,
};
use rsync_engine::signature::SignatureAlgorithm;
use rsync_filters::FilterRule as EngineFilterRule;
use rsync_meta::ChmodModifiers;
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX, LegacyDaemonMessage, NegotiationError, ProtocolVersion,
    parse_legacy_daemon_message, parse_legacy_error_message, parse_legacy_warning_message,
};
use rsync_transport::negotiate_legacy_daemon_session;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
#[cfg(test)]
use std::cell::RefCell;
use tempfile::NamedTempFile;

use crate::{
    bandwidth::{self, BandwidthParseError},
    fallback::{FallbackOverride, fallback_override},
    message::{Message, Role},
    rsync_error,
};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// Exit code returned when client functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code used when a copy partially or wholly fails.
const PARTIAL_TRANSFER_EXIT_CODE: i32 = 23;
/// Exit code returned when socket I/O fails.
const SOCKET_IO_EXIT_CODE: i32 = 10;
/// Exit code returned when a daemon violates the protocol.
const PROTOCOL_INCOMPATIBLE_EXIT_CODE: i32 = 2;
/// Exit code returned when the `--max-delete` limit stops deletions.
const MAX_DELETE_EXIT_CODE: i32 = 25;
/// Timeout applied to daemon sockets to avoid hanging handshakes when the caller
/// does not provide an override.
const DAEMON_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum exit code representable in the traditional 8-bit rsync transport.
const MAX_EXIT_CODE: i32 = u8::MAX as i32;

/// Describes the timeout configuration applied to network operations.
///
/// The variant captures whether the caller requested a custom timeout, disabled
/// socket timeouts entirely, or asked to rely on the default for the current
/// operation.  Higher layers convert the setting into concrete [`Duration`]
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
    update: bool,
    numeric_ids: bool,
    preallocate: bool,
    preserve_hard_links: bool,
    filter_rules: Vec<FilterRuleSpec>,
    debug_flags: Vec<OsString>,
    sparse: bool,
    copy_links: bool,
    copy_dirlinks: bool,
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
            update: false,
            numeric_ids: false,
            preallocate: false,
            preserve_hard_links: false,
            filter_rules: Vec::new(),
            debug_flags: Vec::new(),
            sparse: false,
            copy_links: false,
            copy_dirlinks: false,
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
    update: bool,
    numeric_ids: bool,
    preallocate: bool,
    preserve_hard_links: bool,
    filter_rules: Vec<FilterRuleSpec>,
    debug_flags: Vec<OsString>,
    sparse: bool,
    copy_links: bool,
    copy_dirlinks: bool,
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
        if compress {
            if self.compression_setting.is_disabled() {
                self.compression_setting = CompressionSetting::level(CompressionLevel::Default);
            }
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
            update: self.update,
            numeric_ids: self.numeric_ids,
            preallocate: self.preallocate,
            preserve_hard_links: self.preserve_hard_links,
            filter_rules: self.filter_rules,
            debug_flags: self.debug_flags,
            sparse: self.sparse,
            copy_links: self.copy_links,
            copy_dirlinks: self.copy_dirlinks,
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
}

impl BandwidthLimit {
    /// Creates a new [`BandwidthLimit`] from the supplied byte-per-second value.
    #[must_use]
    pub const fn from_bytes_per_second(bytes_per_second: NonZeroU64) -> Self {
        Self::from_rate_and_burst(bytes_per_second, None)
    }

    /// Creates a new [`BandwidthLimit`] from a rate and optional burst size.
    #[must_use]
    pub const fn from_rate_and_burst(
        bytes_per_second: NonZeroU64,
        burst: Option<NonZeroU64>,
    ) -> Self {
        Self {
            bytes_per_second,
            burst_bytes: burst,
        }
    }

    /// Converts parsed [`BandwidthLimitComponents`] into a [`BandwidthLimit`].
    ///
    /// Returning `None` mirrors upstream rsync's interpretation of `0` as an
    /// unlimited rate. Callers that parse `--bwlimit` arguments can therefore
    /// reuse the shared decoding logic and only materialise a [`BandwidthLimit`]
    /// when throttling is active.
    #[must_use]
    pub const fn from_components(components: bandwidth::BandwidthLimitComponents) -> Option<Self> {
        match components.rate() {
            Some(rate) => Some(Self::from_rate_and_burst(rate, components.burst())),
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
}

/// Error returned when the client orchestration fails.
#[derive(Clone, Debug)]
pub struct ClientError {
    exit_code: i32,
    message: Message,
}

impl ClientError {
    /// Creates a new [`ClientError`] from the supplied message.
    fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the formatted diagnostic message that should be emitted.
    pub fn message(&self) -> &Message {
        &self.message
    }
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for ClientError {}

/// Summary of the work performed by [`run_client`].
#[derive(Clone, Debug, Default)]
pub struct ClientSummary {
    stats: LocalCopySummary,
    events: Vec<ClientEvent>,
}

impl ClientSummary {
    fn from_report(report: LocalCopyReport) -> Self {
        let stats = *report.summary();
        let destination_root = Arc::new(report.destination_root().to_path_buf());
        let events = report
            .records()
            .iter()
            .map(|record| ClientEvent::from_record(record, Arc::clone(&destination_root)))
            .collect();
        Self { stats, events }
    }

    fn from_summary(summary: LocalCopySummary) -> Self {
        Self {
            stats: summary,
            events: Vec::new(),
        }
    }

    /// Returns the list of recorded transfer actions.
    #[must_use]
    pub fn events(&self) -> &[ClientEvent] {
        &self.events
    }

    /// Consumes the summary and returns the recorded actions.
    #[must_use]
    pub fn into_events(self) -> Vec<ClientEvent> {
        self.events
    }

    /// Returns the number of regular files copied or updated during the transfer.
    #[must_use]
    pub fn files_copied(&self) -> u64 {
        self.stats.files_copied()
    }

    /// Returns the number of regular files encountered in the source set.
    #[must_use]
    pub fn regular_files_total(&self) -> u64 {
        self.stats.regular_files_total()
    }

    /// Returns the number of regular files that were already up-to-date.
    #[must_use]
    pub fn regular_files_matched(&self) -> u64 {
        self.stats.regular_files_matched()
    }

    /// Returns the number of regular files skipped due to `--ignore-existing`.
    #[must_use]
    pub fn regular_files_ignored_existing(&self) -> u64 {
        self.stats.regular_files_ignored_existing()
    }

    /// Returns the number of regular files skipped because the destination was newer.
    #[must_use]
    pub fn regular_files_skipped_newer(&self) -> u64 {
        self.stats.regular_files_skipped_newer()
    }

    /// Returns the number of directories created during the transfer.
    #[must_use]
    pub fn directories_created(&self) -> u64 {
        self.stats.directories_created()
    }

    /// Returns the number of directories encountered in the source set.
    #[must_use]
    pub fn directories_total(&self) -> u64 {
        self.stats.directories_total()
    }

    /// Returns the number of symbolic links copied during the transfer.
    #[must_use]
    pub fn symlinks_copied(&self) -> u64 {
        self.stats.symlinks_copied()
    }

    /// Returns the number of symbolic links encountered in the source set.
    #[must_use]
    pub fn symlinks_total(&self) -> u64 {
        self.stats.symlinks_total()
    }

    /// Returns the number of hard links materialised during the transfer.
    #[must_use]
    pub fn hard_links_created(&self) -> u64 {
        self.stats.hard_links_created()
    }

    /// Returns the number of device nodes created during the transfer.
    #[must_use]
    pub fn devices_created(&self) -> u64 {
        self.stats.devices_created()
    }

    /// Returns the number of device nodes encountered in the source set.
    #[must_use]
    pub fn devices_total(&self) -> u64 {
        self.stats.devices_total()
    }

    /// Returns the number of FIFOs created during the transfer.
    #[must_use]
    pub fn fifos_created(&self) -> u64 {
        self.stats.fifos_created()
    }

    /// Returns the number of FIFOs encountered in the source set.
    #[must_use]
    pub fn fifos_total(&self) -> u64 {
        self.stats.fifos_total()
    }

    /// Returns the number of extraneous entries removed due to `--delete`.
    #[must_use]
    pub fn items_deleted(&self) -> u64 {
        self.stats.items_deleted()
    }

    /// Returns the aggregate number of bytes copied.
    #[must_use]
    pub fn bytes_copied(&self) -> u64 {
        self.stats.bytes_copied()
    }

    /// Returns the aggregate number of bytes reused from the destination instead of being
    /// rewritten during the transfer.
    #[must_use]
    #[doc(alias = "--stats")]
    pub fn matched_bytes(&self) -> u64 {
        self.stats.matched_bytes()
    }

    /// Returns the aggregate number of bytes received during the transfer.
    #[must_use]
    pub fn bytes_received(&self) -> u64 {
        self.stats.bytes_received()
    }

    /// Returns the aggregate number of bytes sent during the transfer.
    #[must_use]
    pub fn bytes_sent(&self) -> u64 {
        self.stats.bytes_sent()
    }

    /// Returns the aggregate size of files that were rewritten or created.
    #[must_use]
    pub fn transferred_file_size(&self) -> u64 {
        self.stats.transferred_file_size()
    }

    /// Returns the number of bytes that would be sent after applying compression.
    #[must_use]
    pub fn compressed_bytes(&self) -> Option<u64> {
        if self.stats.compression_used() {
            Some(self.stats.compressed_bytes())
        } else {
            None
        }
    }

    /// Reports whether compression participated in the transfer.
    #[must_use]
    pub fn compression_used(&self) -> bool {
        self.stats.compression_used()
    }

    /// Returns the number of source entries removed due to `--remove-source-files`.
    #[must_use]
    pub fn sources_removed(&self) -> u64 {
        self.stats.sources_removed()
    }

    /// Returns the aggregate size of all source files considered during the transfer.
    #[must_use]
    pub fn total_source_bytes(&self) -> u64 {
        self.stats.total_source_bytes()
    }

    /// Returns the total elapsed time spent transferring file payloads.
    #[must_use]
    pub fn total_elapsed(&self) -> Duration {
        self.stats.total_elapsed()
    }

    /// Returns the number of bytes that would be transmitted for the file list.
    #[must_use]
    pub fn file_list_size(&self) -> u64 {
        self.stats.file_list_size()
    }

    /// Returns the duration spent generating the in-memory file list.
    #[must_use]
    pub fn file_list_generation_time(&self) -> Duration {
        self.stats.file_list_generation_time()
    }

    /// Returns the duration spent transmitting the file list to the peer.
    #[must_use]
    pub fn file_list_transfer_time(&self) -> Duration {
        self.stats.file_list_transfer_time()
    }
}

/// Outcome returned when executing [`run_client_or_fallback`].
#[derive(Debug)]
pub enum ClientOutcome {
    /// The transfer was handled by the local copy engine.
    Local(Box<ClientSummary>),
    /// The transfer was delegated to an upstream `rsync` binary.
    Fallback(FallbackSummary),
}

impl ClientOutcome {
    /// Returns the contained [`ClientSummary`] when the outcome represents a local execution.
    pub fn into_local(self) -> Option<ClientSummary> {
        match self {
            Self::Local(summary) => Some(*summary),
            Self::Fallback(_) => None,
        }
    }
}

/// Summary describing the result of a fallback invocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FallbackSummary {
    exit_code: i32,
}

impl FallbackSummary {
    const fn new(exit_code: i32) -> Self {
        Self { exit_code }
    }

    /// Returns the exit code reported by the fallback process.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        self.exit_code
    }
}

/// Arguments used to spawn the legacy `rsync` binary when remote operands are present.
///
/// The fallback path preserves the command-line semantics of upstream rsync while the
/// native protocol engine is completed. Higher level consumers such as the CLI build
/// this structure from parsed flags before handing control to
/// [`run_remote_transfer_fallback`].
#[derive(Clone, Debug)]
pub struct RemoteFallbackArgs {
    /// Enables `--dry-run`.
    pub dry_run: bool,
    /// Enables `--list-only`.
    pub list_only: bool,
    /// Supplies the remote shell command forwarded via `-e`/`--rsh`.
    pub remote_shell: Option<OsString>,
    /// Additional options forwarded to the remote rsync invocation via `--remote-option`/`-M`.
    pub remote_options: Vec<OsString>,
    /// Optional command executed to reach rsync:// daemons.
    pub connect_program: Option<OsString>,
    /// Default daemon port forwarded via `--port` when contacting rsync:// daemons.
    #[doc(alias = "--port")]
    pub port: Option<u16>,
    /// Optional bind address forwarded via `--address`.
    pub bind_address: Option<OsString>,
    /// Controls whether remote shell arguments are protected from expansion.
    ///
    /// When `Some(true)` the fallback command receives `--protect-args`,
    /// while `Some(false)` forwards `--no-protect-args`. A `None` value keeps
    /// rsync's default behaviour.
    pub protect_args: Option<bool>,
    /// Optional `--human-readable` level forwarded to the fallback binary.
    pub human_readable: Option<HumanReadableMode>,
    /// Enables archive mode (`-a`).
    pub archive: bool,
    /// Enables `--delete`.
    pub delete: bool,
    /// Selects the deletion timing to forward to the fallback binary.
    pub delete_mode: DeleteMode,
    /// Enables `--delete-excluded`.
    pub delete_excluded: bool,
    /// Limits deletions via `--max-delete`.
    pub max_delete: Option<u64>,
    /// Skips files smaller than the provided size via `--min-size`.
    pub min_size: Option<OsString>,
    /// Skips files larger than the provided size via `--max-size`.
    pub max_size: Option<OsString>,
    /// Enables `--checksum`.
    pub checksum: bool,
    /// Optional strong checksum selection forwarded via `--checksum-choice`.
    pub checksum_choice: Option<OsString>,
    /// Optional checksum seed forwarded via `--checksum-seed`.
    pub checksum_seed: Option<u32>,
    /// Enables `--size-only`.
    pub size_only: bool,
    /// Enables `--ignore-existing`.
    pub ignore_existing: bool,
    /// Enables `--update`.
    pub update: bool,
    /// Optional `--modify-window` tolerance forwarded to the fallback binary.
    pub modify_window: Option<u64>,
    /// Enables `--compress`.
    pub compress: bool,
    /// Enables `--no-compress` when `true` and compression is otherwise disabled.
    pub compress_disabled: bool,
    /// Optional compression level forwarded via `--compress-level`.
    pub compress_level: Option<OsString>,
    /// Optional suffix list forwarded via `--skip-compress`.
    pub skip_compress: Option<OsString>,
    /// Optional ownership override forwarded via `--chown`.
    pub chown: Option<OsString>,
    /// Optional `--owner`/`--no-owner` toggle.
    pub owner: Option<bool>,
    /// Optional `--group`/`--no-group` toggle.
    pub group: Option<bool>,
    /// Repeated `--chmod` specifications forwarded to the fallback binary.
    pub chmod: Vec<OsString>,
    /// Optional `--perms`/`--no-perms` toggle.
    pub perms: Option<bool>,
    /// Optional `--super`/`--no-super` toggle.
    pub super_mode: Option<bool>,
    /// Optional `--times`/`--no-times` toggle.
    pub times: Option<bool>,
    /// Optional `--omit-dir-times`/`--no-omit-dir-times` toggle.
    pub omit_dir_times: Option<bool>,
    /// Optional `--omit-link-times`/`--no-omit-link-times` toggle.
    pub omit_link_times: Option<bool>,
    /// Optional `--numeric-ids`/`--no-numeric-ids` toggle.
    pub numeric_ids: Option<bool>,
    /// Optional `--hard-links`/`--no-hard-links` toggle.
    pub hard_links: Option<bool>,
    /// Optional `--copy-links`/`--no-copy-links` toggle.
    pub copy_links: Option<bool>,
    /// Enables `--copy-dirlinks` when `true`.
    pub copy_dirlinks: bool,
    /// Optional `--keep-dirlinks`/`--no-keep-dirlinks` toggle.
    pub keep_dirlinks: Option<bool>,
    /// Enables `--safe-links` when `true`.
    pub safe_links: bool,
    /// Optional `--sparse`/`--no-sparse` toggle.
    pub sparse: Option<bool>,
    /// Optional `--devices`/`--no-devices` toggle.
    pub devices: Option<bool>,
    /// Optional `--specials`/`--no-specials` toggle.
    pub specials: Option<bool>,
    /// Optional `--relative`/`--no-relative` toggle.
    pub relative: Option<bool>,
    /// Optional `--one-file-system`/`--no-one-file-system` toggle.
    pub one_file_system: Option<bool>,
    /// Optional `--implied-dirs`/`--no-implied-dirs` toggle.
    pub implied_dirs: Option<bool>,
    /// Enables `--mkpath`.
    pub mkpath: bool,
    /// Controls pruning of empty directories via `--prune-empty-dirs`.
    pub prune_empty_dirs: Option<bool>,
    /// Verbosity level translated into repeated `-v` flags.
    pub verbosity: u8,
    /// Enables `--progress`.
    pub progress: bool,
    /// Enables `--stats`.
    pub stats: bool,
    /// Enables `--itemize-changes` on the fallback command line.
    pub itemize_changes: bool,
    /// Enables `--partial`.
    pub partial: bool,
    /// Enables `--preallocate`.
    pub preallocate: bool,
    /// Enables `--delay-updates`.
    pub delay_updates: bool,
    /// Optional directory forwarded via `--partial-dir`.
    pub partial_dir: Option<PathBuf>,
    /// Optional directory forwarded via `--temp-dir`.
    pub temp_directory: Option<PathBuf>,
    /// Enables `--backup`.
    pub backup: bool,
    /// Optional directory forwarded via `--backup-dir`.
    pub backup_dir: Option<PathBuf>,
    /// Optional suffix forwarded via `--suffix`.
    pub backup_suffix: Option<OsString>,
    /// Directories forwarded via repeated `--link-dest` flags.
    pub link_dests: Vec<PathBuf>,
    /// Enables `--remove-source-files`.
    pub remove_source_files: bool,
    /// Optional `--append`/`--no-append` toggle.
    pub append: Option<bool>,
    /// Enables `--append-verify`.
    pub append_verify: bool,
    /// Optional `--inplace`/`--no-inplace` toggle.
    pub inplace: Option<bool>,
    /// Routes daemon messages to standard error via `--msgs2stderr`.
    pub msgs_to_stderr: bool,
    /// Optional `--whole-file`/`--no-whole-file` toggle.
    pub whole_file: Option<bool>,
    /// Optional bandwidth limit forwarded through `--bwlimit`.
    ///
    /// Values are normalised to their rounded byte-per-second representation so
    /// legacy fallback binaries that predate burst support continue accepting
    /// the argument. An unlimited transfer is represented using `"0"`.
    pub bwlimit: Option<OsString>,
    /// Patterns forwarded via repeated `--exclude` flags.
    pub excludes: Vec<OsString>,
    /// Patterns forwarded via repeated `--include` flags.
    pub includes: Vec<OsString>,
    /// File paths forwarded via repeated `--exclude-from` flags.
    pub exclude_from: Vec<OsString>,
    /// File paths forwarded via repeated `--include-from` flags.
    pub include_from: Vec<OsString>,
    /// Raw filter directives forwarded via repeated `--filter` flags.
    pub filters: Vec<OsString>,
    /// Number of times the `-F` shortcut was supplied.
    pub rsync_filter_shortcuts: u8,
    /// Reference directories forwarded via repeated `--compare-dest` flags.
    pub compare_destinations: Vec<OsString>,
    /// Reference directories forwarded via repeated `--copy-dest` flags.
    pub copy_destinations: Vec<OsString>,
    /// Reference directories forwarded via repeated `--link-dest` flags.
    pub link_destinations: Vec<OsString>,
    /// Enables `--cvs-exclude` on the fallback binary.
    pub cvs_exclude: bool,
    /// Values forwarded to the fallback binary via repeated `--info=FLAGS` occurrences.
    pub info_flags: Vec<OsString>,
    /// Values forwarded to the fallback binary via repeated `--debug=FLAGS` occurrences.
    pub debug_flags: Vec<OsString>,
    /// Whether the original invocation used `--files-from`.
    pub files_from_used: bool,
    /// Entries collected from `--files-from` operands.
    pub file_list_entries: Vec<OsString>,
    /// Indicates that `--from0` was supplied.
    pub from0: bool,
    /// Optional path provided via `--password-file`.
    pub password_file: Option<PathBuf>,
    /// Optional daemon password supplied via `--password-file=-`.
    ///
    /// When populated the helper writes the password to the fallback
    /// process' standard input so callers do not need to re-enter
    /// credentials after the CLI has already consumed them.
    pub daemon_password: Option<Vec<u8>>,
    /// Optional protocol override forwarded via `--protocol`.
    pub protocol: Option<ProtocolVersion>,
    /// Timeout applied to the spawned process via `--timeout`.
    pub timeout: TransferTimeout,
    /// Connection timeout forwarded via `--contimeout`.
    pub connect_timeout: TransferTimeout,
    /// Optional `--out-format` template.
    pub out_format: Option<OsString>,
    /// Enables `--no-motd`.
    pub no_motd: bool,
    /// Preferred address family forwarded via `--ipv4`/`--ipv6`.
    pub address_mode: AddressMode,
    /// Optional override for the fallback executable path.
    ///
    /// When unspecified the helper consults the `OC_RSYNC_FALLBACK` environment variable and
    /// defaults to `rsync` if the override is missing or empty.
    pub fallback_binary: Option<OsString>,
    /// Optional override for the remote rsync executable.
    ///
    /// When populated the helper forwards `--rsync-path` to the fallback command so upstream
    /// rsync executes the specified program on the remote system. The option is ignored when
    /// remote operands are absent because local transfers never invoke the fallback binary.
    pub rsync_path: Option<OsString>,
    /// Remaining operands to forward to the fallback binary.
    pub remainder: Vec<OsString>,
    /// Controls ACL forwarding (`--acls`/`--no-acls`).
    #[cfg(feature = "acl")]
    pub acls: Option<bool>,
    /// Controls xattr forwarding (`--xattrs`/`--no-xattrs`).
    #[cfg(feature = "xattr")]
    pub xattrs: Option<bool>,
}

/// Writer references and arguments required to invoke the fallback binary.
pub struct RemoteFallbackContext<'a, Out, Err>
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    stdout: &'a mut Out,
    stderr: &'a mut Err,
    args: RemoteFallbackArgs,
}

impl<'a, Out, Err> RemoteFallbackContext<'a, Out, Err>
where
    Out: Write + 'a,
    Err: Write + 'a,
{
    /// Creates a new context that streams output into the supplied writers.
    #[must_use]
    pub fn new(stdout: &'a mut Out, stderr: &'a mut Err, args: RemoteFallbackArgs) -> Self {
        Self {
            stdout,
            stderr,
            args,
        }
    }

    fn split(self) -> (&'a mut Out, &'a mut Err, RemoteFallbackArgs) {
        let Self {
            stdout,
            stderr,
            args,
        } = self;
        (stdout, stderr, args)
    }
}

/// Spawns the fallback `rsync` binary with arguments derived from [`RemoteFallbackArgs`].
///
/// The helper forwards the subprocess stdout/stderr into the provided writers and returns
/// the exit status code on success. Errors surface as [`ClientError`] instances with
/// fully formatted diagnostics.
pub fn run_remote_transfer_fallback<Out, Err>(
    stdout: &mut Out,
    stderr: &mut Err,
    args: RemoteFallbackArgs,
) -> Result<i32, ClientError>
where
    Out: Write,
    Err: Write,
{
    let RemoteFallbackArgs {
        dry_run,
        list_only,
        remote_shell,
        remote_options,
        connect_program,
        port,
        bind_address,
        protect_args,
        human_readable: human_readable_mode,
        archive,
        delete,
        delete_mode,
        delete_excluded,
        max_delete,
        min_size,
        max_size,
        checksum,
        checksum_choice,
        checksum_seed,
        size_only,
        ignore_existing,
        update,
        modify_window,
        compress,
        compress_disabled,
        compress_level,
        skip_compress,
        chown,
        owner,
        group,
        chmod,
        perms,
        super_mode,
        times,
        omit_dir_times,
        omit_link_times,
        numeric_ids,
        hard_links,
        copy_links,
        copy_dirlinks,
        keep_dirlinks,
        safe_links,
        sparse,
        devices,
        specials,
        relative,
        one_file_system,
        implied_dirs,
        mkpath,
        prune_empty_dirs,
        verbosity,
        progress,
        stats,
        itemize_changes,
        partial,
        preallocate,
        delay_updates,
        partial_dir,
        temp_directory,
        backup,
        backup_dir,
        backup_suffix,
        link_dests,
        remove_source_files,
        append,
        append_verify,
        inplace,
        msgs_to_stderr,
        whole_file,
        bwlimit,
        excludes,
        includes,
        exclude_from,
        include_from,
        filters,
        rsync_filter_shortcuts,
        compare_destinations,
        copy_destinations,
        link_destinations,
        cvs_exclude,
        info_flags,
        debug_flags,
        files_from_used,
        file_list_entries,
        from0,
        password_file,
        mut daemon_password,
        protocol,
        timeout,
        connect_timeout,
        out_format,
        no_motd,
        address_mode,
        fallback_binary,
        rsync_path,
        mut remainder,
        #[cfg(feature = "acl")]
        acls,
        #[cfg(feature = "xattr")]
        xattrs,
    } = args;

    let mut command_args = Vec::new();
    if archive {
        command_args.push(OsString::from("-a"));
    }
    if dry_run {
        command_args.push(OsString::from("--dry-run"));
    }
    if list_only {
        command_args.push(OsString::from("--list-only"));
    }
    if delete {
        command_args.push(OsString::from("--delete"));
        match delete_mode {
            DeleteMode::Before => command_args.push(OsString::from("--delete-before")),
            DeleteMode::After => command_args.push(OsString::from("--delete-after")),
            DeleteMode::Delay => command_args.push(OsString::from("--delete-delay")),
            DeleteMode::During => command_args.push(OsString::from("--delete-during")),
            DeleteMode::Disabled => {}
        }
    }
    if delete_excluded {
        command_args.push(OsString::from("--delete-excluded"));
    }
    if backup {
        command_args.push(OsString::from("--backup"));
    }
    if let Some(dir) = backup_dir {
        command_args.push(OsString::from("--backup-dir"));
        command_args.push(dir.into_os_string());
    }
    if let Some(suffix) = backup_suffix {
        command_args.push(OsString::from("--suffix"));
        command_args.push(suffix);
    }
    if let Some(limit) = max_delete {
        let mut arg = OsString::from("--max-delete=");
        arg.push(limit.to_string());
        command_args.push(arg);
    }
    if let Some(spec) = min_size {
        let mut arg = OsString::from("--min-size=");
        arg.push(spec);
        command_args.push(arg);
    }
    if let Some(spec) = max_size {
        let mut arg = OsString::from("--max-size=");
        arg.push(spec);
        command_args.push(arg);
    }
    if checksum {
        command_args.push(OsString::from("--checksum"));
    }
    if let Some(choice) = checksum_choice {
        let mut arg = OsString::from("--checksum-choice=");
        arg.push(choice);
        command_args.push(arg);
    }
    if let Some(seed) = checksum_seed {
        let mut arg = OsString::from("--checksum-seed=");
        arg.push(seed.to_string());
        command_args.push(arg);
    }
    if size_only {
        command_args.push(OsString::from("--size-only"));
    }
    if ignore_existing {
        command_args.push(OsString::from("--ignore-existing"));
    }
    if update {
        command_args.push(OsString::from("--update"));
    }
    if let Some(window) = modify_window {
        let mut arg = OsString::from("--modify-window=");
        arg.push(window.to_string());
        command_args.push(arg);
    }
    if compress {
        command_args.push(OsString::from("--compress"));
    } else if compress_disabled {
        command_args.push(OsString::from("--no-compress"));
        if whole_file.is_none() {
            command_args.push(OsString::from("--no-whole-file"));
        }
    }
    if let Some(level) = compress_level {
        command_args.push(OsString::from("--compress-level"));
        command_args.push(level);
    }

    if let Some(spec) = skip_compress {
        let mut arg = OsString::from("--skip-compress=");
        arg.push(spec);
        command_args.push(arg);
    }

    if let Some(spec) = chown {
        let mut arg = OsString::from("--chown=");
        arg.push(spec);
        command_args.push(arg);
    }

    push_toggle(&mut command_args, "--owner", "--no-owner", owner);
    push_toggle(&mut command_args, "--group", "--no-group", group);
    for spec in chmod {
        let mut arg = OsString::from("--chmod=");
        arg.push(&spec);
        command_args.push(arg);
    }
    push_toggle(&mut command_args, "--perms", "--no-perms", perms);
    push_toggle(&mut command_args, "--super", "--no-super", super_mode);
    push_toggle(&mut command_args, "--times", "--no-times", times);
    push_toggle(
        &mut command_args,
        "--omit-dir-times",
        "--no-omit-dir-times",
        omit_dir_times,
    );
    push_toggle(
        &mut command_args,
        "--omit-link-times",
        "--no-omit-link-times",
        omit_link_times,
    );
    push_toggle(
        &mut command_args,
        "--numeric-ids",
        "--no-numeric-ids",
        numeric_ids,
    );
    push_toggle(
        &mut command_args,
        "--hard-links",
        "--no-hard-links",
        hard_links,
    );
    push_toggle(
        &mut command_args,
        "--copy-links",
        "--no-copy-links",
        copy_links,
    );
    if copy_dirlinks {
        command_args.push(OsString::from("--copy-dirlinks"));
    }
    push_toggle(
        &mut command_args,
        "--keep-dirlinks",
        "--no-keep-dirlinks",
        keep_dirlinks,
    );
    if safe_links {
        command_args.push(OsString::from("--safe-links"));
    }
    push_toggle(&mut command_args, "--sparse", "--no-sparse", sparse);
    push_toggle(&mut command_args, "--devices", "--no-devices", devices);
    push_toggle(&mut command_args, "--specials", "--no-specials", specials);
    push_toggle(&mut command_args, "--relative", "--no-relative", relative);
    push_toggle(
        &mut command_args,
        "--one-file-system",
        "--no-one-file-system",
        one_file_system,
    );
    push_toggle(
        &mut command_args,
        "--implied-dirs",
        "--no-implied-dirs",
        implied_dirs,
    );
    if mkpath {
        command_args.push(OsString::from("--mkpath"));
    }
    push_toggle(
        &mut command_args,
        "--prune-empty-dirs",
        "--no-prune-empty-dirs",
        prune_empty_dirs,
    );
    push_toggle(&mut command_args, "--inplace", "--no-inplace", inplace);
    #[cfg(feature = "acl")]
    push_toggle(&mut command_args, "--acls", "--no-acls", acls);
    push_toggle(
        &mut command_args,
        "--whole-file",
        "--no-whole-file",
        whole_file,
    );
    #[cfg(feature = "xattr")]
    push_toggle(&mut command_args, "--xattrs", "--no-xattrs", xattrs);

    for _ in 0..verbosity {
        command_args.push(OsString::from("-v"));
    }
    if progress {
        command_args.push(OsString::from("--progress"));
    }
    if stats {
        command_args.push(OsString::from("--stats"));
    }
    if itemize_changes {
        command_args.push(OsString::from("--itemize-changes"));
    }
    if partial {
        command_args.push(OsString::from("--partial"));
    }
    if preallocate {
        command_args.push(OsString::from("--preallocate"));
    }
    if delay_updates {
        command_args.push(OsString::from("--delay-updates"));
    }
    if let Some(dir) = partial_dir {
        command_args.push(OsString::from("--partial-dir"));
        command_args.push(dir.into_os_string());
    }
    if let Some(dir) = temp_directory {
        command_args.push(OsString::from("--temp-dir"));
        command_args.push(dir.into_os_string());
    }
    for dir in link_dests {
        let mut arg = OsString::from("--link-dest=");
        arg.push(dir);
        command_args.push(arg);
    }
    if remove_source_files {
        command_args.push(OsString::from("--remove-source-files"));
    }
    if append_verify {
        command_args.push(OsString::from("--append-verify"));
    } else {
        push_toggle(&mut command_args, "--append", "--no-append", append);
    }
    if msgs_to_stderr {
        command_args.push(OsString::from("--msgs2stderr"));
    }

    if let Some(enabled) = protect_args {
        if enabled {
            command_args.push(OsString::from("--protect-args"));
        } else {
            command_args.push(OsString::from("--no-protect-args"));
        }
    }

    push_human_readable(&mut command_args, human_readable_mode);

    if let Some(limit) = bwlimit {
        command_args.push(OsString::from("--bwlimit"));
        command_args.push(limit);
    }

    if let Some(format) = out_format {
        command_args.push(OsString::from("--out-format"));
        command_args.push(format);
    }

    for exclude in excludes {
        command_args.push(OsString::from("--exclude"));
        command_args.push(exclude);
    }
    for include in includes {
        command_args.push(OsString::from("--include"));
        command_args.push(include);
    }
    for path in exclude_from {
        command_args.push(OsString::from("--exclude-from"));
        command_args.push(path);
    }
    for path in include_from {
        command_args.push(OsString::from("--include-from"));
        command_args.push(path);
    }
    if cvs_exclude {
        command_args.push(OsString::from("--cvs-exclude"));
    }
    for _ in 0..rsync_filter_shortcuts {
        command_args.push(OsString::from("-F"));
    }
    for filter in filters {
        command_args.push(OsString::from("--filter"));
        command_args.push(filter);
    }

    for path in compare_destinations {
        command_args.push(OsString::from("--compare-dest"));
        command_args.push(path);
    }

    for path in copy_destinations {
        command_args.push(OsString::from("--copy-dest"));
        command_args.push(path);
    }

    for path in link_destinations {
        command_args.push(OsString::from("--link-dest"));
        command_args.push(path);
    }

    for flag in info_flags {
        let mut arg = OsString::from("--info=");
        arg.push(&flag);
        command_args.push(arg);
    }

    for flag in debug_flags {
        let mut arg = OsString::from("--debug=");
        arg.push(&flag);
        command_args.push(arg);
    }

    let files_from_temp =
        prepare_file_list(&file_list_entries, files_from_used, from0).map_err(|error| {
            fallback_error(format!(
                "failed to prepare file list for fallback rsync invocation: {error}"
            ))
        })?;

    if let Some(temp) = files_from_temp.as_ref() {
        command_args.push(OsString::from("--files-from"));
        command_args.push(temp.path().as_os_str().to_os_string());
        if from0 {
            command_args.push(OsString::from("--from0"));
        }
    }

    if let Some(path) = password_file {
        command_args.push(OsString::from("--password-file"));
        command_args.push(path.into_os_string());
    }

    if let Some(protocol) = protocol {
        command_args.push(OsString::from("--protocol"));
        command_args.push(OsString::from(protocol.to_string()));
    }

    match timeout {
        TransferTimeout::Default => {}
        TransferTimeout::Disabled => {
            command_args.push(OsString::from("--timeout"));
            command_args.push(OsString::from("0"));
        }
        TransferTimeout::Seconds(value) => {
            command_args.push(OsString::from("--timeout"));
            command_args.push(OsString::from(value.get().to_string()));
        }
    }

    match connect_timeout {
        TransferTimeout::Default => {}
        TransferTimeout::Disabled => {
            command_args.push(OsString::from("--contimeout"));
            command_args.push(OsString::from("0"));
        }
        TransferTimeout::Seconds(value) => {
            command_args.push(OsString::from("--contimeout"));
            command_args.push(OsString::from(value.get().to_string()));
        }
    }

    if no_motd {
        command_args.push(OsString::from("--no-motd"));
    }

    for option in remote_options {
        command_args.push(OsString::from("--remote-option"));
        command_args.push(option);
    }

    if let Some(program) = connect_program {
        command_args.push(OsString::from("--connect-program"));
        command_args.push(program);
    }

    if let Some(shell) = remote_shell {
        command_args.push(OsString::from("-e"));
        command_args.push(shell);
    }

    match address_mode {
        AddressMode::Default => {}
        AddressMode::Ipv4 => command_args.push(OsString::from("--ipv4")),
        AddressMode::Ipv6 => command_args.push(OsString::from("--ipv6")),
    }

    if let Some(port) = port {
        let mut arg = OsString::from("--port=");
        arg.push(port.to_string());
        command_args.push(arg);
    }

    if let Some(address) = bind_address {
        let mut arg = OsString::from("--address=");
        arg.push(address);
        command_args.push(arg);
    }

    if let Some(path) = rsync_path {
        command_args.push(OsString::from("--rsync-path"));
        command_args.push(path);
    }

    command_args.append(&mut remainder);

    let binary = if let Some(path) = fallback_binary {
        path
    } else {
        match fallback_override("OC_RSYNC_FALLBACK") {
            Some(FallbackOverride::Disabled) => {
                return Err(fallback_error(
                    "remote transfers are unavailable because OC_RSYNC_FALLBACK is disabled; set OC_RSYNC_FALLBACK to point to an upstream rsync binary",
                ));
            }
            Some(other) => other
                .resolve_or_default(OsStr::new("rsync"))
                .unwrap_or_else(|| OsString::from("rsync")),
            None => OsString::from("rsync"),
        }
    };

    let mut command = Command::new(&binary);
    command.args(&command_args);
    if daemon_password.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::inherit());
    }
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        fallback_error(format!(
            "failed to launch fallback rsync binary '{}': {error}",
            Path::new(&binary).display()
        ))
    })?;

    if let Some(mut password) = daemon_password.take() {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| fallback_error("fallback rsync did not expose a writable stdin"))?;

        write_daemon_password(&mut stdin, &mut password).map_err(|error| {
            fallback_error(format!(
                "failed to write password to fallback rsync stdin: {error}"
            ))
        })?;
    }

    let (sender, receiver) = mpsc::channel();
    let mut stdout_thread = child
        .stdout
        .take()
        .map(|handle| spawn_fallback_reader(handle, FallbackStreamKind::Stdout, sender.clone()));
    let mut stderr_thread = child
        .stderr
        .take()
        .map(|handle| spawn_fallback_reader(handle, FallbackStreamKind::Stderr, sender.clone()));
    drop(sender);

    let mut stdout_open = stdout_thread.is_some();
    let mut stderr_open = stderr_thread.is_some();

    while stdout_open || stderr_open {
        match receiver.recv() {
            Ok(FallbackStreamMessage::Data(FallbackStreamKind::Stdout, data)) => {
                if let Err(error) = stdout.write_all(&data) {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(format!(
                        "failed to forward fallback stdout: {error}"
                    )));
                }
            }
            Ok(FallbackStreamMessage::Data(FallbackStreamKind::Stderr, data)) => {
                if let Err(error) = stderr.write_all(&data) {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(format!(
                        "failed to forward fallback stderr: {error}"
                    )));
                }
            }
            Ok(FallbackStreamMessage::Error(FallbackStreamKind::Stdout, error)) => {
                terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                return Err(fallback_error(format!(
                    "failed to read stdout from fallback rsync: {error}"
                )));
            }
            Ok(FallbackStreamMessage::Error(FallbackStreamKind::Stderr, error)) => {
                terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                return Err(fallback_error(format!(
                    "failed to read stderr from fallback rsync: {error}"
                )));
            }
            Ok(FallbackStreamMessage::Finished(kind)) => match kind {
                FallbackStreamKind::Stdout => stdout_open = false,
                FallbackStreamKind::Stderr => stderr_open = false,
            },
            Err(_) => {
                if stdout_open {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(
                        "failed to capture stdout from fallback rsync binary",
                    ));
                }
                if stderr_open {
                    terminate_fallback_process(&mut child, &mut stdout_thread, &mut stderr_thread);
                    return Err(fallback_error(
                        "failed to capture stderr from fallback rsync binary",
                    ));
                }
                break;
            }
        }
    }

    join_fallback_thread(&mut stdout_thread);
    join_fallback_thread(&mut stderr_thread);

    let status = child.wait().map_err(|error| {
        fallback_error(format!(
            "failed to wait for fallback rsync process: {error}"
        ))
    })?;

    drop(files_from_temp);

    Ok(status.code().unwrap_or(MAX_EXIT_CODE))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FallbackStreamKind {
    Stdout,
    Stderr,
}

enum FallbackStreamMessage {
    Data(FallbackStreamKind, Vec<u8>),
    Error(FallbackStreamKind, io::Error),
    Finished(FallbackStreamKind),
}

fn fallback_error(text: impl Into<String>) -> ClientError {
    let message = rsync_error!(1, "{}", text.into()).with_role(Role::Client);
    ClientError::new(1, message)
}

fn push_toggle(args: &mut Vec<OsString>, enable: &str, disable: &str, setting: Option<bool>) {
    match setting {
        Some(true) => args.push(OsString::from(enable)),
        Some(false) => args.push(OsString::from(disable)),
        None => {}
    }
}

fn push_human_readable(args: &mut Vec<OsString>, mode: Option<HumanReadableMode>) {
    match mode {
        Some(HumanReadableMode::Disabled) => args.push(OsString::from("--no-human-readable")),
        Some(HumanReadableMode::Enabled) => args.push(OsString::from("--human-readable")),
        Some(HumanReadableMode::Combined) => args.push(OsString::from("--human-readable=2")),
        None => {}
    }
}

fn prepare_file_list(
    entries: &[OsString],
    files_from_used: bool,
    zero_terminated: bool,
) -> io::Result<Option<NamedTempFile>> {
    if !files_from_used {
        return Ok(None);
    }

    let mut file = NamedTempFile::new()?;
    {
        let writer = file.as_file_mut();
        for entry in entries {
            write_file_list_entry(writer, entry.as_os_str())?;
            if zero_terminated {
                writer.write_all(&[0])?;
            } else {
                writer.write_all(b"\n")?;
            }
        }
        writer.flush()?;
    }

    Ok(Some(file))
}

fn write_file_list_entry<W: Write>(writer: &mut W, value: &OsStr) -> io::Result<()> {
    #[cfg(unix)]
    {
        writer.write_all(value.as_bytes())
    }

    #[cfg(not(unix))]
    {
        writer.write_all(value.to_string_lossy().as_bytes())
    }
}

fn spawn_fallback_reader<R>(
    mut reader: R,
    kind: FallbackStreamKind,
    sender: Sender<FallbackStreamMessage>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = sender.send(FallbackStreamMessage::Finished(kind));
                    break;
                }
                Ok(n) => {
                    if sender
                        .send(FallbackStreamMessage::Data(kind, Vec::from(&buffer[..n])))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    let _ = sender.send(FallbackStreamMessage::Error(kind, error));
                    break;
                }
            }
        }
    })
}

/// Writes the daemon password into `writer`, appending a newline when required and
/// scrubbing the buffer afterwards.
fn write_daemon_password<W: Write>(writer: &mut W, password: &mut Vec<u8>) -> io::Result<()> {
    if !password.ends_with(b"\n") {
        password.push(b'\n');
    }

    writer.write_all(password)?;
    writer.flush()?;

    for byte in password.iter_mut() {
        *byte = 0;
    }

    Ok(())
}

fn join_fallback_thread(handle: &mut Option<thread::JoinHandle<()>>) {
    if let Some(join_handle) = handle.take() {
        let _ = join_handle.join();
    }
}

fn terminate_fallback_process(
    child: &mut Child,
    stdout_thread: &mut Option<thread::JoinHandle<()>>,
    stderr_thread: &mut Option<thread::JoinHandle<()>>,
) {
    let _ = child.kill();
    let _ = child.wait();
    join_fallback_thread(stdout_thread);
    join_fallback_thread(stderr_thread);
}

/// Describes a transfer action performed by the local copy engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientEventKind {
    /// File data was copied into place.
    DataCopied,
    /// The destination already matched the source and metadata was reused.
    MetadataReused,
    /// A hard link was created to a previously copied destination file.
    HardLink,
    /// A symbolic link was recreated.
    SymlinkCopied,
    /// A FIFO node was recreated.
    FifoCopied,
    /// A device node was recreated.
    DeviceCopied,
    /// A directory was created during traversal.
    DirectoryCreated,
    /// An existing destination file was left untouched due to `--ignore-existing`.
    SkippedExisting,
    /// An existing destination file was left untouched because it is newer.
    SkippedNewerDestination,
    /// A non-regular entry was skipped because support was disabled.
    SkippedNonRegular,
    /// A symbolic link was skipped because it was deemed unsafe.
    SkippedUnsafeSymlink,
    /// A directory was skipped to honour `--one-file-system`.
    SkippedMountPoint,
    /// An entry was deleted due to `--delete`.
    EntryDeleted,
    /// A source entry was removed after a successful transfer.
    SourceRemoved,
}

/// Event describing a single action performed during a client transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientEvent {
    relative_path: PathBuf,
    kind: ClientEventKind,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
    metadata: Option<ClientEntryMetadata>,
    created: bool,
    destination_root: Arc<PathBuf>,
    destination_path: PathBuf,
}

impl ClientEvent {
    fn from_record(record: &LocalCopyRecord, destination_root: Arc<PathBuf>) -> Self {
        let kind = match record.action() {
            LocalCopyAction::DataCopied => ClientEventKind::DataCopied,
            LocalCopyAction::MetadataReused => ClientEventKind::MetadataReused,
            LocalCopyAction::HardLink => ClientEventKind::HardLink,
            LocalCopyAction::SymlinkCopied => ClientEventKind::SymlinkCopied,
            LocalCopyAction::FifoCopied => ClientEventKind::FifoCopied,
            LocalCopyAction::DeviceCopied => ClientEventKind::DeviceCopied,
            LocalCopyAction::DirectoryCreated => ClientEventKind::DirectoryCreated,
            LocalCopyAction::SkippedExisting => ClientEventKind::SkippedExisting,
            LocalCopyAction::SkippedNewerDestination => ClientEventKind::SkippedNewerDestination,
            LocalCopyAction::SkippedNonRegular => ClientEventKind::SkippedNonRegular,
            LocalCopyAction::SkippedUnsafeSymlink => ClientEventKind::SkippedUnsafeSymlink,
            LocalCopyAction::SkippedMountPoint => ClientEventKind::SkippedMountPoint,
            LocalCopyAction::EntryDeleted => ClientEventKind::EntryDeleted,
            LocalCopyAction::SourceRemoved => ClientEventKind::SourceRemoved,
        };
        let created = match record.action() {
            LocalCopyAction::DataCopied => record.was_created(),
            LocalCopyAction::DirectoryCreated
            | LocalCopyAction::SymlinkCopied
            | LocalCopyAction::FifoCopied
            | LocalCopyAction::DeviceCopied
            | LocalCopyAction::HardLink => true,
            LocalCopyAction::MetadataReused
            | LocalCopyAction::SkippedExisting
            | LocalCopyAction::SkippedNewerDestination
            | LocalCopyAction::SkippedNonRegular
            | LocalCopyAction::SkippedUnsafeSymlink
            | LocalCopyAction::SkippedMountPoint
            | LocalCopyAction::EntryDeleted
            | LocalCopyAction::SourceRemoved => false,
        };
        let destination_path =
            Self::resolve_destination_path(&destination_root, record.relative_path());
        Self {
            relative_path: record.relative_path().to_path_buf(),
            kind,
            bytes_transferred: record.bytes_transferred(),
            total_bytes: record.total_bytes(),
            elapsed: record.elapsed(),
            metadata: record
                .metadata()
                .map(ClientEntryMetadata::from_local_copy_metadata),
            created,
            destination_root,
            destination_path,
        }
    }

    fn from_progress(
        relative: &Path,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        elapsed: Duration,
        destination_root: Arc<PathBuf>,
    ) -> Self {
        let destination_path = Self::resolve_destination_path(&destination_root, relative);
        Self {
            relative_path: relative.to_path_buf(),
            kind: ClientEventKind::DataCopied,
            bytes_transferred,
            total_bytes,
            elapsed,
            metadata: None,
            created: false,
            destination_root,
            destination_path,
        }
    }

    /// Returns the relative path affected by this event.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the action recorded by this event.
    #[must_use]
    pub fn kind(&self) -> &ClientEventKind {
        &self.kind
    }

    /// Returns the number of bytes transferred as part of this event.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this event, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent on this event.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Returns the metadata associated with the event, when available.
    #[must_use]
    pub fn metadata(&self) -> Option<&ClientEntryMetadata> {
        self.metadata.as_ref()
    }

    /// Returns whether the event corresponds to the creation of a new destination entry.
    #[must_use]
    pub const fn was_created(&self) -> bool {
        self.created
    }

    /// Returns the root directory of the destination tree.
    #[must_use]
    pub fn destination_root(&self) -> &Path {
        &self.destination_root
    }

    /// Returns the absolute destination path associated with this event.
    #[must_use]
    pub fn destination_path(&self) -> PathBuf {
        self.destination_path.clone()
    }

    fn resolve_destination_path(destination_root: &Path, relative: &Path) -> PathBuf {
        let candidate = destination_root.join(relative);
        if candidate.exists() {
            return candidate;
        }

        if let Some(file_name) = destination_root.file_name() {
            if relative == Path::new(file_name) {
                return destination_root.to_path_buf();
            }
        }

        candidate
    }
}

impl ClientEventKind {
    /// Returns whether the event contributes to progress reporting.
    #[must_use]
    pub const fn is_progress(&self) -> bool {
        matches!(
            self,
            Self::DataCopied
                | Self::MetadataReused
                | Self::HardLink
                | Self::SymlinkCopied
                | Self::FifoCopied
                | Self::DeviceCopied
        )
    }
}

/// Kind of entry described by [`ClientEntryMetadata`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientEntryKind {
    /// Regular file entry.
    File,
    /// Directory entry.
    Directory,
    /// Symbolic link entry.
    Symlink,
    /// FIFO entry.
    Fifo,
    /// Character device entry.
    CharDevice,
    /// Block device entry.
    BlockDevice,
    /// Unix domain socket entry.
    Socket,
    /// Entry of an unknown or platform-specific type.
    Other,
}

impl ClientEntryKind {
    /// Returns whether the metadata describes a directory entry.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// Returns whether the metadata describes a symbolic link entry.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// Metadata snapshot describing an entry affected by a client event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEntryMetadata {
    kind: ClientEntryKind,
    length: u64,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

impl ClientEntryMetadata {
    fn from_local_copy_metadata(metadata: &LocalCopyMetadata) -> Self {
        Self {
            kind: match metadata.kind() {
                LocalCopyFileKind::File => ClientEntryKind::File,
                LocalCopyFileKind::Directory => ClientEntryKind::Directory,
                LocalCopyFileKind::Symlink => ClientEntryKind::Symlink,
                LocalCopyFileKind::Fifo => ClientEntryKind::Fifo,
                LocalCopyFileKind::CharDevice => ClientEntryKind::CharDevice,
                LocalCopyFileKind::BlockDevice => ClientEntryKind::BlockDevice,
                LocalCopyFileKind::Socket => ClientEntryKind::Socket,
                LocalCopyFileKind::Other => ClientEntryKind::Other,
            },
            length: metadata.len(),
            modified: metadata.modified(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            nlink: metadata.nlink(),
            symlink_target: metadata.symlink_target().map(Path::to_path_buf),
        }
    }

    /// Returns the kind of entry represented by this metadata snapshot.
    #[must_use]
    pub const fn kind(&self) -> ClientEntryKind {
        self.kind
    }

    /// Returns the logical length of the entry in bytes.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// Returns the recorded modification timestamp, when available.
    #[must_use]
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Returns the Unix permission bits when available.
    #[must_use]
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    /// Returns the numeric owner identifier when available.
    #[must_use]
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the numeric group identifier when available.
    #[must_use]
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the recorded link count when available.
    #[must_use]
    pub const fn nlink(&self) -> Option<u64> {
        self.nlink
    }

    /// Returns the recorded symbolic link target when the entry represents a symlink.
    #[must_use]
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }
}

/// Progress update emitted while executing [`run_client_with_observer`].
#[derive(Clone, Debug)]
pub struct ClientProgressUpdate {
    event: ClientEvent,
    total: usize,
    remaining: usize,
    index: usize,
    total_bytes: Option<u64>,
    final_update: bool,
    overall_transferred: u64,
    overall_total_bytes: Option<u64>,
    overall_elapsed: Duration,
}

impl ClientProgressUpdate {
    /// Returns the event associated with this progress update.
    #[must_use]
    pub fn event(&self) -> &ClientEvent {
        &self.event
    }

    /// Returns the number of remaining progress events after this update.
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.remaining
    }

    /// Returns the total number of progress events in the transfer.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Returns the 1-based index of the completed progress event.
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Returns the total number of bytes expected for this transfer step, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Reports whether this update corresponds to the completion of an action.
    #[must_use]
    pub const fn is_final(&self) -> bool {
        self.final_update
    }

    /// Returns the aggregate number of bytes transferred across the entire transfer.
    #[must_use]
    pub const fn overall_transferred(&self) -> u64 {
        self.overall_transferred
    }

    /// Returns the total number of bytes expected for the entire transfer, when known.
    #[must_use]
    pub const fn overall_total_bytes(&self) -> Option<u64> {
        self.overall_total_bytes
    }

    /// Returns the elapsed time since the transfer began.
    #[must_use]
    pub const fn overall_elapsed(&self) -> Duration {
        self.overall_elapsed
    }
}

/// Observer invoked for each progress update generated during client execution.
///
/// Implementations should expect multiple updates for a single path as file
/// contents stream into place.
pub trait ClientProgressObserver {
    /// Handles a new progress update.
    fn on_progress(&mut self, update: &ClientProgressUpdate);
}

impl<F> ClientProgressObserver for F
where
    F: FnMut(&ClientProgressUpdate),
{
    fn on_progress(&mut self, update: &ClientProgressUpdate) {
        self(update);
    }
}

struct ClientProgressForwarder<'a> {
    observer: &'a mut dyn ClientProgressObserver,
    total: usize,
    emitted: usize,
    overall_total_bytes: Option<u64>,
    overall_transferred: u64,
    overall_start: Instant,
    in_flight: HashMap<PathBuf, u64>,
    destination_root: Arc<PathBuf>,
}

impl<'a> ClientProgressForwarder<'a> {
    fn new(
        observer: &'a mut dyn ClientProgressObserver,
        plan: &LocalCopyPlan,
        mut options: LocalCopyOptions,
    ) -> Result<Self, ClientError> {
        if !options.events_enabled() {
            options = options.collect_events(true);
        }

        let preview_report = plan
            .execute_with_report(LocalCopyExecution::DryRun, options.clone())
            .map_err(map_local_copy_error)?;

        let destination_root = Arc::new(preview_report.destination_root().to_path_buf());
        let total = preview_report
            .records()
            .iter()
            .map(|record| ClientEvent::from_record(record, Arc::clone(&destination_root)))
            .filter(|event| event.kind().is_progress())
            .count();

        let summary = preview_report.summary();
        let total_bytes = summary.total_source_bytes();

        Ok(Self {
            observer,
            total,
            emitted: 0,
            overall_total_bytes: (total_bytes > 0).then_some(total_bytes),
            overall_transferred: 0,
            overall_start: Instant::now(),
            in_flight: HashMap::new(),
            destination_root,
        })
    }

    fn as_handler_mut(&mut self) -> &mut dyn LocalCopyRecordHandler {
        self
    }
}

impl<'a> LocalCopyRecordHandler for ClientProgressForwarder<'a> {
    fn handle(&mut self, record: LocalCopyRecord) {
        let event = ClientEvent::from_record(&record, Arc::clone(&self.destination_root));
        if !event.kind().is_progress() {
            return;
        }

        self.emitted = self.emitted.saturating_add(1);
        let index = self.emitted;
        let remaining = self.total.saturating_sub(index);

        let total_bytes = if matches!(record.action(), LocalCopyAction::DataCopied) {
            record.total_bytes()
        } else {
            None
        };

        let path = event.relative_path().to_path_buf();
        let previous = self.in_flight.remove(&path).unwrap_or_default();
        let additional = event.bytes_transferred().saturating_sub(previous);
        if additional > 0 {
            self.overall_transferred = self.overall_transferred.saturating_add(additional);
        }

        let update = ClientProgressUpdate {
            event,
            total: self.total,
            remaining,
            index,
            total_bytes,
            final_update: true,
            overall_transferred: self.overall_transferred,
            overall_total_bytes: self.overall_total_bytes,
            overall_elapsed: self.overall_start.elapsed(),
        };

        self.observer.on_progress(&update);
    }

    fn handle_progress(&mut self, progress: LocalCopyProgress<'_>) {
        if self.total == 0 {
            return;
        }

        let index = (self.emitted + 1).min(self.total);
        let remaining = self.total.saturating_sub(index);
        let event = ClientEvent::from_progress(
            progress.relative_path(),
            progress.bytes_transferred(),
            progress.total_bytes(),
            progress.elapsed(),
            Arc::clone(&self.destination_root),
        );

        let entry = self
            .in_flight
            .entry(progress.relative_path().to_path_buf())
            .or_insert(0);
        let additional = progress.bytes_transferred().saturating_sub(*entry);
        if additional > 0 {
            self.overall_transferred = self.overall_transferred.saturating_add(additional);
            *entry = (*entry).saturating_add(additional);
        }

        let update = ClientProgressUpdate {
            event,
            total: self.total,
            remaining,
            index,
            total_bytes: progress.total_bytes(),
            final_update: false,
            overall_transferred: self.overall_transferred,
            overall_total_bytes: self.overall_total_bytes,
            overall_elapsed: self.overall_start.elapsed(),
        };

        self.observer.on_progress(&update);
    }
}

/// Runs the client orchestration using the provided configuration.
///
/// The current implementation offers best-effort local copies covering
/// directories, regular files, and symbolic links. Metadata preservation and
/// delta compression remain works in progress, while remote operands delegate to
/// the system `rsync` binary until the native network engine is available.
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError> {
    match run_client_internal::<io::Sink, io::Sink>(config, None, None) {
        Ok(ClientOutcome::Local(summary)) => Ok(*summary),
        Ok(ClientOutcome::Fallback(_)) => unreachable!("fallback unavailable without context"),
        Err(error) => Err(error),
    }
}

/// Runs the client orchestration while forwarding progress updates to the provided observer.
pub fn run_client_with_observer(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
) -> Result<ClientSummary, ClientError> {
    match run_client_internal::<io::Sink, io::Sink>(config, observer, None) {
        Ok(ClientOutcome::Local(summary)) => Ok(*summary),
        Ok(ClientOutcome::Fallback(_)) => unreachable!("fallback unavailable without context"),
        Err(error) => Err(error),
    }
}

/// Executes the client flow, delegating to a fallback `rsync` binary when provided.
pub fn run_client_or_fallback<Out, Err>(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    fallback: Option<RemoteFallbackContext<'_, Out, Err>>,
) -> Result<ClientOutcome, ClientError>
where
    Out: Write,
    Err: Write,
{
    run_client_internal(config, observer, fallback)
}

fn run_client_internal<Out, Err>(
    config: ClientConfig,
    observer: Option<&mut dyn ClientProgressObserver>,
    fallback: Option<RemoteFallbackContext<'_, Out, Err>>,
) -> Result<ClientOutcome, ClientError>
where
    Out: Write,
    Err: Write,
{
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    let mut fallback = fallback;

    let plan = match LocalCopyPlan::from_operands(config.transfer_args()) {
        Ok(plan) => plan,
        Err(error) => {
            let requires_fallback =
                matches!(
                    error.kind(),
                    LocalCopyErrorKind::InvalidArgument(
                        LocalCopyArgumentError::RemoteOperandUnsupported,
                    )
                ) || matches!(error.kind(), LocalCopyErrorKind::MissingSourceOperands);

            if requires_fallback && let Some(ctx) = fallback.take() {
                return invoke_fallback(ctx);
            }

            return Err(map_local_copy_error(error));
        }
    };

    let filter_program = compile_filter_program(config.filter_rules())?;

    let mut options = build_local_copy_options(&config, filter_program);
    let mode = if config.dry_run() || config.list_only() {
        LocalCopyExecution::DryRun
    } else {
        LocalCopyExecution::Apply
    };

    let collect_events = config.collect_events();

    if collect_events {
        options = options.collect_events(true);
    }

    let mut handler_adapter = observer
        .map(|observer| ClientProgressForwarder::new(observer, &plan, options.clone()))
        .transpose()?;

    let summary = if collect_events {
        plan.execute_with_report_and_handler(
            mode,
            options,
            handler_adapter
                .as_mut()
                .map(ClientProgressForwarder::as_handler_mut),
        )
        .map(ClientSummary::from_report)
    } else {
        plan.execute_with_options_and_handler(
            mode,
            options,
            handler_adapter
                .as_mut()
                .map(ClientProgressForwarder::as_handler_mut),
        )
        .map(ClientSummary::from_summary)
    };

    summary
        .map(|summary| ClientOutcome::Local(Box::new(summary)))
        .map_err(map_local_copy_error)
}

fn build_local_copy_options(
    config: &ClientConfig,
    filter_program: Option<FilterProgram>,
) -> LocalCopyOptions {
    let mut options = LocalCopyOptions::default();
    if config.delete_mode().is_enabled() || config.delete_excluded() {
        options = options.delete(true);
    }
    options = match config.delete_mode() {
        DeleteMode::Before => options.delete_before(true),
        DeleteMode::After => options.delete_after(true),
        DeleteMode::Delay => options.delete_delay(true),
        DeleteMode::During | DeleteMode::Disabled => options,
    };
    options = options
        .delete_excluded(config.delete_excluded())
        .max_deletions(config.max_delete())
        .min_file_size(config.min_file_size())
        .max_file_size(config.max_file_size())
        .remove_source_files(config.remove_source_files())
        .bandwidth_limit(
            config
                .bandwidth_limit()
                .map(|limit| limit.bytes_per_second()),
        )
        .bandwidth_burst(
            config
                .bandwidth_limit()
                .and_then(|limit| limit.burst_bytes()),
        )
        .with_default_compression_level(config.compression_setting().level_or_default())
        .with_skip_compress(config.skip_compress().clone())
        .whole_file(config.whole_file())
        .compress(config.compress())
        .with_compression_level_override(config.compression_level())
        .owner(config.preserve_owner())
        .with_owner_override(config.owner_override())
        .group(config.preserve_group())
        .with_group_override(config.group_override())
        .with_chmod(config.chmod().cloned())
        .permissions(config.preserve_permissions())
        .times(config.preserve_times())
        .omit_dir_times(config.omit_dir_times())
        .omit_link_times(config.omit_link_times())
        .checksum(config.checksum())
        .with_checksum_algorithm(config.checksum_signature_algorithm())
        .size_only(config.size_only())
        .ignore_existing(config.ignore_existing())
        .update(config.update())
        .with_modify_window(config.modify_window_duration())
        .with_filter_program(filter_program)
        .numeric_ids(config.numeric_ids())
        .preallocate(config.preallocate())
        .hard_links(config.preserve_hard_links())
        .sparse(config.sparse())
        .copy_links(config.copy_links())
        .copy_dirlinks(config.copy_dirlinks())
        .keep_dirlinks(config.keep_dirlinks())
        .safe_links(config.safe_links())
        .devices(config.preserve_devices())
        .specials(config.preserve_specials())
        .relative_paths(config.relative_paths())
        .implied_dirs(config.implied_dirs())
        .mkpath(config.mkpath())
        .prune_empty_dirs(config.prune_empty_dirs())
        .inplace(config.inplace())
        .append(config.append())
        .append_verify(config.append_verify())
        .partial(config.partial())
        .with_temp_directory(config.temp_directory().map(|path| path.to_path_buf()))
        .backup(config.backup())
        .with_backup_directory(config.backup_directory().map(|path| path.to_path_buf()))
        .with_backup_suffix(config.backup_suffix().map(OsStr::to_os_string))
        .with_partial_directory(config.partial_directory().map(|path| path.to_path_buf()))
        .delay_updates(config.delay_updates())
        .extend_link_dests(config.link_dest_paths().iter().cloned())
        .with_timeout(
            config
                .timeout()
                .as_seconds()
                .map(|seconds| Duration::from_secs(seconds.get())),
        );
    #[cfg(feature = "acl")]
    let options = options.acls(config.preserve_acls());
    #[cfg(feature = "xattr")]
    let options = options.xattrs(config.preserve_xattrs());

    if !config.reference_directories().is_empty() {
        let references = config.reference_directories().iter().map(|reference| {
            let kind = match reference.kind() {
                ReferenceDirectoryKind::Compare => EngineReferenceDirectoryKind::Compare,
                ReferenceDirectoryKind::Copy => EngineReferenceDirectoryKind::Copy,
                ReferenceDirectoryKind::Link => EngineReferenceDirectoryKind::Link,
            };
            EngineReferenceDirectory::new(kind, reference.path().to_path_buf())
        });
        options = options.extend_reference_directories(references);
    }

    options
}

fn invoke_fallback<Out, Err>(
    ctx: RemoteFallbackContext<'_, Out, Err>,
) -> Result<ClientOutcome, ClientError>
where
    Out: Write,
    Err: Write,
{
    let (stdout, stderr, args) = ctx.split();
    run_remote_transfer_fallback(stdout, stderr, args)
        .map(|code| ClientOutcome::Fallback(FallbackSummary::new(code)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsync_compress::zlib::CompressionLevel;
    use std::ffi::{OsStr, OsString};
    use std::fs;
    use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::num::{NonZeroU8, NonZeroU64};
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

    #[test]
    fn sensitive_bytes_zeroizes_on_drop() {
        let bytes = SensitiveBytes::new(b"topsecret".to_vec());
        let zeroed = bytes.into_zeroized_vec();
        assert!(zeroed.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn daemon_auth_context_zeroizes_secret_on_drop() {
        let context = DaemonAuthContext::new("user".to_string(), b"supersecret".to_vec());
        let zeroed = context.into_zeroized_secret();
        assert!(zeroed.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn resolve_destination_path_returns_existing_candidate() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("dest");
        fs::create_dir_all(&root).expect("create dest root");
        let subdir = root.join("sub");
        fs::create_dir_all(&subdir).expect("create subdir");
        let file_path = subdir.join("file.txt");
        fs::write(&file_path, b"payload").expect("write file");

        let relative = Path::new("sub").join("file.txt");
        let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative.as_path());

        assert_eq!(resolved, file_path);
    }

    #[test]
    fn resolve_destination_path_returns_root_for_file_destination() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("target.bin");
        let relative = Path::new("target.bin");

        let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative);

        assert_eq!(resolved, root);
    }

    #[test]
    fn bandwidth_limit_from_components_returns_none_for_unlimited() {
        let components = bandwidth::BandwidthLimitComponents::new(None, None);
        assert!(BandwidthLimit::from_components(components).is_none());
    }

    #[test]
    fn bandwidth_limit_from_components_preserves_rate_and_burst() {
        let rate = NonZeroU64::new(8 * 1024).expect("non-zero");
        let burst = NonZeroU64::new(64 * 1024).expect("non-zero");
        let components = bandwidth::BandwidthLimitComponents::new(Some(rate), Some(burst));
        let limit = BandwidthLimit::from_components(components).expect("limit produced");

        assert_eq!(limit.bytes_per_second(), rate);
        assert_eq!(limit.burst_bytes(), Some(burst));
    }

    #[test]
    fn resolve_destination_path_preserves_missing_entries() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path().join("dest");
        fs::create_dir_all(&root).expect("create destination root");
        let relative = Path::new("missing.bin");

        let resolved = ClientEvent::resolve_destination_path(root.as_path(), relative);

        assert_eq!(resolved, root.join(relative));
    }

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    const FALLBACK_SCRIPT: &str = r#"#!/bin/sh
set -eu

while [ "$#" -gt 0 ]; do
  case "$1" in
    --files-from)
      FILE="$2"
      cat "$FILE"
      shift 2
      ;;
    --from0)
      shift
      ;;
    *)
      shift
      ;;
  esac
done

printf 'fallback stdout\n'
printf 'fallback stderr\n' >&2
exit 42
"#;

    static ENV_GUARD: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_GUARD.get_or_init(|| Mutex::new(()))
    }

    #[cfg(unix)]
    fn write_fallback_script(dir: &Path) -> PathBuf {
        let path = dir.join("fallback.sh");
        fs::write(&path, FALLBACK_SCRIPT).expect("script written");
        let metadata = fs::metadata(&path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).expect("script permissions set");
        path
    }

    fn baseline_fallback_args() -> RemoteFallbackArgs {
        RemoteFallbackArgs {
            dry_run: false,
            list_only: false,
            remote_shell: None,
            remote_options: Vec::new(),
            connect_program: None,
            port: None,
            bind_address: None,
            protect_args: None,
            human_readable: None,
            address_mode: AddressMode::Default,
            archive: false,
            delete: false,
            delete_mode: DeleteMode::Disabled,
            delete_excluded: false,
            max_delete: None,
            min_size: None,
            max_size: None,
            checksum: false,
            checksum_choice: None,
            checksum_seed: None,
            size_only: false,
            ignore_existing: false,
            update: false,
            modify_window: None,
            compress: false,
            compress_disabled: false,
            compress_level: None,
            skip_compress: None,
            chown: None,
            owner: None,
            group: None,
            chmod: Vec::new(),
            perms: None,
            super_mode: None,
            times: None,
            omit_dir_times: None,
            omit_link_times: None,
            numeric_ids: None,
            hard_links: None,
            copy_links: None,
            copy_dirlinks: false,
            keep_dirlinks: None,
            safe_links: false,
            sparse: None,
            devices: None,
            specials: None,
            relative: None,
            one_file_system: None,
            implied_dirs: None,
            mkpath: false,
            prune_empty_dirs: None,
            verbosity: 0,
            progress: false,
            stats: false,
            itemize_changes: false,
            partial: false,
            preallocate: false,
            delay_updates: false,
            partial_dir: None,
            temp_directory: None,
            backup: false,
            backup_dir: None,
            backup_suffix: None,
            link_dests: Vec::new(),
            remove_source_files: false,
            append: None,
            append_verify: false,
            inplace: None,
            msgs_to_stderr: false,
            whole_file: None,
            bwlimit: None,
            excludes: Vec::new(),
            includes: Vec::new(),
            exclude_from: Vec::new(),
            include_from: Vec::new(),
            filters: Vec::new(),
            rsync_filter_shortcuts: 0,
            compare_destinations: Vec::new(),
            copy_destinations: Vec::new(),
            link_destinations: Vec::new(),
            cvs_exclude: false,
            info_flags: Vec::new(),
            debug_flags: Vec::new(),
            files_from_used: false,
            file_list_entries: Vec::new(),
            from0: false,
            password_file: None,
            daemon_password: None,
            protocol: None,
            timeout: TransferTimeout::Default,
            connect_timeout: TransferTimeout::Default,
            out_format: None,
            no_motd: false,
            fallback_binary: None,
            rsync_path: None,
            remainder: Vec::new(),
            #[cfg(feature = "acl")]
            acls: None,
            #[cfg(feature = "xattr")]
            xattrs: None,
        }
    }

    #[cfg(unix)]
    struct FailingWriter;

    #[cfg(unix)]
    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::Other, "forced failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::Other, "forced failure"))
        }
    }

    #[test]
    fn builder_collects_transfer_arguments() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("source"), OsString::from("dest")])
            .build();

        assert_eq!(
            config.transfer_args(),
            &[OsString::from("source"), OsString::from("dest")]
        );
        assert!(config.has_transfer_request());
        assert!(!config.dry_run());
    }

    #[test]
    fn builder_records_bind_address() {
        let bind = BindAddress::new(
            OsString::from("127.0.0.1"),
            "127.0.0.1:0".parse().expect("socket"),
        );
        let config = ClientConfig::builder()
            .bind_address(Some(bind.clone()))
            .build();

        let recorded = config.bind_address().expect("bind address present");
        assert_eq!(recorded.raw(), bind.raw());
        assert_eq!(recorded.socket(), bind.socket());
    }

    #[test]
    fn builder_append_round_trip() {
        let enabled = ClientConfig::builder().append(true).build();
        assert!(enabled.append());
        assert!(!enabled.append_verify());

        let disabled = ClientConfig::builder().append(false).build();
        assert!(!disabled.append());
        assert!(!disabled.append_verify());
    }

    #[test]
    fn builder_safe_links_round_trip() {
        let enabled = ClientConfig::builder().safe_links(true).build();
        assert!(enabled.safe_links());

        let disabled = ClientConfig::builder().safe_links(false).build();
        assert!(!disabled.safe_links());

        let default_config = ClientConfig::builder().build();
        assert!(!default_config.safe_links());
    }

    #[test]
    fn builder_append_verify_implies_append() {
        let verified = ClientConfig::builder().append_verify(true).build();
        assert!(verified.append());
        assert!(verified.append_verify());

        let cleared = ClientConfig::builder()
            .append(true)
            .append_verify(true)
            .append_verify(false)
            .build();
        assert!(cleared.append());
        assert!(!cleared.append_verify());
    }

    #[test]
    fn builder_backup_round_trip() {
        let enabled = ClientConfig::builder().backup(true).build();
        assert!(enabled.backup());

        let disabled = ClientConfig::builder().build();
        assert!(!disabled.backup());
    }

    #[test]
    fn builder_backup_directory_implies_backup() {
        let config = ClientConfig::builder()
            .backup_directory(Some(PathBuf::from("backups")))
            .build();

        assert!(config.backup());
        assert_eq!(
            config.backup_directory(),
            Some(std::path::Path::new("backups"))
        );

        let cleared = ClientConfig::builder()
            .backup_directory(None::<PathBuf>)
            .build();
        assert!(!cleared.backup());
        assert!(cleared.backup_directory().is_none());
    }

    #[test]
    fn builder_backup_suffix_implies_backup() {
        let config = ClientConfig::builder()
            .backup_suffix(Some(OsString::from(".bak")))
            .build();

        assert!(config.backup());
        assert_eq!(config.backup_suffix(), Some(OsStr::new(".bak")));

        let cleared = ClientConfig::builder()
            .backup(true)
            .backup_suffix(None::<OsString>)
            .build();
        assert!(cleared.backup());
        assert_eq!(cleared.backup_suffix(), None);
    }

    #[test]
    fn builder_enables_dry_run() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .dry_run(true)
            .build();

        assert!(config.dry_run());
    }

    #[test]
    fn builder_enables_list_only() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .list_only(true)
            .build();

        assert!(config.list_only());
    }

    #[test]
    fn builder_sets_compression_setting() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .compression_setting(CompressionSetting::level(CompressionLevel::Best))
            .build();

        assert_eq!(
            config.compression_setting(),
            CompressionSetting::level(CompressionLevel::Best)
        );
    }

    #[test]
    fn builder_defaults_disable_compression() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(!config.compress());
        assert!(config.compression_setting().is_disabled());
    }

    #[test]
    fn builder_enabling_compress_sets_default_level() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .compress(true)
            .build();

        assert!(config.compress());
        assert!(config.compression_setting().is_enabled());
        assert_eq!(
            config.compression_setting().level_or_default(),
            CompressionLevel::Default
        );
    }

    #[test]
    fn builder_sets_min_file_size_limit() {
        let config = ClientConfig::builder().min_file_size(Some(1_024)).build();

        assert_eq!(config.min_file_size(), Some(1_024));

        let cleared = ClientConfig::builder()
            .min_file_size(Some(2048))
            .min_file_size(None)
            .build();

        assert_eq!(cleared.min_file_size(), None);
    }

    #[test]
    fn builder_sets_max_file_size_limit() {
        let config = ClientConfig::builder().max_file_size(Some(8_192)).build();

        assert_eq!(config.max_file_size(), Some(8_192));

        let cleared = ClientConfig::builder()
            .max_file_size(Some(4_096))
            .max_file_size(None)
            .build();

        assert_eq!(cleared.max_file_size(), None);
    }

    #[test]
    fn builder_disabling_compress_clears_override() {
        let level = NonZeroU8::new(5).unwrap();
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .compression_level(Some(CompressionLevel::precise(level)))
            .compress(false)
            .build();

        assert!(!config.compress());
        assert!(config.compression_setting().is_disabled());
        assert_eq!(config.compression_level(), None);
    }

    #[test]
    fn builder_enables_delete() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete(true)
            .build();

        assert!(config.delete());
        assert_eq!(config.delete_mode(), DeleteMode::During);
    }

    #[test]
    fn builder_enables_delete_after() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete_after(true)
            .build();

        assert!(config.delete());
        assert!(config.delete_after());
        assert_eq!(config.delete_mode(), DeleteMode::After);
    }

    #[test]
    fn builder_enables_delete_delay() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete_delay(true)
            .build();

        assert!(config.delete());
        assert!(config.delete_delay());
        assert_eq!(config.delete_mode(), DeleteMode::Delay);
    }

    #[test]
    fn builder_enables_delete_before() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete_before(true)
            .build();

        assert!(config.delete());
        assert!(config.delete_before());
        assert_eq!(config.delete_mode(), DeleteMode::Before);
    }

    #[test]
    fn builder_enables_delete_excluded() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete_excluded(true)
            .build();

        assert!(config.delete_excluded());
        assert!(!ClientConfig::default().delete_excluded());
    }

    #[test]
    fn builder_sets_max_delete_limit() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .max_delete(Some(4))
            .build();

        assert_eq!(config.max_delete(), Some(4));
        assert_eq!(ClientConfig::default().max_delete(), None);
    }

    #[test]
    fn builder_enables_checksum() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .checksum(true)
            .build();

        assert!(config.checksum());
    }

    #[test]
    fn builder_applies_checksum_seed_to_signature_algorithm() {
        let choice = StrongChecksumChoice::parse("xxh64").expect("checksum choice parsed");
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .checksum_choice(choice)
            .checksum_seed(Some(7))
            .build();

        assert_eq!(config.checksum_seed(), Some(7));
        match config.checksum_signature_algorithm() {
            SignatureAlgorithm::Xxh64 { seed } => assert_eq!(seed, 7),
            other => panic!("unexpected signature algorithm: {other:?}"),
        }
    }

    #[test]
    fn builder_sets_bandwidth_limit() {
        let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(4096).unwrap());
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .bandwidth_limit(Some(limit))
            .build();

        assert_eq!(config.bandwidth_limit(), Some(limit));
    }

    #[test]
    fn builder_sets_compression_level() {
        let level = NonZeroU8::new(7).unwrap();
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .compress(true)
            .compression_level(Some(CompressionLevel::precise(level)))
            .build();

        assert!(config.compress());
        assert_eq!(
            config.compression_level(),
            Some(CompressionLevel::precise(level))
        );
        assert_eq!(ClientConfig::default().compression_level(), None);
    }

    #[test]
    fn builder_enables_update() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .update(true)
            .build();

        assert!(config.update());
        assert!(!ClientConfig::default().update());
    }

    #[test]
    fn builder_sets_timeout() {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(30).unwrap());
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .timeout(timeout)
            .build();

        assert_eq!(config.timeout(), timeout);
        assert_eq!(ClientConfig::default().timeout(), TransferTimeout::Default);
    }

    #[test]
    fn builder_sets_connect_timeout() {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(12).unwrap());
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .connect_timeout(timeout)
            .build();

        assert_eq!(config.connect_timeout(), timeout);
        assert_eq!(
            ClientConfig::default().connect_timeout(),
            TransferTimeout::Default
        );
    }

    #[test]
    fn builder_records_modify_window() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .modify_window(Some(5))
            .build();

        assert_eq!(config.modify_window(), Some(5));
        assert_eq!(config.modify_window_duration(), Duration::from_secs(5));
        assert_eq!(
            ClientConfig::default().modify_window_duration(),
            Duration::ZERO
        );
    }

    #[test]
    fn builder_collects_reference_directories() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .compare_destination(PathBuf::from("compare"))
            .copy_destination(PathBuf::from("copy"))
            .link_destination(PathBuf::from("link"))
            .build();

        let references = config.reference_directories();
        assert_eq!(references.len(), 3);
        assert_eq!(references[0].kind(), ReferenceDirectoryKind::Compare);
        assert_eq!(references[1].kind(), ReferenceDirectoryKind::Copy);
        assert_eq!(references[2].kind(), ReferenceDirectoryKind::Link);
        assert_eq!(references[0].path(), PathBuf::from("compare").as_path());
    }

    #[test]
    fn local_copy_options_apply_explicit_timeout() {
        let timeout = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .timeout(timeout)
            .build();

        let options = build_local_copy_options(&config, None);
        assert_eq!(options.timeout(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn local_copy_options_apply_modify_window() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .modify_window(Some(3))
            .build();

        let options = build_local_copy_options(&config, None);
        assert_eq!(options.modify_window(), Duration::from_secs(3));

        let default_config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();
        assert_eq!(
            build_local_copy_options(&default_config, None).modify_window(),
            Duration::ZERO
        );
    }

    #[test]
    fn local_copy_options_omit_timeout_when_unset() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        let options = build_local_copy_options(&config, None);
        assert!(options.timeout().is_none());
    }

    #[test]
    fn local_copy_options_delay_updates_enable_partial_transfers() {
        let enabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delay_updates(true)
            .build();

        let enabled_options = build_local_copy_options(&enabled, None);
        assert!(enabled_options.delay_updates_enabled());
        assert!(enabled_options.partial_enabled());

        let disabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        let disabled_options = build_local_copy_options(&disabled, None);
        assert!(!disabled_options.delay_updates_enabled());
        assert!(!disabled_options.partial_enabled());
    }

    #[test]
    fn local_copy_options_honour_temp_directory_setting() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .temp_directory(Some(PathBuf::from(".staging")))
            .build();

        let options = build_local_copy_options(&config, None);
        assert_eq!(options.temp_directory_path(), Some(Path::new(".staging")));

        let default_config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(
            build_local_copy_options(&default_config, None)
                .temp_directory_path()
                .is_none()
        );
    }

    #[test]
    fn resolve_connect_timeout_prefers_explicit_setting() {
        let explicit = TransferTimeout::Seconds(NonZeroU64::new(5).unwrap());
        let resolved =
            resolve_connect_timeout(explicit, TransferTimeout::Default, Duration::from_secs(30));
        assert_eq!(resolved, Some(Duration::from_secs(5)));
    }

    #[test]
    fn resolve_connect_timeout_uses_transfer_timeout_when_default() {
        let transfer = TransferTimeout::Seconds(NonZeroU64::new(8).unwrap());
        let resolved =
            resolve_connect_timeout(TransferTimeout::Default, transfer, Duration::from_secs(30));
        assert_eq!(resolved, Some(Duration::from_secs(8)));
    }

    #[test]
    fn resolve_connect_timeout_disables_when_requested() {
        let resolved = resolve_connect_timeout(
            TransferTimeout::Disabled,
            TransferTimeout::Seconds(NonZeroU64::new(9).unwrap()),
            Duration::from_secs(30),
        );
        assert!(resolved.is_none());

        let resolved_default = resolve_connect_timeout(
            TransferTimeout::Default,
            TransferTimeout::Disabled,
            Duration::from_secs(30),
        );
        assert!(resolved_default.is_none());
    }

    #[test]
    fn connect_direct_applies_io_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let daemon_addr =
            DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("daemon addr");

        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1];
                let _ = stream.read(&mut buf);
            }
        });

        let timeout = Some(Duration::from_secs(4));
        let mut stream = connect_direct(
            &daemon_addr,
            Some(Duration::from_secs(10)),
            timeout,
            AddressMode::Default,
            None,
        )
        .expect("connect directly");

        assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
        assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

        // Wake the accept loop and close cleanly.
        let _ = stream.write_all(&[0]);
        handle.join().expect("listener thread");
    }

    #[test]
    fn connect_via_proxy_applies_io_timeout() {
        let proxy_listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let proxy_addr = proxy_listener.local_addr().expect("proxy addr");
        let proxy = ProxyConfig {
            host: proxy_addr.ip().to_string(),
            port: proxy_addr.port(),
            credentials: None,
        };

        let target = DaemonAddress::new(String::from("daemon.example"), 873).expect("daemon addr");

        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = proxy_listener.accept() {
                let mut reader = BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).expect("read request") == 0 {
                        return;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }

                let mut stream = reader.into_inner();
                stream
                    .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
                    .expect("respond to connect");
                let _ = stream.flush();
            }
        });

        let timeout = Some(Duration::from_secs(6));
        let stream =
            connect_via_proxy(&target, &proxy, Some(Duration::from_secs(9)), timeout, None)
                .expect("proxy connect");

        assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
        assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

        drop(stream);
        handle.join().expect("proxy thread");
    }

    #[test]
    fn builder_sets_numeric_ids() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .numeric_ids(true)
            .build();

        assert!(config.numeric_ids());

        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(!config.numeric_ids());
    }

    #[test]
    fn builder_preserves_owner_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .owner(true)
            .build();

        assert!(config.preserve_owner());
        assert!(!config.preserve_group());
    }

    #[test]
    fn builder_preserves_group_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .group(true)
            .build();

        assert!(config.preserve_group());
        assert!(!config.preserve_owner());
    }

    #[test]
    fn builder_preserves_permissions_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .permissions(true)
            .build();

        assert!(config.preserve_permissions());
        assert!(!config.preserve_times());
    }

    #[test]
    fn builder_preserves_times_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .times(true)
            .build();

        assert!(config.preserve_times());
        assert!(!config.preserve_permissions());
    }

    #[test]
    fn builder_sets_chmod_modifiers() {
        let modifiers = ChmodModifiers::parse("a+rw").expect("chmod parses");
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .chmod(Some(modifiers.clone()))
            .build();

        assert_eq!(config.chmod(), Some(&modifiers));
    }

    #[test]
    fn builder_preserves_devices_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .devices(true)
            .build();

        assert!(config.preserve_devices());
        assert!(!config.preserve_specials());
    }

    #[test]
    fn builder_preserves_specials_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .specials(true)
            .build();

        assert!(config.preserve_specials());
        assert!(!config.preserve_devices());
    }

    #[test]
    fn builder_preserves_hard_links_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .hard_links(true)
            .build();

        assert!(config.preserve_hard_links());
        assert!(!ClientConfig::default().preserve_hard_links());
    }

    #[cfg(feature = "acl")]
    #[test]
    fn builder_preserves_acls_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .acls(true)
            .build();

        assert!(config.preserve_acls());
        assert!(!ClientConfig::default().preserve_acls());
    }

    #[cfg(feature = "xattr")]
    #[test]
    fn builder_preserves_xattrs_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .xattrs(true)
            .build();

        assert!(config.preserve_xattrs());
        assert!(!ClientConfig::default().preserve_xattrs());
    }

    #[test]
    fn builder_preserves_remove_source_files_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .remove_source_files(true)
            .build();

        assert!(config.remove_source_files());
        assert!(!ClientConfig::default().remove_source_files());
    }

    #[test]
    fn builder_controls_omit_dir_times_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .omit_dir_times(true)
            .build();

        assert!(config.omit_dir_times());
        assert!(!ClientConfig::default().omit_dir_times());
    }

    #[test]
    fn builder_controls_omit_link_times_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .omit_link_times(true)
            .build();

        assert!(config.omit_link_times());
        assert!(!ClientConfig::default().omit_link_times());
    }

    #[test]
    fn builder_enables_sparse() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .sparse(true)
            .build();

        assert!(config.sparse());
    }

    #[test]
    fn builder_enables_size_only() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .size_only(true)
            .build();

        assert!(config.size_only());
        assert!(!ClientConfig::default().size_only());
    }

    #[test]
    fn builder_configures_implied_dirs_flag() {
        let default_config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(default_config.implied_dirs());
        assert!(ClientConfig::default().implied_dirs());

        let disabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .implied_dirs(false)
            .build();

        assert!(!disabled.implied_dirs());

        let enabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .implied_dirs(true)
            .build();

        assert!(enabled.implied_dirs());
    }

    #[test]
    fn builder_sets_mkpath_flag() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .mkpath(true)
            .build();

        assert!(config.mkpath());
        assert!(!ClientConfig::default().mkpath());
    }

    #[test]
    fn builder_sets_inplace() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .inplace(true)
            .build();

        assert!(config.inplace());

        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(!config.inplace());
    }

    #[test]
    fn builder_sets_delay_updates() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delay_updates(true)
            .build();

        assert!(config.delay_updates());

        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(!config.delay_updates());
    }

    #[test]
    fn builder_sets_copy_dirlinks() {
        let enabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .copy_dirlinks(true)
            .build();

        assert!(enabled.copy_dirlinks());

        let disabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(!disabled.copy_dirlinks());
    }

    #[test]
    fn builder_sets_keep_dirlinks() {
        let enabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .keep_dirlinks(true)
            .build();

        assert!(enabled.keep_dirlinks());

        let disabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(!disabled.keep_dirlinks());
    }

    #[test]
    fn builder_enables_stats() {
        let enabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .stats(true)
            .build();

        assert!(enabled.stats());

        let disabled = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        assert!(!disabled.stats());
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_invocation_forwards_streams() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let script = write_fallback_script(temp.path());

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut args = baseline_fallback_args();
        args.files_from_used = true;
        args.file_list_entries = vec![OsString::from("alpha"), OsString::from("beta")];
        args.fallback_binary = Some(script.into_os_string());

        let exit_code = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert_eq!(exit_code, 42);
        assert_eq!(
            String::from_utf8(stdout).expect("stdout utf8"),
            "alpha\nbeta\nfallback stdout\n"
        );
        assert_eq!(
            String::from_utf8(stderr).expect("stderr utf8"),
            "fallback stderr\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_writes_password_to_stdin() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("password.txt");
        let script_path = temp.path().join("capture-password.sh");
        let script = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\ncat > \"$OUTPUT\"\n"
        );
        fs::write(&script_path, script).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.password_file = Some(PathBuf::from("-"));
        args.daemon_password = Some(b"topsecret".to_vec());
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert_eq!(exit, 0);
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let captured = fs::read(&capture_path).expect("captured password");
        assert_eq!(captured, b"topsecret\n");
    }

    #[test]
    fn map_local_copy_error_reports_delete_limit() {
        let mapped = map_local_copy_error(LocalCopyError::delete_limit_exceeded(2));
        assert_eq!(mapped.exit_code(), MAX_DELETE_EXIT_CODE);
        let rendered = mapped.message().to_string();
        assert!(
            rendered.contains("Deletions stopped due to --max-delete limit (2 entries skipped)"),
            "unexpected diagnostic: {rendered}"
        );
    }

    #[test]
    fn write_daemon_password_appends_newline_and_zeroizes_buffer() {
        let mut output = Vec::new();
        let mut secret = b"swordfish".to_vec();

        write_daemon_password(&mut output, &mut secret).expect("write succeeds");

        assert_eq!(output, b"swordfish\n");
        assert!(secret.iter().all(|&byte| byte == 0));
    }

    #[test]
    fn write_daemon_password_handles_existing_newline() {
        let mut output = Vec::new();
        let mut secret = b"hunter2\n".to_vec();

        write_daemon_password(&mut output, &mut secret).expect("write succeeds");

        assert_eq!(output, b"hunter2\n");
        assert!(secret.iter().all(|&byte| byte == 0));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_copy_links_toggle() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n",
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.copy_links = Some(true);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--copy-links"));

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.copy_links = Some(false);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--no-copy-links"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_checksum_choice() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.checksum = true;
        args.checksum_choice = Some(OsString::from("xxh128"));
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--checksum"));
        assert!(
            captured
                .lines()
                .any(|line| line == "--checksum-choice=xxh128")
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_checksum_seed() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.checksum = true;
        args.checksum_seed = Some(123);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--checksum"));
        assert!(captured.lines().any(|line| line == "--checksum-seed=123"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_copy_dirlinks_flag() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.copy_dirlinks = true;
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--copy-dirlinks"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_one_file_system_toggle() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n",
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.one_file_system = Some(true);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--one-file-system"));

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.one_file_system = Some(false);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--no-one-file-system"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_backup_arguments() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.backup = true;
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--backup"));
        assert!(!captured.lines().any(|line| line == "--backup-dir"));
        assert!(!captured.lines().any(|line| line == "--suffix"));

        fs::write(&capture_path, b"").expect("truncate capture");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.backup = true;
        args.backup_dir = Some(PathBuf::from("backups"));
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--backup"));
        assert!(captured.lines().any(|line| line == "--backup-dir"));
        assert!(captured.lines().any(|line| line == "backups"));

        fs::write(&capture_path, b"").expect("truncate capture");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.backup = true;
        args.backup_suffix = Some(OsString::from(".bak"));
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--backup"));
        assert!(captured.lines().any(|line| line == "--suffix"));
        assert!(captured.lines().any(|line| line == ".bak"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_keep_dirlinks_flags() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.keep_dirlinks = Some(true);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--keep-dirlinks"));

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.keep_dirlinks = Some(false);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--no-keep-dirlinks"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_safe_links_flag() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.safe_links = true;
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--safe-links"));

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.safe_links = false;
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(!captured.lines().any(|line| line == "--safe-links"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_chmod_arguments() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.chmod = vec![OsString::from("Du+rwx"), OsString::from("Fgo-w")];
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--chmod=Du+rwx"));
        assert!(captured.lines().any(|line| line == "--chmod=Fgo-w"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_reference_directory_flags() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.compare_destinations =
            vec![OsString::from("compare-one"), OsString::from("compare-two")];
        args.copy_destinations = vec![OsString::from("copy-one")];
        args.link_destinations = vec![OsString::from("link-one"), OsString::from("link-two")];
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        let lines: Vec<&str> = captured.lines().collect();
        let expected_pairs = [
            ("--compare-dest", "compare-one"),
            ("--compare-dest", "compare-two"),
            ("--copy-dest", "copy-one"),
            ("--link-dest", "link-one"),
            ("--link-dest", "link-two"),
        ];

        for (flag, path) in expected_pairs {
            assert!(
                lines
                    .windows(2)
                    .any(|window| window[0] == flag && window[1] == path),
                "missing pair {flag} {path} in {:?}",
                lines
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_cvs_exclude_flag() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.into_os_string());
        args.cvs_exclude = true;
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--cvs-exclude"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_mkpath_flag() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.mkpath = true;
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--mkpath"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_partial_dir_argument() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n",
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.partial = true;
        args.partial_dir = Some(PathBuf::from(".rsync-partial"));
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--partial"));
        assert!(captured.lines().any(|line| line == "--partial-dir"));
        assert!(captured.lines().any(|line| line == ".rsync-partial"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_delay_updates_flag() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.delay_updates = true;
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");
        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--delay-updates"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_itemize_changes_flag() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        const ITEMIZE_FORMAT: &str = "%i %n%L";

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.itemize_changes = true;
        args.out_format = Some(OsString::from(ITEMIZE_FORMAT));
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--itemize-changes"));
        assert!(captured.lines().any(|line| line == "--out-format"));
        assert!(captured.lines().any(|line| line == ITEMIZE_FORMAT));
    }

    #[cfg(unix)]
    #[test]
    fn run_client_or_fallback_uses_fallback_for_remote_operands() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let script = write_fallback_script(temp.path());

        let config = ClientConfig::builder()
            .transfer_args([OsString::from("remote::module"), OsString::from("/tmp/dst")])
            .build();

        let mut args = baseline_fallback_args();
        args.remainder = vec![OsString::from("remote::module"), OsString::from("/tmp/dst")];
        args.fallback_binary = Some(script.into_os_string());

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

        let outcome = run_client_or_fallback(config, None, Some(context))
            .expect("fallback invocation succeeds");

        match outcome {
            ClientOutcome::Fallback(summary) => {
                assert_eq!(summary.exit_code(), 42);
            }
            ClientOutcome::Local(_) => panic!("expected fallback outcome"),
        }

        assert_eq!(
            String::from_utf8(stdout).expect("stdout utf8"),
            "fallback stdout\n"
        );
        assert_eq!(
            String::from_utf8(stderr).expect("stderr utf8"),
            "fallback stderr\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_client_or_fallback_handles_delta_mode_locally() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let script = write_fallback_script(temp.path());

        let source_path = temp.path().join("source.txt");
        let dest_path = temp.path().join("dest.txt");
        fs::write(&source_path, b"delta-test").expect("source created");

        let source = OsString::from(source_path.as_os_str());
        let dest = OsString::from(dest_path.as_os_str());

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), dest.clone()])
            .whole_file(false)
            .build();

        let mut args = baseline_fallback_args();
        args.remainder = vec![source, dest];
        args.whole_file = Some(false);
        args.fallback_binary = Some(script.into_os_string());

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let context = RemoteFallbackContext::new(&mut stdout, &mut stderr, args);

        let outcome =
            run_client_or_fallback(config, None, Some(context)).expect("local delta copy succeeds");

        match outcome {
            ClientOutcome::Local(summary) => {
                assert_eq!(summary.files_copied(), 1);
            }
            ClientOutcome::Fallback(_) => panic!("unexpected fallback execution"),
        }

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        assert_eq!(fs::read(dest_path).expect("dest contents"), b"delta-test");
    }

    #[test]
    fn remote_fallback_reports_launch_errors() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let missing = temp.path().join("missing-rsync");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(missing.into_os_string());

        let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect_err("spawn failure reported");

        assert_eq!(error.exit_code(), 1);
        let message = format!("{error}");
        assert!(message.contains("failed to launch fallback rsync binary"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_reports_stdout_forward_errors() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let script = write_fallback_script(temp.path());

        let mut stdout = FailingWriter;
        let mut stderr = Vec::new();

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script.into_os_string());

        let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect_err("stdout forwarding failure surfaces");

        assert_eq!(error.exit_code(), 1);
        let message = format!("{error}");
        assert!(message.contains("failed to forward fallback stdout"));
    }

    #[test]
    fn remote_fallback_reports_disabled_override() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set_os("OC_RSYNC_FALLBACK", OsStr::new("no"));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let args = baseline_fallback_args();
        let error = run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect_err("disabled override prevents fallback execution");

        assert_eq!(error.exit_code(), 1);
        let message = format!("{error}");
        assert!(message.contains(
            "remote transfers are unavailable because OC_RSYNC_FALLBACK is disabled",
        ));
    }

    #[test]
    fn builder_forces_event_collection() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .force_event_collection(true)
            .build();

        assert!(config.force_event_collection());
        assert!(config.collect_events());
    }

    #[test]
    fn run_client_reports_missing_operands() {
        let config = ClientConfig::builder().build();
        let error = run_client(config).expect_err("missing operands should error");

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(rendered.contains("missing source operands"));
        assert!(rendered.contains("[client=3.4.1-rust]"));
    }

    #[test]
    fn run_client_handles_delta_transfer_mode_locally() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.bin");
        let destination = tmp.path().join("dest.bin");
        fs::write(&source, b"payload").expect("write source");

        let config = ClientConfig::builder()
            .transfer_args([
                source.clone().into_os_string(),
                destination.clone().into_os_string(),
            ])
            .whole_file(false)
            .build();

        let summary = run_client(config).expect("delta mode executes locally");

        assert_eq!(fs::read(&destination).expect("read dest"), b"payload");
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.bytes_copied(), b"payload".len() as u64);
    }

    #[test]
    fn run_client_copies_single_file() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("dest.txt");
        fs::write(&source, b"example").expect("write source");

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .permissions(true)
            .times(true)
            .build();

        assert!(config.preserve_permissions());
        assert!(config.preserve_times());

        let summary = run_client(config).expect("copy succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), b"example");
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.bytes_copied(), b"example".len() as u64);
        assert!(!summary.compression_used());
        assert!(summary.compressed_bytes().is_none());
    }

    #[test]
    fn run_client_with_compress_records_compressed_bytes() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.bin");
        let destination = tmp.path().join("dest.bin");
        let payload = vec![b'Z'; 32 * 1024];
        fs::write(&source, &payload).expect("write source");

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .compress(true)
            .build();

        let summary = run_client(config).expect("copy succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), payload);
        assert!(summary.compression_used());
        let compressed = summary
            .compressed_bytes()
            .expect("compressed bytes recorded");
        assert!(compressed > 0);
        assert!(compressed <= summary.bytes_copied());
    }

    #[test]
    fn run_client_skip_compress_disables_compression_for_matching_suffix() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("archive.gz");
        let destination = tmp.path().join("dest.gz");
        let payload = vec![b'X'; 16 * 1024];
        fs::write(&source, &payload).expect("write source");

        let skip = SkipCompressList::parse("gz").expect("parse list");
        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .compress(true)
            .skip_compress(skip)
            .build();

        let summary = run_client(config).expect("copy succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), payload);
        assert!(!summary.compression_used());
        assert!(summary.compressed_bytes().is_none());
    }

    #[test]
    fn run_client_remove_source_files_deletes_source() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("dest.txt");
        fs::write(&source, b"move me").expect("write source");

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .remove_source_files(true)
            .build();

        let summary = run_client(config).expect("copy succeeds");

        assert_eq!(summary.sources_removed(), 1);
        assert!(!source.exists(), "source should be removed after transfer");
        assert_eq!(fs::read(&destination).expect("read dest"), b"move me");
    }

    #[test]
    fn run_client_remove_source_files_preserves_matched_source() {
        use filetime::{FileTime, set_file_times};

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("dest.txt");
        let payload = b"stable";
        fs::write(&source, payload).expect("write source");
        fs::write(&destination, payload).expect("write destination");

        let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_times(&source, timestamp, timestamp).expect("set source times");
        set_file_times(&destination, timestamp, timestamp).expect("set dest times");

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .remove_source_files(true)
            .times(true)
            .build();

        let summary = run_client(config).expect("transfer succeeds");

        assert_eq!(summary.sources_removed(), 0, "unchanged sources remain");
        assert!(source.exists(), "matched source should not be removed");
        assert_eq!(fs::read(&destination).expect("read dest"), payload);
    }

    #[test]
    fn run_client_dry_run_skips_copy() {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source.txt");
        let destination = tmp.path().join("dest.txt");
        fs::write(&source, b"dry-run").expect("write source");

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .dry_run(true)
            .build();

        let summary = run_client(config).expect("dry-run succeeds");

        assert!(!destination.exists());
        assert_eq!(summary.files_copied(), 1);
    }

    #[test]
    fn run_client_delete_removes_extraneous_entries() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

        let dest_root = tmp.path().join("dest");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
        fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

        let mut source_operand = source_root.clone().into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_operand, dest_root.clone().into_os_string()])
            .delete(true)
            .build();

        let summary = run_client(config).expect("copy succeeds");

        assert_eq!(
            fs::read(dest_root.join("keep.txt")).expect("read keep"),
            b"fresh"
        );
        assert!(!dest_root.join("extra.txt").exists());
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.items_deleted(), 1);
    }

    #[test]
    fn run_client_delete_respects_dry_run() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

        let dest_root = tmp.path().join("dest");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
        fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

        let mut source_operand = source_root.clone().into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_operand, dest_root.clone().into_os_string()])
            .dry_run(true)
            .delete(true)
            .build();

        let summary = run_client(config).expect("dry-run succeeds");

        assert_eq!(
            fs::read(dest_root.join("keep.txt")).expect("read keep"),
            b"stale"
        );
        assert!(dest_root.join("extra.txt").exists());
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.items_deleted(), 1);
    }

    #[test]
    fn run_client_update_skips_newer_destination() {
        use filetime::{FileTime, set_file_times};

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-update.txt");
        let destination = tmp.path().join("dest-update.txt");
        fs::write(&source, b"fresh").expect("write source");
        fs::write(&destination, b"existing").expect("write destination");

        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let newer = FileTime::from_unix_time(1_700_000_100, 0);
        set_file_times(&source, older, older).expect("set source times");
        set_file_times(&destination, newer, newer).expect("set dest times");

        let summary = run_client(
            ClientConfig::builder()
                .transfer_args([
                    source.clone().into_os_string(),
                    destination.clone().into_os_string(),
                ])
                .update(true)
                .build(),
        )
        .expect("run client");

        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.regular_files_skipped_newer(), 1);
        assert_eq!(
            fs::read(destination).expect("read destination"),
            b"existing"
        );
    }

    #[test]
    fn run_client_respects_filter_rules() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .extend_filter_rules([FilterRuleSpec::exclude("*.tmp".to_string())])
            .build();

        let summary = run_client(config).expect("copy succeeds");

        assert!(dest_root.join("source").join("keep.txt").exists());
        assert!(!dest_root.join("source").join("skip.tmp").exists());
        assert!(summary.files_copied() >= 1);
    }

    #[test]
    fn run_client_filter_clear_resets_previous_rules() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let dest_root = tmp.path().join("dest");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        fs::write(source_root.join("skip.tmp"), b"skip").expect("write skip");

        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .extend_filter_rules([
                FilterRuleSpec::exclude("*.tmp".to_string()),
                FilterRuleSpec::clear(),
                FilterRuleSpec::exclude("keep.txt".to_string()),
            ])
            .build();

        let summary = run_client(config).expect("copy succeeds");

        let copied_root = dest_root.join("source");
        assert!(copied_root.join("skip.tmp").exists());
        assert!(!copied_root.join("keep.txt").exists());
        assert!(summary.files_copied() >= 1);
    }

    #[test]
    fn run_client_copies_directory_tree() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let nested = source_root.join("nested");
        let source_file = nested.join("file.txt");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(&source_file, b"tree").expect("write source file");

        let dest_root = tmp.path().join("destination");

        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .build();

        let summary = run_client(config).expect("directory copy succeeds");

        let copied_file = dest_root.join("nested").join("file.txt");
        assert_eq!(fs::read(copied_file).expect("read copied"), b"tree");
        assert!(summary.files_copied() >= 1);
        assert!(summary.directories_created() >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_copies_symbolic_link() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let target_file = tmp.path().join("target.txt");
        fs::write(&target_file, b"symlink target").expect("write target");

        let source_link = tmp.path().join("source-link");
        symlink(&target_file, &source_link).expect("create source symlink");

        let destination_link = tmp.path().join("dest-link");
        let config = ClientConfig::builder()
            .transfer_args([source_link.clone(), destination_link.clone()])
            .force_event_collection(true)
            .build();

        let summary = run_client(config).expect("link copy succeeds");

        let copied = fs::read_link(destination_link).expect("read copied link");
        assert_eq!(copied, target_file);
        assert_eq!(summary.symlinks_copied(), 1);

        let event = summary
            .events()
            .iter()
            .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
            .expect("symlink event present");
        let recorded_target = event
            .metadata()
            .and_then(ClientEntryMetadata::symlink_target)
            .expect("symlink target recorded");
        assert_eq!(recorded_target, target_file.as_path());
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_symbolic_links_in_directories() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let nested = source_root.join("nested");
        fs::create_dir_all(&nested).expect("create nested");

        let target_file = tmp.path().join("target.txt");
        fs::write(&target_file, b"data").expect("write target");
        let link_path = nested.join("link");
        symlink(&target_file, &link_path).expect("create link");

        let dest_root = tmp.path().join("destination");
        let config = ClientConfig::builder()
            .transfer_args([source_root.clone(), dest_root.clone()])
            .force_event_collection(true)
            .build();

        let summary = run_client(config).expect("directory copy succeeds");

        let copied_link = dest_root.join("nested").join("link");
        let copied_target = fs::read_link(copied_link).expect("read copied link");
        assert_eq!(copied_target, target_file);
        assert_eq!(summary.symlinks_copied(), 1);

        let event = summary
            .events()
            .iter()
            .find(|event| matches!(event.kind(), ClientEventKind::SymlinkCopied))
            .expect("symlink event present");
        let recorded_target = event
            .metadata()
            .and_then(ClientEntryMetadata::symlink_target)
            .expect("symlink target recorded");
        assert_eq!(recorded_target, target_file.as_path());
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_file_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("source-metadata.txt");
        let destination = tmp.path().join("dest-metadata.txt");
        fs::write(&source, b"metadata").expect("write source");

        let mode = 0o640;
        fs::set_permissions(&source, PermissionsExt::from_mode(mode))
            .expect("set source permissions");
        let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
        let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
        set_file_times(&source, atime, mtime).expect("set source timestamps");

        let source_metadata = fs::metadata(&source).expect("source metadata");
        assert_eq!(source_metadata.permissions().mode() & 0o777, mode);
        let src_atime = FileTime::from_last_access_time(&source_metadata);
        let src_mtime = FileTime::from_last_modification_time(&source_metadata);
        assert_eq!(src_atime, atime);
        assert_eq!(src_mtime, mtime);

        let config = ClientConfig::builder()
            .transfer_args([source.clone(), destination.clone()])
            .permissions(true)
            .times(true)
            .build();

        let summary = run_client(config).expect("copy succeeds");

        let dest_metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
        assert_eq!(summary.files_copied(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_preserves_directory_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source-dir");
        fs::create_dir(&source_dir).expect("create source dir");

        let mode = 0o751;
        fs::set_permissions(&source_dir, PermissionsExt::from_mode(mode))
            .expect("set directory permissions");
        let atime = FileTime::from_unix_time(1_700_010_000, 0);
        let mtime = FileTime::from_unix_time(1_700_020_000, 789_000_000);
        set_file_times(&source_dir, atime, mtime).expect("set directory timestamps");

        let destination_dir = tmp.path().join("dest-dir");
        let config = ClientConfig::builder()
            .transfer_args([source_dir.clone(), destination_dir.clone()])
            .permissions(true)
            .times(true)
            .build();

        assert!(config.preserve_permissions());
        assert!(config.preserve_times());

        let summary = run_client(config).expect("directory copy succeeds");

        let dest_metadata = fs::metadata(&destination_dir).expect("dest metadata");
        assert!(dest_metadata.is_dir());
        assert_eq!(dest_metadata.permissions().mode() & 0o777, mode);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
        assert!(summary.directories_created() >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_updates_existing_directory_metadata() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().expect("tempdir");
        let source_dir = tmp.path().join("source-tree");
        let source_nested = source_dir.join("nested");
        fs::create_dir_all(&source_nested).expect("create source tree");

        let source_mode = 0o745;
        fs::set_permissions(&source_nested, PermissionsExt::from_mode(source_mode))
            .expect("set source nested permissions");
        let source_atime = FileTime::from_unix_time(1_700_030_000, 1_000_000);
        let source_mtime = FileTime::from_unix_time(1_700_040_000, 2_000_000);
        set_file_times(&source_nested, source_atime, source_mtime)
            .expect("set source nested timestamps");

        let dest_root = tmp.path().join("dest-root");
        fs::create_dir(&dest_root).expect("create dest root");
        let dest_dir = dest_root.join("source-tree");
        let dest_nested = dest_dir.join("nested");
        fs::create_dir_all(&dest_nested).expect("pre-create destination tree");

        let dest_mode = 0o711;
        fs::set_permissions(&dest_nested, PermissionsExt::from_mode(dest_mode))
            .expect("set dest nested permissions");
        let dest_atime = FileTime::from_unix_time(1_600_000_000, 0);
        let dest_mtime = FileTime::from_unix_time(1_600_100_000, 0);
        set_file_times(&dest_nested, dest_atime, dest_mtime).expect("set dest nested timestamps");

        let config = ClientConfig::builder()
            .transfer_args([source_dir.clone(), dest_root.clone()])
            .permissions(true)
            .times(true)
            .build();

        assert!(config.preserve_permissions());
        assert!(config.preserve_times());

        let _summary = run_client(config).expect("directory copy succeeds");

        let copied_nested = dest_root.join("source-tree").join("nested");
        let copied_metadata = fs::metadata(&copied_nested).expect("dest metadata");
        assert!(copied_metadata.is_dir());
        assert_eq!(copied_metadata.permissions().mode() & 0o777, source_mode);
        let copied_atime = FileTime::from_last_access_time(&copied_metadata);
        let copied_mtime = FileTime::from_last_modification_time(&copied_metadata);
        assert_eq!(copied_atime, source_atime);
        assert_eq!(copied_mtime, source_mtime);
    }

    #[cfg(unix)]
    #[test]
    fn run_client_sparse_copy_creates_holes() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("sparse-source.bin");
        let mut source_file = fs::File::create(&source).expect("create source");
        source_file.write_all(&[0x11]).expect("write leading");
        source_file
            .seek(SeekFrom::Start(1 * 1024 * 1024))
            .expect("seek to hole");
        source_file.write_all(&[0x22]).expect("write middle");
        source_file
            .seek(SeekFrom::Start(4 * 1024 * 1024))
            .expect("seek to tail");
        source_file.write_all(&[0x33]).expect("write tail");
        source_file.set_len(6 * 1024 * 1024).expect("extend source");

        let dense_dest = tmp.path().join("dense.bin");
        let sparse_dest = tmp.path().join("sparse.bin");

        let dense_config = ClientConfig::builder()
            .transfer_args([
                source.clone().into_os_string(),
                dense_dest.clone().into_os_string(),
            ])
            .permissions(true)
            .times(true)
            .build();
        let summary = run_client(dense_config).expect("dense copy succeeds");
        assert!(summary.events().is_empty());

        let sparse_config = ClientConfig::builder()
            .transfer_args([
                source.into_os_string(),
                sparse_dest.clone().into_os_string(),
            ])
            .permissions(true)
            .times(true)
            .sparse(true)
            .build();
        let summary = run_client(sparse_config).expect("sparse copy succeeds");
        assert!(summary.events().is_empty());

        let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
        let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

        assert_eq!(dense_meta.len(), sparse_meta.len());
        assert!(sparse_meta.blocks() < dense_meta.blocks());
    }

    #[test]
    fn run_client_merges_directory_contents_when_trailing_separator_present() {
        let tmp = tempdir().expect("tempdir");
        let source_root = tmp.path().join("source");
        let nested = source_root.join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        let file_path = nested.join("file.txt");
        fs::write(&file_path, b"contents").expect("write file");

        let dest_root = tmp.path().join("dest");
        let mut source_arg = source_root.clone().into_os_string();
        source_arg.push(std::path::MAIN_SEPARATOR.to_string());

        let config = ClientConfig::builder()
            .transfer_args([source_arg, dest_root.clone().into_os_string()])
            .build();

        let summary = run_client(config).expect("directory contents copy succeeds");

        assert!(dest_root.is_dir());
        assert!(dest_root.join("nested").is_dir());
        assert_eq!(
            fs::read(dest_root.join("nested").join("file.txt")).expect("read copied"),
            b"contents"
        );
        assert!(!dest_root.join("source").exists());
        assert!(summary.files_copied() >= 1);
    }

    #[test]
    fn module_list_request_detects_remote_url() {
        let operands = vec![OsString::from("rsync://example.com:8730/")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "example.com");
        assert_eq!(request.address().port(), 8730);
    }

    #[test]
    fn module_list_request_accepts_mixed_case_scheme() {
        let operands = vec![OsString::from("RSyNc://Example.COM/")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "Example.COM");
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn module_list_request_honours_custom_default_port() {
        let operands = vec![OsString::from("rsync://example.com/")];
        let request = ModuleListRequest::from_operands_with_port(&operands, 10_873)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "example.com");
        assert_eq!(request.address().port(), 10_873);
    }

    #[test]
    fn module_list_request_rejects_remote_transfer() {
        let operands = vec![OsString::from("rsync://example.com/module")];
        let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
        assert!(request.is_none());
    }

    #[test]
    fn module_list_request_accepts_username_in_rsync_url() {
        let operands = vec![OsString::from("rsync://user@example.com/")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "example.com");
        assert_eq!(request.address().port(), 873);
        assert_eq!(request.username(), Some("user"));
    }

    #[test]
    fn module_list_request_accepts_username_in_legacy_syntax() {
        let operands = vec![OsString::from("user@example.com::")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "example.com");
        assert_eq!(request.address().port(), 873);
        assert_eq!(request.username(), Some("user"));
    }

    #[test]
    fn module_list_request_supports_ipv6_in_rsync_url() {
        let operands = vec![OsString::from("rsync://[2001:db8::1]/")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "2001:db8::1");
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn module_list_request_supports_ipv6_in_legacy_syntax() {
        let operands = vec![OsString::from("[fe80::1]::")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "fe80::1");
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn module_list_request_decodes_percent_encoded_host() {
        let operands = vec![OsString::from("rsync://example%2Ecom/")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "example.com");
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn module_list_request_supports_ipv6_zone_identifier() {
        let operands = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "fe80::1%eth0");
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn module_list_request_supports_raw_ipv6_zone_identifier() {
        let operands = vec![OsString::from("[fe80::1%eth0]::")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "fe80::1%eth0");
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn module_list_request_decodes_percent_encoded_username() {
        let operands = vec![OsString::from("user%2Bname@localhost::")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.username(), Some("user+name"));
        assert_eq!(request.address().host(), "localhost");
    }

    #[test]
    fn module_list_request_rejects_truncated_percent_encoding_in_username() {
        let operands = vec![OsString::from("user%2@localhost::")];
        let error =
            ModuleListRequest::from_operands(&operands).expect_err("invalid encoding should fail");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("invalid percent-encoding in daemon username")
        );
    }

    #[test]
    fn module_list_request_defaults_to_localhost_for_shorthand() {
        let operands = vec![OsString::from("::")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "localhost");
        assert_eq!(request.address().port(), 873);
        assert!(request.username().is_none());
    }

    #[test]
    fn module_list_request_preserves_username_with_default_host() {
        let operands = vec![OsString::from("user@::")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request detected");
        assert_eq!(request.address().host(), "localhost");
        assert_eq!(request.address().port(), 873);
        assert_eq!(request.username(), Some("user"));
    }

    #[test]
    fn module_list_options_reports_address_mode() {
        let options = ModuleListOptions::default().with_address_mode(AddressMode::Ipv6);
        assert_eq!(options.address_mode(), AddressMode::Ipv6);

        let default_options = ModuleListOptions::default();
        assert_eq!(default_options.address_mode(), AddressMode::Default);
    }

    #[test]
    fn module_list_options_records_bind_address() {
        let socket = "198.51.100.4:0".parse().expect("socket");
        let options = ModuleListOptions::default().with_bind_address(Some(socket));
        assert_eq!(options.bind_address(), Some(socket));

        let default_options = ModuleListOptions::default();
        assert!(default_options.bind_address().is_none());
    }

    #[test]
    fn resolve_daemon_addresses_filters_ipv4_mode() {
        let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
        let addresses = resolve_daemon_addresses(&address, AddressMode::Ipv4)
            .expect("ipv4 resolution succeeds");

        assert!(!addresses.is_empty());
        assert!(addresses.iter().all(std::net::SocketAddr::is_ipv4));
    }

    #[test]
    fn resolve_daemon_addresses_rejects_missing_ipv6_addresses() {
        let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
        let error = resolve_daemon_addresses(&address, AddressMode::Ipv6)
            .expect_err("IPv6 filtering should fail for IPv4-only host");

        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(rendered.contains("does not have IPv6 addresses"));
    }

    #[test]
    fn resolve_daemon_addresses_filters_ipv6_mode() {
        let address = DaemonAddress::new(String::from("::1"), 873).expect("address");
        let addresses = resolve_daemon_addresses(&address, AddressMode::Ipv6)
            .expect("ipv6 resolution succeeds");

        assert!(!addresses.is_empty());
        assert!(addresses.iter().all(std::net::SocketAddr::is_ipv6));
    }

    #[test]
    fn daemon_address_accepts_ipv6_zone_identifier() {
        let address = DaemonAddress::new(String::from("fe80::1%eth0"), 873)
            .expect("zone identifier accepted");
        assert_eq!(address.host(), "fe80::1%eth0");
        assert_eq!(address.port(), 873);

        let display = format!("{}", address.socket_addr_display());
        assert_eq!(display, "[fe80::1%eth0]:873");
    }

    #[test]
    fn module_list_request_parses_ipv6_zone_identifier() {
        let operands = vec![OsString::from("rsync://fe80::1%eth0/")];
        let request = ModuleListRequest::from_operands(&operands)
            .expect("parse succeeds")
            .expect("request present");
        assert_eq!(request.address().host(), "fe80::1%eth0");
        assert_eq!(request.address().port(), 873);

        let bracketed = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
        let request = ModuleListRequest::from_operands(&bracketed)
            .expect("parse succeeds")
            .expect("request present");
        assert_eq!(request.address().host(), "fe80::1%eth0");
        assert_eq!(request.address().port(), 873);
    }

    #[test]
    fn module_list_request_rejects_truncated_percent_encoding() {
        let operands = vec![OsString::from("rsync://example%2/")];
        let error = ModuleListRequest::from_operands(&operands)
            .expect_err("truncated percent encoding should fail");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("invalid percent-encoding in daemon host")
        );
    }

    #[test]
    fn daemon_address_trims_host_whitespace() {
        let address =
            DaemonAddress::new("  example.com  ".to_string(), 873).expect("address trims host");
        assert_eq!(address.host(), "example.com");
        assert_eq!(address.port(), 873);
    }

    #[test]
    fn module_list_request_rejects_empty_username() {
        let operands = vec![OsString::from("@example.com::")];
        let error = ModuleListRequest::from_operands(&operands)
            .expect_err("empty username should be rejected");
        let rendered = error.message().to_string();
        assert!(rendered.contains("daemon username must be non-empty"));
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    }

    #[test]
    fn module_list_request_rejects_ipv6_module_transfer() {
        let operands = vec![OsString::from("[fe80::1]::module")];
        let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
        assert!(request.is_none());
    }

    #[test]
    fn module_list_request_requires_bracketed_ipv6_host() {
        let operands = vec![OsString::from("fe80::1::")];
        let error = ModuleListRequest::from_operands(&operands)
            .expect_err("unbracketed IPv6 host should be rejected");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("IPv6 daemon addresses must be enclosed in brackets")
        );
    }

    #[test]
    fn run_module_list_collects_entries() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec![
            "@RSYNCD: MOTD Welcome to the test daemon\n",
            "@RSYNCD: MOTD Maintenance window at 02:00 UTC\n",
            "@RSYNCD: OK\n",
            "alpha\tPrimary module\n",
            "beta\n",
            "@RSYNCD: EXIT\n",
        ];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("module list succeeds");
        assert_eq!(
            list.motd_lines(),
            &[
                String::from("Welcome to the test daemon"),
                String::from("Maintenance window at 02:00 UTC"),
            ]
        );
        assert!(list.capabilities().is_empty());
        assert_eq!(list.entries().len(), 2);
        assert_eq!(list.entries()[0].name(), "alpha");
        assert_eq!(list.entries()[0].comment(), Some("Primary module"));
        assert_eq!(list.entries()[1].name(), "beta");
        assert_eq!(list.entries()[1].comment(), None);

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_uses_connect_program_command() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let command = OsString::from(
            "sh -c 'CONNECT_HOST=%H\n\
             CONNECT_PORT=%P\n\
             printf \"@RSYNCD: 31.0\\n\"\n\
             read greeting\n\
             printf \"@RSYNCD: OK\\n\"\n\
             read request\n\
             printf \"example\\t$CONNECT_HOST:$CONNECT_PORT\\n@RSYNCD: EXIT\\n\"'",
        );

        let _prog_guard = EnvGuard::set_os("RSYNC_CONNECT_PROG", &command);
        let _shell_guard = EnvGuard::remove("RSYNC_SHELL");
        let _proxy_guard = EnvGuard::remove("RSYNC_PROXY");

        let request = ModuleListRequest {
            address: DaemonAddress::new("example.com".to_string(), 873).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("connect program listing succeeds");
        assert_eq!(list.entries().len(), 1);
        let entry = &list.entries()[0];
        assert_eq!(entry.name(), "example");
        assert_eq!(entry.comment(), Some("example.com:873"));
    }

    #[test]
    fn connect_program_token_expansion_matches_upstream_rules() {
        let template = OsString::from("netcat %H %P %%");
        let config = ConnectProgramConfig::new(template, None).expect("config");
        let rendered = config
            .format_command("daemon.example", 10873)
            .expect("rendered command");

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert_eq!(rendered.as_bytes(), b"netcat daemon.example 10873 %");
        }

        #[cfg(not(unix))]
        {
            assert_eq!(rendered, OsString::from("netcat daemon.example 10873 %"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn remote_fallback_forwards_port_option() {
        let _lock = env_lock().lock().expect("env mutex poisoned");
        let temp = tempdir().expect("tempdir created");
        let capture_path = temp.path().join("args.txt");
        let script_path = temp.path().join("capture.sh");
        let script_contents = format!(
            "#!/bin/sh\nset -eu\nOUTPUT=\"\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      OUTPUT=\"${{arg#CAPTURE=}}\"\n      ;;\n  esac\ndone\n: \"${{OUTPUT:?}}\"\n: > \"$OUTPUT\"\nfor arg in \"$@\"; do\n  case \"$arg\" in\n    CAPTURE=*)\n      ;;\n    *)\n      printf '%s\\n' \"$arg\" >> \"$OUTPUT\"\n      ;;\n  esac\ndone\n"
        );
        fs::write(&script_path, script_contents).expect("script written");
        let metadata = fs::metadata(&script_path).expect("script metadata");
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script_path, permissions).expect("script permissions set");

        let mut args = baseline_fallback_args();
        args.fallback_binary = Some(script_path.clone().into_os_string());
        args.port = Some(10_873);
        args.remainder = vec![OsString::from(format!(
            "CAPTURE={}",
            capture_path.display()
        ))];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_remote_transfer_fallback(&mut stdout, &mut stderr, args)
            .expect("fallback invocation succeeds");

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());

        let captured = fs::read_to_string(&capture_path).expect("capture contents");
        assert!(captured.lines().any(|line| line == "--port=10873"));
    }

    #[test]
    fn run_module_list_collects_motd_after_acknowledgement() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec![
            "@RSYNCD: OK\n",
            "@RSYNCD: MOTD: Post-acknowledgement notice\n",
            "gamma\n",
            "@RSYNCD: EXIT\n",
        ];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("module list succeeds");
        assert_eq!(
            list.motd_lines(),
            &[String::from("Post-acknowledgement notice")]
        );
        assert!(list.capabilities().is_empty());
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "gamma");
        assert!(list.entries()[0].comment().is_none());

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_suppresses_motd_when_requested() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec![
            "@RSYNCD: MOTD Welcome to the test daemon\n",
            "@RSYNCD: OK\n",
            "alpha\tPrimary module\n",
            "@RSYNCD: EXIT\n",
        ];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list =
            run_module_list_with_options(request, ModuleListOptions::default().suppress_motd(true))
                .expect("module list succeeds");
        assert!(list.motd_lines().is_empty());
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "alpha");
        assert_eq!(list.entries()[0].comment(), Some("Primary module"));

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_collects_warnings() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec![
            "@WARNING: Maintenance scheduled\n",
            "@RSYNCD: OK\n",
            "delta\n",
            "@WARNING: Additional notice\n",
            "@RSYNCD: EXIT\n",
        ];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("module list succeeds");
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "delta");
        assert_eq!(
            list.warnings(),
            &[
                String::from("Maintenance scheduled"),
                String::from("Additional notice")
            ]
        );
        assert!(list.capabilities().is_empty());

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_collects_capabilities() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec![
            "@RSYNCD: CAP modules uid\n",
            "@RSYNCD: OK\n",
            "epsilon\n",
            "@RSYNCD: CAP compression\n",
            "@RSYNCD: EXIT\n",
        ];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("module list succeeds");
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "epsilon");
        assert_eq!(
            list.capabilities(),
            &[String::from("modules uid"), String::from("compression")]
        );

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_via_proxy_connects_through_tunnel() {
        let responses = vec!["@RSYNCD: OK\n", "theta\n", "@RSYNCD: EXIT\n"];
        let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
        let (proxy_addr, request_rx, proxy_handle) =
            spawn_stub_proxy(daemon_addr, None, DEFAULT_PROXY_STATUS_LINE);

        let _env_lock = env_lock().lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set(
            "RSYNC_PROXY",
            &format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
        );

        let request = ModuleListRequest {
            address: DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port())
                .expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("module list succeeds");
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "theta");

        let captured = request_rx.recv().expect("proxy request");
        assert!(
            captured
                .lines()
                .next()
                .is_some_and(|line| line.starts_with("CONNECT "))
        );

        proxy_handle.join().expect("proxy thread");
        daemon_handle.join().expect("daemon thread");
    }

    #[test]
    fn run_module_list_via_proxy_includes_auth_header() {
        let responses = vec!["@RSYNCD: OK\n", "iota\n", "@RSYNCD: EXIT\n"];
        let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
        let expected_header = "Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=";
        let (proxy_addr, request_rx, proxy_handle) = spawn_stub_proxy(
            daemon_addr,
            Some(expected_header),
            DEFAULT_PROXY_STATUS_LINE,
        );

        let _env_lock = env_lock().lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set(
            "RSYNC_PROXY",
            &format!("user:secret@{}:{}", proxy_addr.ip(), proxy_addr.port()),
        );

        let request = ModuleListRequest {
            address: DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port())
                .expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("module list succeeds");
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "iota");

        let captured = request_rx.recv().expect("proxy request");
        assert!(captured.contains(expected_header));

        proxy_handle.join().expect("proxy thread");
        daemon_handle.join().expect("daemon thread");
    }

    #[test]
    fn establish_proxy_tunnel_formats_ipv6_authority_without_brackets() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
        let addr = listener.local_addr().expect("proxy addr");
        let expected_line = "CONNECT fe80::1%eth0:873 HTTP/1.0\r\n";

        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept proxy connection");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read CONNECT request");
            assert_eq!(line, expected_line);

            line.clear();
            reader.read_line(&mut line).expect("read blank line");
            assert!(line == "\r\n" || line == "\n");

            let mut stream = reader.into_inner();
            stream
                .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
                .expect("write proxy response");
            stream.flush().expect("flush proxy response");
        });

        let daemon_addr =
            DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("daemon addr");
        let proxy = ProxyConfig {
            host: String::from("proxy.example"),
            port: addr.port(),
            credentials: None,
        };

        let mut stream = TcpStream::connect(addr).expect("connect to proxy listener");
        establish_proxy_tunnel(&mut stream, &daemon_addr, &proxy)
            .expect("tunnel negotiation succeeds");

        handle.join().expect("proxy thread completes");
    }

    #[test]
    fn run_module_list_accepts_lowercase_proxy_status_line() {
        let responses = vec!["@RSYNCD: OK\n", "kappa\n", "@RSYNCD: EXIT\n"];
        let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
        let (proxy_addr, _request_rx, proxy_handle) =
            spawn_stub_proxy(daemon_addr, None, LOWERCASE_PROXY_STATUS_LINE);

        let _env_lock = env_lock().lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set(
            "RSYNC_PROXY",
            &format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
        );

        let request = ModuleListRequest {
            address: DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port())
                .expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let list = run_module_list(request).expect("module list succeeds");
        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "kappa");

        proxy_handle.join().expect("proxy thread");
        daemon_handle.join().expect("daemon thread");
    }

    #[test]
    fn run_module_list_reports_invalid_proxy_configuration() {
        let _env_lock = env_lock().lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROXY", "invalid-proxy");

        let request = ModuleListRequest {
            address: DaemonAddress::new(String::from("localhost"), 873).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let error = run_module_list(request).expect_err("invalid proxy should fail");
        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("RSYNC_PROXY must be in HOST:PORT form")
        );
    }

    #[test]
    fn parse_proxy_spec_accepts_http_scheme() {
        let proxy =
            parse_proxy_spec("http://user:secret@proxy.example:8080").expect("http proxy parses");
        assert_eq!(proxy.host, "proxy.example");
        assert_eq!(proxy.port, 8080);
        assert_eq!(
            proxy.authorization_header(),
            Some(String::from("dXNlcjpzZWNyZXQ="))
        );
    }

    #[test]
    fn parse_proxy_spec_decodes_percent_encoded_credentials() {
        let proxy = parse_proxy_spec("http://user%3Aname:p%40ss%25word@proxy.example:1080")
            .expect("percent-encoded proxy parses");
        assert_eq!(proxy.host, "proxy.example");
        assert_eq!(proxy.port, 1080);
        assert_eq!(
            proxy.authorization_header(),
            Some(String::from("dXNlcjpuYW1lOnBAc3Mld29yZA=="))
        );
    }

    #[test]
    fn parse_proxy_spec_accepts_https_scheme() {
        let proxy = parse_proxy_spec("https://proxy.example:3128").expect("https proxy parses");
        assert_eq!(proxy.host, "proxy.example");
        assert_eq!(proxy.port, 3128);
        assert!(proxy.authorization_header().is_none());
    }

    #[test]
    fn parse_proxy_spec_rejects_unknown_scheme() {
        let error = match parse_proxy_spec("socks5://proxy:1080") {
            Ok(_) => panic!("invalid proxy scheme should be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("RSYNC_PROXY scheme must be http:// or https://")
        );
    }

    #[test]
    fn parse_proxy_spec_rejects_path_component() {
        let error = match parse_proxy_spec("http://proxy.example:3128/path") {
            Ok(_) => panic!("proxy specification with path should be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("RSYNC_PROXY must not include a path component")
        );
    }

    #[test]
    fn parse_proxy_spec_rejects_invalid_percent_encoding_in_credentials() {
        let error = match parse_proxy_spec("user%zz:secret@proxy.example:8080") {
            Ok(_) => panic!("invalid percent-encoding should be rejected"),
            Err(error) => error,
        };

        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("RSYNC_PROXY username contains invalid percent-encoding")
        );

        let error = match parse_proxy_spec("user:secret%@proxy.example:8080") {
            Ok(_) => panic!("truncated percent-encoding should be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("RSYNC_PROXY password contains truncated percent-encoding")
        );
    }

    #[test]
    fn run_module_list_reports_daemon_error() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec!["@ERROR: unavailable\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let error = run_module_list(request).expect_err("daemon error should surface");
        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(error.message().to_string().contains("unavailable"));

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_reports_daemon_error_without_colon() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec!["@ERROR unavailable\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let error = run_module_list(request).expect_err("daemon error should surface");
        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(error.message().to_string().contains("unavailable"));

        handle.join().expect("server thread");
    }

    #[test]
    fn map_daemon_handshake_error_converts_error_payload() {
        let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
        let error = io::Error::new(
            io::ErrorKind::InvalidData,
            NegotiationError::MalformedLegacyGreeting {
                input: "@ERROR module unavailable".to_string(),
            },
        );

        let mapped = map_daemon_handshake_error(error, &addr);
        assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(mapped.message().to_string().contains("module unavailable"));
    }

    #[test]
    fn map_daemon_handshake_error_converts_plain_invalid_data_error() {
        let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
        let error = io::Error::new(io::ErrorKind::InvalidData, "@ERROR daemon unavailable");

        let mapped = map_daemon_handshake_error(error, &addr);
        assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(mapped.message().to_string().contains("daemon unavailable"));
    }

    #[test]
    fn map_daemon_handshake_error_converts_other_malformed_greetings() {
        let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
        let error = io::Error::new(
            io::ErrorKind::InvalidData,
            NegotiationError::MalformedLegacyGreeting {
                input: "@RSYNCD? unexpected".to_string(),
            },
        );

        let mapped = map_daemon_handshake_error(error, &addr);
        assert_eq!(mapped.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
        assert!(mapped.message().to_string().contains("@RSYNCD? unexpected"));
    }

    #[test]
    fn map_daemon_handshake_error_propagates_other_failures() {
        let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
        let error = io::Error::new(io::ErrorKind::TimedOut, "timed out");

        let mapped = map_daemon_handshake_error(error, &addr);
        assert_eq!(mapped.exit_code(), SOCKET_IO_EXIT_CODE);
        let rendered = mapped.message().to_string();
        assert!(rendered.contains("timed out"));
        assert!(rendered.contains("negotiate with"));
    }

    #[test]
    fn run_module_list_reports_daemon_error_with_case_insensitive_prefix() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec!["@error:\tunavailable\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let error = run_module_list(request).expect_err("daemon error should surface");
        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(error.message().to_string().contains("unavailable"));

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_reports_authentication_required() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec!["@RSYNCD: AUTHREQD modules\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let error = run_module_list(request).expect_err("auth requirement should surface");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(rendered.contains("requires authentication"));
        assert!(rendered.contains("username"));

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_requires_password_for_authentication() {
        let responses = vec!["@RSYNCD: AUTHREQD challenge\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: Some(String::from("user")),
            protocol: ProtocolVersion::NEWEST,
        };

        let _guard = env_lock().lock().unwrap();
        super::set_test_daemon_password(None);

        let error = run_module_list(request).expect_err("missing password should fail");
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(error.message().to_string().contains("RSYNC_PASSWORD"));

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_authenticates_with_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
        let addr = listener.local_addr().expect("local addr");
        let challenge = "abc123";
        let expected = compute_daemon_auth_response(b"secret", challenge);

        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .expect("write timeout");

                stream
                    .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                    .expect("write greeting");
                stream.flush().expect("flush greeting");

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read client greeting");
                assert_eq!(line, LEGACY_DAEMON_GREETING);

                line.clear();
                reader.read_line(&mut line).expect("read request");
                assert_eq!(line, "#list\n");

                reader
                    .get_mut()
                    .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                    .expect("write challenge");
                reader.get_mut().flush().expect("flush challenge");

                line.clear();
                reader.read_line(&mut line).expect("read credentials");
                let received = line.trim_end_matches(['\n', '\r']);
                assert_eq!(received, format!("user {expected}"));

                for response in ["@RSYNCD: OK\n", "secured\n", "@RSYNCD: EXIT\n"] {
                    reader
                        .get_mut()
                        .write_all(response.as_bytes())
                        .expect("write response");
                }
                reader.get_mut().flush().expect("flush response");
            }
        });

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: Some(String::from("user")),
            protocol: ProtocolVersion::NEWEST,
        };

        let _guard = env_lock().lock().unwrap();
        super::set_test_daemon_password(Some(b"secret".to_vec()));
        let list = run_module_list(request).expect("module list succeeds");
        super::set_test_daemon_password(None);

        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "secured");

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_authenticates_with_password_override() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind override daemon");
        let addr = listener.local_addr().expect("local addr");
        let challenge = "override";
        let expected = compute_daemon_auth_response(b"override-secret", challenge);

        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .expect("write timeout");

                stream
                    .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                    .expect("write greeting");
                stream.flush().expect("flush greeting");

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read client greeting");
                assert_eq!(line, LEGACY_DAEMON_GREETING);

                line.clear();
                reader.read_line(&mut line).expect("read request");
                assert_eq!(line, "#list\n");

                reader
                    .get_mut()
                    .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                    .expect("write challenge");
                reader.get_mut().flush().expect("flush challenge");

                line.clear();
                reader.read_line(&mut line).expect("read credentials");
                let received = line.trim_end_matches(['\n', '\r']);
                assert_eq!(received, format!("user {expected}"));

                for response in ["@RSYNCD: OK\n", "override\n", "@RSYNCD: EXIT\n"] {
                    reader
                        .get_mut()
                        .write_all(response.as_bytes())
                        .expect("write response");
                }
                reader.get_mut().flush().expect("flush response");
            }
        });

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: Some(String::from("user")),
            protocol: ProtocolVersion::NEWEST,
        };

        let _guard = env_lock().lock().unwrap();
        super::set_test_daemon_password(Some(b"wrong".to_vec()));
        let list = run_module_list_with_password(
            request,
            Some(b"override-secret".to_vec()),
            TransferTimeout::Default,
        )
        .expect("module list succeeds");
        super::set_test_daemon_password(None);

        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "override");

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_authenticates_with_split_challenge() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind split auth daemon");
        let addr = listener.local_addr().expect("local addr");
        let challenge = "split123";
        let expected = compute_daemon_auth_response(b"secret", challenge);

        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .expect("write timeout");

                stream
                    .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                    .expect("write greeting");
                stream.flush().expect("flush greeting");

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read client greeting");
                assert_eq!(line, LEGACY_DAEMON_GREETING);

                line.clear();
                reader.read_line(&mut line).expect("read request");
                assert_eq!(line, "#list\n");

                reader
                    .get_mut()
                    .write_all(b"@RSYNCD: AUTHREQD\n")
                    .expect("write authreqd");
                reader.get_mut().flush().expect("flush authreqd");

                reader
                    .get_mut()
                    .write_all(format!("@RSYNCD: AUTH {challenge}\n").as_bytes())
                    .expect("write challenge");
                reader.get_mut().flush().expect("flush challenge");

                line.clear();
                reader.read_line(&mut line).expect("read credentials");
                let received = line.trim_end_matches(['\n', '\r']);
                assert_eq!(received, format!("user {expected}"));

                for response in ["@RSYNCD: OK\n", "protected\n", "@RSYNCD: EXIT\n"] {
                    reader
                        .get_mut()
                        .write_all(response.as_bytes())
                        .expect("write response");
                }
                reader.get_mut().flush().expect("flush response");
            }
        });

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: Some(String::from("user")),
            protocol: ProtocolVersion::NEWEST,
        };

        let _guard = env_lock().lock().unwrap();
        super::set_test_daemon_password(Some(b"secret".to_vec()));
        let list = run_module_list(request).expect("module list succeeds");
        super::set_test_daemon_password(None);

        assert_eq!(list.entries().len(), 1);
        assert_eq!(list.entries()[0].name(), "protected");

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_reports_authentication_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
        let addr = listener.local_addr().expect("local addr");
        let challenge = "abcdef";
        let expected = compute_daemon_auth_response(b"secret", challenge);

        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .expect("read timeout");
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .expect("write timeout");

                stream
                    .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                    .expect("write greeting");
                stream.flush().expect("flush greeting");

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read client greeting");
                assert_eq!(line, LEGACY_DAEMON_GREETING);

                line.clear();
                reader.read_line(&mut line).expect("read request");
                assert_eq!(line, "#list\n");

                reader
                    .get_mut()
                    .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                    .expect("write challenge");
                reader.get_mut().flush().expect("flush challenge");

                line.clear();
                reader.read_line(&mut line).expect("read credentials");
                let received = line.trim_end_matches(['\n', '\r']);
                assert_eq!(received, format!("user {expected}"));

                reader
                    .get_mut()
                    .write_all(b"@RSYNCD: AUTHFAILED credentials rejected\n")
                    .expect("write failure");
                reader
                    .get_mut()
                    .write_all(b"@RSYNCD: EXIT\n")
                    .expect("write exit");
                reader.get_mut().flush().expect("flush failure");
            }
        });

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: Some(String::from("user")),
            protocol: ProtocolVersion::NEWEST,
        };

        let _guard = env_lock().lock().unwrap();
        super::set_test_daemon_password(Some(b"secret".to_vec()));
        let error = run_module_list(request).expect_err("auth failure surfaces");
        super::set_test_daemon_password(None);

        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("rejected provided credentials")
        );

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_reports_access_denied() {
        let _guard = env_lock().lock().expect("env mutex poisoned");

        let responses = vec!["@RSYNCD: DENIED host rules\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
            protocol: ProtocolVersion::NEWEST,
        };

        let error = run_module_list(request).expect_err("denied response should surface");
        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(rendered.contains("denied access"));
        assert!(rendered.contains("host rules"));

        handle.join().expect("server thread");
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn set_os(key: &'static str, value: &OsStr) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.take() {
                #[allow(unsafe_code)]
                unsafe {
                    env::set_var(self.key, value);
                }
            } else {
                #[allow(unsafe_code)]
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }

    const DEFAULT_PROXY_STATUS_LINE: &str = "HTTP/1.0 200 Connection established";
    const LOWERCASE_PROXY_STATUS_LINE: &str = "http/1.1 200 Connection Established";

    fn spawn_stub_proxy(
        target: std::net::SocketAddr,
        expected_header: Option<&'static str>,
        status_line: &'static str,
    ) -> (
        std::net::SocketAddr,
        mpsc::Receiver<String>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        let addr = listener.local_addr().expect("proxy addr");
        let (tx, rx) = mpsc::channel();

        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream);
                let mut captured = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).expect("read request line") == 0 {
                        break;
                    }
                    captured.push_str(&line);
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                }

                if let Some(expected) = expected_header {
                    assert!(captured.contains(expected), "missing proxy header");
                }

                tx.send(captured).expect("send captured request");

                let mut client_stream = reader.into_inner();
                let mut server_stream = TcpStream::connect(target).expect("connect daemon");
                client_stream
                    .write_all(status_line.as_bytes())
                    .expect("write proxy response");
                client_stream
                    .write_all(b"\r\n\r\n")
                    .expect("terminate proxy status");

                let mut client_clone = client_stream.try_clone().expect("clone client");
                let mut server_clone = server_stream.try_clone().expect("clone server");

                let forward = thread::spawn(move || {
                    let _ = io::copy(&mut client_clone, &mut server_stream);
                });
                let backward = thread::spawn(move || {
                    let _ = io::copy(&mut server_clone, &mut client_stream);
                });

                let _ = forward.join();
                let _ = backward.join();
            }
        });

        (addr, rx, handle)
    }

    fn spawn_stub_daemon(
        responses: Vec<&'static str>,
    ) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
        let addr = listener.local_addr().expect("local addr");

        let handle = thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                handle_connection(stream, responses);
            }
        });

        (addr, handle)
    }

    fn handle_connection(mut stream: TcpStream, responses: Vec<&'static str>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .expect("set write timeout");

        stream
            .write_all(LEGACY_DAEMON_GREETING.as_bytes())
            .expect("write greeting");
        stream.flush().expect("flush greeting");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read client greeting");
        assert_eq!(line, LEGACY_DAEMON_GREETING);

        line.clear();
        reader.read_line(&mut line).expect("read request");
        assert_eq!(line, "#list\n");

        for response in responses {
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .expect("write response");
        }
        reader.get_mut().flush().expect("flush response");

        let stream = reader.into_inner();
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn missing_operands_error() -> ClientError {
    let message = rsync_error!(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        "missing source operands: supply at least one source and a destination"
    )
    .with_role(Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn invalid_argument_error(text: &str, exit_code: i32) -> ClientError {
    let message = rsync_error!(exit_code, "{}", text).with_role(Role::Client);
    ClientError::new(exit_code, message)
}

fn map_local_copy_error(error: LocalCopyError) -> ClientError {
    let exit_code = error.exit_code();
    match error.into_kind() {
        LocalCopyErrorKind::MissingSourceOperands => missing_operands_error(),
        LocalCopyErrorKind::InvalidArgument(reason) => {
            invalid_argument_error(reason.message(), exit_code)
        }
        LocalCopyErrorKind::Io {
            action,
            path,
            source,
        } => io_error(action, &path, source),
        LocalCopyErrorKind::Timeout { duration } => {
            let text = format!(
                "transfer timed out after {:.3} seconds without progress",
                duration.as_secs_f64()
            );
            let message = rsync_error!(exit_code, text).with_role(Role::Client);
            ClientError::new(exit_code, message)
        }
        LocalCopyErrorKind::DeleteLimitExceeded { skipped } => {
            let noun = if skipped == 1 { "entry" } else { "entries" };
            let text = format!(
                "Deletions stopped due to --max-delete limit ({} {noun} skipped)",
                skipped
            );
            let message = rsync_error!(MAX_DELETE_EXIT_CODE, text).with_role(Role::Client);
            ClientError::new(MAX_DELETE_EXIT_CODE, message)
        }
    }
}

fn compile_filter_program(rules: &[FilterRuleSpec]) -> Result<Option<FilterProgram>, ClientError> {
    if rules.is_empty() {
        return Ok(None);
    }

    let mut entries = Vec::new();
    for rule in rules {
        match rule.kind() {
            FilterRuleKind::Include => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::include(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
            )),
            FilterRuleKind::Exclude => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::exclude(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
            )),
            FilterRuleKind::Clear => entries.push(FilterProgramEntry::Clear),
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::protect(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
            )),
            FilterRuleKind::Risk => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::risk(rule.pattern().to_string())
                    .with_sides(rule.applies_to_sender(), rule.applies_to_receiver()),
            )),
            FilterRuleKind::DirMerge => {
                entries.push(FilterProgramEntry::DirMerge(DirMergeRule::new(
                    rule.pattern().to_string(),
                    rule.dir_merge_options().cloned().unwrap_or_default(),
                )))
            }
            FilterRuleKind::ExcludeIfPresent => entries.push(FilterProgramEntry::ExcludeIfPresent(
                ExcludeIfPresentRule::new(rule.pattern().to_string()),
            )),
        }
    }

    FilterProgram::new(entries).map(Some).map_err(|error| {
        let text = format!(
            "failed to compile filter pattern '{}': {}",
            error.pattern(),
            error
        );
        let message = rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, text).with_role(Role::Client);
        ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
    })
}

fn io_error(action: &str, path: &Path, error: io::Error) -> ClientError {
    let text = format!(
        "failed to {action} '{path}': {error}",
        action = action,
        path = path.display(),
        error = error
    );
    let message = rsync_error!(PARTIAL_TRANSFER_EXIT_CODE, text).with_role(Role::Client);
    ClientError::new(PARTIAL_TRANSFER_EXIT_CODE, message)
}

fn socket_error(action: &str, target: impl fmt::Display, error: io::Error) -> ClientError {
    let text = format!("failed to {action} {target}: {error}");
    let message = rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

fn daemon_error(text: impl Into<String>, exit_code: i32) -> ClientError {
    let message = rsync_error!(exit_code, "{}", text.into()).with_role(Role::Client);
    ClientError::new(exit_code, message)
}

fn daemon_protocol_error(text: &str) -> ClientError {
    daemon_error(
        format!("unexpected response from daemon: {text}"),
        PROTOCOL_INCOMPATIBLE_EXIT_CODE,
    )
}

fn daemon_authentication_required_error(reason: &str) -> ClientError {
    let detail = if reason.is_empty() {
        "daemon requires authentication for module listing".to_string()
    } else {
        format!("daemon requires authentication for module listing: {reason}")
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}

fn daemon_authentication_failed_error(reason: Option<&str>) -> ClientError {
    let detail = match reason {
        Some(text) if !text.is_empty() => {
            format!("daemon rejected provided credentials: {text}")
        }
        _ => "daemon rejected provided credentials".to_string(),
    };

    daemon_error(detail, FEATURE_UNAVAILABLE_EXIT_CODE)
}

fn daemon_access_denied_error(reason: &str) -> ClientError {
    let detail = if reason.is_empty() {
        "daemon denied access to module listing".to_string()
    } else {
        format!("daemon denied access to module listing: {reason}")
    };

    daemon_error(detail, PARTIAL_TRANSFER_EXIT_CODE)
}

fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

fn legacy_daemon_error_payload(line: &str) -> Option<String> {
    if let Some(payload) = parse_legacy_error_message(line) {
        return Some(payload.to_string());
    }

    let trimmed = line.trim_matches(['\r', '\n']).trim_start();
    let remainder = strip_prefix_ignore_ascii_case(trimmed, "@ERROR")?;

    if let Some(ch) = remainder.chars().next()
        && ch != ':'
        && !ch.is_ascii_whitespace()
    {
        return None;
    }

    let payload = remainder
        .trim_start_matches(|ch: char| ch == ':' || ch.is_ascii_whitespace())
        .trim();

    Some(payload.to_string())
}

fn map_daemon_handshake_error(error: io::Error, addr: &DaemonAddress) -> ClientError {
    if let Some(mapped) = handshake_error_to_client_error(&error) {
        mapped
    } else {
        match daemon_error_from_invalid_data(&error) {
            Some(mapped) => mapped,
            None => socket_error("negotiate with", addr.socket_addr_display(), error),
        }
    }
}

fn handshake_error_to_client_error(error: &io::Error) -> Option<ClientError> {
    let negotiation_error = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<NegotiationError>())?;

    if let Some(input) = negotiation_error.malformed_legacy_greeting() {
        if let Some(payload) = legacy_daemon_error_payload(input) {
            return Some(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }

        return Some(daemon_protocol_error(input));
    }

    None
}

fn daemon_error_from_invalid_data(error: &io::Error) -> Option<ClientError> {
    if error.kind() != io::ErrorKind::InvalidData {
        return None;
    }

    let payload_candidates = error
        .get_ref()
        .map(|inner| inner.to_string())
        .into_iter()
        .chain(std::iter::once(error.to_string()));

    for candidate in payload_candidates {
        if let Some(payload) = legacy_daemon_error_payload(&candidate) {
            return Some(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }
    }

    None
}

/// Target daemon address used for module listing requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonAddress {
    host: String,
    port: u16,
}

impl DaemonAddress {
    /// Creates a new daemon address from the supplied host and port.
    pub fn new(host: String, port: u16) -> Result<Self, ClientError> {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(daemon_error(
                "daemon host must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
        Ok(Self {
            host: trimmed.to_string(),
            port,
        })
    }

    /// Returns the daemon host name or address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the daemon TCP port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    fn socket_addr_display(&self) -> SocketAddrDisplay<'_> {
        SocketAddrDisplay {
            host: &self.host,
            port: self.port,
        }
    }
}

struct SocketAddrDisplay<'a> {
    host: &'a str,
    port: u16,
}

impl fmt::Display for SocketAddrDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') && !self.host.starts_with('[') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// Specification describing a daemon module listing request parsed from CLI operands.
///
/// The request retains the optional username embedded in the operand so future
/// authentication flows can reuse the caller-supplied identity even though the
/// current module listing implementation performs anonymous queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListRequest {
    address: DaemonAddress,
    username: Option<String>,
    protocol: ProtocolVersion,
}

impl ModuleListRequest {
    /// Default TCP port used by rsync daemons when a port is not specified.
    pub const DEFAULT_PORT: u16 = 873;

    /// Attempts to derive a module listing request from CLI-style operands.
    pub fn from_operands(operands: &[OsString]) -> Result<Option<Self>, ClientError> {
        Self::from_operands_with_port(operands, Self::DEFAULT_PORT)
    }

    /// Equivalent to [`from_operands`] but allows overriding the default daemon port.
    pub fn from_operands_with_port(
        operands: &[OsString],
        default_port: u16,
    ) -> Result<Option<Self>, ClientError> {
        if operands.len() != 1 {
            return Ok(None);
        }

        Self::from_operand(&operands[0], default_port)
    }

    fn from_operand(operand: &OsString, default_port: u16) -> Result<Option<Self>, ClientError> {
        let text = operand.to_string_lossy();

        if let Some(rest) = strip_prefix_ignore_ascii_case(&text, "rsync://") {
            return parse_rsync_url(rest, default_port);
        }

        if let Some((host_part, module_part)) = split_daemon_host_module(&text)? {
            if module_part.is_empty() {
                let target = parse_host_port(host_part, default_port)?;
                return Ok(Some(Self::new(target.address, target.username)));
            }
            return Ok(None);
        }

        Ok(None)
    }

    fn new(address: DaemonAddress, username: Option<String>) -> Self {
        Self {
            address,
            username,
            protocol: ProtocolVersion::NEWEST,
        }
    }

    /// Returns the parsed daemon address.
    #[must_use]
    pub fn address(&self) -> &DaemonAddress {
        &self.address
    }

    /// Returns the optional username supplied in the daemon URL or legacy syntax.
    #[must_use]
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// Returns the desired protocol version for daemon negotiation.
    #[must_use]
    pub const fn protocol(&self) -> ProtocolVersion {
        self.protocol
    }

    /// Returns a new request that clamps the negotiation to the provided protocol.
    #[must_use]
    pub const fn with_protocol(mut self, protocol: ProtocolVersion) -> Self {
        self.protocol = protocol;
        self
    }
}

/// Configuration toggles that influence daemon module listings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListOptions {
    suppress_motd: bool,
    address_mode: AddressMode,
    connect_program: Option<OsString>,
    bind_address: Option<SocketAddr>,
}

impl ModuleListOptions {
    /// Creates a new options structure with all toggles disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            suppress_motd: false,
            address_mode: AddressMode::Default,
            connect_program: None,
            bind_address: None,
        }
    }

    /// Returns a new configuration that suppresses daemon MOTD lines.
    #[must_use]
    pub const fn suppress_motd(mut self, suppress: bool) -> Self {
        self.suppress_motd = suppress;
        self
    }

    /// Returns whether MOTD lines should be suppressed.
    #[must_use]
    pub const fn suppresses_motd(&self) -> bool {
        self.suppress_motd
    }

    /// Configures the preferred address family for the daemon connection.
    #[must_use]
    #[doc(alias = "--ipv4")]
    #[doc(alias = "--ipv6")]
    pub const fn with_address_mode(mut self, mode: AddressMode) -> Self {
        self.address_mode = mode;
        self
    }

    /// Returns the preferred address family.
    #[must_use]
    pub const fn address_mode(&self) -> AddressMode {
        self.address_mode
    }

    /// Supplies an explicit connect program command.
    #[must_use]
    #[doc(alias = "--connect-program")]
    pub fn with_connect_program(mut self, program: Option<OsString>) -> Self {
        self.connect_program = program;
        self
    }

    /// Returns the configured connect program command, if any.
    #[must_use]
    pub fn connect_program(&self) -> Option<&OsStr> {
        self.connect_program.as_deref()
    }

    /// Configures the bind address used when contacting the daemon directly or via a proxy.
    #[must_use]
    pub const fn with_bind_address(mut self, address: Option<SocketAddr>) -> Self {
        self.bind_address = address;
        self
    }

    /// Returns the configured bind address, if any.
    #[must_use]
    pub const fn bind_address(&self) -> Option<SocketAddr> {
        self.bind_address
    }
}

impl Default for ModuleListOptions {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_rsync_url(
    rest: &str,
    default_port: u16,
) -> Result<Option<ModuleListRequest>, ClientError> {
    let mut parts = rest.splitn(2, '/');
    let host_port = parts.next().unwrap_or("");
    let remainder = parts.next();

    if remainder.is_some_and(|path| !path.is_empty()) {
        return Ok(None);
    }

    let target = parse_host_port(host_port, default_port)?;
    Ok(Some(ModuleListRequest::new(
        target.address,
        target.username,
    )))
}

struct ParsedDaemonTarget {
    address: DaemonAddress,
    username: Option<String>,
}

fn parse_host_port(input: &str, default_port: u16) -> Result<ParsedDaemonTarget, ClientError> {
    const DEFAULT_HOST: &str = "localhost";

    let (username, input) = split_daemon_username(input)?;
    let username = username.map(decode_daemon_username).transpose()?;

    if input.is_empty() {
        let address = DaemonAddress::new(DEFAULT_HOST.to_string(), default_port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some(host) = input.strip_prefix('[') {
        let (address, port) = parse_bracketed_host(host, default_port)?;
        let address = DaemonAddress::new(address, port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    if let Some((host, port)) = split_host_port(input) {
        let port = port
            .parse::<u16>()
            .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;
        let host = decode_host_component(host)?;
        let address = DaemonAddress::new(host, port)?;
        return Ok(ParsedDaemonTarget { address, username });
    }

    let host = decode_host_component(input)?;
    let address = DaemonAddress::new(host, default_port)?;
    Ok(ParsedDaemonTarget { address, username })
}

fn split_daemon_host_module(input: &str) -> Result<Option<(&str, &str)>, ClientError> {
    if !input.contains('[') {
        let segments = input.split("::");
        if segments.clone().count() > 2 {
            return Err(daemon_error(
                "IPv6 daemon addresses must be enclosed in brackets",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
    }

    let mut in_brackets = false;
    let mut previous_colon = None;

    for (idx, ch) in input.char_indices() {
        match ch {
            '[' => {
                in_brackets = true;
                previous_colon = None;
            }
            ']' => {
                in_brackets = false;
                previous_colon = None;
            }
            ':' if !in_brackets => {
                if let Some(prev) = previous_colon
                    && prev + 1 == idx
                {
                    let host = &input[..prev];
                    if !host.contains('[') {
                        let colon_count = host.chars().filter(|&ch| ch == ':').count();
                        if colon_count > 1 {
                            return Err(daemon_error(
                                "IPv6 daemon addresses must be enclosed in brackets",
                                FEATURE_UNAVAILABLE_EXIT_CODE,
                            ));
                        }
                    }
                    let module = &input[idx + 1..];
                    return Ok(Some((host, module)));
                }
                previous_colon = Some(idx);
            }
            _ => {
                previous_colon = None;
            }
        }
    }

    Ok(None)
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() < prefix.len() {
        return None;
    }

    let (candidate, remainder) = text.split_at(prefix.len());
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(remainder)
    } else {
        None
    }
}

fn parse_bracketed_host(host: &str, default_port: u16) -> Result<(String, u16), ClientError> {
    let (addr, remainder) = host.split_once(']').ok_or_else(|| {
        daemon_error(
            "invalid bracketed daemon host",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let decoded = decode_host_component(addr)?;

    if remainder.is_empty() {
        return Ok((decoded, default_port));
    }

    let port = remainder
        .strip_prefix(':')
        .ok_or_else(|| {
            daemon_error(
                "invalid bracketed daemon host",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            )
        })?
        .parse::<u16>()
        .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;

    Ok((decoded, port))
}

fn decode_host_component(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_percent_encoding_error,
        invalid_host_utf8_error,
    )
}

fn decode_daemon_username(input: &str) -> Result<String, ClientError> {
    decode_percent_component(
        input,
        invalid_username_percent_encoding_error,
        invalid_username_utf8_error,
    )
}

fn decode_percent_component(
    input: &str,
    truncated_error: fn() -> ClientError,
    invalid_utf8_error: fn() -> ClientError,
) -> Result<String, ClientError> {
    if !input.contains('%') {
        return Ok(input.to_string());
    }

    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(truncated_error());
            }

            let hi = hex_value(bytes[index + 1]);
            let lo = hex_value(bytes[index + 2]);

            if let (Some(hi), Some(lo)) = (hi, lo) {
                decoded.push((hi << 4) | lo);
                index += 3;
                continue;
            }
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).map_err(|_| invalid_utf8_error())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(10 + byte - b'a'),
        b'A'..=b'F' => Some(10 + byte - b'A'),
        _ => None,
    }
}

fn invalid_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon host",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_host_utf8_error() -> ClientError {
    daemon_error(
        "daemon host contains invalid UTF-8 after percent-decoding",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_username_percent_encoding_error() -> ClientError {
    daemon_error(
        "invalid percent-encoding in daemon username",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn invalid_username_utf8_error() -> ClientError {
    daemon_error(
        "daemon username contains invalid UTF-8 after percent-decoding",
        FEATURE_UNAVAILABLE_EXIT_CODE,
    )
}

fn split_host_port(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind(':')?;
    let (host, port) = input.split_at(idx);
    if host.contains(':') {
        return None;
    }
    Some((host, &port[1..]))
}

fn split_daemon_username(input: &str) -> Result<(Option<&str>, &str), ClientError> {
    if let Some(idx) = input.rfind('@') {
        let (user, host) = input.split_at(idx);
        if user.is_empty() {
            return Err(daemon_error(
                "daemon username must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }

        return Ok((Some(user), &host[1..]));
    }

    Ok((None, input))
}

/// Describes the module entries advertised by a daemon together with ancillary metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModuleList {
    motd: Vec<String>,
    warnings: Vec<String>,
    capabilities: Vec<String>,
    entries: Vec<ModuleListEntry>,
}

impl ModuleList {
    /// Creates a new list from the supplied entries, warning lines, MOTD lines, and capability advertisements.
    fn new(
        motd: Vec<String>,
        warnings: Vec<String>,
        capabilities: Vec<String>,
        entries: Vec<ModuleListEntry>,
    ) -> Self {
        Self {
            motd,
            warnings,
            capabilities,
            entries,
        }
    }

    /// Returns the advertised module entries.
    #[must_use]
    pub fn entries(&self) -> &[ModuleListEntry] {
        &self.entries
    }

    /// Returns the optional message-of-the-day lines emitted by the daemon.
    #[must_use]
    pub fn motd_lines(&self) -> &[String] {
        &self.motd
    }

    /// Returns the warning messages emitted by the daemon while processing the request.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Returns the capability strings advertised by the daemon via `@RSYNCD: CAP` lines.
    #[must_use]
    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }
}

/// Entry describing a single daemon module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleListEntry {
    name: String,
    comment: Option<String>,
}

impl ModuleListEntry {
    fn from_line(line: &str) -> Self {
        match line.split_once('\t') {
            Some((name, comment)) => Self {
                name: name.to_string(),
                comment: if comment.is_empty() {
                    None
                } else {
                    Some(comment.to_string())
                },
            },
            None => Self {
                name: line.to_string(),
                comment: None,
            },
        }
    }

    /// Returns the module name advertised by the daemon.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the optional comment associated with the module.
    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }
}

/// Performs a daemon module listing by connecting to the supplied address.
///
/// The helper honours the `RSYNC_PROXY` environment variable, establishing an
/// HTTP `CONNECT` tunnel through the specified proxy before negotiating with
/// the daemon when the variable is set. This mirrors the behaviour of
/// upstream rsync.
pub fn run_module_list(request: ModuleListRequest) -> Result<ModuleList, ClientError> {
    run_module_list_with_options(request, ModuleListOptions::default())
}

fn open_daemon_stream(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    connect_program: Option<&OsStr>,
    bind_address: Option<SocketAddr>,
) -> Result<DaemonStream, ClientError> {
    if let Some(program) = load_daemon_connect_program(connect_program)? {
        return connect_via_program(addr, &program);
    }

    let stream = match load_daemon_proxy()? {
        Some(proxy) => connect_via_proxy(addr, &proxy, connect_timeout, io_timeout, bind_address)?,
        None => connect_direct(
            addr,
            connect_timeout,
            io_timeout,
            address_mode,
            bind_address,
        )?,
    };

    Ok(DaemonStream::tcp(stream))
}

fn connect_direct(
    addr: &DaemonAddress,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    address_mode: AddressMode,
    bind_address: Option<SocketAddr>,
) -> Result<TcpStream, ClientError> {
    let addresses = resolve_daemon_addresses(addr, address_mode)?;
    let mut last_error: Option<(SocketAddr, io::Error)> = None;

    for candidate in addresses {
        match connect_with_optional_bind(candidate, bind_address, connect_timeout) {
            Ok(stream) => {
                if let Some(duration) = io_timeout {
                    stream.set_read_timeout(Some(duration)).map_err(|error| {
                        socket_error("set read timeout on", addr.socket_addr_display(), error)
                    })?;
                    stream.set_write_timeout(Some(duration)).map_err(|error| {
                        socket_error("set write timeout on", addr.socket_addr_display(), error)
                    })?;
                }

                return Ok(stream);
            }
            Err(error) => last_error = Some((candidate, error)),
        }
    }

    let (candidate, error) = last_error.expect("no addresses available for daemon connection");
    Err(socket_error("connect to", candidate, error))
}

fn resolve_daemon_addresses(
    addr: &DaemonAddress,
    mode: AddressMode,
) -> Result<Vec<SocketAddr>, ClientError> {
    let iterator = (addr.host.as_str(), addr.port)
        .to_socket_addrs()
        .map_err(|error| {
            socket_error(
                "resolve daemon address for",
                addr.socket_addr_display(),
                error,
            )
        })?;

    let addresses: Vec<SocketAddr> = iterator.collect();

    if addresses.is_empty() {
        return Err(daemon_error(
            format!(
                "daemon host '{}' did not resolve to any addresses",
                addr.host()
            ),
            SOCKET_IO_EXIT_CODE,
        ));
    }

    let filtered = match mode {
        AddressMode::Default => addresses,
        AddressMode::Ipv4 => {
            let retain: Vec<SocketAddr> = addresses
                .into_iter()
                .filter(|candidate| candidate.is_ipv4())
                .collect();
            if retain.is_empty() {
                return Err(daemon_error(
                    format!("daemon host '{}' does not have IPv4 addresses", addr.host()),
                    SOCKET_IO_EXIT_CODE,
                ));
            }
            retain
        }
        AddressMode::Ipv6 => {
            let retain: Vec<SocketAddr> = addresses
                .into_iter()
                .filter(|candidate| candidate.is_ipv6())
                .collect();
            if retain.is_empty() {
                return Err(daemon_error(
                    format!("daemon host '{}' does not have IPv6 addresses", addr.host()),
                    SOCKET_IO_EXIT_CODE,
                ));
            }
            retain
        }
    };

    Ok(filtered)
}

fn connect_with_optional_bind(
    target: SocketAddr,
    bind_address: Option<SocketAddr>,
    timeout: Option<Duration>,
) -> io::Result<TcpStream> {
    if let Some(bind) = bind_address {
        if target.is_ipv4() != bind.is_ipv4() {
            return Err(io::Error::new(
                ErrorKind::AddrNotAvailable,
                "bind address family does not match target",
            ));
        }

        let domain = if target.is_ipv4() {
            Domain::IPV4
        } else {
            Domain::IPV6
        };

        let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
        let mut bind_addr = bind;
        match &mut bind_addr {
            SocketAddr::V4(addr) => addr.set_port(0),
            SocketAddr::V6(addr) => addr.set_port(0),
        }
        socket.bind(&SockAddr::from(bind_addr))?;

        let target_addr = SockAddr::from(target);
        if let Some(duration) = timeout {
            socket.connect_timeout(&target_addr, duration)?;
        } else {
            socket.connect(&target_addr)?;
        }

        Ok(socket.into())
    } else if let Some(duration) = timeout {
        TcpStream::connect_timeout(&target, duration)
    } else {
        TcpStream::connect(target)
    }
}

fn connect_via_proxy(
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
    connect_timeout: Option<Duration>,
    io_timeout: Option<Duration>,
    bind_address: Option<SocketAddr>,
) -> Result<TcpStream, ClientError> {
    let target = (proxy.host.as_str(), proxy.port);
    let addrs = target
        .to_socket_addrs()
        .map_err(|error| socket_error("resolve proxy address for", proxy.display(), error))?;

    let mut last_error: Option<(SocketAddr, io::Error)> = None;
    let mut stream_result: Option<TcpStream> = None;

    for candidate in addrs {
        match connect_with_optional_bind(candidate, bind_address, connect_timeout) {
            Ok(stream) => {
                stream_result = Some(stream);
                break;
            }
            Err(error) => last_error = Some((candidate, error)),
        }
    }

    let mut stream = if let Some(stream) = stream_result {
        stream
    } else if let Some((candidate, error)) = last_error {
        return Err(socket_error("connect to", candidate, error));
    } else {
        return Err(socket_error(
            "resolve proxy address for",
            proxy.display(),
            io::Error::new(
                ErrorKind::AddrNotAvailable,
                "proxy resolution returned no addresses",
            ),
        ));
    };

    establish_proxy_tunnel(&mut stream, addr, proxy)?;

    if let Some(duration) = io_timeout {
        stream
            .set_read_timeout(Some(duration))
            .map_err(|error| socket_error("configure", proxy.display(), error))?;
        stream
            .set_write_timeout(Some(duration))
            .map_err(|error| socket_error("configure", proxy.display(), error))?;
    }

    Ok(stream)
}

fn establish_proxy_tunnel(
    stream: &mut TcpStream,
    addr: &DaemonAddress,
    proxy: &ProxyConfig,
) -> Result<(), ClientError> {
    let mut request = format!("CONNECT {}:{} HTTP/1.0\r\n", addr.host(), addr.port());

    if let Some(header) = proxy.authorization_header() {
        request.push_str("Proxy-Authorization: Basic ");
        request.push_str(&header);
        request.push_str("\r\n");
    }

    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .map_err(|error| socket_error("write to", proxy.display(), error))?;
    stream
        .flush()
        .map_err(|error| socket_error("flush", proxy.display(), error))?;

    let mut line = Vec::with_capacity(128);
    read_proxy_line(stream, &mut line, proxy.display())?;
    let status = String::from_utf8(line.clone())
        .map_err(|_| proxy_response_error("proxy status line contained invalid UTF-8"))?;
    line.clear();

    let trimmed_status = status.trim_start_matches([' ', '\t']);
    if !trimmed_status
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("HTTP/"))
    {
        return Err(proxy_response_error(format!(
            "proxy response did not start with HTTP/: {status}"
        )));
    }

    let mut parts = trimmed_status.split_whitespace();
    let _ = parts.next();
    let code = parts.next().ok_or_else(|| {
        proxy_response_error(format!("proxy response missing status code: {status}"))
    })?;

    if !code.starts_with('2') {
        return Err(proxy_response_error(format!(
            "proxy rejected CONNECT with status {status}"
        )));
    }

    loop {
        read_proxy_line(stream, &mut line, proxy.display())?;
        if line.is_empty() {
            break;
        }
    }

    Ok(())
}

fn connect_via_program(
    addr: &DaemonAddress,
    program: &ConnectProgramConfig,
) -> Result<DaemonStream, ClientError> {
    let command = program
        .format_command(addr.host(), addr.port())
        .map_err(|error| daemon_error(error, FEATURE_UNAVAILABLE_EXIT_CODE))?;

    let shell = program
        .shell()
        .cloned()
        .unwrap_or_else(|| OsString::from("sh"));

    let mut builder = Command::new(&shell);
    builder.arg("-c").arg(&command);
    builder.stdin(Stdio::piped());
    builder.stdout(Stdio::piped());
    builder.stderr(Stdio::inherit());
    builder.env("RSYNC_PORT", addr.port().to_string());

    let mut child = builder.spawn().map_err(|error| {
        daemon_error(
            format!(
                "failed to spawn RSYNC_CONNECT_PROG using shell '{}': {error}",
                Path::new(&shell).display()
            ),
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdin = child.stdin.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a writable stdin",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        daemon_error(
            "RSYNC_CONNECT_PROG command did not expose a readable stdout",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    Ok(DaemonStream::program(ConnectProgramStream::new(
        child, stdin, stdout,
    )))
}

fn load_daemon_connect_program(
    override_template: Option<&OsStr>,
) -> Result<Option<ConnectProgramConfig>, ClientError> {
    if let Some(template) = override_template {
        if template.is_empty() {
            return Err(connect_program_configuration_error(
                "the --connect-program option requires a non-empty command",
            ));
        }

        let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());
        return ConnectProgramConfig::new(OsString::from(template), shell)
            .map(Some)
            .map_err(connect_program_configuration_error);
    }

    let Some(template) = env::var_os("RSYNC_CONNECT_PROG") else {
        return Ok(None);
    };

    if template.is_empty() {
        return Err(connect_program_configuration_error(
            "RSYNC_CONNECT_PROG must not be empty",
        ));
    }

    let shell = env::var_os("RSYNC_SHELL").filter(|value| !value.is_empty());

    ConnectProgramConfig::new(template, shell)
        .map(Some)
        .map_err(connect_program_configuration_error)
}

enum DaemonStream {
    Tcp(TcpStream),
    Program(ConnectProgramStream),
}

impl DaemonStream {
    fn tcp(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }

    fn program(stream: ConnectProgramStream) -> Self {
        Self::Program(stream)
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.read(buf),
            Self::Program(stream) => stream.read(buf),
        }
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(stream) => stream.write(buf),
            Self::Program(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.flush(),
            Self::Program(stream) => stream.flush(),
        }
    }
}

struct ConnectProgramConfig {
    template: OsString,
    shell: Option<OsString>,
}

impl ConnectProgramConfig {
    fn new(template: OsString, shell: Option<OsString>) -> Result<Self, String> {
        if template.is_empty() {
            return Err("RSYNC_CONNECT_PROG must not be empty".to_string());
        }

        if shell.as_ref().is_some_and(|value| value.is_empty()) {
            return Err("RSYNC_SHELL must not be empty".to_string());
        }

        Ok(Self { template, shell })
    }

    fn shell(&self) -> Option<&OsString> {
        self.shell.as_ref()
    }

    /// Expands the configured template by substituting daemon metadata placeholders.
    ///
    /// `%H` is replaced with the daemon host, `%P` with the decimal TCP port, and `%%`
    /// yields a literal percent sign.
    fn format_command(&self, host: &str, port: u16) -> Result<OsString, String> {
        #[cfg(unix)]
        {
            let template = self.template.as_bytes();
            let mut rendered = Vec::with_capacity(template.len() + host.len());
            let mut iter = template.iter().copied();
            let host_bytes = host.as_bytes();
            let port_string = port.to_string();
            let port_bytes = port_string.as_bytes();

            while let Some(byte) = iter.next() {
                if byte == b'%' {
                    match iter.next() {
                        Some(b'%') => rendered.push(b'%'),
                        Some(b'H') => rendered.extend_from_slice(host_bytes),
                        Some(b'P') => rendered.extend_from_slice(port_bytes),
                        Some(other) => {
                            rendered.push(b'%');
                            rendered.push(other);
                        }
                        None => rendered.push(b'%'),
                    }
                } else {
                    rendered.push(byte);
                }
            }

            Ok(OsString::from_vec(rendered))
        }

        #[cfg(not(unix))]
        {
            let template = self.template.as_os_str().to_string_lossy();
            let mut rendered = String::with_capacity(template.len() + host.len());
            let mut chars = template.chars();
            let port_string = port.to_string();

            while let Some(ch) = chars.next() {
                if ch == '%' {
                    match chars.next() {
                        Some('%') => rendered.push('%'),
                        Some('H') => rendered.push_str(host),
                        Some('P') => rendered.push_str(&port_string),
                        Some(other) => {
                            rendered.push('%');
                            rendered.push(other);
                        }
                        None => rendered.push('%'),
                    }
                } else {
                    rendered.push(ch);
                }
            }

            Ok(OsString::from(rendered))
        }
    }
}

struct ConnectProgramStream {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl ConnectProgramStream {
    fn new(child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
        Self {
            child,
            stdin,
            stdout,
        }
    }
}

impl Read for ConnectProgramStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Write for ConnectProgramStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

impl Drop for ConnectProgramStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct ProxyConfig {
    host: String,
    port: u16,
    credentials: Option<ProxyCredentials>,
}

impl ProxyConfig {
    fn display(&self) -> SocketAddrDisplay<'_> {
        SocketAddrDisplay {
            host: &self.host,
            port: self.port,
        }
    }

    fn authorization_header(&self) -> Option<String> {
        self.credentials
            .as_ref()
            .map(ProxyCredentials::authorization_value)
    }
}

struct ProxyCredentials {
    username: String,
    password: String,
}

impl ProxyCredentials {
    fn new(username: String, password: String) -> Self {
        Self { username, password }
    }

    fn authorization_value(&self) -> String {
        let mut bytes = Vec::with_capacity(self.username.len() + self.password.len() + 1);
        bytes.extend_from_slice(self.username.as_bytes());
        bytes.push(b':');
        bytes.extend_from_slice(self.password.as_bytes());
        STANDARD.encode(bytes)
    }
}

fn load_daemon_proxy() -> Result<Option<ProxyConfig>, ClientError> {
    match env::var("RSYNC_PROXY") {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            parse_proxy_spec(trimmed).map(Some)
        }
        Err(VarError::NotPresent) => Ok(None),
        Err(VarError::NotUnicode(_)) => Err(proxy_configuration_error(
            "RSYNC_PROXY value must be valid UTF-8",
        )),
    }
}

fn parse_proxy_spec(spec: &str) -> Result<ProxyConfig, ClientError> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must specify a proxy host",
        ));
    }

    let mut remainder = trimmed;
    if let Some(idx) = trimmed.find("://") {
        let (scheme, rest_with_separator) = trimmed.split_at(idx);
        let rest = &rest_with_separator[3..];
        if rest.is_empty() {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY must specify a proxy host",
            ));
        }

        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY scheme must be http:// or https://",
            ));
        }

        remainder = rest;
    }

    if remainder.contains('/') {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must not include a path component",
        ));
    }

    let (credentials, remainder) = if let Some(idx) = remainder.rfind('@') {
        let (userinfo, host_part) = remainder.split_at(idx);
        if userinfo.is_empty() {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY user information must be non-empty when '@' is present",
            ));
        }

        let mut segments = userinfo.splitn(2, ':');
        let username = segments.next().unwrap();
        let password = segments.next().ok_or_else(|| {
            proxy_configuration_error("RSYNC_PROXY credentials must use USER:PASS@HOST:PORT format")
        })?;

        let username = decode_proxy_component(username, "username")?;
        let password = decode_proxy_component(password, "password")?;
        let credentials = ProxyCredentials::new(username, password);
        (Some(credentials), &host_part[1..])
    } else {
        (None, remainder)
    };

    let (host, port) = parse_proxy_host_port(remainder)?;

    Ok(ProxyConfig {
        host,
        port,
        credentials,
    })
}

fn parse_proxy_host_port(input: &str) -> Result<(String, u16), ClientError> {
    if input.is_empty() {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must specify a proxy host and port",
        ));
    }

    if let Some(rest) = input.strip_prefix('[') {
        let (host, port) = parse_bracketed_host(rest, 0).map_err(|_| {
            proxy_configuration_error("RSYNC_PROXY contains an invalid bracketed host")
        })?;
        if port == 0 {
            return Err(proxy_configuration_error(
                "RSYNC_PROXY bracketed host must include a port",
            ));
        }
        return Ok((host, port));
    }

    let idx = input
        .rfind(':')
        .ok_or_else(|| proxy_configuration_error("RSYNC_PROXY must be in HOST:PORT form"))?;
    let host = &input[..idx];
    let port_text = &input[idx + 1..];

    if port_text.is_empty() {
        return Err(proxy_configuration_error(
            "RSYNC_PROXY must include a proxy port",
        ));
    }

    let host = decode_host_component(host).map_err(|_| {
        proxy_configuration_error("RSYNC_PROXY proxy host contains invalid percent-encoding")
    })?;
    let port = port_text
        .parse::<u16>()
        .map_err(|_| proxy_configuration_error("RSYNC_PROXY specified an invalid port"))?;

    Ok((host, port))
}

fn decode_proxy_component(input: &str, field: &str) -> Result<String, ClientError> {
    if !input.contains('%') {
        return Ok(input.to_string());
    }

    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(proxy_configuration_error(format!(
                    "RSYNC_PROXY {field} contains truncated percent-encoding"
                )));
            }

            let hi = hex_value(bytes[index + 1]).ok_or_else(|| {
                proxy_configuration_error(format!(
                    "RSYNC_PROXY {field} contains invalid percent-encoding"
                ))
            })?;
            let lo = hex_value(bytes[index + 2]).ok_or_else(|| {
                proxy_configuration_error(format!(
                    "RSYNC_PROXY {field} contains invalid percent-encoding"
                ))
            })?;

            decoded.push((hi << 4) | lo);
            index += 3;
            continue;
        }

        decoded.push(bytes[index]);
        index += 1;
    }

    String::from_utf8(decoded).map_err(|_| {
        proxy_configuration_error(format!(
            "RSYNC_PROXY {field} contains invalid UTF-8 after percent-decoding"
        ))
    })
}

fn proxy_configuration_error(text: impl Into<String>) -> ClientError {
    let message = rsync_error!(SOCKET_IO_EXIT_CODE, "{}", text.into()).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

fn proxy_response_error(text: impl Into<String>) -> ClientError {
    let message =
        rsync_error!(SOCKET_IO_EXIT_CODE, "proxy error: {}", text.into()).with_role(Role::Client);
    ClientError::new(SOCKET_IO_EXIT_CODE, message)
}

fn connect_program_configuration_error(text: impl Into<String>) -> ClientError {
    let message =
        rsync_error!(FEATURE_UNAVAILABLE_EXIT_CODE, "{}", text.into()).with_role(Role::Client);
    ClientError::new(FEATURE_UNAVAILABLE_EXIT_CODE, message)
}

fn read_proxy_line(
    stream: &mut TcpStream,
    buffer: &mut Vec<u8>,
    proxy: SocketAddrDisplay<'_>,
) -> Result<(), ClientError> {
    buffer.clear();

    loop {
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(proxy_response_error(
                    "proxy closed the connection during CONNECT negotiation",
                ));
            }
            Ok(_) => {
                buffer.push(byte[0]);
                if byte[0] == b'\n' {
                    while matches!(buffer.last(), Some(b'\n' | b'\r')) {
                        buffer.pop();
                    }
                    break;
                }
                if buffer.len() > 4096 {
                    return Err(proxy_response_error(
                        "proxy response line exceeded 4096 bytes",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(socket_error("read from", proxy, error)),
        }
    }

    Ok(())
}

/// Performs a daemon module listing using caller-provided options.
///
/// This variant mirrors [`run_module_list`] while allowing callers to configure
/// behaviours such as suppressing daemon MOTD lines when `--no-motd` is supplied.
pub fn run_module_list_with_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        options,
        None,
        TransferTimeout::Default,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing using an optional password override.
///
/// When `password_override` is `Some`, the bytes are used for authentication
/// instead of loading `RSYNC_PASSWORD`. This mirrors `--password-file` in the
/// CLI and simplifies testing by avoiding environment manipulation.
pub fn run_module_list_with_password(
    request: ModuleListRequest,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(
        request,
        ModuleListOptions::default(),
        password_override,
        timeout,
        TransferTimeout::Default,
    )
}

/// Performs a daemon module listing with the supplied options and password override.
///
/// The helper is primarily used by the CLI to honour flags such as `--no-motd`
/// while still exercising the optional password override path used for
/// `--password-file`. The [`ModuleListOptions`] parameter defaults to the same
/// behaviour as [`run_module_list`].
pub fn run_module_list_with_password_and_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
    password_override: Option<Vec<u8>>,
    timeout: TransferTimeout,
    connect_timeout: TransferTimeout,
) -> Result<ModuleList, ClientError> {
    let addr = request.address();
    let username = request.username().map(str::to_owned);
    let mut password_bytes = password_override.map(SensitiveBytes::new);
    let mut auth_attempted = false;
    let mut auth_context: Option<DaemonAuthContext> = None;
    let suppress_motd = options.suppresses_motd();
    let address_mode = options.address_mode();

    let effective_timeout = timeout.effective(DAEMON_SOCKET_TIMEOUT);
    let connect_duration = resolve_connect_timeout(connect_timeout, timeout, DAEMON_SOCKET_TIMEOUT);

    let stream = open_daemon_stream(
        addr,
        connect_duration,
        effective_timeout,
        address_mode,
        options.connect_program(),
        options.bind_address(),
    )?;

    let handshake = negotiate_legacy_daemon_session(stream, request.protocol())
        .map_err(|error| map_daemon_handshake_error(error, addr))?;
    let stream = handshake.into_stream();
    let mut reader = BufReader::new(stream);

    reader
        .get_mut()
        .write_all(b"#list\n")
        .map_err(|error| socket_error("write to", addr.socket_addr_display(), error))?;
    reader
        .get_mut()
        .flush()
        .map_err(|error| socket_error("flush", addr.socket_addr_display(), error))?;

    let mut entries = Vec::new();
    let mut motd = Vec::new();
    let mut warnings = Vec::new();
    let mut capabilities = Vec::new();
    let mut acknowledged = false;

    while let Some(line) = read_trimmed_line(&mut reader)
        .map_err(|error| socket_error("read from", addr.socket_addr_display(), error))?
    {
        if let Some(payload) = legacy_daemon_error_payload(&line) {
            return Err(daemon_error(payload, PARTIAL_TRANSFER_EXIT_CODE));
        }

        if let Some(payload) = parse_legacy_warning_message(&line) {
            warnings.push(payload.to_string());
            continue;
        }

        if line.starts_with(LEGACY_DAEMON_PREFIX) {
            match parse_legacy_daemon_message(&line) {
                Ok(LegacyDaemonMessage::Ok) => {
                    acknowledged = true;
                    continue;
                }
                Ok(LegacyDaemonMessage::Exit) => break,
                Ok(LegacyDaemonMessage::Capabilities { flags }) => {
                    capabilities.push(flags.to_string());
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthRequired { module }) => {
                    if auth_attempted {
                        return Err(daemon_protocol_error(
                            "daemon repeated authentication challenge",
                        ));
                    }

                    let username = username.as_deref().ok_or_else(|| {
                        daemon_authentication_required_error(
                            "supply a username in the daemon URL (e.g. rsync://user@host/)",
                        )
                    })?;

                    let secret = if let Some(secret) = password_bytes.as_ref() {
                        secret.to_vec()
                    } else {
                        password_bytes = load_daemon_password().map(SensitiveBytes::new);
                        password_bytes
                            .as_ref()
                            .map(SensitiveBytes::to_vec)
                            .ok_or_else(|| {
                                daemon_authentication_required_error(
                                    "set RSYNC_PASSWORD before contacting authenticated daemons",
                                )
                            })?
                    };

                    let context = DaemonAuthContext::new(username.to_owned(), secret);
                    if let Some(challenge) = module {
                        send_daemon_auth_credentials(&mut reader, &context, challenge, addr)?;
                    }

                    auth_context = Some(context);
                    auth_attempted = true;
                    continue;
                }
                Ok(LegacyDaemonMessage::AuthChallenge { challenge }) => {
                    let context = auth_context.as_ref().ok_or_else(|| {
                        daemon_protocol_error(
                            "daemon issued authentication challenge before requesting credentials",
                        )
                    })?;

                    send_daemon_auth_credentials(&mut reader, context, challenge, addr)?;
                    continue;
                }
                Ok(LegacyDaemonMessage::Other(payload)) => {
                    if let Some(reason) = payload.strip_prefix("DENIED") {
                        return Err(daemon_access_denied_error(reason.trim()));
                    }

                    if let Some(reason) = payload.strip_prefix("AUTHFAILED") {
                        let reason = reason.trim();
                        return Err(daemon_authentication_failed_error(if reason.is_empty() {
                            None
                        } else {
                            Some(reason)
                        }));
                    }

                    if is_motd_payload(payload) {
                        if !suppress_motd {
                            motd.push(normalize_motd_payload(payload));
                        }
                        continue;
                    }

                    return Err(daemon_protocol_error(&line));
                }
                Ok(LegacyDaemonMessage::Version(_)) => {
                    return Err(daemon_protocol_error(&line));
                }
                Err(_) => {
                    return Err(daemon_protocol_error(&line));
                }
            }
        }

        if !acknowledged {
            return Err(daemon_protocol_error(&line));
        }

        entries.push(ModuleListEntry::from_line(&line));
    }

    if !acknowledged {
        return Err(daemon_protocol_error(
            "daemon did not acknowledge module listing",
        ));
    }

    Ok(ModuleList::new(motd, warnings, capabilities, entries))
}

/// Derives the socket connect timeout from the explicit connection setting and
/// the general transfer timeout.
fn resolve_connect_timeout(
    connect_timeout: TransferTimeout,
    fallback: TransferTimeout,
    default: Duration,
) -> Option<Duration> {
    match connect_timeout {
        TransferTimeout::Default => match fallback {
            TransferTimeout::Default => Some(default),
            TransferTimeout::Disabled => None,
            TransferTimeout::Seconds(value) => Some(Duration::from_secs(value.get())),
        },
        TransferTimeout::Disabled => None,
        TransferTimeout::Seconds(value) => Some(Duration::from_secs(value.get())),
    }
}

struct DaemonAuthContext {
    username: String,
    secret: SensitiveBytes,
}

impl DaemonAuthContext {
    fn new(username: String, secret: Vec<u8>) -> Self {
        Self {
            username,
            secret: SensitiveBytes::new(secret),
        }
    }

    fn secret(&self) -> &[u8] {
        self.secret.as_slice()
    }
}

#[cfg(test)]
impl DaemonAuthContext {
    fn into_zeroized_secret(self) -> Vec<u8> {
        self.secret.into_zeroized_vec()
    }
}

struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    fn to_vec(&self) -> Vec<u8> {
        self.0.clone()
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
impl SensitiveBytes {
    fn into_zeroized_vec(mut self) -> Vec<u8> {
        for byte in &mut self.0 {
            *byte = 0;
        }
        std::mem::take(&mut self.0)
    }
}

impl Drop for SensitiveBytes {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            *byte = 0;
        }
    }
}

fn send_daemon_auth_credentials<S>(
    reader: &mut BufReader<S>,
    context: &DaemonAuthContext,
    challenge: &str,
    addr: &DaemonAddress,
) -> Result<(), ClientError>
where
    S: Write,
{
    let digest = compute_daemon_auth_response(context.secret(), challenge);
    let mut command = String::with_capacity(context.username.len() + digest.len() + 2);
    command.push_str(&context.username);
    command.push(' ');
    command.push_str(&digest);
    command.push('\n');

    reader
        .get_mut()
        .write_all(command.as_bytes())
        .map_err(|error| socket_error("write to", addr.socket_addr_display(), error))?;
    reader
        .get_mut()
        .flush()
        .map_err(|error| socket_error("flush", addr.socket_addr_display(), error))?;

    Ok(())
}

#[cfg(test)]
thread_local! {
    static TEST_PASSWORD_OVERRIDE: RefCell<Option<Vec<u8>>> = const { RefCell::new(None) };
}

#[cfg(test)]
fn set_test_daemon_password(password: Option<Vec<u8>>) {
    TEST_PASSWORD_OVERRIDE.with(|slot| *slot.borrow_mut() = password);
}

fn load_daemon_password() -> Option<Vec<u8>> {
    #[cfg(test)]
    if let Some(password) = TEST_PASSWORD_OVERRIDE.with(|slot| slot.borrow().clone()) {
        return Some(password);
    }

    env::var_os("RSYNC_PASSWORD").map(|value| {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;

            value.into_vec()
        }

        #[cfg(not(unix))]
        {
            value.to_string_lossy().into_owned().into_bytes()
        }
    })
}

fn compute_daemon_auth_response(secret: &[u8], challenge: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(secret);
    hasher.update(challenge.as_bytes());
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

fn normalize_motd_payload(payload: &str) -> String {
    if !is_motd_payload(payload) {
        return payload.to_string();
    }

    let remainder = &payload[4..];
    let remainder = remainder.trim_start_matches([' ', '\t', ':']);
    remainder.trim_start().to_string()
}

fn is_motd_payload(payload: &str) -> bool {
    let bytes = payload.as_bytes();
    if bytes.len() < 4 {
        return false;
    }

    if !bytes[..4].eq_ignore_ascii_case(b"motd") {
        return false;
    }

    if bytes.len() == 4 {
        return true;
    }

    matches!(bytes[4], b' ' | b'\t' | b'\r' | b'\n' | b':')
}
