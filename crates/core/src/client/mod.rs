#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `client` module exposes the orchestration entry points consumed by the
//! `rsync` CLI binary. The current implementation focuses on providing a
//! deterministic, synchronous local copy engine that mirrors the high-level
//! behaviour of `rsync SOURCE DEST` when no remote shells or daemons are
//! involved. The API models the configuration and error structures that higher
//! layers will reuse once network transports and the full delta-transfer engine
//! land.
//!
//! # Design
//!
//! - [`ClientConfig`](crate::client::ClientConfig) encapsulates the caller-provided
//!   transfer arguments. A
//!   builder is offered so future options (e.g. logging verbosity) can be wired
//!   through without breaking call sites. Today it exposes toggles for dry-run
//!   validation (`--dry-run`) and extraneous-destination cleanup (`--delete`).
//! - [`run_client`](crate::client::run_client) executes the client flow. The helper
//!   delegates to
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
//! - [`ModuleListRequest`](crate::client::ModuleListRequest) parses
//!   daemon-style operands (`rsync://host/` or `host::`) and
//!   [`run_module_list`](crate::client::run_module_list) connects to the remote
//!   daemon using the legacy `@RSYNCD:` negotiation to retrieve the advertised
//!   module table.
//! - [`ClientError`](crate::client::ClientError) carries the exit code and fully
//!   formatted [`crate::message::Message`] so binaries can surface diagnostics
//!   via the central rendering helpers.
//!
//! # Invariants
//!
//! - `ClientError::exit_code` always matches the exit code embedded in the
//!   [`crate::message::Message`].
//! - `run_client` never panics and preserves the provided configuration even
//!   when reporting unsupported functionality.
//!
//! # Errors
//!
//! All failures are routed through [`ClientError`](crate::client::ClientError).
//! The structure implements [`std::error::Error`], allowing integration with
//! higher-level error handling stacks without losing access to the formatted
//! diagnostic.
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

mod config;
mod fallback;
mod module_list;

pub use self::config::{
    AddressMode, BandwidthLimit, BindAddress, ClientConfig, ClientConfigBuilder,
    CompressionSetting, DeleteMode, FilterRuleKind, FilterRuleSpec, HumanReadableMode,
    ReferenceDirectory, ReferenceDirectoryKind, StrongChecksumAlgorithm, StrongChecksumChoice,
    TransferTimeout, parse_skip_compress_list, skip_compress_from_env,
};
pub use self::fallback::{RemoteFallbackArgs, RemoteFallbackContext, run_remote_transfer_fallback};
#[cfg(test)]
pub(crate) use self::module_list::set_test_daemon_password;
pub use self::module_list::{
    DaemonAddress, ModuleList, ModuleListEntry, ModuleListOptions, ModuleListRequest,
    run_module_list, run_module_list_with_options, run_module_list_with_password,
    run_module_list_with_password_and_options,
};

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use std::{env, error::Error};

use crate::{
    message::{Message, Role},
    rsync_error,
};
pub use rsync_engine::local_copy::{DirMergeEnforcedKind, DirMergeOptions};
use rsync_engine::local_copy::{
    DirMergeRule, ExcludeIfPresentRule, FilterProgram, FilterProgramEntry, LocalCopyAction,
    LocalCopyArgumentError, LocalCopyError, LocalCopyErrorKind, LocalCopyExecution,
    LocalCopyFileKind, LocalCopyMetadata, LocalCopyOptions, LocalCopyPlan, LocalCopyProgress,
    LocalCopyRecord, LocalCopyRecordHandler, LocalCopyReport, LocalCopySummary,
    ReferenceDirectory as EngineReferenceDirectory,
    ReferenceDirectoryKind as EngineReferenceDirectoryKind,
};
use rsync_filters::FilterRule as EngineFilterRule;
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

    /// Returns the cumulative duration spent sleeping due to bandwidth throttling.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub fn bandwidth_sleep(&self) -> Duration {
        self.stats.bandwidth_sleep()
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

        if destination_root
            .file_name()
            .is_some_and(|file_name| relative == Path::new(file_name))
        {
            return destination_root.to_path_buf();
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

            if let Some(ctx) = requires_fallback.then(|| fallback.take()).flatten() {
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
        .ignore_missing_args(config.ignore_missing_args())
        .update(config.update())
        .with_modify_window(config.modify_window_duration())
        .with_filter_program(filter_program)
        .numeric_ids(config.numeric_ids())
        .preallocate(config.preallocate())
        .hard_links(config.preserve_hard_links())
        .sparse(config.sparse())
        .copy_links(config.copy_links())
        .copy_dirlinks(config.copy_dirlinks())
        .copy_unsafe_links(config.copy_unsafe_links())
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
    {
        options = options.acls(config.preserve_acls());
    }
    #[cfg(feature = "xattr")]
    {
        options = options.xattrs(config.preserve_xattrs());
    }

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
mod tests;

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
