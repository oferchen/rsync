//! # Overview
//!
//! Implements deterministic local filesystem copies used by the current
//! `oc-rsync` development snapshot. The module constructs
//! [`LocalCopyPlan`] values from CLI-style operands and executes them while
//! preserving permissions, timestamps, and optional ownership metadata via
//! [`rsync_meta`].
//!
//! # Design
//!
//! - [`LocalCopyPlan`] encapsulates parsed operands and exposes
//!   [`LocalCopyPlan::execute`] for performing the copy.
//! - [`LocalCopyError`] mirrors upstream exit codes so higher layers can render
//!   canonical diagnostics.
//! - [`LocalCopyOptions`] configures behaviours such as deleting destination
//!   entries that are absent from the source when `--delete` is requested or
//!   preserving ownership/group metadata when `--owner`/`--group` are supplied.
//! - Helper functions preserve metadata after content writes, matching upstream
//!   rsync's ordering and covering regular files, directories, symbolic links,
//!   FIFOs, and device nodes. Hard linked files are reproduced as hard links in
//!   the destination when the platform exposes inode identifiers, and optional
//!   sparse handling skips zero-filled regions when requested so destination
//!   files retain holes present in the source.
//!
//! # Invariants
//!
//! - Plans never mutate their source list after construction.
//! - Copy operations create parent directories before writing files or links.
//! - Metadata application occurs after file contents are written.
//!
//! # Examples
//!
//! ```
//! use rsync_engine::local_copy::LocalCopyPlan;
//! use std::ffi::OsString;
//!
//! # let temp = tempfile::tempdir().unwrap();
//! # let source = temp.path().join("source.txt");
//! # let dest = temp.path().join("dest.txt");
//! # std::fs::write(&source, b"data").unwrap();
//! # std::fs::write(&dest, b"").unwrap();
//! let operands = vec![OsString::from("source.txt"), OsString::from("dest.txt")];
//! let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
//! # let operands = vec![source.into_os_string(), dest.into_os_string()];
//! # let plan = LocalCopyPlan::from_operands(&operands).unwrap();
//! plan.execute().expect("copy succeeds");
//! ```

use std::cmp::Ordering;
#[cfg(unix)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use rsync_filters::FilterSet;
use rsync_meta::{
    MetadataError, MetadataOptions, apply_directory_metadata_with_options,
    apply_file_metadata_with_options, apply_symlink_metadata_with_options, create_device_node,
    create_fifo,
};

const COPY_BUFFER_SIZE: usize = 128 * 1024;

/// Exit code returned when operand validation fails.
const INVALID_OPERAND_EXIT_CODE: i32 = 23;
/// Exit code returned when no transfer operands are supplied.
const MISSING_OPERANDS_EXIT_CODE: i32 = 1;

/// Plan describing a local filesystem copy.
///
/// Instances are constructed from CLI-style operands using
/// [`LocalCopyPlan::from_operands`]. Execution copies regular files, directories,
/// and symbolic links while preserving permissions, timestamps, and
/// optional ownership metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalCopyPlan {
    sources: Vec<SourceSpec>,
    destination: DestinationSpec,
}

impl LocalCopyPlan {
    /// Constructs a plan from CLI-style operands.
    ///
    /// The operands must contain at least one source and a destination. A
    /// trailing path separator on a source operand mirrors upstream rsync's
    /// behaviour of copying the directory *contents* rather than the directory
    /// itself. Remote operands such as `host::module`, `host:/path`, or
    /// `rsync://server/module` are rejected with
    /// [`LocalCopyArgumentError::RemoteOperandUnsupported`] so callers receive a
    /// deterministic diagnostic explaining that this build only supports local
    /// filesystem copies.
    ///
    /// # Errors
    ///
    /// Returns [`LocalCopyErrorKind::MissingSourceOperands`] when fewer than two
    /// operands are supplied. Empty operands and invalid destination states are
    /// reported via [`LocalCopyErrorKind::InvalidArgument`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_engine::local_copy::LocalCopyPlan;
    /// use std::ffi::OsString;
    ///
    /// let operands = vec![OsString::from("src"), OsString::from("dst")];
    /// let plan = LocalCopyPlan::from_operands(&operands).expect("plan succeeds");
    /// assert_eq!(plan.sources().len(), 1);
    /// assert_eq!(plan.destination(), std::path::Path::new("dst"));
    /// ```
    pub fn from_operands(operands: &[OsString]) -> Result<Self, LocalCopyError> {
        if operands.len() < 2 {
            return Err(LocalCopyError::missing_operands());
        }

        let sources: Vec<SourceSpec> = operands[..operands.len() - 1]
            .iter()
            .map(SourceSpec::from_operand)
            .collect::<Result<_, _>>()?;

        if sources.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptySourceOperand,
            ));
        }

        let destination_operand = &operands[operands.len() - 1];
        if destination_operand.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptyDestinationOperand,
            ));
        }

        if operand_is_remote(destination_operand.as_os_str()) {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::RemoteOperandUnsupported,
            ));
        }

        let destination = DestinationSpec::from_operand(destination_operand);

        Ok(Self {
            sources,
            destination,
        })
    }

    /// Returns the planned source operands.
    #[must_use]
    pub fn sources(&self) -> &[SourceSpec] {
        &self.sources
    }

    /// Returns the planned destination path.
    #[must_use]
    pub fn destination(&self) -> &Path {
        self.destination.path()
    }

    /// Executes the planned copy.
    ///
    /// # Errors
    ///
    /// Reports [`LocalCopyError`] variants when operand validation fails or I/O
    /// operations encounter errors.
    pub fn execute(&self) -> Result<(), LocalCopyError> {
        self.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
    }

    /// Executes the planned copy using the requested execution mode.
    ///
    /// When [`LocalCopyExecution::DryRun`] is selected the filesystem is left
    /// untouched while operand validation and readability checks still occur.
    pub fn execute_with(&self, mode: LocalCopyExecution) -> Result<(), LocalCopyError> {
        self.execute_with_options(mode, LocalCopyOptions::default())
    }

    /// Executes the planned copy with additional behavioural options.
    pub fn execute_with_options(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<(), LocalCopyError> {
        copy_sources(self, mode, options).map(|_| ())
    }

    /// Executes the plan while collecting a detailed report of the operations performed.
    pub fn execute_with_report(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<LocalCopyReport, LocalCopyError> {
        copy_sources(self, mode, options.collect_events(true))
            .map(|report| report.unwrap_or_default())
    }
}

/// Describes how a [`LocalCopyPlan`] should be executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyExecution {
    /// Perform the copy and mutate the destination filesystem.
    Apply,
    /// Validate the copy without mutating the destination tree.
    DryRun,
}

impl LocalCopyExecution {
    const fn is_dry_run(self) -> bool {
        matches!(self, Self::DryRun)
    }
}

/// Describes an action performed while executing a [`LocalCopyPlan`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalCopyAction {
    /// File data was copied into place.
    DataCopied,
    /// An existing destination file already matched the source.
    MetadataReused,
    /// A hard link was created pointing at a previously copied destination.
    HardLink,
    /// A symbolic link was recreated.
    SymlinkCopied,
    /// A FIFO node was recreated.
    FifoCopied,
    /// A character or block device was recreated.
    DeviceCopied,
    /// A directory was created.
    DirectoryCreated,
    /// An entry was removed due to `--delete`.
    EntryDeleted,
}

/// Record describing a single filesystem action performed during local copy execution.
#[derive(Clone, Debug)]
pub struct LocalCopyRecord {
    relative_path: PathBuf,
    action: LocalCopyAction,
    bytes_transferred: u64,
    elapsed: Duration,
}

impl LocalCopyRecord {
    /// Creates a new [`LocalCopyRecord`].
    fn new(
        relative_path: PathBuf,
        action: LocalCopyAction,
        bytes_transferred: u64,
        elapsed: Duration,
    ) -> Self {
        Self {
            relative_path,
            action,
            bytes_transferred,
            elapsed,
        }
    }

    /// Returns the relative path affected by this record.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the action performed by this record.
    #[must_use]
    pub fn action(&self) -> &LocalCopyAction {
        &self.action
    }

    /// Returns the number of bytes transferred for this record.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the elapsed time spent performing the action.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// Report returned after executing a [`LocalCopyPlan`] with event collection enabled.
#[derive(Clone, Debug, Default)]
pub struct LocalCopyReport {
    records: Vec<LocalCopyRecord>,
}

impl LocalCopyReport {
    fn new(records: Vec<LocalCopyRecord>) -> Self {
        Self { records }
    }

    /// Returns the list of records captured during execution.
    #[must_use]
    pub fn records(&self) -> &[LocalCopyRecord] {
        &self.records
    }
}

/// Options that influence how a [`LocalCopyPlan`] is executed.
#[derive(Clone, Debug, Default)]
pub struct LocalCopyOptions {
    delete: bool,
    bandwidth_limit: Option<NonZeroU64>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    filters: Option<FilterSet>,
    numeric_ids: bool,
    sparse: bool,
    partial: bool,
    collect_events: bool,
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptions`] value with defaults applied.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            delete: false,
            bandwidth_limit: None,
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: false,
            preserve_times: false,
            filters: None,
            numeric_ids: false,
            sparse: false,
            partial: false,
            collect_events: false,
        }
    }

    /// Requests that destination files absent from the source be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete = delete;
        self
    }

    /// Applies an optional bandwidth limit expressed in bytes per second.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_limit(mut self, limit: Option<NonZeroU64>) -> Self {
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

    /// Requests that the group be preserved when applying metadata.
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

    /// Applies the supplied filter set to the copy plan.
    #[must_use]
    pub fn filters(mut self, filters: Option<FilterSet>) -> Self {
        self.filters = filters;
        self
    }

    /// Requests that UID/GID preservation use numeric identifiers.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric: bool) -> Self {
        self.numeric_ids = numeric;
        self
    }

    /// Requests that sparse files be recreated using holes rather than literal zero writes.
    #[must_use]
    #[doc(alias = "--sparse")]
    pub const fn sparse(mut self, sparse: bool) -> Self {
        self.sparse = sparse;
        self
    }

    /// Requests that partial transfers write into a temporary file that is preserved on failure.
    #[must_use]
    #[doc(alias = "--partial")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// Enables collection of transfer events that describe the work performed by the engine.
    #[must_use]
    pub const fn collect_events(mut self, collect: bool) -> Self {
        self.collect_events = collect;
        self
    }

    /// Reports whether extraneous destination files should be removed.
    #[must_use]
    pub const fn delete_extraneous(&self) -> bool {
        self.delete
    }

    /// Returns the configured bandwidth limit, if any, in bytes per second.
    #[must_use]
    pub const fn bandwidth_limit_bytes(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    /// Reports whether ownership preservation has been requested.
    #[must_use]
    pub const fn preserve_owner(&self) -> bool {
        self.preserve_owner
    }

    /// Reports whether group preservation has been requested.
    #[must_use]
    pub const fn preserve_group(&self) -> bool {
        self.preserve_group
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    pub const fn preserve_permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    pub const fn preserve_times(&self) -> bool {
        self.preserve_times
    }

    /// Returns the configured filter set, if any.
    #[must_use]
    pub fn filter_set(&self) -> Option<&FilterSet> {
        self.filters.as_ref()
    }

    /// Reports whether numeric UID/GID preservation should be used.
    #[must_use]
    pub const fn numeric_ids_enabled(&self) -> bool {
        self.numeric_ids
    }

    /// Reports whether sparse handling has been requested.
    #[must_use]
    pub const fn sparse_enabled(&self) -> bool {
        self.sparse
    }

    /// Reports whether partial transfer handling has been requested.
    #[must_use]
    pub const fn partial_enabled(&self) -> bool {
        self.partial
    }

    /// Reports whether the execution should record transfer events.
    #[must_use]
    pub const fn events_enabled(&self) -> bool {
        self.collect_events
    }
}

#[cfg(unix)]
#[derive(Default)]
struct HardLinkTracker {
    entries: HashMap<HardLinkKey, PathBuf>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct HardLinkKey {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl HardLinkTracker {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    fn existing_target(&self, metadata: &fs::Metadata) -> Option<PathBuf> {
        Self::key(metadata).and_then(|key| self.entries.get(&key).cloned())
    }

    fn record(&mut self, metadata: &fs::Metadata, destination: &Path) {
        if let Some(key) = Self::key(metadata) {
            self.entries.insert(key, destination.to_path_buf());
        }
    }

    fn key(metadata: &fs::Metadata) -> Option<HardLinkKey> {
        use std::os::unix::fs::MetadataExt;

        if metadata.nlink() > 1 {
            Some(HardLinkKey {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
#[derive(Default)]
struct HardLinkTracker;

#[cfg(not(unix))]
impl HardLinkTracker {
    const fn new() -> Self {
        Self
    }

    fn existing_target(&self, _metadata: &fs::Metadata) -> Option<PathBuf> {
        None
    }

    fn record(&mut self, _metadata: &fs::Metadata, _destination: &Path) {}
}

struct CopyContext {
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    hard_links: HardLinkTracker,
    limiter: Option<BandwidthLimiter>,
    events: Option<Vec<LocalCopyRecord>>,
}

impl CopyContext {
    fn new(mode: LocalCopyExecution, options: LocalCopyOptions) -> Self {
        let limiter = options.bandwidth_limit_bytes().map(BandwidthLimiter::new);
        let events = if options.events_enabled() {
            Some(Vec::new())
        } else {
            None
        };
        Self {
            mode,
            options,
            hard_links: HardLinkTracker::new(),
            limiter,
            events,
        }
    }

    fn mode(&self) -> LocalCopyExecution {
        self.mode
    }

    fn options(&self) -> &LocalCopyOptions {
        &self.options
    }

    fn metadata_options(&self) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_owner(self.options.preserve_owner())
            .preserve_group(self.options.preserve_group())
            .preserve_permissions(self.options.preserve_permissions())
            .preserve_times(self.options.preserve_times())
            .numeric_ids(self.options.numeric_ids_enabled())
    }

    fn split_mut(&mut self) -> (&mut HardLinkTracker, Option<&mut BandwidthLimiter>) {
        let Self {
            hard_links,
            limiter,
            ..
        } = self;
        (hard_links, limiter.as_mut())
    }

    fn sparse_enabled(&self) -> bool {
        self.options.sparse_enabled()
    }

    fn allows(&self, relative: &Path, is_dir: bool) -> bool {
        match self.options.filter_set() {
            Some(filters) => filters.allows(relative, is_dir),
            None => true,
        }
    }

    fn partial_enabled(&self) -> bool {
        self.options.partial_enabled()
    }

    fn record(&mut self, record: LocalCopyRecord) {
        if let Some(events) = &mut self.events {
            events.push(record);
        }
    }

    fn into_events(self) -> Option<Vec<LocalCopyRecord>> {
        self.events
    }
}

const NANOS_PER_SECOND: u128 = 1_000_000_000;

struct BandwidthLimiter {
    bytes_per_second: NonZeroU64,
    debt_ns: i128,
    last_instant: Instant,
}

impl BandwidthLimiter {
    fn new(limit: NonZeroU64) -> Self {
        Self {
            bytes_per_second: limit,
            debt_ns: 0,
            last_instant: Instant::now(),
        }
    }

    fn register(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }

        let now = Instant::now();
        let elapsed_ns = now
            .duration_since(self.last_instant)
            .as_nanos()
            .min(i128::MAX as u128) as i128;
        self.debt_ns = self.debt_ns.saturating_sub(elapsed_ns);

        let required_ns = self.required_nanoseconds(bytes);
        let required_ns = required_ns.min(i128::MAX as u128) as i128;
        self.debt_ns = self.debt_ns.saturating_add(required_ns);

        if self.debt_ns > 0 {
            let sleep_ns = self.debt_ns as u128;
            let duration = duration_from_nanoseconds(sleep_ns);
            if !duration.is_zero() {
                sleep_for(duration);
            }
            self.last_instant = Instant::now();
            self.debt_ns = 0;
        } else {
            const MAX_CREDIT_NS: i128 = NANOS_PER_SECOND as i128;
            if self.debt_ns < -MAX_CREDIT_NS {
                self.debt_ns = -MAX_CREDIT_NS;
            }
            self.last_instant = now;
        }
    }

    fn required_nanoseconds(&self, bytes: usize) -> u128 {
        let rate = self.bytes_per_second.get() as u128;
        let bytes = bytes as u128;
        let numerator = bytes.saturating_mul(NANOS_PER_SECOND);
        let mut ns = numerator / rate;
        if numerator % rate != 0 {
            ns = ns.saturating_add(1);
        }
        ns
    }
}

fn duration_from_nanoseconds(ns: u128) -> Duration {
    if ns == 0 {
        return Duration::ZERO;
    }

    let seconds = ns / NANOS_PER_SECOND;
    let nanos = (ns % NANOS_PER_SECOND) as u32;

    if seconds >= u128::from(u64::MAX) {
        Duration::MAX
    } else {
        Duration::new(seconds as u64, nanos)
    }
}

#[cfg(not(test))]
fn sleep_for(duration: Duration) {
    if !duration.is_zero() {
        std::thread::sleep(duration);
    }
}

#[cfg(test)]
thread_local! {
    static RECORDED_SLEEPS: std::cell::RefCell<Vec<Duration>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn sleep_for(duration: Duration) {
    RECORDED_SLEEPS.with(|log| log.borrow_mut().push(duration));
}

#[cfg(test)]
pub(super) fn take_recorded_sleeps() -> Vec<Duration> {
    RECORDED_SLEEPS.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

/// Error produced when planning or executing a local copy fails.
#[derive(Debug)]
pub struct LocalCopyError {
    kind: LocalCopyErrorKind,
}

impl LocalCopyError {
    fn new(kind: LocalCopyErrorKind) -> Self {
        Self { kind }
    }

    /// Constructs an error representing missing operands.
    #[must_use]
    pub fn missing_operands() -> Self {
        Self::new(LocalCopyErrorKind::MissingSourceOperands)
    }

    /// Constructs an invalid-argument error.
    #[must_use]
    pub fn invalid_argument(reason: LocalCopyArgumentError) -> Self {
        Self::new(LocalCopyErrorKind::InvalidArgument(reason))
    }

    /// Constructs an I/O error with action context.
    #[must_use]
    pub fn io(action: &'static str, path: PathBuf, source: io::Error) -> Self {
        Self::new(LocalCopyErrorKind::Io {
            action,
            path,
            source,
        })
    }

    /// Returns the exit code that mirrors upstream rsync's behaviour.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self.kind {
            LocalCopyErrorKind::MissingSourceOperands => MISSING_OPERANDS_EXIT_CODE,
            LocalCopyErrorKind::InvalidArgument(_) | LocalCopyErrorKind::Io { .. } => {
                INVALID_OPERAND_EXIT_CODE
            }
        }
    }

    /// Provides access to the underlying error kind.
    #[must_use]
    pub fn kind(&self) -> &LocalCopyErrorKind {
        &self.kind
    }

    /// Consumes the error and returns its kind.
    #[must_use]
    pub fn into_kind(self) -> LocalCopyErrorKind {
        self.kind
    }
}

impl fmt::Display for LocalCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            LocalCopyErrorKind::MissingSourceOperands => {
                write!(
                    f,
                    "missing source operands: supply at least one source and a destination"
                )
            }
            LocalCopyErrorKind::InvalidArgument(reason) => write!(f, "{}", reason.message()),
            LocalCopyErrorKind::Io {
                action,
                path,
                source,
            } => {
                write!(f, "failed to {action} '{}': {source}", path.display())
            }
        }
    }
}

impl Error for LocalCopyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            LocalCopyErrorKind::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Classification of local copy failures.
#[derive(Debug)]
pub enum LocalCopyErrorKind {
    /// No operands were supplied.
    MissingSourceOperands,
    /// Operands were invalid.
    InvalidArgument(LocalCopyArgumentError),
    /// Filesystem interaction failed.
    Io {
        /// Action being performed.
        action: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying error.
        source: io::Error,
    },
}

impl LocalCopyErrorKind {
    /// Returns the action, path, and source error for [`LocalCopyErrorKind::Io`] values.
    #[must_use]
    pub fn as_io(&self) -> Option<(&'static str, &Path, &io::Error)> {
        match self {
            Self::Io {
                action,
                path,
                source,
            } => Some((action, path.as_path(), source)),
            _ => None,
        }
    }
}

/// Detailed reason for operand validation failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyArgumentError {
    /// A source operand was empty.
    EmptySourceOperand,
    /// The destination operand was empty.
    EmptyDestinationOperand,
    /// Multiple sources targeted a non-directory destination.
    DestinationMustBeDirectory,
    /// Unable to determine the directory name from the source operand.
    DirectoryNameUnavailable,
    /// Unable to determine the file name from the source operand.
    FileNameUnavailable,
    /// Unable to determine the link name from the source operand.
    LinkNameUnavailable,
    /// Encountered a file type that is unsupported.
    UnsupportedFileType,
    /// Attempted to replace an existing directory with a symbolic link.
    ReplaceDirectoryWithSymlink,
    /// Attempted to replace an existing directory with a regular file.
    ReplaceDirectoryWithFile,
    /// Attempted to replace an existing directory with a special file.
    ReplaceDirectoryWithSpecial,
    /// Attempted to replace a non-directory with a directory.
    ReplaceNonDirectoryWithDirectory,
    /// Encountered an operand that refers to a remote host or module.
    RemoteOperandUnsupported,
}

impl LocalCopyArgumentError {
    /// Returns the canonical diagnostic message associated with the error.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::EmptySourceOperand => "source operands must be non-empty",
            Self::EmptyDestinationOperand => "destination operand must be non-empty",
            Self::DestinationMustBeDirectory => {
                "destination must be an existing directory when copying multiple sources"
            }
            Self::DirectoryNameUnavailable => "cannot determine directory name",
            Self::FileNameUnavailable => "cannot determine file name",
            Self::LinkNameUnavailable => "cannot determine link name",
            Self::UnsupportedFileType => "unsupported file type encountered",
            Self::ReplaceDirectoryWithSymlink => {
                "cannot replace existing directory with symbolic link"
            }
            Self::ReplaceDirectoryWithFile => "cannot replace existing directory with regular file",
            Self::ReplaceDirectoryWithSpecial => {
                "cannot replace existing directory with special file"
            }
            Self::ReplaceNonDirectoryWithDirectory => {
                "cannot replace non-directory destination with directory"
            }
            Self::RemoteOperandUnsupported => {
                "remote operands are not supported: this build handles local filesystem copies only"
            }
        }
    }
}

/// Source operand within a [`LocalCopyPlan`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSpec {
    path: PathBuf,
    copy_contents: bool,
}

impl SourceSpec {
    fn from_operand(operand: &OsString) -> Result<Self, LocalCopyError> {
        if operand.is_empty() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::EmptySourceOperand,
            ));
        }

        if operand_is_remote(operand.as_os_str()) {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::RemoteOperandUnsupported,
            ));
        }

        let copy_contents = has_trailing_separator(operand.as_os_str());
        Ok(Self {
            path: PathBuf::from(operand),
            copy_contents,
        })
    }

    /// Returns the source path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reports whether the directory contents should be copied.
    #[must_use]
    pub const fn copy_contents(&self) -> bool {
        self.copy_contents
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DestinationState {
    exists: bool,
    is_dir: bool,
}

#[derive(Debug)]
struct DirectoryEntry {
    file_name: OsString,
    path: PathBuf,
    metadata: fs::Metadata,
}

/// Destination operand capturing directory semantics requested by the caller.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DestinationSpec {
    path: PathBuf,
    force_directory: bool,
}

impl DestinationSpec {
    fn from_operand(operand: &OsString) -> Self {
        let force_directory = has_trailing_separator(operand.as_os_str());
        Self {
            path: PathBuf::from(operand),
            force_directory,
        }
    }

    /// Returns the destination path supplied by the caller.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reports whether the operand explicitly requested directory semantics.
    #[must_use]
    pub const fn force_directory(&self) -> bool {
        self.force_directory
    }
}

fn copy_sources(
    plan: &LocalCopyPlan,
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
) -> Result<Option<LocalCopyReport>, LocalCopyError> {
    let collect_events = options.events_enabled();
    let mut context = CopyContext::new(mode, options);

    let multiple_sources = plan.sources.len() > 1;
    let destination_path = plan.destination.path();
    let mut destination_state = query_destination_state(destination_path)?;

    if plan.destination.force_directory() {
        ensure_destination_directory(destination_path, &mut destination_state, context.mode())?;
    }

    if multiple_sources {
        ensure_destination_directory(destination_path, &mut destination_state, context.mode())?;
    }

    let destination_behaves_like_directory =
        destination_state.is_dir || plan.destination.force_directory();

    for source in &plan.sources {
        let source_path = source.path();
        let metadata = fs::symlink_metadata(source_path).map_err(|error| {
            LocalCopyError::io("access source", source_path.to_path_buf(), error)
        })?;
        let file_type = metadata.file_type();
        let metadata_options = context.metadata_options();

        if file_type.is_dir() {
            if source.copy_contents() {
                copy_directory_recursive(
                    &mut context,
                    source_path,
                    destination_path,
                    &metadata,
                    None,
                )?;
                continue;
            }

            let name = source_path.file_name().ok_or_else(|| {
                LocalCopyError::invalid_argument(LocalCopyArgumentError::DirectoryNameUnavailable)
            })?;
            let relative = PathBuf::from(Path::new(name));
            if !context.allows(&relative, true) {
                continue;
            }

            let target = if destination_behaves_like_directory || multiple_sources {
                destination_path.join(name)
            } else {
                destination_path.to_path_buf()
            };

            copy_directory_recursive(
                &mut context,
                source_path,
                &target,
                &metadata,
                Some(relative.as_path()),
            )?;
        } else {
            let name = source_path.file_name().ok_or_else(|| {
                LocalCopyError::invalid_argument(LocalCopyArgumentError::FileNameUnavailable)
            })?;
            let relative = PathBuf::from(Path::new(name));
            if !context.allows(&relative, file_type.is_dir()) {
                continue;
            }

            let target = if destination_behaves_like_directory {
                destination_path.join(name)
            } else {
                destination_path.to_path_buf()
            };

            if file_type.is_file() {
                copy_file(
                    &mut context,
                    source_path,
                    &target,
                    &metadata,
                    Some(relative.as_path()),
                )?;
            } else if file_type.is_symlink() {
                copy_symlink(
                    source_path,
                    &target,
                    &metadata,
                    context.mode(),
                    metadata_options,
                )?;
                context.record(LocalCopyRecord::new(
                    relative,
                    LocalCopyAction::SymlinkCopied,
                    0,
                    Duration::default(),
                ));
            } else if is_fifo(&file_type) {
                copy_fifo(&target, &metadata, context.mode(), metadata_options)?;
                context.record(LocalCopyRecord::new(
                    relative,
                    LocalCopyAction::FifoCopied,
                    0,
                    Duration::default(),
                ));
            } else if is_device(&file_type) {
                copy_device(&target, &metadata, context.mode(), metadata_options)?;
                context.record(LocalCopyRecord::new(
                    relative,
                    LocalCopyAction::DeviceCopied,
                    0,
                    Duration::default(),
                ));
            } else {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::UnsupportedFileType,
                ));
            }
        }
    }

    if collect_events {
        Ok(context.into_events().map(LocalCopyReport::new))
    } else {
        Ok(None)
    }
}

fn query_destination_state(path: &Path) -> Result<DestinationState, LocalCopyError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            Ok(DestinationState {
                exists: true,
                is_dir: file_type.is_dir(),
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DestinationState::default()),
        Err(error) => Err(LocalCopyError::io(
            "inspect destination",
            path.to_path_buf(),
            error,
        )),
    }
}

fn copy_directory_recursive(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            if !existing.file_type().is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !context.mode().is_dry_run() {
                fs::create_dir_all(destination).map_err(|error| {
                    LocalCopyError::io("create directory", destination.to_path_buf(), error)
                })?;
                if let Some(rel) = relative {
                    context.record(LocalCopyRecord::new(
                        rel.to_path_buf(),
                        LocalCopyAction::DirectoryCreated,
                        0,
                        Duration::default(),
                    ));
                }
            }
        }
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect destination directory",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    let entries = read_directory_entries_sorted(source)?;

    let mut keep_names = Vec::new();

    for entry in entries.iter() {
        let file_name = &entry.file_name;
        let entry_path = &entry.path;
        let entry_metadata = &entry.metadata;
        let entry_type = entry_metadata.file_type();
        let target_path = destination.join(Path::new(file_name));
        let metadata_options = context.metadata_options();

        let entry_relative = match relative {
            Some(base) => base.join(Path::new(file_name)),
            None => PathBuf::from(Path::new(file_name)),
        };

        if !context.allows(&entry_relative, entry_type.is_dir()) {
            keep_names.push(file_name.clone());
            continue;
        }

        keep_names.push(file_name.clone());

        if entry_type.is_dir() {
            copy_directory_recursive(
                context,
                entry_path,
                &target_path,
                entry_metadata,
                Some(entry_relative.as_path()),
            )?;
        } else if entry_type.is_file() {
            copy_file(
                context,
                entry_path,
                &target_path,
                entry_metadata,
                Some(entry_relative.as_path()),
            )?;
        } else if entry_type.is_symlink() {
            copy_symlink(
                entry_path,
                &target_path,
                entry_metadata,
                context.mode(),
                metadata_options,
            )?;
        } else if is_fifo(&entry_type) {
            copy_fifo(
                &target_path,
                entry_metadata,
                context.mode(),
                metadata_options,
            )?;
        } else if is_device(&entry_type) {
            copy_device(
                &target_path,
                entry_metadata,
                context.mode(),
                metadata_options,
            )?;
        } else {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::UnsupportedFileType,
            ));
        }
    }

    if context.options().delete_extraneous() {
        let filters = context.options().filter_set().cloned();
        delete_extraneous_entries(
            context,
            destination,
            relative,
            &keep_names,
            filters.as_ref(),
        )?;
    }

    if !context.mode().is_dry_run() {
        let metadata_options = context.metadata_options();
        apply_directory_metadata_with_options(destination, metadata, metadata_options)
            .map_err(map_metadata_error)?;
    }

    Ok(())
}

fn copy_file(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    let metadata_options = context.metadata_options();
    let mode = context.mode();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| source.file_name().map(PathBuf::from))
        .unwrap_or_else(|| {
            destination
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(PathBuf::new)
        });
    let file_size = metadata.len();
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
            }
        }
    }

    if mode.is_dry_run() {
        match fs::symlink_metadata(destination) {
            Ok(existing) => {
                if existing.file_type().is_dir() {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceDirectoryWithFile,
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect existing destination",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        if let Err(error) = fs::File::open(source) {
            return Err(LocalCopyError::io(
                "open source file",
                source.to_path_buf(),
                error,
            ));
        }

        context.record(LocalCopyRecord::new(
            record_path.clone(),
            LocalCopyAction::DataCopied,
            file_size,
            Duration::default(),
        ));
        return Ok(());
    }

    let existing_metadata = match fs::symlink_metadata(destination) {
        Ok(existing) => Some(existing),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    };

    if let Some(existing) = &existing_metadata {
        if existing.file_type().is_dir() {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::ReplaceDirectoryWithFile,
            ));
        }
    }

    let use_sparse_writes = context.sparse_enabled();
    let partial_enabled = context.partial_enabled();
    let (hard_links, limiter) = context.split_mut();

    if let Some(existing_target) = hard_links.existing_target(metadata) {
        match fs::hard_link(&existing_target, destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_file(destination).map_err(|remove_error| {
                    LocalCopyError::io(
                        "remove existing destination",
                        destination.to_path_buf(),
                        remove_error,
                    )
                })?;
                fs::hard_link(&existing_target, destination).map_err(|link_error| {
                    LocalCopyError::io("create hard link", destination.to_path_buf(), link_error)
                })?;
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    "create hard link",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        hard_links.record(metadata, destination);
        context.record(LocalCopyRecord::new(
            record_path.clone(),
            LocalCopyAction::HardLink,
            file_size,
            Duration::default(),
        ));
        return Ok(());
    }

    if let Some(existing) = existing_metadata.as_ref() {
        if should_skip_copy(source, metadata, destination, existing, metadata_options) {
            apply_file_metadata_with_options(destination, metadata, metadata_options)
                .map_err(map_metadata_error)?;
            hard_links.record(metadata, destination);
            context.record(LocalCopyRecord::new(
                record_path.clone(),
                LocalCopyAction::MetadataReused,
                0,
                Duration::default(),
            ));
            return Ok(());
        }
    }

    let mut reader = fs::File::open(source)
        .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    let write_target = if partial_enabled {
        let partial = partial_destination_path(destination);
        if let Err(error) = fs::remove_file(&partial) {
            if error.kind() != io::ErrorKind::NotFound {
                return Err(LocalCopyError::io(
                    "remove existing partial file",
                    partial.clone(),
                    error,
                ));
            }
        }
        partial
    } else {
        destination.to_path_buf()
    };

    let mut writer = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&write_target)
        .map_err(|error| LocalCopyError::io("copy file", write_target.clone(), error))?;
    let mut buffer = vec![0u8; COPY_BUFFER_SIZE];

    let start = Instant::now();

    copy_file_contents(
        &mut reader,
        &mut writer,
        &mut buffer,
        limiter,
        use_sparse_writes,
        source,
        destination,
    )?;

    drop(writer);

    if partial_enabled {
        if let Err(error) = fs::rename(&write_target, destination) {
            if error.kind() == io::ErrorKind::AlreadyExists {
                fs::remove_file(destination).map_err(|remove_error| {
                    LocalCopyError::io(
                        "remove existing destination",
                        destination.to_path_buf(),
                        remove_error,
                    )
                })?;
                fs::rename(&write_target, destination).map_err(|rename_error| {
                    LocalCopyError::io("finalise partial file", write_target.clone(), rename_error)
                })?;
            } else {
                return Err(LocalCopyError::io(
                    "finalise partial file",
                    write_target.clone(),
                    error,
                ));
            }
        }
    }

    apply_file_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;
    hard_links.record(metadata, destination);
    let elapsed = start.elapsed();
    context.record(LocalCopyRecord::new(
        record_path,
        LocalCopyAction::DataCopied,
        file_size,
        elapsed,
    ));
    Ok(())
}

fn partial_destination_path(destination: &Path) -> PathBuf {
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "partial".to_string());
    let partial_name = format!(".oc-rsync-partial-{}", file_name);
    destination.with_file_name(partial_name)
}

fn copy_file_contents(
    reader: &mut fs::File,
    writer: &mut fs::File,
    buffer: &mut [u8],
    mut limiter: Option<&mut BandwidthLimiter>,
    sparse: bool,
    source: &Path,
    destination: &Path,
) -> Result<(), LocalCopyError> {
    let mut total_bytes: u64 = 0;

    loop {
        let read = reader
            .read(buffer)
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
        if read == 0 {
            break;
        }

        if let Some(ref mut limiter) = limiter {
            limiter.register(read);
        }

        if sparse {
            write_sparse_chunk(writer, &buffer[..read], destination)?;
        } else {
            writer.write_all(&buffer[..read]).map_err(|error| {
                LocalCopyError::io("copy file", destination.to_path_buf(), error)
            })?;
        }

        total_bytes = total_bytes.saturating_add(read as u64);
    }

    if sparse {
        writer.set_len(total_bytes).map_err(|error| {
            LocalCopyError::io(
                "truncate destination file",
                destination.to_path_buf(),
                error,
            )
        })?;
    }

    Ok(())
}

fn write_sparse_chunk(
    writer: &mut fs::File,
    chunk: &[u8],
    destination: &Path,
) -> Result<(), LocalCopyError> {
    let mut index = 0usize;

    while index < chunk.len() {
        if chunk[index] == 0 {
            let start = index;
            while index < chunk.len() && chunk[index] == 0 {
                index += 1;
            }
            let span = index - start;
            if span > 0 {
                writer
                    .seek(SeekFrom::Current(span as i64))
                    .map_err(|error| {
                        LocalCopyError::io(
                            "seek in destination file",
                            destination.to_path_buf(),
                            error,
                        )
                    })?;
            }
        } else {
            let start = index;
            while index < chunk.len() && chunk[index] != 0 {
                index += 1;
            }
            writer.write_all(&chunk[start..index]).map_err(|error| {
                LocalCopyError::io("copy file", destination.to_path_buf(), error)
            })?;
        }
    }

    Ok(())
}

fn should_skip_copy(
    source_path: &Path,
    source: &fs::Metadata,
    destination_path: &Path,
    destination: &fs::Metadata,
    options: MetadataOptions,
) -> bool {
    if destination.len() != source.len() {
        return false;
    }

    if options.times() {
        match (source.modified(), destination.modified()) {
            (Ok(src), Ok(dst)) if system_time_eq(src, dst) => {}
            _ => return false,
        }
    }

    files_match(source_path, destination_path)
}

fn system_time_eq(a: SystemTime, b: SystemTime) -> bool {
    a.eq(&b)
}

fn files_match(source: &Path, destination: &Path) -> bool {
    let mut source_file = match fs::File::open(source) {
        Ok(file) => file,
        Err(_) => return false,
    };
    let mut destination_file = match fs::File::open(destination) {
        Ok(file) => file,
        Err(_) => return false,
    };

    let mut source_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0u8; COPY_BUFFER_SIZE];

    loop {
        let source_read = match source_file.read(&mut source_buffer) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };
        let destination_read = match destination_file.read(&mut destination_buffer) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        };

        if source_read != destination_read {
            return false;
        }

        if source_read == 0 {
            return true;
        }

        if source_buffer[..source_read] != destination_buffer[..destination_read] {
            return false;
        }
    }
}

fn copy_fifo(
    destination: &Path,
    metadata: &fs::Metadata,
    mode: LocalCopyExecution,
    metadata_options: MetadataOptions,
) -> Result<(), LocalCopyError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
            }
        }
    }

    if mode.is_dry_run() {
        match fs::symlink_metadata(destination) {
            Ok(existing) => {
                if existing.file_type().is_dir() {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect existing destination",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        return Ok(());
    }

    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            if existing.file_type().is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                ));
            }

            fs::remove_file(destination).map_err(|error| {
                LocalCopyError::io(
                    "remove existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    create_fifo(destination, metadata).map_err(map_metadata_error)?;
    apply_file_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;
    Ok(())
}

fn copy_device(
    destination: &Path,
    metadata: &fs::Metadata,
    mode: LocalCopyExecution,
    metadata_options: MetadataOptions,
) -> Result<(), LocalCopyError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
            }
        }
    }

    if mode.is_dry_run() {
        match fs::symlink_metadata(destination) {
            Ok(existing) => {
                if existing.file_type().is_dir() {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect existing destination",
                    destination.to_path_buf(),
                    error,
                ));
            }
        }

        return Ok(());
    }

    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            if existing.file_type().is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceDirectoryWithSpecial,
                ));
            }

            fs::remove_file(destination).map_err(|error| {
                LocalCopyError::io(
                    "remove existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    create_device_node(destination, metadata).map_err(map_metadata_error)?;
    apply_file_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;
    Ok(())
}

fn delete_extraneous_entries(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[OsString],
    filters: Option<&FilterSet>,
) -> Result<(), LocalCopyError> {
    let mut keep = HashSet::with_capacity(source_entries.len());
    for name in source_entries {
        keep.insert(name.clone());
    }

    let read_dir = match fs::read_dir(destination) {
        Ok(iter) => iter,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "read destination directory",
                destination.to_path_buf(),
                error,
            ));
        }
    };

    for entry in read_dir {
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read destination entry", destination.to_path_buf(), error)
        })?;
        let name = entry.file_name();

        if keep.contains(&name) {
            continue;
        }

        let name_path = PathBuf::from(name.as_os_str());
        let path = destination.join(&name_path);

        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io("inspect extraneous destination entry", path.clone(), error)
        })?;

        if let Some(filters) = filters {
            let entry_relative = match relative {
                Some(base) => base.join(&name_path),
                None => name_path.clone(),
            };

            if !filters.allows(entry_relative.as_path(), file_type.is_dir()) {
                continue;
            }
        }

        if context.mode().is_dry_run() {
            continue;
        }

        remove_extraneous_path(path, file_type)?;

        let entry_relative = match relative {
            Some(base) => base.join(&name_path),
            None => name_path.clone(),
        };
        context.record(LocalCopyRecord::new(
            entry_relative,
            LocalCopyAction::EntryDeleted,
            0,
            Duration::default(),
        ));
    }

    Ok(())
}

fn remove_extraneous_path(path: PathBuf, file_type: fs::FileType) -> Result<(), LocalCopyError> {
    let context = if file_type.is_dir() {
        "remove extraneous directory"
    } else {
        "remove extraneous entry"
    };

    let result = if file_type.is_dir() {
        fs::remove_dir_all(&path)
    } else {
        fs::remove_file(&path)
    };

    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalCopyError::io(context, path, error)),
    }
}

fn copy_symlink(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    mode: LocalCopyExecution,
    metadata_options: MetadataOptions,
) -> Result<(), LocalCopyError> {
    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            if mode.is_dry_run() {
                match fs::symlink_metadata(parent) {
                    Ok(existing) if !existing.file_type().is_dir() => {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "inspect existing destination",
                            parent.to_path_buf(),
                            error,
                        ));
                    }
                }
            } else {
                fs::create_dir_all(parent).map_err(|error| {
                    LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                })?;
            }
        }
    }

    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            let file_type = existing.file_type();
            if file_type.is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceDirectoryWithSymlink,
                ));
            }

            if !mode.is_dry_run() {
                fs::remove_file(destination).map_err(|error| {
                    LocalCopyError::io(
                        "remove existing destination",
                        destination.to_path_buf(),
                        error,
                    )
                })?;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    let target = fs::read_link(source)
        .map_err(|error| LocalCopyError::io("read symbolic link", source.to_path_buf(), error))?;

    if mode.is_dry_run() {
        return Ok(());
    }

    create_symlink(&target, source, destination).map_err(|error| {
        LocalCopyError::io("create symbolic link", destination.to_path_buf(), error)
    })?;

    apply_symlink_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;

    Ok(())
}

fn ensure_destination_directory(
    destination_path: &Path,
    state: &mut DestinationState,
    mode: LocalCopyExecution,
) -> Result<(), LocalCopyError> {
    if state.exists {
        if !state.is_dir {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::DestinationMustBeDirectory,
            ));
        }
        return Ok(());
    }

    if mode.is_dry_run() {
        state.exists = true;
        state.is_dir = true;
        return Ok(());
    }

    fs::create_dir_all(destination_path).map_err(|error| {
        LocalCopyError::io(
            "create destination directory",
            destination_path.to_path_buf(),
            error,
        )
    })?;
    state.exists = true;
    state.is_dir = true;
    Ok(())
}

fn map_metadata_error(error: MetadataError) -> LocalCopyError {
    let (context, path, source) = error.into_parts();
    LocalCopyError::io(context, path, source)
}

fn read_directory_entries_sorted(path: &Path) -> Result<Vec<DirectoryEntry>, LocalCopyError> {
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(path)
        .map_err(|error| LocalCopyError::io("read directory", path.to_path_buf(), error))?;

    for entry in read_dir {
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read directory entry", path.to_path_buf(), error)
        })?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            LocalCopyError::io("inspect directory entry", entry_path.to_path_buf(), error)
        })?;
        entries.push(DirectoryEntry {
            file_name: entry.file_name(),
            path: entry_path,
            metadata,
        });
    }

    entries.sort_by(|a, b| compare_file_names(&a.file_name, &b.file_name));
    Ok(entries)
}

fn compare_file_names(left: &OsStr, right: &OsStr) -> Ordering {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        return left.as_bytes().cmp(right.as_bytes());
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let left_wide: Vec<u16> = left.encode_wide().collect();
        let right_wide: Vec<u16> = right.encode_wide().collect();
        return left_wide.cmp(&right_wide);
    }

    #[cfg(not(any(unix, windows)))]
    {
        return left.to_string_lossy().cmp(&right.to_string_lossy());
    }
}

fn has_trailing_separator(path: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let bytes = path.as_bytes();
        !bytes.is_empty() && bytes.ends_with(&[b'/'])
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        path.encode_wide()
            .rev()
            .find(|&ch| ch != 0)
            .is_some_and(|ch| ch == b'/' as u16 || ch == b'\\' as u16)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let text = path.to_string_lossy();
        text.ends_with('/') || text.ends_with('\\')
    }
}

fn is_fifo(file_type: &fs::FileType) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        return file_type.is_fifo();
    }

    #[cfg(not(unix))]
    {
        let _ = file_type;
        false
    }
}

fn is_device(file_type: &fs::FileType) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        return file_type.is_char_device() || file_type.is_block_device();
    }

    #[cfg(not(unix))]
    {
        let _ = file_type;
        false
    }
}

fn operand_is_remote(path: &OsStr) -> bool {
    let text = path.to_string_lossy();

    if text.starts_with("rsync://") {
        return true;
    }

    if text.contains("::") {
        return true;
    }

    if let Some(colon_index) = text.find(':') {
        let after = &text[colon_index + 1..];
        if after.starts_with(':') {
            return true;
        }

        let before = &text[..colon_index];
        if before.contains('/') || before.contains('\\') {
            return false;
        }

        if colon_index == 1 && before.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return false;
        }

        return true;
    }

    false
}

#[cfg(unix)]
fn create_symlink(target: &Path, _source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::fs::symlink;

    symlink(target, destination)
}

#[cfg(windows)]
fn create_symlink(target: &Path, source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::fs::{symlink_dir, symlink_file};

    match source.metadata() {
        Ok(metadata) if metadata.file_type().is_dir() => symlink_dir(target, destination),
        Ok(_) => symlink_file(target, destination),
        Err(_) => symlink_file(target, destination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::{FileTime, set_file_mtime};
    use std::io::{Seek, SeekFrom, Write};
    use std::num::NonZeroU64;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn local_copy_options_numeric_ids_round_trip() {
        let options = LocalCopyOptions::default().numeric_ids(true);
        assert!(options.numeric_ids_enabled());
    }

    #[test]
    fn metadata_options_reflect_numeric_ids_setting() {
        let options = LocalCopyOptions::default().numeric_ids(true);
        let context = CopyContext::new(LocalCopyExecution::Apply, options);
        assert!(context.metadata_options().numeric_ids_enabled());
    }

    #[test]
    fn local_copy_options_sparse_round_trip() {
        let options = LocalCopyOptions::default().sparse(true);
        assert!(options.sparse_enabled());
    }

    #[cfg(unix)]
    mod unix_ids {
        #![allow(unsafe_code)]

        pub(super) fn uid(raw: u32) -> rustix::fs::Uid {
            unsafe { rustix::fs::Uid::from_raw(raw) }
        }

        pub(super) fn gid(raw: u32) -> rustix::fs::Gid {
            unsafe { rustix::fs::Gid::from_raw(raw) }
        }
    }

    #[test]
    fn plan_from_operands_requires_destination() {
        let operands = vec![OsString::from("only-source")];
        let error = LocalCopyPlan::from_operands(&operands).expect_err("missing destination");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::MissingSourceOperands
        ));
    }

    #[test]
    fn plan_rejects_empty_operands() {
        let operands = vec![OsString::new(), OsString::from("dest")];
        let error = LocalCopyPlan::from_operands(&operands).expect_err("empty source");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::EmptySourceOperand)
        ));
    }

    #[test]
    fn plan_rejects_empty_destination() {
        let operands = vec![OsString::from("src"), OsString::new()];
        let error = LocalCopyPlan::from_operands(&operands).expect_err("empty destination");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::EmptyDestinationOperand)
        ));
    }

    #[test]
    fn plan_rejects_remote_module_source() {
        let operands = vec![OsString::from("host::module"), OsString::from("dest")];
        let error = LocalCopyPlan::from_operands(&operands).expect_err("remote module");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
        ));
    }

    #[test]
    fn plan_rejects_remote_shell_source() {
        let operands = vec![OsString::from("host:/path"), OsString::from("dest")];
        let error = LocalCopyPlan::from_operands(&operands).expect_err("remote shell source");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
        ));
    }

    #[test]
    fn plan_rejects_remote_destination() {
        let operands = vec![OsString::from("src"), OsString::from("rsync://host/module")];
        let error = LocalCopyPlan::from_operands(&operands).expect_err("remote destination");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::RemoteOperandUnsupported)
        ));
    }

    #[test]
    fn plan_accepts_windows_drive_style_paths() {
        let operands = vec![OsString::from("C:\\source"), OsString::from("C:\\dest")];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan accepts drive paths");
        assert_eq!(plan.sources().len(), 1);
    }

    #[test]
    fn plan_detects_trailing_separator() {
        let operands = vec![OsString::from("dir/"), OsString::from("dest")];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        assert!(plan.sources()[0].copy_contents());
    }

    #[test]
    fn execute_creates_directory_for_trailing_destination_separator() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        fs::write(&source, b"payload").expect("write source");

        let dest_dir = temp.path().join("dest");
        let mut destination_operand = dest_dir.clone().into_os_string();
        destination_operand.push(std::path::MAIN_SEPARATOR_STR);

        let operands = vec![source.clone().into_os_string(), destination_operand];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute().expect("copy succeeds");

        let copied = dest_dir.join(source.file_name().expect("source name"));
        assert_eq!(fs::read(copied).expect("read copied"), b"payload");
    }

    #[test]
    fn execute_copies_single_file() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"example").expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute().expect("copy succeeds");

        assert_eq!(fs::read(destination).expect("read dest"), b"example");
    }

    #[test]
    fn execute_copies_directory_tree() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let nested = source_root.join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(nested.join("file.txt"), b"tree").expect("write file");

        let dest_root = temp.path().join("dest");
        let operands = vec![
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute().expect("copy succeeds");
        assert_eq!(
            fs::read(dest_root.join("nested").join("file.txt")).expect("read"),
            b"tree"
        );
    }

    #[test]
    fn execute_skips_rewriting_identical_destination() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"identical").expect("write source");
        fs::write(&destination, b"identical").expect("write destination");

        let source_metadata = fs::metadata(&source).expect("source metadata");
        let source_mtime = FileTime::from_last_modification_time(&source_metadata);
        set_file_mtime(&destination, source_mtime).expect("align destination mtime");

        let mut dest_perms = fs::metadata(&destination)
            .expect("destination metadata")
            .permissions();
        dest_perms.set_readonly(true);
        fs::set_permissions(&destination, dest_perms).expect("set destination readonly");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true).times(true),
        )
        .expect("copy succeeds without rewriting");

        let final_perms = fs::metadata(&destination)
            .expect("destination metadata")
            .permissions();
        assert!(
            !final_perms.readonly(),
            "destination permissions should match writable source"
        );
        assert_eq!(
            fs::read(&destination).expect("destination contents"),
            b"identical"
        );
    }

    #[cfg(unix)]
    #[test]
    fn execute_copies_symbolic_link() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        fs::write(&target, b"target").expect("write target");

        let link = temp.path().join("link");
        symlink(&target, &link).expect("create link");
        let dest_link = temp.path().join("dest-link");

        let operands = vec![link.into_os_string(), dest_link.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute().expect("copy succeeds");
        let copied = fs::read_link(dest_link).expect("read copied link");
        assert_eq!(copied, target);
    }

    #[cfg(unix)]
    #[test]
    fn execute_does_not_preserve_metadata_by_default() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"metadata").expect("write source");
        fs::write(&destination, b"metadata").expect("write dest");

        fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
        let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
        let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
        set_file_times(&source, atime, mtime).expect("set times");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        plan.execute().expect("copy succeeds");

        let metadata = fs::metadata(&destination).expect("dest metadata");
        assert_ne!(metadata.permissions().mode() & 0o777, 0o640);
        let dest_atime = FileTime::from_last_access_time(&metadata);
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_ne!(dest_atime, atime);
        assert_ne!(dest_mtime, mtime);
    }

    #[cfg(unix)]
    #[test]
    fn execute_preserves_metadata_when_requested() {
        use filetime::{FileTime, set_file_times};
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"metadata").expect("write source");
        fs::write(&destination, b"metadata").expect("write dest");

        fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
        let atime = FileTime::from_unix_time(1_700_000_000, 123_000_000);
        let mtime = FileTime::from_unix_time(1_700_000_100, 456_000_000);
        set_file_times(&source, atime, mtime).expect("set times");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().permissions(true).times(true);
        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        let dest_atime = FileTime::from_last_access_time(&metadata);
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
    }

    #[cfg(unix)]
    #[test]
    fn execute_preserves_ownership_when_requested() {
        use rustix::fs::{AtFlags, chownat};
        use std::os::unix::fs::MetadataExt;

        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"metadata").expect("write source");

        let owner = 23_456;
        let group = 65_432;
        chownat(
            rustix::fs::CWD,
            &source,
            Some(unix_ids::uid(owner)),
            Some(unix_ids::gid(group)),
            AtFlags::empty(),
        )
        .expect("assign ownership");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().owner(true).group(true),
        )
        .expect("copy succeeds");

        let metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(metadata.uid(), owner);
        assert_eq!(metadata.gid(), group);
    }

    #[cfg(unix)]
    #[test]
    fn execute_copies_fifo() {
        use filetime::{FileTime, set_file_times};
        use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let temp = tempdir().expect("tempdir");
        let source_fifo = temp.path().join("source.pipe");
        mknodat(
            CWD,
            &source_fifo,
            FileType::Fifo,
            Mode::from_bits_truncate(0o640),
            makedev(0, 0),
        )
        .expect("mkfifo");

        let atime = FileTime::from_unix_time(1_700_050_000, 123_000_000);
        let mtime = FileTime::from_unix_time(1_700_060_000, 456_000_000);
        set_file_times(&source_fifo, atime, mtime).expect("set fifo timestamps");
        fs::set_permissions(&source_fifo, PermissionsExt::from_mode(0o640))
            .expect("set fifo permissions");

        let source_fifo_path = source_fifo.clone();
        let destination_fifo = temp.path().join("dest.pipe");
        let operands = vec![
            source_fifo.into_os_string(),
            destination_fifo.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let src_metadata = fs::symlink_metadata(&source_fifo_path).expect("source metadata");
        assert_eq!(src_metadata.permissions().mode() & 0o777, 0o640);
        let src_atime = FileTime::from_last_access_time(&src_metadata);
        let src_mtime = FileTime::from_last_modification_time(&src_metadata);
        assert_eq!(src_atime, atime);
        assert_eq!(src_mtime, mtime);

        plan.execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true).times(true),
        )
        .expect("fifo copy succeeds");

        let dest_metadata = fs::symlink_metadata(&destination_fifo).expect("dest metadata");
        assert!(dest_metadata.file_type().is_fifo());
        assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o640);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
    }

    #[cfg(unix)]
    #[test]
    fn execute_copies_fifo_within_directory() {
        use filetime::{FileTime, set_file_times};
        use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let nested = source_root.join("dir");
        fs::create_dir_all(&nested).expect("create nested");

        let source_fifo = nested.join("pipe");
        mknodat(
            CWD,
            &source_fifo,
            FileType::Fifo,
            Mode::from_bits_truncate(0o620),
            makedev(0, 0),
        )
        .expect("mkfifo");

        let atime = FileTime::from_unix_time(1_700_070_000, 111_000_000);
        let mtime = FileTime::from_unix_time(1_700_080_000, 222_000_000);
        set_file_times(&source_fifo, atime, mtime).expect("set fifo timestamps");
        fs::set_permissions(&source_fifo, PermissionsExt::from_mode(0o620))
            .expect("set fifo permissions");

        let source_fifo_path = source_fifo.clone();
        let dest_root = temp.path().join("dest");
        let mut source_operand = source_root.clone().into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());
        let operands = vec![source_operand, dest_root.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let src_metadata = fs::symlink_metadata(&source_fifo_path).expect("source metadata");
        assert_eq!(src_metadata.permissions().mode() & 0o777, 0o620);
        let src_atime = FileTime::from_last_access_time(&src_metadata);
        let src_mtime = FileTime::from_last_modification_time(&src_metadata);
        assert_eq!(src_atime, atime);
        assert_eq!(src_mtime, mtime);

        plan.execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().permissions(true).times(true),
        )
        .expect("fifo copy succeeds");

        let dest_fifo = dest_root.join("dir").join("pipe");
        let metadata = fs::symlink_metadata(&dest_fifo).expect("dest fifo metadata");
        assert!(metadata.file_type().is_fifo());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o620);
        let dest_atime = FileTime::from_last_access_time(&metadata);
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
    }

    #[test]
    fn execute_with_trailing_separator_copies_contents() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let nested = source_root.join("nested");
        fs::create_dir_all(&nested).expect("create nested");
        fs::write(nested.join("file.txt"), b"contents").expect("write file");

        let dest_root = temp.path().join("dest");
        let mut source_operand = source_root.clone().into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());
        let operands = vec![source_operand, dest_root.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute().expect("copy succeeds");
        assert!(dest_root.join("nested").exists());
        assert!(!dest_root.join("source").exists());
    }

    #[test]
    fn execute_with_delete_removes_extraneous_entries() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

        let dest_root = temp.path().join("dest");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
        fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

        let mut source_operand = source_root.clone().into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());
        let operands = vec![source_operand, dest_root.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().delete(true);

        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert_eq!(
            fs::read(dest_root.join("keep.txt")).expect("read keep"),
            b"fresh"
        );
        assert!(!dest_root.join("extra.txt").exists());
    }

    #[test]
    fn execute_with_delete_respects_dry_run() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::write(source_root.join("keep.txt"), b"fresh").expect("write keep");

        let dest_root = temp.path().join("dest");
        fs::create_dir_all(&dest_root).expect("create dest root");
        fs::write(dest_root.join("keep.txt"), b"stale").expect("write stale");
        fs::write(dest_root.join("extra.txt"), b"extra").expect("write extra");

        let operands = vec![
            source_root.into_os_string(),
            dest_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().delete(true);

        plan.execute_with_options(LocalCopyExecution::DryRun, options)
            .expect("dry-run succeeds");

        assert_eq!(
            fs::read(dest_root.join("keep.txt")).expect("read keep"),
            b"stale"
        );
        assert!(dest_root.join("extra.txt").exists());
    }

    #[test]
    fn execute_with_dry_run_leaves_destination_absent() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"preview").expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute_with(LocalCopyExecution::DryRun)
            .expect("dry-run succeeds");

        assert!(!destination.exists());
    }

    #[test]
    fn execute_with_dry_run_detects_directory_conflict() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        fs::write(&source, b"data").expect("write source");

        let dest_root = temp.path().join("dest");
        fs::create_dir_all(&dest_root).expect("create dest root");
        let conflict_dir = dest_root.join("source.txt");
        fs::create_dir_all(&conflict_dir).expect("create conflicting directory");

        let operands = vec![source.into_os_string(), dest_root.into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let error = plan
            .execute_with(LocalCopyExecution::DryRun)
            .expect_err("dry-run should detect conflict");

        match error.into_kind() {
            LocalCopyErrorKind::InvalidArgument(reason) => {
                assert_eq!(reason, LocalCopyArgumentError::ReplaceDirectoryWithFile);
            }
            other => panic!("unexpected error kind: {:?}", other),
        }
    }

    #[cfg(unix)]
    #[test]
    fn execute_preserves_hard_links() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source root");
        let file_a = source_root.join("file-a");
        let file_b = source_root.join("file-b");
        fs::write(&file_a, b"shared").expect("write source file");
        fs::hard_link(&file_a, &file_b).expect("create hard link");

        let dest_root = temp.path().join("dest");
        let operands = vec![
            source_root.into_os_string(),
            dest_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute().expect("copy succeeds");

        let dest_a = dest_root.join("file-a");
        let dest_b = dest_root.join("file-b");
        let metadata_a = fs::metadata(&dest_a).expect("metadata a");
        let metadata_b = fs::metadata(&dest_b).expect("metadata b");

        assert_eq!(metadata_a.ino(), metadata_b.ino());
        assert_eq!(metadata_a.nlink(), 2);
        assert_eq!(metadata_b.nlink(), 2);
        assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
        assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
    }

    #[cfg(unix)]
    #[test]
    fn execute_with_sparse_enabled_creates_holes() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("sparse.bin");
        let mut source_file = fs::File::create(&source).expect("create source");
        source_file.write_all(&[0xAA]).expect("write leading byte");
        source_file
            .seek(SeekFrom::Start(2 * 1024 * 1024))
            .expect("seek to create hole");
        source_file.write_all(&[0xBB]).expect("write trailing byte");
        source_file.set_len(4 * 1024 * 1024).expect("extend source");

        let dense_dest = temp.path().join("dense.bin");
        let sparse_dest = temp.path().join("sparse-copy.bin");

        let plan_dense = LocalCopyPlan::from_operands(&[
            source.clone().into_os_string(),
            dense_dest.clone().into_os_string(),
        ])
        .expect("plan dense");
        plan_dense
            .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
            .expect("dense copy succeeds");

        let plan_sparse = LocalCopyPlan::from_operands(&[
            source.into_os_string(),
            sparse_dest.clone().into_os_string(),
        ])
        .expect("plan sparse");
        plan_sparse
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().sparse(true),
            )
            .expect("sparse copy succeeds");

        let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
        let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

        assert_eq!(dense_meta.len(), sparse_meta.len());
        assert!(sparse_meta.blocks() < dense_meta.blocks());
    }

    #[test]
    fn execute_with_bandwidth_limit_records_sleep() {
        super::take_recorded_sleeps();

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        fs::write(&source, vec![0xAA; 4 * 1024]).expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options =
            LocalCopyOptions::default().bandwidth_limit(Some(NonZeroU64::new(1024).unwrap()));
        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert_eq!(fs::read(&destination).expect("read dest").len(), 4 * 1024);

        let recorded = super::take_recorded_sleeps();
        assert!(
            !recorded.is_empty(),
            "expected bandwidth limiter to schedule sleeps"
        );
        let total = recorded
            .into_iter()
            .fold(Duration::ZERO, |acc, duration| acc + duration);
        let expected = Duration::from_secs(4);
        let diff = if total > expected {
            total - expected
        } else {
            expected - total
        };
        assert!(
            diff <= Duration::from_millis(50),
            "expected sleep duration near {:?}, got {:?}",
            expected,
            total
        );
    }

    #[test]
    fn execute_without_bandwidth_limit_does_not_sleep() {
        super::take_recorded_sleeps();

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"no limit").expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        plan.execute().expect("copy succeeds");

        assert_eq!(fs::read(destination).expect("read dest"), b"no limit");
        let recorded = super::take_recorded_sleeps();
        assert!(recorded.is_empty(), "unexpected sleep durations recorded");
    }

    #[test]
    fn execute_respects_exclude_filter() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(&dest).expect("create dest");
        fs::write(source.join("keep.txt"), b"keep").expect("write keep");
        fs::write(source.join("skip.tmp"), b"skip").expect("write skip");

        let operands = vec![
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
            .expect("compile filters");
        let options = LocalCopyOptions::default().filters(Some(filters));

        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert!(!target_root.join("skip.tmp").exists());
    }

    #[test]
    fn execute_respects_include_filter_override() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(&dest).expect("create dest");
        fs::write(source.join("keep.tmp"), b"keep").expect("write keep");
        fs::write(source.join("skip.tmp"), b"skip").expect("write skip");

        let operands = vec![
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let filters = FilterSet::from_rules([
            rsync_filters::FilterRule::exclude("*.tmp"),
            rsync_filters::FilterRule::include("keep.tmp"),
        ])
        .expect("compile filters");
        let options = LocalCopyOptions::default().filters(Some(filters));

        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.tmp").exists());
        assert!(!target_root.join("skip.tmp").exists());
    }

    #[test]
    fn delete_respects_exclude_filters() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(&dest).expect("create dest");
        fs::write(source.join("keep.txt"), b"keep").expect("write keep");

        let target_root = dest.join("source");
        fs::create_dir_all(&target_root).expect("create target root");
        fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");
        fs::write(target_root.join("extra.txt"), b"extra").expect("write extra");

        let operands = vec![
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
            .expect("compile filters");
        let options = LocalCopyOptions::default()
            .delete(true)
            .filters(Some(filters));

        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert!(!target_root.join("extra.txt").exists());
        let skip_path = target_root.join("skip.tmp");
        assert!(skip_path.exists());
        assert_eq!(fs::read(skip_path).expect("read skip"), b"dest skip");
    }
}
