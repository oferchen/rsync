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
//!   compression and advanced metadata such as
//!   ACLs or extended attributes remain out of scope for this snapshot. When
//!   deletion is requested, the helper removes
//!   destination entries that are absent from the source tree before applying
//!   metadata.
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

use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, error::Error};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use rsync_checksums::strong::Md5;
use rsync_engine::local_copy::{
    DirMergeRule, FilterProgram, FilterProgramEntry, LocalCopyAction, LocalCopyError,
    LocalCopyErrorKind, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, LocalCopyRecord,
    LocalCopyReport, LocalCopySummary,
};
use rsync_filters::FilterRule as EngineFilterRule;
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX, LegacyDaemonMessage, ProtocolVersion, parse_legacy_daemon_message,
    parse_legacy_error_message, parse_legacy_warning_message,
};
use rsync_transport::negotiate_legacy_daemon_session;
#[cfg(test)]
use std::cell::RefCell;

use crate::{
    bandwidth::{self, BandwidthParseError},
    message::{Message, Role},
    rsync_error,
};

/// Exit code returned when client functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code used when a copy partially or wholly fails.
const PARTIAL_TRANSFER_EXIT_CODE: i32 = 23;
/// Exit code returned when socket I/O fails.
const SOCKET_IO_EXIT_CODE: i32 = 10;
/// Exit code returned when a daemon violates the protocol.
const PROTOCOL_INCOMPATIBLE_EXIT_CODE: i32 = 2;
/// Timeout applied to daemon sockets to avoid hanging handshakes.
const DAEMON_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Configuration describing the requested client operation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfig {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    checksum: bool,
    numeric_ids: bool,
    filter_rules: Vec<FilterRuleSpec>,
    sparse: bool,
    relative_paths: bool,
    verbosity: u8,
    progress: bool,
    stats: bool,
    partial: bool,
    inplace: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
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

    /// Reports whether a transfer was explicitly requested.
    #[must_use]
    pub fn has_transfer_request(&self) -> bool {
        !self.transfer_args.is_empty()
    }

    /// Returns the ordered list of filter rules supplied by the caller.
    #[must_use]
    pub fn filter_rules(&self) -> &[FilterRuleSpec] {
        &self.filter_rules
    }

    /// Returns whether the run should avoid mutating the destination filesystem.
    #[must_use]
    #[doc(alias = "--dry-run")]
    #[doc(alias = "-n")]
    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }

    /// Returns whether extraneous destination files should be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(&self) -> bool {
        self.delete
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

    /// Reports whether group preservation was requested.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn preserve_group(&self) -> bool {
        self.preserve_group
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

    /// Reports whether numeric UID/GID values should be preserved.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(&self) -> bool {
        self.numeric_ids
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

    /// Reports whether partial transfers were requested.
    #[must_use]
    #[doc(alias = "--partial")]
    #[doc(alias = "-P")]
    pub const fn partial(&self) -> bool {
        self.partial
    }

    /// Reports whether destination updates should be performed in place.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(&self) -> bool {
        self.inplace
    }

    /// Returns whether the configuration requires collection of transfer events.
    #[must_use]
    pub const fn collect_events(&self) -> bool {
        self.verbosity > 0 || self.progress
    }
}

/// Builder used to assemble a [`ClientConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfigBuilder {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    checksum: bool,
    numeric_ids: bool,
    filter_rules: Vec<FilterRuleSpec>,
    sparse: bool,
    relative_paths: bool,
    verbosity: u8,
    progress: bool,
    stats: bool,
    partial: bool,
    inplace: bool,
    preserve_devices: bool,
    preserve_specials: bool,
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

    /// Enables or disables deletion of extraneous destination files.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete = delete;
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

    /// Requests that group metadata be preserved.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
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

    /// Enables or disables checksum-based change detection.
    #[must_use]
    #[doc(alias = "--checksum")]
    #[doc(alias = "-c")]
    pub const fn checksum(mut self, checksum: bool) -> Self {
        self.checksum = checksum;
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

    /// Enables or disables retention of partial files on failure.
    #[must_use]
    #[doc(alias = "--partial")]
    #[doc(alias = "--no-partial")]
    #[doc(alias = "-P")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
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

    /// Enables or disables extended attribute preservation for the transfer.
    #[cfg(feature = "xattr")]
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
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

    /// Finalises the builder and constructs a [`ClientConfig`].
    #[must_use]
    pub fn build(self) -> ClientConfig {
        ClientConfig {
            transfer_args: self.transfer_args,
            dry_run: self.dry_run,
            delete: self.delete,
            bandwidth_limit: self.bandwidth_limit,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
            checksum: self.checksum,
            numeric_ids: self.numeric_ids,
            filter_rules: self.filter_rules,
            sparse: self.sparse,
            relative_paths: self.relative_paths,
            verbosity: self.verbosity,
            progress: self.progress,
            stats: self.stats,
            partial: self.partial,
            inplace: self.inplace,
            preserve_devices: self.preserve_devices,
            preserve_specials: self.preserve_specials,
            #[cfg(feature = "xattr")]
            preserve_xattrs: self.preserve_xattrs,
        }
    }
}

/// Classifies a filter rule as inclusive or exclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilterRuleKind {
    /// Include matching paths.
    Include,
    /// Exclude matching paths.
    Exclude,
    /// Protect matching destination paths from deletion.
    Protect,
    /// Merge per-directory filter rules from `.rsync-filter` style files.
    DirMerge,
}

/// Filter rule supplied by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilterRuleSpec {
    kind: FilterRuleKind,
    pattern: String,
    dir_merge_remove_source: bool,
}

impl FilterRuleSpec {
    /// Creates an include rule for the given pattern text.
    #[must_use]
    #[doc(alias = "show")]
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Include,
            pattern: pattern.into(),
            dir_merge_remove_source: false,
        }
    }

    /// Creates an exclude rule for the given pattern text.
    #[must_use]
    #[doc(alias = "hide")]
    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Exclude,
            pattern: pattern.into(),
            dir_merge_remove_source: false,
        }
    }

    /// Creates a protect rule for the given pattern text.
    #[must_use]
    pub fn protect(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterRuleKind::Protect,
            pattern: pattern.into(),
            dir_merge_remove_source: false,
        }
    }

    /// Creates a per-directory merge rule for the provided filter file pattern.
    #[must_use]
    pub fn dir_merge(pattern: impl Into<String>, remove_source: bool) -> Self {
        Self {
            kind: FilterRuleKind::DirMerge,
            pattern: pattern.into(),
            dir_merge_remove_source: remove_source,
        }
    }

    /// Returns the rule kind.
    #[must_use]
    pub const fn kind(&self) -> FilterRuleKind {
        self.kind
    }

    /// Returns the pattern associated with this rule.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Returns whether the `.rsync-filter` file should be removed after loading.
    #[must_use]
    pub const fn dir_merge_remove_source(&self) -> bool {
        self.dir_merge_remove_source
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
}

impl BandwidthLimit {
    /// Creates a new [`BandwidthLimit`] from the supplied byte-per-second value.
    #[must_use]
    pub const fn from_bytes_per_second(bytes_per_second: NonZeroU64) -> Self {
        Self { bytes_per_second }
    }

    /// Parses a textual `--bwlimit` value into an optional [`BandwidthLimit`].
    pub fn parse(text: &str) -> Result<Option<Self>, BandwidthParseError> {
        bandwidth::parse_bandwidth_argument(text)
            .map(|value| value.map(Self::from_bytes_per_second))
    }

    /// Returns the configured rate in bytes per second.
    #[must_use]
    pub const fn bytes_per_second(self) -> NonZeroU64 {
        self.bytes_per_second
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
    #[must_use]
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
        let events = report
            .records()
            .iter()
            .map(ClientEvent::from_record)
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

    /// Returns the number of directories created during the transfer.
    #[must_use]
    pub fn directories_created(&self) -> u64 {
        self.stats.directories_created()
    }

    /// Returns the number of symbolic links copied during the transfer.
    #[must_use]
    pub fn symlinks_copied(&self) -> u64 {
        self.stats.symlinks_copied()
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

    /// Returns the number of FIFOs created during the transfer.
    #[must_use]
    pub fn fifos_created(&self) -> u64 {
        self.stats.fifos_created()
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
    /// A non-regular entry was skipped because support was disabled.
    SkippedNonRegular,
    /// An entry was deleted due to `--delete`.
    EntryDeleted,
}

/// Event describing a single action performed during a client transfer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientEvent {
    relative_path: PathBuf,
    kind: ClientEventKind,
    bytes_transferred: u64,
    elapsed: Duration,
}

impl ClientEvent {
    fn from_record(record: &LocalCopyRecord) -> Self {
        let kind = match record.action() {
            LocalCopyAction::DataCopied => ClientEventKind::DataCopied,
            LocalCopyAction::MetadataReused => ClientEventKind::MetadataReused,
            LocalCopyAction::HardLink => ClientEventKind::HardLink,
            LocalCopyAction::SymlinkCopied => ClientEventKind::SymlinkCopied,
            LocalCopyAction::FifoCopied => ClientEventKind::FifoCopied,
            LocalCopyAction::DeviceCopied => ClientEventKind::DeviceCopied,
            LocalCopyAction::DirectoryCreated => ClientEventKind::DirectoryCreated,
            LocalCopyAction::SkippedNonRegular => ClientEventKind::SkippedNonRegular,
            LocalCopyAction::EntryDeleted => ClientEventKind::EntryDeleted,
        };
        Self {
            relative_path: record.relative_path().to_path_buf(),
            kind,
            bytes_transferred: record.bytes_transferred(),
            elapsed: record.elapsed(),
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

    /// Returns the elapsed time spent on this event.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// Runs the client orchestration using the provided configuration.
///
/// The current implementation offers best-effort local copies covering
/// directories, regular files, and symbolic links. Metadata preservation, delta
/// compression, and remote transports remain unimplemented.
pub fn run_client(config: ClientConfig) -> Result<ClientSummary, ClientError> {
    if !config.has_transfer_request() {
        return Err(missing_operands_error());
    }

    let plan =
        LocalCopyPlan::from_operands(config.transfer_args()).map_err(map_local_copy_error)?;

    let filter_program = compile_filter_program(config.filter_rules())?;

    let options = {
        let options = LocalCopyOptions::default()
            .delete(config.delete())
            .bandwidth_limit(
                config
                    .bandwidth_limit()
                    .map(|limit| limit.bytes_per_second()),
            )
            .owner(config.preserve_owner())
            .group(config.preserve_group())
            .permissions(config.preserve_permissions())
            .times(config.preserve_times())
            .checksum(config.checksum())
            .with_filter_program(filter_program.clone())
            .numeric_ids(config.numeric_ids())
            .sparse(config.sparse())
            .devices(config.preserve_devices())
            .specials(config.preserve_specials())
            .relative_paths(config.relative_paths())
            .inplace(config.inplace())
            .partial(config.partial());
        #[cfg(feature = "xattr")]
        let options = options.xattrs(config.preserve_xattrs());
        options
    };
    let mode = if config.dry_run() {
        LocalCopyExecution::DryRun
    } else {
        LocalCopyExecution::Apply
    };

    let collect_events = config.collect_events();

    if collect_events {
        plan.execute_with_report(mode, options.collect_events(true))
            .map(ClientSummary::from_report)
            .map_err(map_local_copy_error)
    } else {
        plan.execute_with_options(mode, options)
            .map(ClientSummary::from_summary)
            .map_err(map_local_copy_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
    use std::net::{TcpListener, TcpStream};
    use std::num::NonZeroU64;
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    static ENV_GUARD: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_GUARD.get_or_init(|| Mutex::new(()))
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
    fn builder_enables_dry_run() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .dry_run(true)
            .build();

        assert!(config.dry_run());
    }

    #[test]
    fn builder_enables_delete() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .delete(true)
            .build();

        assert!(config.delete());
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
    fn builder_sets_bandwidth_limit() {
        let limit = BandwidthLimit::from_bytes_per_second(NonZeroU64::new(4096).unwrap());
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .bandwidth_limit(Some(limit))
            .build();

        assert_eq!(config.bandwidth_limit(), Some(limit));
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
    fn builder_enables_sparse() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .sparse(true)
            .build();

        assert!(config.sparse());
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
            .build();

        let summary = run_client(config).expect("link copy succeeds");

        let copied = fs::read_link(destination_link).expect("read copied link");
        assert_eq!(copied, target_file);
        assert_eq!(summary.symlinks_copied(), 1);
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
            .build();

        let summary = run_client(config).expect("directory copy succeeds");

        let copied_link = dest_root.join("nested").join("link");
        let copied_target = fs::read_link(copied_link).expect("read copied link");
        assert_eq!(copied_target, target_file);
        assert_eq!(summary.symlinks_copied(), 1);
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
    fn module_list_request_rejects_remote_transfer() {
        let operands = vec![OsString::from("rsync://example.com/module")];
        let error = ModuleListRequest::from_operands(&operands)
            .expect_err("module transfer should be rejected");
        assert!(error.message().to_string().contains("remote operands"));
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
        let error = ModuleListRequest::from_operands(&operands)
            .expect_err("module transfers should be rejected");
        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(
            error
                .message()
                .to_string()
                .contains("remote operands are not supported")
        );
    }

    #[test]
    fn run_module_list_collects_entries() {
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
    fn run_module_list_collects_motd_after_acknowledgement() {
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
    fn run_module_list_collects_warnings() {
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
    fn run_module_list_reports_daemon_error() {
        let responses = vec!["@ERROR: unavailable\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
        };

        let error = run_module_list(request).expect_err("daemon error should surface");
        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        assert!(error.message().to_string().contains("unavailable"));

        handle.join().expect("server thread");
    }

    #[test]
    fn run_module_list_reports_authentication_required() {
        let responses = vec!["@RSYNCD: AUTHREQD modules\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
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
                    .write_all(b"@RSYNCD: 32.0\n")
                    .expect("write greeting");
                stream.flush().expect("flush greeting");

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read client greeting");
                assert_eq!(line, "@RSYNCD: 32.0\n");

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
                    .write_all(b"@RSYNCD: 32.0\n")
                    .expect("write greeting");
                stream.flush().expect("flush greeting");

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read client greeting");
                assert_eq!(line, "@RSYNCD: 32.0\n");

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
                    .write_all(b"@RSYNCD: 32.0\n")
                    .expect("write greeting");
                stream.flush().expect("flush greeting");

                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).expect("read client greeting");
                assert_eq!(line, "@RSYNCD: 32.0\n");

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
        let responses = vec!["@RSYNCD: DENIED host rules\n", "@RSYNCD: EXIT\n"];
        let (addr, handle) = spawn_stub_daemon(responses);

        let request = ModuleListRequest {
            address: DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
            username: None,
        };

        let error = run_module_list(request).expect_err("denied response should surface");
        assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
        let rendered = error.message().to_string();
        assert!(rendered.contains("denied access"));
        assert!(rendered.contains("host rules"));

        handle.join().expect("server thread");
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
            .write_all(b"@RSYNCD: 32.0\n")
            .expect("write greeting");
        stream.flush().expect("flush greeting");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read client greeting");
        assert_eq!(line, "@RSYNCD: 32.0\n");

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
                EngineFilterRule::include(rule.pattern()),
            )),
            FilterRuleKind::Exclude => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::exclude(rule.pattern()),
            )),
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::protect(rule.pattern()),
            )),
            FilterRuleKind::DirMerge => entries.push(FilterProgramEntry::DirMerge(
                DirMergeRule::new(rule.pattern().to_string(), rule.dir_merge_remove_source()),
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

fn remote_operands_unsupported() -> ClientError {
    daemon_error(
        "remote operands are not supported: this build handles local filesystem copies only",
        PARTIAL_TRANSFER_EXIT_CODE,
    )
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

/// Target daemon address used for module listing requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonAddress {
    host: String,
    port: u16,
}

impl DaemonAddress {
    /// Creates a new daemon address from the supplied host and port.
    pub fn new(host: String, port: u16) -> Result<Self, ClientError> {
        if host.trim().is_empty() {
            return Err(daemon_error(
                "daemon host must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
        Ok(Self { host, port })
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
}

impl ModuleListRequest {
    /// Attempts to derive a module listing request from CLI-style operands.
    pub fn from_operands(operands: &[OsString]) -> Result<Option<Self>, ClientError> {
        if operands.len() != 1 {
            return Ok(None);
        }

        Self::from_operand(&operands[0])
    }

    fn from_operand(operand: &OsString) -> Result<Option<Self>, ClientError> {
        let text = operand.to_string_lossy();

        if let Some(rest) = text.strip_prefix("rsync://") {
            return parse_rsync_url(rest).map(Some);
        }

        if let Some((host_part, module_part)) = split_daemon_host_module(&text) {
            if module_part.is_empty() {
                let target = parse_host_port(host_part)?;
                return Ok(Some(Self::new(target.address, target.username)));
            }
            return Err(remote_operands_unsupported());
        }

        Ok(None)
    }

    fn new(address: DaemonAddress, username: Option<String>) -> Self {
        Self { address, username }
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
}

fn parse_rsync_url(rest: &str) -> Result<ModuleListRequest, ClientError> {
    let mut parts = rest.splitn(2, '/');
    let host_port = parts.next().unwrap_or("");
    let remainder = parts.next();

    if let Some(path) = remainder {
        if !path.is_empty() {
            return Err(remote_operands_unsupported());
        }
    }

    let target = parse_host_port(host_port)?;
    Ok(ModuleListRequest::new(target.address, target.username))
}

struct ParsedDaemonTarget {
    address: DaemonAddress,
    username: Option<String>,
}

fn parse_host_port(input: &str) -> Result<ParsedDaemonTarget, ClientError> {
    const DEFAULT_PORT: u16 = 873;

    if input.is_empty() {
        return Err(daemon_error(
            "daemon host must be non-empty",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        ));
    }

    let (username, input) = split_daemon_username(input)?;

    if let Some(host) = input.strip_prefix('[') {
        let (address, port) = parse_bracketed_host(host, DEFAULT_PORT)?;
        let address = DaemonAddress::new(address, port)?;
        return Ok(ParsedDaemonTarget {
            address,
            username: username.map(|value| value.to_owned()),
        });
    }

    if let Some((host, port)) = split_host_port(input) {
        let port = port
            .parse::<u16>()
            .map_err(|_| daemon_error("invalid daemon port", FEATURE_UNAVAILABLE_EXIT_CODE))?;
        let address = DaemonAddress::new(host.to_string(), port)?;
        return Ok(ParsedDaemonTarget {
            address,
            username: username.map(|value| value.to_owned()),
        });
    }

    let address = DaemonAddress::new(input.to_string(), DEFAULT_PORT)?;
    Ok(ParsedDaemonTarget {
        address,
        username: username.map(|value| value.to_owned()),
    })
}

fn split_daemon_host_module(input: &str) -> Option<(&str, &str)> {
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
                if let Some(prev) = previous_colon {
                    if prev + 1 == idx {
                        let host = &input[..prev];
                        let module = &input[idx + 1..];
                        return Some((host, module));
                    }
                }
                previous_colon = Some(idx);
            }
            _ => {
                previous_colon = None;
            }
        }
    }

    None
}

fn parse_bracketed_host(host: &str, default_port: u16) -> Result<(String, u16), ClientError> {
    let (addr, remainder) = host.split_once(']').ok_or_else(|| {
        daemon_error(
            "invalid bracketed daemon host",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })?;

    if remainder.is_empty() {
        return Ok((addr.to_string(), default_port));
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

    Ok((addr.to_string(), port))
}

fn split_host_port(input: &str) -> Option<(&str, &str)> {
    let idx = input.rfind(':')?;
    let (host, port) = input.split_at(idx);
    if host.contains(':') {
        return None;
    }
    Some((&host[..], &port[1..]))
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

        if host.len() <= 1 {
            return Err(daemon_error(
                "daemon host must be non-empty",
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
pub fn run_module_list(request: ModuleListRequest) -> Result<ModuleList, ClientError> {
    let addr = request.address();
    let username = request.username().map(str::to_owned);
    let mut password_bytes: Option<Vec<u8>> = None;
    let mut auth_attempted = false;
    let mut auth_context: Option<DaemonAuthContext> = None;

    let stream = TcpStream::connect((addr.host.as_str(), addr.port))
        .map_err(|error| socket_error("connect to", addr.socket_addr_display(), error))?;
    stream
        .set_read_timeout(Some(DAEMON_SOCKET_TIMEOUT))
        .map_err(|error| socket_error("configure", addr.socket_addr_display(), error))?;
    stream
        .set_write_timeout(Some(DAEMON_SOCKET_TIMEOUT))
        .map_err(|error| socket_error("configure", addr.socket_addr_display(), error))?;

    let handshake = negotiate_legacy_daemon_session(stream, ProtocolVersion::NEWEST)
        .map_err(|error| socket_error("negotiate with", addr.socket_addr_display(), error))?;
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
        if let Some(payload) = parse_legacy_error_message(&line) {
            return Err(daemon_error(
                payload.to_string(),
                PARTIAL_TRANSFER_EXIT_CODE,
            ));
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
                        secret.clone()
                    } else {
                        password_bytes = load_daemon_password();
                        password_bytes.clone().ok_or_else(|| {
                            daemon_authentication_required_error(
                                "set RSYNC_PASSWORD before contacting authenticated daemons",
                            )
                        })?
                    };

                    let context = DaemonAuthContext::new(username.to_owned(), secret);
                    if let Some(challenge) = module {
                        send_daemon_auth_credentials(&mut reader, &context, challenge, &addr)?;
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

                    send_daemon_auth_credentials(&mut reader, context, challenge, &addr)?;
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
                        motd.push(normalize_motd_payload(payload));
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

struct DaemonAuthContext {
    username: String,
    secret: Vec<u8>,
}

impl DaemonAuthContext {
    fn new(username: String, secret: Vec<u8>) -> Self {
        Self { username, secret }
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
    let digest = compute_daemon_auth_response(context.secret.as_slice(), challenge);
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
    let remainder = remainder.trim_start_matches(&[' ', '\t', ':']);
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
