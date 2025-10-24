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
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use std::{env, error::Error};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use rsync_checksums::strong::Md5;
use rsync_compress::zlib::{CompressionLevel, CompressionLevelError};
pub use rsync_engine::local_copy::{DirMergeEnforcedKind, DirMergeOptions};
use rsync_engine::local_copy::{
    DirMergeRule, ExcludeIfPresentRule, FilterProgram, FilterProgramEntry, LocalCopyAction,
    LocalCopyArgumentError, LocalCopyError, LocalCopyErrorKind, LocalCopyExecution,
    LocalCopyFileKind, LocalCopyMetadata, LocalCopyOptions, LocalCopyPlan, LocalCopyProgress,
    LocalCopyRecord, LocalCopyRecordHandler, LocalCopyReport, LocalCopySummary,
};
use rsync_filters::FilterRule as EngineFilterRule;
use rsync_protocol::{
    LEGACY_DAEMON_PREFIX, LegacyDaemonMessage, ProtocolVersion, parse_legacy_daemon_message,
    parse_legacy_error_message, parse_legacy_warning_message,
};
use rsync_transport::negotiate_legacy_daemon_session;
#[cfg(test)]
use std::cell::RefCell;
use tempfile::NamedTempFile;

use crate::{
    bandwidth::{self, BandwidthParseError},
    message::{Message, Role},
    rsync_error,
};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

/// Exit code returned when client functionality is unavailable.
const FEATURE_UNAVAILABLE_EXIT_CODE: i32 = 1;
/// Exit code used when a copy partially or wholly fails.
const PARTIAL_TRANSFER_EXIT_CODE: i32 = 23;
/// Exit code returned when socket I/O fails.
const SOCKET_IO_EXIT_CODE: i32 = 10;
/// Exit code returned when a daemon violates the protocol.
const PROTOCOL_INCOMPATIBLE_EXIT_CODE: i32 = 2;
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

/// Configuration describing the requested client operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientConfig {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete_mode: DeleteMode,
    delete_excluded: bool,
    remove_source_files: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    omit_dir_times: bool,
    compress: bool,
    compression_level: Option<CompressionLevel>,
    compression_setting: CompressionSetting,
    whole_file: bool,
    checksum: bool,
    size_only: bool,
    ignore_existing: bool,
    update: bool,
    numeric_ids: bool,
    filter_rules: Vec<FilterRuleSpec>,
    sparse: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    relative_paths: bool,
    implied_dirs: bool,
    mkpath: bool,
    verbosity: u8,
    progress: bool,
    stats: bool,
    partial: bool,
    partial_dir: Option<PathBuf>,
    inplace: bool,
    force_event_collection: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    list_only: bool,
    timeout: TransferTimeout,
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
            remove_source_files: false,
            bandwidth_limit: None,
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: false,
            preserve_times: false,
            omit_dir_times: false,
            compress: false,
            compression_level: None,
            compression_setting: CompressionSetting::default(),
            whole_file: true,
            checksum: false,
            size_only: false,
            ignore_existing: false,
            update: false,
            numeric_ids: false,
            filter_rules: Vec::new(),
            sparse: false,
            copy_links: false,
            copy_dirlinks: false,
            relative_paths: false,
            implied_dirs: true,
            mkpath: false,
            verbosity: 0,
            progress: false,
            stats: false,
            partial: false,
            partial_dir: None,
            inplace: false,
            force_event_collection: false,
            preserve_devices: false,
            preserve_specials: false,
            list_only: false,
            timeout: TransferTimeout::Default,
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

    /// Reports whether transfers should be listed without mutating the destination.
    #[must_use]
    #[doc(alias = "--list-only")]
    pub const fn list_only(&self) -> bool {
        self.list_only
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

    /// Returns the ordered list of filter rules supplied by the caller.
    #[must_use]
    pub fn filter_rules(&self) -> &[FilterRuleSpec] {
        &self.filter_rules
    }

    /// Returns the configured transfer timeout.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub const fn timeout(&self) -> TransferTimeout {
        self.timeout
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

    /// Reports whether directory timestamps should be skipped when preserving times.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(&self) -> bool {
        self.omit_dir_times
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

    /// Returns the optional directory used to store partial files.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn partial_directory(&self) -> Option<&Path> {
        self.partial_dir.as_deref()
    }

    /// Reports whether destination updates should be performed in place.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(&self) -> bool {
        self.inplace
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
}

/// Builder used to assemble a [`ClientConfig`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ClientConfigBuilder {
    transfer_args: Vec<OsString>,
    dry_run: bool,
    delete_mode: DeleteMode,
    delete_excluded: bool,
    remove_source_files: bool,
    bandwidth_limit: Option<BandwidthLimit>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    omit_dir_times: bool,
    compress: bool,
    compression_level: Option<CompressionLevel>,
    compression_setting: CompressionSetting,
    whole_file: Option<bool>,
    checksum: bool,
    size_only: bool,
    ignore_existing: bool,
    update: bool,
    numeric_ids: bool,
    filter_rules: Vec<FilterRuleSpec>,
    sparse: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    relative_paths: bool,
    implied_dirs: Option<bool>,
    mkpath: bool,
    verbosity: u8,
    progress: bool,
    stats: bool,
    partial: bool,
    partial_dir: Option<PathBuf>,
    inplace: bool,
    force_event_collection: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    list_only: bool,
    timeout: TransferTimeout,
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

    /// Enables or disables removal of source files after a successful transfer.
    #[must_use]
    #[doc(alias = "--remove-source-files")]
    #[doc(alias = "--remove-sent-files")]
    pub const fn remove_source_files(mut self, remove: bool) -> Self {
        self.remove_source_files = remove;
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

    /// Requests that directory timestamps be skipped when preserving times.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(mut self, omit: bool) -> Self {
        self.omit_dir_times = omit;
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

    /// Enables or disables in-place updates for destination files.
    #[must_use]
    #[doc(alias = "--inplace")]
    #[doc(alias = "--no-inplace")]
    pub const fn inplace(mut self, inplace: bool) -> Self {
        self.inplace = inplace;
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

    /// Finalises the builder and constructs a [`ClientConfig`].
    #[must_use]
    pub fn build(self) -> ClientConfig {
        ClientConfig {
            transfer_args: self.transfer_args,
            dry_run: self.dry_run,
            delete_mode: self.delete_mode,
            delete_excluded: self.delete_excluded,
            remove_source_files: self.remove_source_files,
            bandwidth_limit: self.bandwidth_limit,
            preserve_owner: self.preserve_owner,
            preserve_group: self.preserve_group,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
            omit_dir_times: self.omit_dir_times,
            compress: self.compress,
            compression_level: self.compression_level,
            compression_setting: self.compression_setting,
            whole_file: self.whole_file.unwrap_or(true),
            checksum: self.checksum,
            size_only: self.size_only,
            ignore_existing: self.ignore_existing,
            update: self.update,
            numeric_ids: self.numeric_ids,
            filter_rules: self.filter_rules,
            sparse: self.sparse,
            copy_links: self.copy_links,
            copy_dirlinks: self.copy_dirlinks,
            relative_paths: self.relative_paths,
            implied_dirs: self.implied_dirs.unwrap_or(true),
            mkpath: self.mkpath,
            verbosity: self.verbosity,
            progress: self.progress,
            stats: self.stats,
            partial: self.partial,
            partial_dir: self.partial_dir,
            inplace: self.inplace,
            force_event_collection: self.force_event_collection,
            preserve_devices: self.preserve_devices,
            preserve_specials: self.preserve_specials,
            list_only: self.list_only,
            timeout: self.timeout,
            #[cfg(feature = "acl")]
            preserve_acls: self.preserve_acls,
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
    /// Controls whether remote shell arguments are protected from expansion.
    ///
    /// When `Some(true)` the fallback command receives `--protect-args`,
    /// while `Some(false)` forwards `--no-protect-args`. A `None` value keeps
    /// rsync's default behaviour.
    pub protect_args: Option<bool>,
    /// Enables archive mode (`-a`).
    pub archive: bool,
    /// Enables `--delete`.
    pub delete: bool,
    /// Selects the deletion timing to forward to the fallback binary.
    pub delete_mode: DeleteMode,
    /// Enables `--delete-excluded`.
    pub delete_excluded: bool,
    /// Enables `--checksum`.
    pub checksum: bool,
    /// Enables `--size-only`.
    pub size_only: bool,
    /// Enables `--ignore-existing`.
    pub ignore_existing: bool,
    /// Enables `--update`.
    pub update: bool,
    /// Enables `--compress`.
    pub compress: bool,
    /// Enables `--no-compress` when `true` and compression is otherwise disabled.
    pub compress_disabled: bool,
    /// Optional compression level forwarded via `--compress-level`.
    pub compress_level: Option<OsString>,
    /// Optional `--owner`/`--no-owner` toggle.
    pub owner: Option<bool>,
    /// Optional `--group`/`--no-group` toggle.
    pub group: Option<bool>,
    /// Optional `--perms`/`--no-perms` toggle.
    pub perms: Option<bool>,
    /// Optional `--times`/`--no-times` toggle.
    pub times: Option<bool>,
    /// Optional `--omit-dir-times`/`--no-omit-dir-times` toggle.
    pub omit_dir_times: Option<bool>,
    /// Optional `--numeric-ids`/`--no-numeric-ids` toggle.
    pub numeric_ids: Option<bool>,
    /// Optional `--copy-links`/`--no-copy-links` toggle.
    pub copy_links: Option<bool>,
    /// Enables `--copy-dirlinks` when `true`.
    pub copy_dirlinks: bool,
    /// Optional `--sparse`/`--no-sparse` toggle.
    pub sparse: Option<bool>,
    /// Optional `--devices`/`--no-devices` toggle.
    pub devices: Option<bool>,
    /// Optional `--specials`/`--no-specials` toggle.
    pub specials: Option<bool>,
    /// Optional `--relative`/`--no-relative` toggle.
    pub relative: Option<bool>,
    /// Optional `--implied-dirs`/`--no-implied-dirs` toggle.
    pub implied_dirs: Option<bool>,
    /// Enables `--mkpath`.
    pub mkpath: bool,
    /// Verbosity level translated into repeated `-v` flags.
    pub verbosity: u8,
    /// Enables `--progress`.
    pub progress: bool,
    /// Enables `--stats`.
    pub stats: bool,
    /// Enables `--partial`.
    pub partial: bool,
    /// Optional directory forwarded via `--partial-dir`.
    pub partial_dir: Option<PathBuf>,
    /// Enables `--remove-source-files`.
    pub remove_source_files: bool,
    /// Optional `--inplace`/`--no-inplace` toggle.
    pub inplace: Option<bool>,
    /// Routes daemon messages to standard error via `--msgs2stderr`.
    pub msgs_to_stderr: bool,
    /// Optional `--whole-file`/`--no-whole-file` toggle.
    pub whole_file: Option<bool>,
    /// Optional bandwidth limit forwarded through `--bwlimit`.
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
    /// Values forwarded to the fallback binary via repeated `--info=FLAGS` occurrences.
    pub info_flags: Vec<OsString>,
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
    /// Optional `--out-format` template.
    pub out_format: Option<OsString>,
    /// Enables `--no-motd`.
    pub no_motd: bool,
    /// Optional override for the fallback executable path.
    ///
    /// When unspecified the helper consults the `OC_RSYNC_FALLBACK` environment variable and
    /// defaults to `rsync` if the override is missing or empty.
    pub fallback_binary: Option<OsString>,
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
        protect_args,
        archive,
        delete,
        delete_mode,
        delete_excluded,
        checksum,
        size_only,
        ignore_existing,
        update,
        compress,
        compress_disabled,
        compress_level,
        owner,
        group,
        perms,
        times,
        omit_dir_times,
        numeric_ids,
        copy_links,
        copy_dirlinks,
        sparse,
        devices,
        specials,
        relative,
        implied_dirs,
        mkpath,
        verbosity,
        progress,
        stats,
        partial,
        partial_dir,
        remove_source_files,
        inplace,
        msgs_to_stderr,
        whole_file,
        bwlimit,
        excludes,
        includes,
        exclude_from,
        include_from,
        filters,
        info_flags,
        files_from_used,
        file_list_entries,
        from0,
        password_file,
        mut daemon_password,
        protocol,
        timeout,
        out_format,
        no_motd,
        fallback_binary,
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
    if checksum {
        command_args.push(OsString::from("--checksum"));
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

    push_toggle(&mut command_args, "--owner", "--no-owner", owner);
    push_toggle(&mut command_args, "--group", "--no-group", group);
    push_toggle(&mut command_args, "--perms", "--no-perms", perms);
    push_toggle(&mut command_args, "--times", "--no-times", times);
    push_toggle(
        &mut command_args,
        "--omit-dir-times",
        "--no-omit-dir-times",
        omit_dir_times,
    );
    push_toggle(
        &mut command_args,
        "--numeric-ids",
        "--no-numeric-ids",
        numeric_ids,
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
    push_toggle(&mut command_args, "--sparse", "--no-sparse", sparse);
    push_toggle(&mut command_args, "--devices", "--no-devices", devices);
    push_toggle(&mut command_args, "--specials", "--no-specials", specials);
    push_toggle(&mut command_args, "--relative", "--no-relative", relative);
    push_toggle(
        &mut command_args,
        "--implied-dirs",
        "--no-implied-dirs",
        implied_dirs,
    );
    if mkpath {
        command_args.push(OsString::from("--mkpath"));
    }
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
    if partial {
        command_args.push(OsString::from("--partial"));
    }
    if let Some(dir) = partial_dir {
        command_args.push(OsString::from("--partial-dir"));
        command_args.push(dir.into_os_string());
    }
    if remove_source_files {
        command_args.push(OsString::from("--remove-source-files"));
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
    for filter in filters {
        command_args.push(OsString::from("--filter"));
        command_args.push(filter);
    }

    for flag in info_flags {
        let mut arg = OsString::from("--info=");
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

    if no_motd {
        command_args.push(OsString::from("--no-motd"));
    }

    if let Some(shell) = remote_shell {
        command_args.push(OsString::from("-e"));
        command_args.push(shell);
    }

    command_args.append(&mut remainder);

    let binary = fallback_binary.unwrap_or_else(|| {
        env::var_os("OC_RSYNC_FALLBACK")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| OsString::from("rsync"))
    });

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
    elapsed: Duration,
    metadata: Option<ClientEntryMetadata>,
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
            LocalCopyAction::SkippedExisting => ClientEventKind::SkippedExisting,
            LocalCopyAction::SkippedNewerDestination => ClientEventKind::SkippedNewerDestination,
            LocalCopyAction::SkippedNonRegular => ClientEventKind::SkippedNonRegular,
            LocalCopyAction::EntryDeleted => ClientEventKind::EntryDeleted,
            LocalCopyAction::SourceRemoved => ClientEventKind::SourceRemoved,
        };
        Self {
            relative_path: record.relative_path().to_path_buf(),
            kind,
            bytes_transferred: record.bytes_transferred(),
            elapsed: record.elapsed(),
            metadata: record
                .metadata()
                .map(ClientEntryMetadata::from_local_copy_metadata),
        }
    }

    fn from_progress(relative: &Path, bytes_transferred: u64, elapsed: Duration) -> Self {
        Self {
            relative_path: relative.to_path_buf(),
            kind: ClientEventKind::DataCopied,
            bytes_transferred,
            elapsed,
            metadata: None,
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

    /// Returns the metadata associated with the event, when available.
    #[must_use]
    pub fn metadata(&self) -> Option<&ClientEntryMetadata> {
        self.metadata.as_ref()
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

        let total = preview_report
            .records()
            .iter()
            .filter(|record| {
                let event = ClientEvent::from_record(record);
                event.kind().is_progress()
            })
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
        })
    }

    fn as_handler_mut(&mut self) -> &mut dyn LocalCopyRecordHandler {
        self
    }
}

impl<'a> LocalCopyRecordHandler for ClientProgressForwarder<'a> {
    fn handle(&mut self, record: LocalCopyRecord) {
        let event = ClientEvent::from_record(&record);
        if !event.kind().is_progress() {
            return;
        }

        self.emitted = self.emitted.saturating_add(1);
        let index = self.emitted;
        let remaining = self.total.saturating_sub(index);

        let total_bytes = if matches!(record.action(), LocalCopyAction::DataCopied) {
            Some(record.bytes_transferred())
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
            progress.elapsed(),
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
        DeleteMode::Delay => options.delete_after(true),
        DeleteMode::During | DeleteMode::Disabled => options,
    };
    options = options
        .delete_excluded(config.delete_excluded())
        .remove_source_files(config.remove_source_files())
        .bandwidth_limit(
            config
                .bandwidth_limit()
                .map(|limit| limit.bytes_per_second()),
        )
        .with_default_compression_level(config.compression_setting().level_or_default())
        .whole_file(config.whole_file())
        .compress(config.compress())
        .with_compression_level_override(config.compression_level())
        .owner(config.preserve_owner())
        .group(config.preserve_group())
        .permissions(config.preserve_permissions())
        .times(config.preserve_times())
        .omit_dir_times(config.omit_dir_times())
        .checksum(config.checksum())
        .size_only(config.size_only())
        .ignore_existing(config.ignore_existing())
        .update(config.update())
        .with_filter_program(filter_program)
        .numeric_ids(config.numeric_ids())
        .sparse(config.sparse())
        .copy_links(config.copy_links())
        .copy_dirlinks(config.copy_dirlinks())
        .devices(config.preserve_devices())
        .specials(config.preserve_specials())
        .relative_paths(config.relative_paths())
        .implied_dirs(config.implied_dirs())
        .mkpath(config.mkpath())
        .inplace(config.inplace())
        .partial(config.partial())
        .with_partial_directory(config.partial_directory().map(|path| path.to_path_buf()))
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
    use std::ffi::OsString;
    use std::fs;
    use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
    use std::net::{TcpListener, TcpStream};
    use std::num::{NonZeroU8, NonZeroU64};
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

    #[cfg(unix)]
    use std::path::{Path, PathBuf};

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
            protect_args: None,
            archive: false,
            delete: false,
            delete_mode: DeleteMode::Disabled,
            delete_excluded: false,
            checksum: false,
            size_only: false,
            ignore_existing: false,
            update: false,
            compress: false,
            compress_disabled: false,
            compress_level: None,
            owner: None,
            group: None,
            perms: None,
            times: None,
            omit_dir_times: None,
            numeric_ids: None,
            copy_links: None,
            copy_dirlinks: false,
            sparse: None,
            devices: None,
            specials: None,
            relative: None,
            implied_dirs: None,
            mkpath: false,
            verbosity: 0,
            progress: false,
            stats: false,
            partial: false,
            partial_dir: None,
            remove_source_files: false,
            inplace: None,
            msgs_to_stderr: false,
            whole_file: None,
            bwlimit: None,
            excludes: Vec::new(),
            includes: Vec::new(),
            exclude_from: Vec::new(),
            include_from: Vec::new(),
            filters: Vec::new(),
            info_flags: Vec::new(),
            files_from_used: false,
            file_list_entries: Vec::new(),
            from0: false,
            password_file: None,
            daemon_password: None,
            protocol: None,
            timeout: TransferTimeout::Default,
            out_format: None,
            no_motd: false,
            fallback_binary: None,
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
    fn local_copy_options_omit_timeout_when_unset() {
        let config = ClientConfig::builder()
            .transfer_args([OsString::from("src"), OsString::from("dst")])
            .build();

        let options = build_local_copy_options(&config, None);
        assert!(options.timeout().is_none());
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
    fn run_module_list_reports_daemon_error() {
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
    fn run_module_list_reports_authentication_required() {
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
            FilterRuleKind::Protect => entries.push(FilterProgramEntry::Rule(
                EngineFilterRule::protect(rule.pattern().to_string())
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
    /// Attempts to derive a module listing request from CLI-style operands.
    pub fn from_operands(operands: &[OsString]) -> Result<Option<Self>, ClientError> {
        if operands.len() != 1 {
            return Ok(None);
        }

        Self::from_operand(&operands[0])
    }

    fn from_operand(operand: &OsString) -> Result<Option<Self>, ClientError> {
        let text = operand.to_string_lossy();

        if let Some(rest) = strip_prefix_ignore_ascii_case(&text, "rsync://") {
            return parse_rsync_url(rest);
        }

        if let Some((host_part, module_part)) = split_daemon_host_module(&text)? {
            if module_part.is_empty() {
                let target = parse_host_port(host_part)?;
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModuleListOptions {
    suppress_motd: bool,
}

impl ModuleListOptions {
    /// Creates a new options structure with all toggles disabled.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            suppress_motd: false,
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
    pub const fn suppresses_motd(self) -> bool {
        self.suppress_motd
    }
}

impl Default for ModuleListOptions {
    fn default() -> Self {
        Self::new()
    }
}

fn parse_rsync_url(rest: &str) -> Result<Option<ModuleListRequest>, ClientError> {
    let mut parts = rest.splitn(2, '/');
    let host_port = parts.next().unwrap_or("");
    let remainder = parts.next();

    if remainder.is_some_and(|path| !path.is_empty()) {
        return Ok(None);
    }

    let target = parse_host_port(host_port)?;
    Ok(Some(ModuleListRequest::new(
        target.address,
        target.username,
    )))
}

struct ParsedDaemonTarget {
    address: DaemonAddress,
    username: Option<String>,
}

fn parse_host_port(input: &str) -> Result<ParsedDaemonTarget, ClientError> {
    const DEFAULT_PORT: u16 = 873;
    const DEFAULT_HOST: &str = "localhost";

    let (username, input) = split_daemon_username(input)?;

    if input.is_empty() {
        let address = DaemonAddress::new(DEFAULT_HOST.to_string(), DEFAULT_PORT)?;
        return Ok(ParsedDaemonTarget {
            address,
            username: username.map(|value| value.to_owned()),
        });
    }

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
        let host = decode_host_component(host)?;
        let address = DaemonAddress::new(host, port)?;
        return Ok(ParsedDaemonTarget {
            address,
            username: username.map(|value| value.to_owned()),
        });
    }

    let host = decode_host_component(input)?;
    let address = DaemonAddress::new(host, DEFAULT_PORT)?;
    Ok(ParsedDaemonTarget {
        address,
        username: username.map(|value| value.to_owned()),
    })
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
    if !input.contains('%') {
        return Ok(input.to_string());
    }

    let mut decoded = Vec::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(invalid_percent_encoding_error());
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

    String::from_utf8(decoded).map_err(|_| {
        daemon_error(
            "daemon host contains invalid UTF-8 after percent-decoding",
            FEATURE_UNAVAILABLE_EXIT_CODE,
        )
    })
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
pub fn run_module_list(request: ModuleListRequest) -> Result<ModuleList, ClientError> {
    run_module_list_with_options(request, ModuleListOptions::default())
}

/// Performs a daemon module listing using caller-provided options.
///
/// This variant mirrors [`run_module_list`] while allowing callers to configure
/// behaviours such as suppressing daemon MOTD lines when `--no-motd` is supplied.
pub fn run_module_list_with_options(
    request: ModuleListRequest,
    options: ModuleListOptions,
) -> Result<ModuleList, ClientError> {
    run_module_list_with_password_and_options(request, options, None, TransferTimeout::Default)
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
) -> Result<ModuleList, ClientError> {
    let addr = request.address();
    let username = request.username().map(str::to_owned);
    let mut password_bytes = password_override.map(SensitiveBytes::new);
    let mut auth_attempted = false;
    let mut auth_context: Option<DaemonAuthContext> = None;
    let suppress_motd = options.suppresses_motd();

    let stream = TcpStream::connect((addr.host.as_str(), addr.port))
        .map_err(|error| socket_error("connect to", addr.socket_addr_display(), error))?;
    let effective_timeout = timeout.effective(DAEMON_SOCKET_TIMEOUT);
    stream
        .set_read_timeout(effective_timeout)
        .map_err(|error| socket_error("configure", addr.socket_addr_display(), error))?;
    stream
        .set_write_timeout(effective_timeout)
        .map_err(|error| socket_error("configure", addr.socket_addr_display(), error))?;

    let handshake = negotiate_legacy_daemon_session(stream, request.protocol())
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
