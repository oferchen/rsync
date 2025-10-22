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
//!   entries that are absent from the source when `--delete` is requested,
//!   pruning excluded entries when `--delete-excluded` is enabled, or
//!   preserving ownership/group metadata when `--owner`/`--group` are supplied.
//! - Helper functions preserve metadata after content writes, matching upstream
//!   rsync's ordering and covering regular files, directories, symbolic links,
//!   FIFOs, and device nodes when the caller enables the corresponding options.
//!   Hard linked files are reproduced as hard links in the destination when the
//!   platform exposes inode identifiers, and optional sparse handling skips
//!   zero-filled regions when requested so destination files retain holes present
//!   in the source.
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
//! let summary = plan.execute().expect("copy succeeds");
//! assert_eq!(summary.files_copied(), 1);
//! ```

use std::cell::RefCell;
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
use std::process;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant, SystemTime};

use globset::{GlobBuilder, GlobMatcher};
use rsync_checksums::strong::Md5;
use rsync_filters::{FilterRule, FilterSet};
#[cfg(feature = "xattr")]
use rsync_meta::sync_xattrs;
use rsync_meta::{
    MetadataError, MetadataOptions, apply_directory_metadata_with_options,
    apply_file_metadata_with_options, apply_symlink_metadata_with_options, create_device_node,
    create_fifo,
};

const COPY_BUFFER_SIZE: usize = 128 * 1024;
static NEXT_TEMP_FILE_ID: AtomicUsize = AtomicUsize::new(0);

/// Rule kind enforced for entries inside a dir-merge file when modifiers
/// request include-only or exclude-only semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirMergeEnforcedKind {
    /// All entries are treated as include rules.
    Include,
    /// All entries are treated as exclude rules.
    Exclude,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DirMergeParser {
    Lines {
        enforce_kind: Option<DirMergeEnforcedKind>,
        allow_comments: bool,
    },
    Whitespace {
        enforce_kind: Option<DirMergeEnforcedKind>,
    },
}

impl DirMergeParser {
    const fn enforce_kind(&self) -> Option<DirMergeEnforcedKind> {
        match self {
            Self::Lines { enforce_kind, .. } | Self::Whitespace { enforce_kind } => *enforce_kind,
        }
    }

    const fn allows_comments(&self) -> bool {
        matches!(
            self,
            Self::Lines {
                allow_comments: true,
                ..
            }
        )
    }

    const fn is_whitespace(&self) -> bool {
        matches!(self, Self::Whitespace { .. })
    }
}

/// Behavioural modifiers applied to a per-directory filter merge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirMergeOptions {
    inherit: bool,
    exclude_self: bool,
    parser: DirMergeParser,
    allow_list_clear: bool,
}

impl DirMergeOptions {
    /// Creates default merge options: inherited rules, line-based parsing, and
    /// comment support.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inherit: true,
            exclude_self: false,
            parser: DirMergeParser::Lines {
                enforce_kind: None,
                allow_comments: true,
            },
            allow_list_clear: false,
        }
    }

    /// Requests that the parsed rules be inherited by subdirectories.
    #[must_use]
    pub const fn inherit(mut self, inherit: bool) -> Self {
        self.inherit = inherit;
        self
    }

    /// Requests that the filter file be excluded from the transfer.
    #[must_use]
    pub const fn exclude_filter_file(mut self, exclude: bool) -> Self {
        self.exclude_self = exclude;
        self
    }

    /// Applies an enforced rule kind to entries parsed from the file.
    #[must_use]
    pub fn with_enforced_kind(mut self, kind: Option<DirMergeEnforcedKind>) -> Self {
        self.parser = match self.parser {
            DirMergeParser::Lines { allow_comments, .. } => DirMergeParser::Lines {
                enforce_kind: kind,
                allow_comments,
            },
            DirMergeParser::Whitespace { .. } => DirMergeParser::Whitespace { enforce_kind: kind },
        };
        self
    }

    /// Switches parsing to whitespace-separated tokens instead of whole lines.
    #[must_use]
    pub fn use_whitespace(mut self) -> Self {
        let enforce = self.parser.enforce_kind();
        self.parser = DirMergeParser::Whitespace {
            enforce_kind: enforce,
        };
        self
    }

    /// Toggles comment handling for line-based parsing.
    #[must_use]
    pub fn allow_comments(mut self, allow: bool) -> Self {
        self.parser = match self.parser {
            DirMergeParser::Lines { enforce_kind, .. } => DirMergeParser::Lines {
                enforce_kind,
                allow_comments: allow,
            },
            other => other,
        };
        self
    }

    /// Permits list-clearing `!` directives inside the merge file.
    #[must_use]
    pub const fn allow_list_clearing(mut self, allow: bool) -> Self {
        self.allow_list_clear = allow;
        self
    }

    /// Returns whether the parsed rules should be inherited.
    #[must_use]
    pub const fn inherit_rules(&self) -> bool {
        self.inherit
    }

    /// Returns whether the filter file itself should be excluded from transfer.
    #[must_use]
    pub const fn excludes_self(&self) -> bool {
        self.exclude_self
    }

    /// Returns whether list-clearing directives are permitted.
    #[must_use]
    pub const fn list_clear_allowed(&self) -> bool {
        self.allow_list_clear
    }

    /// Returns the parser configuration used when reading the file.
    #[must_use]
    const fn parser(&self) -> &DirMergeParser {
        &self.parser
    }

    /// Reports whether whitespace tokenisation is enabled.
    #[must_use]
    pub const fn uses_whitespace(&self) -> bool {
        self.parser.is_whitespace()
    }

    /// Reports whether comment lines are honoured when parsing.
    #[must_use]
    pub const fn allows_comments(&self) -> bool {
        self.parser.allows_comments()
    }

    /// Returns the enforced rule kind, if any.
    #[must_use]
    pub const fn enforced_kind(&self) -> Option<DirMergeEnforcedKind> {
        self.parser.enforce_kind()
    }
}

impl Default for DirMergeOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Exit code returned when operand validation fails.
const INVALID_OPERAND_EXIT_CODE: i32 = 23;
/// Exit code returned when no transfer operands are supplied.
const MISSING_OPERANDS_EXIT_CODE: i32 = 1;

/// Ordered list of filter rules and per-directory merge directives.
#[derive(Clone, Debug, Default)]
pub struct FilterProgram {
    instructions: Vec<FilterInstruction>,
    dir_merge_rules: Vec<DirMergeRule>,
}

impl FilterProgram {
    /// Builds a [`FilterProgram`] from the supplied entries.
    pub fn new<I>(entries: I) -> Result<Self, FilterProgramError>
    where
        I: IntoIterator<Item = FilterProgramEntry>,
    {
        let mut instructions = Vec::new();
        let mut dir_merge_rules = Vec::new();
        let mut current_segment = FilterSegment::default();

        for entry in entries {
            match entry {
                FilterProgramEntry::Rule(rule) => {
                    current_segment.push_rule(rule)?;
                }
                FilterProgramEntry::DirMerge(rule) => {
                    if !current_segment.is_empty() || instructions.is_empty() {
                        instructions.push(FilterInstruction::Segment(current_segment));
                        current_segment = FilterSegment::default();
                    }
                    let index = dir_merge_rules.len();
                    dir_merge_rules.push(rule);
                    instructions.push(FilterInstruction::DirMerge { index });
                }
            }
        }

        if !current_segment.is_empty() || instructions.is_empty() {
            instructions.push(FilterInstruction::Segment(current_segment));
        }

        Ok(Self {
            instructions,
            dir_merge_rules,
        })
    }

    /// Returns the per-directory merge directives referenced by the program.
    #[must_use]
    pub fn dir_merge_rules(&self) -> &[DirMergeRule] {
        &self.dir_merge_rules
    }

    /// Reports whether the program contains no rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.instructions
            .iter()
            .all(|instruction| matches!(instruction, FilterInstruction::Segment(segment) if segment.is_empty()))
    }

    /// Evaluates the program for the provided path.
    fn evaluate(
        &self,
        path: &Path,
        is_dir: bool,
        dir_merge_layers: &[Vec<FilterSegment>],
        ephemeral_layers: Option<&[(usize, FilterSegment)]>,
    ) -> FilterOutcome {
        let mut outcome = FilterOutcome::default();

        for instruction in &self.instructions {
            match instruction {
                FilterInstruction::Segment(segment) => segment.apply(path, is_dir, &mut outcome),
                FilterInstruction::DirMerge { index } => {
                    if let Some(layers) = dir_merge_layers.get(*index) {
                        for layer in layers {
                            layer.apply(path, is_dir, &mut outcome);
                        }
                    }
                    if let Some(ephemeral) = ephemeral_layers {
                        for (rule_index, segment) in ephemeral {
                            if *rule_index == *index {
                                segment.apply(path, is_dir, &mut outcome);
                            }
                        }
                    }
                }
            }
        }

        outcome
    }
}

/// Entry used to construct a [`FilterProgram`].
#[derive(Clone, Debug)]
pub enum FilterProgramEntry {
    /// Static include/exclude/protect rule.
    Rule(FilterRule),
    /// Per-directory merge directive.
    DirMerge(DirMergeRule),
}

/// Error produced when compiling filter patterns into matchers fails.
#[derive(Debug)]
pub struct FilterProgramError {
    pattern: String,
    source: globset::Error,
}

impl FilterProgramError {
    fn new(pattern: String, source: globset::Error) -> Self {
        Self { pattern, source }
    }

    /// Returns the pattern that failed to compile.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

impl fmt::Display for FilterProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to compile filter pattern '{}': {}",
            self.pattern, self.source
        )
    }
}

impl Error for FilterProgramError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// Description of a `.rsync-filter` style per-directory rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirMergeRule {
    pattern: PathBuf,
    options: DirMergeOptions,
}

impl DirMergeRule {
    /// Creates a new [`DirMergeRule`].
    #[must_use]
    pub fn new(pattern: impl Into<PathBuf>, options: DirMergeOptions) -> Self {
        Self {
            pattern: pattern.into(),
            options,
        }
    }

    /// Returns the configured filter file pattern.
    #[must_use]
    pub fn pattern(&self) -> &Path {
        self.pattern.as_path()
    }

    /// Returns the behavioural modifiers applied to this merge rule.
    #[must_use]
    pub const fn options(&self) -> &DirMergeOptions {
        &self.options
    }
}

#[derive(Clone, Debug)]
enum FilterInstruction {
    Segment(FilterSegment),
    DirMerge { index: usize },
}

/// Compiled list of rules evaluated sequentially.
#[derive(Clone, Debug, Default)]
struct FilterSegment {
    include_exclude: Vec<CompiledRule>,
    protect: Vec<CompiledRule>,
}

impl FilterSegment {
    fn push_rule(&mut self, rule: FilterRule) -> Result<(), FilterProgramError> {
        match rule.action() {
            rsync_filters::FilterAction::Include | rsync_filters::FilterAction::Exclude => {
                self.include_exclude.push(CompiledRule::new(
                    rule.action(),
                    rule.pattern().to_string(),
                )?);
            }
            rsync_filters::FilterAction::Protect => {
                self.protect.push(CompiledRule::new(
                    rule.action(),
                    rule.pattern().to_string(),
                )?);
            }
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.include_exclude.is_empty() && self.protect.is_empty()
    }

    fn apply(&self, path: &Path, is_dir: bool, outcome: &mut FilterOutcome) {
        for rule in &self.include_exclude {
            if rule.matches(path, is_dir) {
                outcome.set_transfer_allowed(matches!(
                    rule.action,
                    rsync_filters::FilterAction::Include
                ));
            }
        }

        for rule in &self.protect {
            if rule.matches(path, is_dir) {
                outcome.protect();
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct FilterOutcome {
    transfer_allowed: bool,
    protected: bool,
}

impl FilterOutcome {
    fn new() -> Self {
        Self {
            transfer_allowed: true,
            protected: false,
        }
    }

    fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    fn allows_deletion(self) -> bool {
        self.transfer_allowed && !self.protected
    }

    fn allows_deletion_when_excluded_removed(self) -> bool {
        !self.protected
    }

    fn set_transfer_allowed(&mut self, allowed: bool) {
        self.transfer_allowed = allowed;
    }

    fn protect(&mut self) {
        self.protected = true;
    }
}

impl Default for FilterOutcome {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
struct CompiledRule {
    action: rsync_filters::FilterAction,
    directory_only: bool,
    direct_matchers: Vec<GlobMatcher>,
    descendant_matchers: Vec<GlobMatcher>,
}

impl CompiledRule {
    fn new(
        action: rsync_filters::FilterAction,
        pattern: String,
    ) -> Result<Self, FilterProgramError> {
        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);

        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.clone());
        if !anchored {
            direct_patterns.insert(format!("**/{}", core_pattern));
        }

        let mut descendant_patterns = HashSet::new();
        if directory_only
            || matches!(
                action,
                rsync_filters::FilterAction::Exclude | rsync_filters::FilterAction::Protect
            )
        {
            descendant_patterns.insert(format!("{}/**", core_pattern));
            if !anchored {
                descendant_patterns.insert(format!("**/{}/**", core_pattern));
            }
        }

        Ok(Self {
            action,
            directory_only,
            direct_matchers: compile_patterns(direct_patterns, &pattern)?,
            descendant_matchers: compile_patterns(descendant_patterns, &pattern)?,
        })
    }

    fn matches(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) && (!self.directory_only || is_dir) {
                return true;
            }
        }

        for matcher in &self.descendant_matchers {
            if matcher.is_match(path) {
                return true;
            }
        }

        false
    }
}

fn compile_patterns(
    patterns: HashSet<String>,
    original: &str,
) -> Result<Vec<GlobMatcher>, FilterProgramError> {
    let mut unique: Vec<_> = patterns.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| FilterProgramError::new(original.to_string(), error))?;
        matchers.push(glob.compile_matcher());
    }

    Ok(matchers)
}

fn normalise_pattern(pattern: &str) -> (bool, bool, String) {
    let anchored = pattern.starts_with('/');
    let directory_only = pattern.ends_with('/');
    let mut core = pattern;
    if anchored {
        core = &core[1..];
    }
    if directory_only && !core.is_empty() {
        core = &core[..core.len() - 1];
    }
    (anchored, directory_only, core.to_string())
}

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
    pub fn execute(&self) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
    }

    /// Executes the planned copy using the requested execution mode.
    ///
    /// When [`LocalCopyExecution::DryRun`] is selected the filesystem is left
    /// untouched while operand validation and readability checks still occur.
    pub fn execute_with(
        &self,
        mode: LocalCopyExecution,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options(mode, LocalCopyOptions::default())
    }

    /// Executes the planned copy with additional behavioural options.
    pub fn execute_with_options(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        self.execute_with_options_and_handler(mode, options, None)
    }

    /// Executes the planned copy and returns a detailed report of performed actions.
    pub fn execute_with_report(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
    ) -> Result<LocalCopyReport, LocalCopyError> {
        self.execute_with_report_and_handler(mode, options, None)
    }

    /// Executes the planned copy while routing records to the supplied handler.
    pub fn execute_with_options_and_handler(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        handler: Option<&mut dyn LocalCopyRecordHandler>,
    ) -> Result<LocalCopySummary, LocalCopyError> {
        copy_sources(self, mode, options, handler).map(CopyOutcome::into_summary)
    }

    /// Executes the planned copy, returning a detailed report and notifying the handler.
    pub fn execute_with_report_and_handler(
        &self,
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        handler: Option<&mut dyn LocalCopyRecordHandler>,
    ) -> Result<LocalCopyReport, LocalCopyError> {
        copy_sources(self, mode, options, handler).map(|outcome| {
            let (_summary, report) = outcome.into_summary_and_report();
            report
        })
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
    /// A non-regular file was skipped because support was disabled.
    SkippedNonRegular,
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
    summary: LocalCopySummary,
    records: Vec<LocalCopyRecord>,
}

impl LocalCopyReport {
    fn new(summary: LocalCopySummary, records: Vec<LocalCopyRecord>) -> Self {
        Self { summary, records }
    }

    /// Returns the high-level summary collected during execution.
    #[must_use]
    pub const fn summary(&self) -> &LocalCopySummary {
        &self.summary
    }

    /// Consumes the report and returns the aggregated summary.
    #[must_use]
    pub fn into_summary(self) -> LocalCopySummary {
        self.summary
    }

    /// Returns the list of records captured during execution.
    #[must_use]
    pub fn records(&self) -> &[LocalCopyRecord] {
        &self.records
    }

    /// Consumes the report and returns the recorded events.
    #[must_use]
    pub fn into_records(self) -> Vec<LocalCopyRecord> {
        self.records
    }
}

/// Observer invoked for each [`LocalCopyRecord`] emitted during execution.
pub trait LocalCopyRecordHandler {
    /// Handles a newly produced [`LocalCopyRecord`].
    fn handle(&mut self, record: LocalCopyRecord);
}

impl<F> LocalCopyRecordHandler for F
where
    F: FnMut(LocalCopyRecord),
{
    fn handle(&mut self, record: LocalCopyRecord) {
        self(record);
    }
}

/// Options that influence how a [`LocalCopyPlan`] is executed.
#[derive(Clone, Debug)]
pub struct LocalCopyOptions {
    delete: bool,
    delete_excluded: bool,
    bandwidth_limit: Option<NonZeroU64>,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    filters: Option<FilterSet>,
    filter_program: Option<FilterProgram>,
    numeric_ids: bool,
    sparse: bool,
    checksum: bool,
    size_only: bool,
    partial: bool,
    inplace: bool,
    collect_events: bool,
    relative_paths: bool,
    devices: bool,
    specials: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptions`] value with defaults applied.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            delete: false,
            delete_excluded: false,
            bandwidth_limit: None,
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: false,
            preserve_times: false,
            filters: None,
            filter_program: None,
            numeric_ids: false,
            sparse: false,
            checksum: false,
            size_only: false,
            partial: false,
            inplace: false,
            collect_events: false,
            relative_paths: false,
            devices: false,
            specials: false,
            #[cfg(feature = "xattr")]
            preserve_xattrs: false,
        }
    }

    /// Requests that destination files absent from the source be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete = delete;
        self
    }

    /// Requests that excluded destination entries be removed during deletion sweeps.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(mut self, delete: bool) -> Self {
        self.delete_excluded = delete;
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

    /// Applies an ordered filter program that may include per-directory merges.
    #[must_use]
    pub fn with_filter_program(mut self, program: Option<FilterProgram>) -> Self {
        self.filter_program = program;
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

    /// Enables or disables copying of device nodes during the transfer.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn devices(mut self, enabled: bool) -> Self {
        self.devices = enabled;
        self
    }

    /// Enables or disables copying of special files such as FIFOs.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn specials(mut self, enabled: bool) -> Self {
        self.specials = enabled;
        self
    }

    /// Requests that source-relative path components be preserved in the destination.
    #[must_use]
    #[doc(alias = "--relative")]
    pub const fn relative_paths(mut self, relative: bool) -> Self {
        self.relative_paths = relative;
        self
    }

    /// Requests that partial transfers write into a temporary file that is preserved on failure.
    #[must_use]
    #[doc(alias = "--partial")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// Requests that destination updates be performed in place instead of via temporary files.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(mut self, inplace: bool) -> Self {
        self.inplace = inplace;
        self
    }

    /// Requests that extended attributes be preserved when copying entries.
    #[cfg(feature = "xattr")]
    #[must_use]
    #[doc(alias = "--xattrs")]
    #[doc(alias = "-X")]
    pub const fn xattrs(mut self, preserve: bool) -> Self {
        self.preserve_xattrs = preserve;
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

    /// Reports whether excluded paths should also be removed during deletion sweeps.
    #[must_use]
    pub const fn delete_excluded_enabled(&self) -> bool {
        self.delete_excluded
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

    /// Returns the configured filter program, if any.
    #[must_use]
    pub fn filter_program(&self) -> Option<&FilterProgram> {
        self.filter_program.as_ref()
    }

    /// Reports whether extended attribute preservation has been requested.
    #[cfg(feature = "xattr")]
    #[must_use]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
    }

    /// Reports whether numeric UID/GID preservation should be used.
    #[must_use]
    pub const fn numeric_ids_enabled(&self) -> bool {
        self.numeric_ids
    }

    /// Reports whether checksum-based change detection has been requested.
    #[must_use]
    pub const fn checksum_enabled(&self) -> bool {
        self.checksum
    }

    /// Reports whether size-only change detection has been requested.
    #[must_use]
    pub const fn size_only_enabled(&self) -> bool {
        self.size_only
    }

    /// Reports whether sparse handling has been requested.
    #[must_use]
    pub const fn sparse_enabled(&self) -> bool {
        self.sparse
    }

    /// Reports whether copying of device nodes has been requested.
    #[must_use]
    pub const fn devices_enabled(&self) -> bool {
        self.devices
    }

    /// Reports whether copying of special files has been requested.
    #[must_use]
    pub const fn specials_enabled(&self) -> bool {
        self.specials
    }

    /// Reports whether relative path preservation has been requested.
    #[must_use]
    pub const fn relative_paths_enabled(&self) -> bool {
        self.relative_paths
    }

    /// Reports whether partial transfer handling has been requested.
    #[must_use]
    pub const fn partial_enabled(&self) -> bool {
        self.partial
    }

    /// Reports whether in-place destination updates have been requested.
    #[must_use]
    pub const fn inplace_enabled(&self) -> bool {
        self.inplace
    }

    /// Reports whether the execution should record transfer events.
    #[must_use]
    pub const fn events_enabled(&self) -> bool {
        self.collect_events
    }
}

impl Default for LocalCopyOptions {
    fn default() -> Self {
        Self::new()
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Statistics describing the outcome of a [`LocalCopyPlan`] execution.
///
/// The summary mirrors the high-level counters printed by upstream rsync's
/// `--stats` output: file/metadata operations and the aggregate payload size
/// transferred. Counts increase even in dry-run mode to reflect the actions
/// that would have been taken.
pub struct LocalCopySummary {
    files_copied: u64,
    directories_created: u64,
    symlinks_copied: u64,
    hard_links_created: u64,
    devices_created: u64,
    fifos_created: u64,
    items_deleted: u64,
    bytes_copied: u64,
    total_source_bytes: u64,
    total_elapsed: Duration,
}

impl LocalCopySummary {
    /// Returns the number of regular files copied or updated.
    #[must_use]
    pub const fn files_copied(&self) -> u64 {
        self.files_copied
    }

    /// Returns the number of directories created during the transfer.
    #[must_use]
    pub const fn directories_created(&self) -> u64 {
        self.directories_created
    }

    /// Returns the number of symbolic links copied.
    #[must_use]
    pub const fn symlinks_copied(&self) -> u64 {
        self.symlinks_copied
    }

    /// Returns the number of hard links materialised.
    #[must_use]
    pub const fn hard_links_created(&self) -> u64 {
        self.hard_links_created
    }

    /// Returns the number of device nodes created.
    #[must_use]
    pub const fn devices_created(&self) -> u64 {
        self.devices_created
    }

    /// Returns the number of FIFOs created.
    #[must_use]
    pub const fn fifos_created(&self) -> u64 {
        self.fifos_created
    }

    /// Returns the number of entries removed because of `--delete`.
    #[must_use]
    pub const fn items_deleted(&self) -> u64 {
        self.items_deleted
    }

    /// Returns the aggregate number of bytes written for copied files.
    #[must_use]
    pub const fn bytes_copied(&self) -> u64 {
        self.bytes_copied
    }

    /// Returns the aggregate size of all source files considered during the transfer.
    #[must_use]
    pub const fn total_source_bytes(&self) -> u64 {
        self.total_source_bytes
    }

    /// Returns the total elapsed time spent copying file payloads.
    #[must_use]
    pub const fn total_elapsed(&self) -> Duration {
        self.total_elapsed
    }

    fn record_file(&mut self, bytes: u64) {
        self.files_copied = self.files_copied.saturating_add(1);
        self.bytes_copied = self.bytes_copied.saturating_add(bytes);
    }

    fn record_total_bytes(&mut self, bytes: u64) {
        self.total_source_bytes = self.total_source_bytes.saturating_add(bytes);
    }

    fn record_elapsed(&mut self, elapsed: Duration) {
        self.total_elapsed = self.total_elapsed.saturating_add(elapsed);
    }

    fn record_directory(&mut self) {
        self.directories_created = self.directories_created.saturating_add(1);
    }

    fn record_symlink(&mut self) {
        self.symlinks_copied = self.symlinks_copied.saturating_add(1);
    }

    fn record_hard_link(&mut self) {
        self.hard_links_created = self.hard_links_created.saturating_add(1);
    }

    fn record_device(&mut self) {
        self.devices_created = self.devices_created.saturating_add(1);
    }

    fn record_fifo(&mut self) {
        self.fifos_created = self.fifos_created.saturating_add(1);
    }

    fn record_deletion(&mut self) {
        self.items_deleted = self.items_deleted.saturating_add(1);
    }
}

struct CopyOutcome {
    summary: LocalCopySummary,
    events: Option<Vec<LocalCopyRecord>>,
}

impl CopyOutcome {
    fn into_summary(self) -> LocalCopySummary {
        self.summary
    }

    fn into_summary_and_report(self) -> (LocalCopySummary, LocalCopyReport) {
        let summary = self.summary;
        let records = self.events.unwrap_or_default();
        (summary, LocalCopyReport::new(summary, records))
    }
}

struct CopyContext<'a> {
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    hard_links: HardLinkTracker,
    limiter: Option<BandwidthLimiter>,
    summary: LocalCopySummary,
    events: Option<Vec<LocalCopyRecord>>,
    filter_program: Option<FilterProgram>,
    dir_merge_layers: Rc<RefCell<Vec<Vec<FilterSegment>>>>,
    observer: Option<&'a mut dyn LocalCopyRecordHandler>,
    dir_merge_ephemeral: Rc<RefCell<Vec<Vec<(usize, FilterSegment)>>>>,
}

impl<'a> CopyContext<'a> {
    fn new(
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        observer: Option<&'a mut dyn LocalCopyRecordHandler>,
    ) -> Self {
        let limiter = options.bandwidth_limit_bytes().map(BandwidthLimiter::new);
        let collect_events = options.events_enabled();
        let filter_program = options.filter_program().cloned();
        let dir_merge_layers = filter_program
            .as_ref()
            .map(|program| vec![Vec::new(); program.dir_merge_rules().len()])
            .unwrap_or_default();
        let dir_merge_ephemeral = Vec::new();
        Self {
            mode,
            options,
            hard_links: HardLinkTracker::new(),
            limiter,
            summary: LocalCopySummary::default(),
            events: if collect_events {
                Some(Vec::new())
            } else {
                None
            },
            filter_program,
            dir_merge_layers: Rc::new(RefCell::new(dir_merge_layers)),
            observer,
            dir_merge_ephemeral: Rc::new(RefCell::new(dir_merge_ephemeral)),
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

    fn devices_enabled(&self) -> bool {
        self.options.devices_enabled()
    }

    fn specials_enabled(&self) -> bool {
        self.options.specials_enabled()
    }

    fn relative_paths_enabled(&self) -> bool {
        self.options.relative_paths_enabled()
    }

    fn checksum_enabled(&self) -> bool {
        self.options.checksum_enabled()
    }

    fn size_only_enabled(&self) -> bool {
        self.options.size_only_enabled()
    }

    fn partial_enabled(&self) -> bool {
        self.options.partial_enabled()
    }

    fn inplace_enabled(&self) -> bool {
        self.options.inplace_enabled()
    }

    #[cfg(feature = "xattr")]
    fn xattrs_enabled(&self) -> bool {
        self.options.preserve_xattrs()
    }

    fn allows(&self, relative: &Path, is_dir: bool) -> bool {
        if let Some(program) = &self.filter_program {
            let layers = self.dir_merge_layers.borrow();
            let ephemeral = self.dir_merge_ephemeral.borrow();
            let temp_layers = ephemeral.last().map(|entries| entries.as_slice());
            program
                .evaluate(relative, is_dir, layers.as_slice(), temp_layers)
                .allows_transfer()
        } else if let Some(filters) = self.options.filter_set() {
            filters.allows(relative, is_dir)
        } else {
            true
        }
    }

    fn allows_deletion(&self, relative: &Path, is_dir: bool) -> bool {
        let delete_excluded = self.options.delete_excluded_enabled();
        if let Some(program) = &self.filter_program {
            let layers = self.dir_merge_layers.borrow();
            let ephemeral = self.dir_merge_ephemeral.borrow();
            let temp_layers = ephemeral.last().map(|entries| entries.as_slice());
            let outcome = program.evaluate(relative, is_dir, layers.as_slice(), temp_layers);
            if delete_excluded {
                outcome.allows_deletion_when_excluded_removed()
            } else {
                outcome.allows_deletion()
            }
        } else if let Some(filters) = self.options.filter_set() {
            if delete_excluded {
                filters.allows_deletion_when_excluded_removed(relative, is_dir)
            } else {
                filters.allows_deletion(relative, is_dir)
            }
        } else {
            true
        }
    }

    fn enter_directory(&self, source: &Path) -> Result<DirectoryFilterGuard, LocalCopyError> {
        let Some(program) = &self.filter_program else {
            return Ok(DirectoryFilterGuard::new(
                Rc::clone(&self.dir_merge_layers),
                Rc::clone(&self.dir_merge_ephemeral),
                Vec::new(),
                false,
            ));
        };

        let mut added_indices = Vec::new();
        let mut layers = self.dir_merge_layers.borrow_mut();
        let mut ephemeral_stack = self.dir_merge_ephemeral.borrow_mut();
        ephemeral_stack.push(Vec::new());

        for (index, rule) in program.dir_merge_rules().iter().enumerate() {
            let candidate = resolve_dir_merge_path(source, rule.pattern());

            let metadata = match fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    ephemeral_stack.pop();
                    return Err(LocalCopyError::io(
                        "inspect filter file",
                        candidate.clone(),
                        error,
                    ));
                }
            };

            if !metadata.is_file() {
                continue;
            }

            let mut visited = Vec::new();
            let rules = match load_dir_merge_rules_recursive(
                candidate.as_path(),
                rule.options(),
                &mut visited,
            ) {
                Ok(rules) => rules,
                Err(error) => {
                    ephemeral_stack.pop();
                    return Err(error);
                }
            };

            let mut segment = FilterSegment::default();
            for compiled in rules {
                if let Err(error) = segment.push_rule(compiled) {
                    ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, error));
                }
            }

            if rule.options().excludes_self() {
                let pattern = rule.pattern().to_string_lossy().into_owned();
                if let Err(error) = segment.push_rule(FilterRule::exclude(pattern)) {
                    ephemeral_stack.pop();
                    return Err(filter_program_local_error(&candidate, error));
                }
            }

            if segment.is_empty() {
                continue;
            }

            if rule.options().inherit_rules() {
                layers[index].push(segment);
                added_indices.push(index);
            } else if let Some(current) = ephemeral_stack.last_mut() {
                current.push((index, segment));
            }
        }

        drop(layers);
        drop(ephemeral_stack);

        Ok(DirectoryFilterGuard::new(
            Rc::clone(&self.dir_merge_layers),
            Rc::clone(&self.dir_merge_ephemeral),
            added_indices,
            true,
        ))
    }

    fn summary_mut(&mut self) -> &mut LocalCopySummary {
        &mut self.summary
    }

    fn record(&mut self, record: LocalCopyRecord) {
        if let Some(observer) = &mut self.observer {
            observer.handle(record.clone());
        }
        if let Some(events) = &mut self.events {
            events.push(record);
        }
    }

    fn record_skipped_non_regular(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedNonRegular,
                0,
                Duration::default(),
            ));
        }
    }

    fn into_outcome(self) -> CopyOutcome {
        CopyOutcome {
            summary: self.summary,
            events: self.events,
        }
    }
}

struct DirectoryFilterGuard {
    layers: Rc<RefCell<Vec<Vec<FilterSegment>>>>,
    ephemeral: Rc<RefCell<Vec<Vec<(usize, FilterSegment)>>>>,
    indices: Vec<usize>,
    ephemeral_active: bool,
}

impl DirectoryFilterGuard {
    fn new(
        layers: Rc<RefCell<Vec<Vec<FilterSegment>>>>,
        ephemeral: Rc<RefCell<Vec<Vec<(usize, FilterSegment)>>>>,
        indices: Vec<usize>,
        ephemeral_active: bool,
    ) -> Self {
        Self {
            layers,
            ephemeral,
            indices,
            ephemeral_active,
        }
    }
}

impl Drop for DirectoryFilterGuard {
    fn drop(&mut self) {
        if self.ephemeral_active {
            let mut stack = self.ephemeral.borrow_mut();
            stack.pop();
        }

        if self.indices.is_empty() {
            return;
        }

        let mut layers = self.layers.borrow_mut();
        for index in self.indices.drain(..).rev() {
            if let Some(layer) = layers.get_mut(index) {
                layer.pop();
            }
        }
    }
}

fn filter_program_local_error(path: &Path, error: FilterProgramError) -> LocalCopyError {
    LocalCopyError::io(
        "compile filter file",
        path.to_path_buf(),
        io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
    )
}

fn resolve_dir_merge_path(base: &Path, pattern: &Path) -> PathBuf {
    if pattern.is_absolute() {
        if let Ok(stripped) = pattern.strip_prefix(Path::new("/")) {
            return base.join(stripped);
        }
    }

    base.join(pattern)
}

fn load_dir_merge_rules_recursive(
    path: &Path,
    options: &DirMergeOptions,
    visited: &mut Vec<PathBuf>,
) -> Result<Vec<FilterRule>, LocalCopyError> {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if visited.contains(&canonical) {
        let message = format!("recursive filter merge detected for '{}'", path.display());
        return Err(LocalCopyError::io(
            "parse filter file",
            path.to_path_buf(),
            io::Error::new(io::ErrorKind::InvalidData, message),
        ));
    }

    visited.push(canonical);

    let file = fs::File::open(path)
        .map_err(|error| LocalCopyError::io("read filter file", path.to_path_buf(), error))?;
    let mut rules = Vec::new();

    let map_error = |error: FilterParseError| {
        LocalCopyError::io(
            "parse filter file",
            path.to_path_buf(),
            io::Error::new(io::ErrorKind::InvalidData, error.to_string()),
        )
    };

    let mut contents = String::new();
    io::BufReader::new(file)
        .read_to_string(&mut contents)
        .map_err(|error| LocalCopyError::io("read filter file", path.to_path_buf(), error))?;

    match options.parser() {
        DirMergeParser::Whitespace { enforce_kind } => {
            let enforce_kind = *enforce_kind;
            let mut iter = contents.split_whitespace();
            while let Some(token) = iter.next() {
                if token.is_empty() {
                    continue;
                }

                if token == "!" {
                    if options.list_clear_allowed() {
                        rules.clear();
                        continue;
                    }
                    return Err(map_error(FilterParseError::new(
                        "list-clearing '!' is not permitted in this filter file",
                    )));
                }

                if let Some(kind) = enforce_kind {
                    rules.push(match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(token.to_string()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(token.to_string()),
                    });
                    continue;
                }

                let mut directive = token.to_string();
                let lower = directive.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "merge" | "include" | "exclude" | "show" | "hide" | "protect"
                ) {
                    if let Some(next) = iter.next() {
                        directive.push(' ');
                        directive.push_str(next);
                    }
                } else if lower.starts_with("dir-merge") {
                    if let Some(next) = iter.next() {
                        directive.push(' ');
                        directive.push_str(next);
                    }
                }

                match parse_filter_directive_line(&directive) {
                    Ok(Some(ParsedFilterDirective::Rule(rule))) => rules.push(rule),
                    Ok(Some(ParsedFilterDirective::Merge(merge_path))) => {
                        let nested = if merge_path.is_absolute() {
                            merge_path
                        } else {
                            let parent = path.parent().unwrap_or_else(|| Path::new("."));
                            parent.join(merge_path)
                        };
                        let nested_rules =
                            load_dir_merge_rules_recursive(&nested, options, visited)?;
                        rules.extend(nested_rules);
                    }
                    Ok(None) => {}
                    Err(error) => return Err(map_error(error)),
                }
            }
        }
        DirMergeParser::Lines {
            enforce_kind,
            allow_comments,
        } => {
            let enforce_kind = *enforce_kind;
            let allow_comments = *allow_comments;
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if allow_comments && trimmed.starts_with('#') {
                    continue;
                }

                if trimmed == "!" {
                    if options.list_clear_allowed() {
                        rules.clear();
                        continue;
                    }
                    return Err(map_error(FilterParseError::new(
                        "list-clearing '!' is not permitted in this filter file",
                    )));
                }

                if let Some(kind) = enforce_kind {
                    rules.push(match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(trimmed.to_string()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(trimmed.to_string()),
                    });
                    continue;
                }

                match parse_filter_directive_line(trimmed) {
                    Ok(Some(ParsedFilterDirective::Rule(rule))) => rules.push(rule),
                    Ok(Some(ParsedFilterDirective::Merge(merge_path))) => {
                        let nested = if merge_path.is_absolute() {
                            merge_path
                        } else {
                            let parent = path.parent().unwrap_or_else(|| Path::new("."));
                            parent.join(merge_path)
                        };
                        let nested_rules =
                            load_dir_merge_rules_recursive(&nested, options, visited)?;
                        rules.extend(nested_rules);
                    }
                    Ok(None) => {}
                    Err(error) => return Err(map_error(error)),
                }
            }
        }
    }

    visited.pop();
    Ok(rules)
}

enum ParsedFilterDirective {
    Rule(FilterRule),
    Merge(PathBuf),
}

#[derive(Debug)]
struct FilterParseError {
    message: String,
}

impl FilterParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FilterParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for FilterParseError {}

fn parse_filter_directive_line(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    if text.is_empty() || text.starts_with('#') {
        return Ok(None);
    }

    if let Some(remainder) = text.strip_prefix('+') {
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '+' requires a pattern"));
        }
        return Ok(Some(ParsedFilterDirective::Rule(FilterRule::include(
            pattern.to_string(),
        ))));
    }

    if let Some(remainder) = text.strip_prefix('-') {
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '-' requires a pattern"));
        }
        return Ok(Some(ParsedFilterDirective::Rule(FilterRule::exclude(
            pattern.to_string(),
        ))));
    }

    let mut parts = text.splitn(2, char::is_whitespace);
    let keyword = parts.next().unwrap_or("");
    let remainder = parts.next().unwrap_or("").trim_start();

    let handle_keyword = |pattern: &str,
                          builder: fn(String) -> FilterRule|
     -> Result<Option<ParsedFilterDirective>, FilterParseError> {
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        Ok(Some(ParsedFilterDirective::Rule(builder(
            pattern.to_string(),
        ))))
    };

    if keyword.eq_ignore_ascii_case("include") {
        return handle_keyword(remainder, FilterRule::include);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return handle_keyword(remainder, FilterRule::exclude);
    }

    if keyword.eq_ignore_ascii_case("show") {
        return handle_keyword(remainder, FilterRule::include);
    }

    if keyword.eq_ignore_ascii_case("hide") {
        return handle_keyword(remainder, FilterRule::exclude);
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword(remainder, FilterRule::protect);
    }

    if keyword.eq_ignore_ascii_case("merge") {
        if remainder.is_empty() {
            return Err(FilterParseError::new(
                "merge directive requires a file path",
            ));
        }
        if remainder == "-" {
            return Err(FilterParseError::new(
                "merge from standard input is not supported in .rsync-filter files",
            ));
        }
        return Ok(Some(ParsedFilterDirective::Merge(PathBuf::from(remainder))));
    }

    Err(FilterParseError::new(format!(
        "unsupported filter directive '{}'",
        text
    )))
}

#[cfg(feature = "xattr")]
fn sync_xattrs_if_requested(
    preserve_xattrs: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), LocalCopyError> {
    if preserve_xattrs && !mode.is_dry_run() {
        sync_xattrs(source, destination, follow_symlinks).map_err(map_metadata_error)?;
    }
    Ok(())
}

const MICROS_PER_SECOND: u128 = 1_000_000;
const MICROS_PER_SECOND_DIV_1024: u128 = MICROS_PER_SECOND / 1024;
const MINIMUM_SLEEP_MICROS: u128 = MICROS_PER_SECOND / 10;

struct BandwidthLimiter {
    kib_per_second: NonZeroU64,
    write_max: usize,
    total_written: u128,
    last_instant: Option<Instant>,
    simulated_elapsed_us: u128,
}

impl BandwidthLimiter {
    fn new(limit: NonZeroU64) -> Self {
        let kib = limit
            .get()
            .checked_div(1024)
            .and_then(NonZeroU64::new)
            .expect("bandwidth limit must be at least 1024 bytes per second");
        let mut write_max = u128::from(kib.get()).saturating_mul(128);
        if write_max < 512 {
            write_max = 512;
        }
        let write_max = write_max.min(usize::MAX as u128) as usize;

        Self {
            kib_per_second: kib,
            write_max,
            total_written: 0,
            last_instant: None,
            simulated_elapsed_us: 0,
        }
    }

    fn recommended_read_size(&self, buffer_len: usize) -> usize {
        let limit = self.write_max.max(1);
        buffer_len.min(limit)
    }

    fn register(&mut self, bytes: usize) {
        if bytes == 0 {
            return;
        }

        self.total_written = self.total_written.saturating_add(bytes as u128);

        let start = Instant::now();

        let mut elapsed_us = self.simulated_elapsed_us;
        if let Some(previous) = self.last_instant {
            let elapsed = start.duration_since(previous);
            let measured = elapsed.as_micros().min(u128::from(u64::MAX));
            elapsed_us = elapsed_us.saturating_add(measured);
        }
        self.simulated_elapsed_us = 0;
        if elapsed_us > 0 {
            let allowed = elapsed_us.saturating_mul(u128::from(self.kib_per_second.get()))
                / MICROS_PER_SECOND_DIV_1024;
            if allowed >= self.total_written {
                self.total_written = 0;
            } else {
                self.total_written -= allowed;
            }
        }

        let sleep_us = self
            .total_written
            .saturating_mul(MICROS_PER_SECOND_DIV_1024)
            / u128::from(self.kib_per_second.get());

        if sleep_us < MINIMUM_SLEEP_MICROS {
            self.last_instant = Some(start);
            return;
        }

        let requested = duration_from_microseconds(sleep_us);
        if !requested.is_zero() {
            sleep_for(requested);
        }

        let end = Instant::now();
        let elapsed_us = end
            .checked_duration_since(start)
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)))
            .unwrap_or(0);
        if sleep_us > elapsed_us {
            self.simulated_elapsed_us = sleep_us - elapsed_us;
        }
        let remaining_us = sleep_us.saturating_sub(elapsed_us);
        let leftover = remaining_us.saturating_mul(u128::from(self.kib_per_second.get()))
            / MICROS_PER_SECOND_DIV_1024;

        self.total_written = leftover;
        self.last_instant = Some(end);
    }
}

fn duration_from_microseconds(us: u128) -> Duration {
    if us == 0 {
        return Duration::ZERO;
    }

    let seconds = us / MICROS_PER_SECOND;
    let micros = (us % MICROS_PER_SECOND) as u32;

    if seconds >= u128::from(u64::MAX) {
        Duration::MAX
    } else {
        Duration::new(seconds as u64, micros.saturating_mul(1_000))
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
    relative_prefix_components: Option<usize>,
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
            relative_prefix_components: detect_relative_prefix_components(operand.as_os_str()),
        })
    }

    fn relative_root(&self) -> Option<PathBuf> {
        use std::path::Component;

        let skip = self.relative_prefix_components.unwrap_or(0);
        let mut index = 0;
        let mut relative = PathBuf::new();

        for component in self.path.components() {
            if index < skip {
                index += 1;
                continue;
            }

            index += 1;

            match component {
                Component::CurDir | Component::RootDir => {}
                Component::Prefix(prefix) => {
                    relative.push(Path::new(prefix.as_os_str()));
                }
                Component::ParentDir => relative.push(Path::new("..")),
                Component::Normal(part) => relative.push(Path::new(part)),
            }
        }

        if relative.as_os_str().is_empty() {
            None
        } else {
            Some(relative)
        }
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

fn detect_relative_prefix_components(operand: &OsStr) -> Option<usize> {
    use std::path::{Component, Path};

    let path = Path::new(operand);

    #[cfg(unix)]
    if let Some(count) = detect_marker_components_unix(operand) {
        return Some(count);
    }

    #[cfg(windows)]
    if let Some(count) = detect_marker_components_windows(operand) {
        return Some(count);
    }

    let components: Vec<Component<'_>> = path.components().collect();

    if components.is_empty() {
        return None;
    }

    let mut skip = 0;

    if let Some(Component::Prefix(_)) = components.first() {
        if !path.has_root() {
            return None;
        }
        skip += 1;
    }

    if let Some(Component::RootDir) = components.get(skip) {
        skip += 1;
    }

    if skip > 0 { Some(skip) } else { None }
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

#[cfg(unix)]
fn detect_marker_components_unix(operand: &OsStr) -> Option<usize> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = operand.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut index = 0;
    let len = bytes.len();
    let mut component_count = 0;

    if bytes[0] == b'/' {
        component_count += 1;
        while index < len && bytes[index] == b'/' {
            index += 1;
        }
    }

    if index >= len {
        return None;
    }

    let mut start = index;
    let mut count = component_count;

    while index <= len {
        if index == len || bytes[index] == b'/' {
            if start != index {
                let component = &bytes[start..index];
                if component == b"." {
                    return Some(count);
                }
                count += 1;
            }

            while index < len && bytes[index] == b'/' {
                index += 1;
            }
            start = index;
            if index == len {
                break;
            }
        } else {
            index += 1;
        }
    }

    None
}

#[cfg(windows)]
fn detect_marker_components_windows(operand: &OsStr) -> Option<usize> {
    use std::os::windows::ffi::OsStrExt;

    fn is_separator(unit: u16) -> bool {
        unit == b'/' as u16 || unit == b'\\' as u16
    }

    fn is_single_dot(units: &[u16]) -> bool {
        units.len() == 1 && units[0] == b'.' as u16
    }

    let units: Vec<u16> = operand.encode_wide().collect();
    if units.is_empty() {
        return None;
    }

    let len = units.len();
    let mut index = 0;
    let mut count = 0;

    if len >= 2 && units[1] == b':' as u16 {
        count += 1;
        index = 2;
        if index < len && is_separator(units[index]) {
            count += 1;
            while index < len && is_separator(units[index]) {
                index += 1;
            }
        }
    } else if len >= 2 && is_separator(units[0]) && is_separator(units[1]) {
        count += 1;
        index = 2;
        while index < len && !is_separator(units[index]) {
            index += 1;
        }
        if index < len && is_separator(units[index]) {
            index += 1;
        }
        while index < len && !is_separator(units[index]) {
            index += 1;
        }
        if index < len && is_separator(units[index]) {
            count += 1;
            while index < len && is_separator(units[index]) {
                index += 1;
            }
        }
    } else if is_separator(units[0]) {
        count += 1;
        while index < len && is_separator(units[index]) {
            index += 1;
        }
    }

    if index >= len {
        return None;
    }

    let mut start = index;
    let mut components = count;

    while index <= len {
        if index == len || is_separator(units[index]) {
            if start != index {
                let component = &units[start..index];
                if is_single_dot(component) {
                    return Some(components);
                }
                components += 1;
            }

            while index < len && is_separator(units[index]) {
                index += 1;
            }
            start = index;
            if index == len {
                break;
            }
        } else {
            index += 1;
        }
    }

    None
}

fn non_empty_path(path: &Path) -> Option<&Path> {
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

fn copy_sources(
    plan: &LocalCopyPlan,
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    handler: Option<&mut dyn LocalCopyRecordHandler>,
) -> Result<CopyOutcome, LocalCopyError> {
    let mut context = CopyContext::new(mode, options, handler);

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

    let relative_enabled = context.relative_paths_enabled();

    for source in &plan.sources {
        let source_path = source.path();
        let metadata = fs::symlink_metadata(source_path).map_err(|error| {
            LocalCopyError::io("access source", source_path.to_path_buf(), error)
        })?;
        let file_type = metadata.file_type();
        let metadata_options = context.metadata_options();

        let relative_root = if relative_enabled {
            source.relative_root()
        } else {
            None
        };
        let relative_root = relative_root.filter(|path| !path.as_os_str().is_empty());
        let relative_parent = relative_root
            .as_ref()
            .and_then(|root| root.parent().map(|parent| parent.to_path_buf()))
            .filter(|parent| !parent.as_os_str().is_empty());

        let requires_directory_destination = relative_parent.is_some()
            || (relative_root.is_some() && (source.copy_contents() || file_type.is_dir()));

        if requires_directory_destination && !destination_behaves_like_directory {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::DestinationMustBeDirectory,
            ));
        }

        let destination_base = if let Some(parent) = &relative_parent {
            destination_path.join(parent)
        } else {
            destination_path.to_path_buf()
        };

        if file_type.is_dir() {
            if source.copy_contents() {
                if let Some(root) = relative_root.as_ref() {
                    if !context.allows(root.as_path(), true) {
                        continue;
                    }
                }

                let mut target_root = destination_path.to_path_buf();
                if let Some(root) = &relative_root {
                    target_root = destination_path.join(root);
                }

                copy_directory_recursive(
                    &mut context,
                    source_path,
                    &target_root,
                    &metadata,
                    relative_root
                        .as_ref()
                        .and_then(|root| non_empty_path(root.as_path())),
                )?;
                continue;
            }

            let name = source_path.file_name().ok_or_else(|| {
                LocalCopyError::invalid_argument(LocalCopyArgumentError::DirectoryNameUnavailable)
            })?;
            let relative = relative_root
                .clone()
                .unwrap_or_else(|| PathBuf::from(Path::new(name)));
            if !context.allows(&relative, true) {
                continue;
            }

            let target = if destination_behaves_like_directory || multiple_sources {
                destination_base.join(name)
            } else {
                destination_path.to_path_buf()
            };

            copy_directory_recursive(
                &mut context,
                source_path,
                &target,
                &metadata,
                non_empty_path(relative.as_path()),
            )?;
        } else {
            let name = source_path.file_name().ok_or_else(|| {
                LocalCopyError::invalid_argument(LocalCopyArgumentError::FileNameUnavailable)
            })?;
            let relative = relative_root
                .clone()
                .unwrap_or_else(|| PathBuf::from(Path::new(name)));
            if !context.allows(&relative, false) {
                continue;
            }

            let target = if destination_behaves_like_directory {
                destination_base.join(name)
            } else {
                destination_path.to_path_buf()
            };

            let record_path = non_empty_path(relative.as_path());
            if file_type.is_file() {
                copy_file(&mut context, source_path, &target, &metadata, record_path)?;
            } else if file_type.is_symlink() {
                copy_symlink(
                    &mut context,
                    source_path,
                    &target,
                    &metadata,
                    metadata_options,
                    record_path,
                )?;
            } else if is_fifo(&file_type) {
                if !context.specials_enabled() {
                    context.record_skipped_non_regular(record_path);
                    continue;
                }
                copy_fifo(
                    &mut context,
                    source_path,
                    &target,
                    &metadata,
                    metadata_options,
                    record_path,
                )?;
            } else if is_device(&file_type) {
                if !context.devices_enabled() {
                    context.record_skipped_non_regular(record_path);
                    continue;
                }
                copy_device(
                    &mut context,
                    source_path,
                    &target,
                    &metadata,
                    metadata_options,
                    record_path,
                )?;
            } else {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::UnsupportedFileType,
                ));
            }
        }
    }

    Ok(context.into_outcome())
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
    let mode = context.mode();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    let mut destination_missing = false;

    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            if !existing.file_type().is_dir() {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            destination_missing = true;
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

    let _dir_merge_guard = context.enter_directory(source)?;

    if destination_missing {
        if relative.is_some() {
            context.summary_mut().record_directory();
        }

        if !mode.is_dry_run() {
            fs::create_dir_all(destination).map_err(|error| {
                LocalCopyError::io("create directory", destination.to_path_buf(), error)
            })?;
        }

        if let Some(rel) = relative {
            context.record(LocalCopyRecord::new(
                rel.to_path_buf(),
                LocalCopyAction::DirectoryCreated,
                0,
                Duration::default(),
            ));
        }
    }

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
                context,
                entry_path,
                &target_path,
                entry_metadata,
                metadata_options,
                Some(entry_relative.as_path()),
            )?;
        } else if is_fifo(&entry_type) {
            if !context.specials_enabled() {
                context.record_skipped_non_regular(Some(entry_relative.as_path()));
                keep_names.pop();
                continue;
            }
            copy_fifo(
                context,
                entry_path,
                &target_path,
                entry_metadata,
                metadata_options,
                Some(entry_relative.as_path()),
            )?;
        } else if is_device(&entry_type) {
            if !context.devices_enabled() {
                context.record_skipped_non_regular(Some(entry_relative.as_path()));
                keep_names.pop();
                continue;
            }
            copy_device(
                context,
                entry_path,
                &target_path,
                entry_metadata,
                metadata_options,
                Some(entry_relative.as_path()),
            )?;
        } else {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::UnsupportedFileType,
            ));
        }
    }

    if context.options().delete_extraneous() {
        delete_extraneous_entries(context, destination, relative, &keep_names)?;
    }

    if !context.mode().is_dry_run() {
        let metadata_options = context.metadata_options();
        apply_directory_metadata_with_options(destination, metadata, metadata_options)
            .map_err(map_metadata_error)?;
        #[cfg(feature = "xattr")]
        sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
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
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
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
    context.summary_mut().record_total_bytes(file_size);
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

        context.summary_mut().record_file(file_size);
        context.record(LocalCopyRecord::new(
            record_path,
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
    let inplace_enabled = context.inplace_enabled();
    let checksum_enabled = context.checksum_enabled();
    let size_only_enabled = context.size_only_enabled();
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
        context.summary_mut().record_hard_link();
        context.record(LocalCopyRecord::new(
            record_path,
            LocalCopyAction::HardLink,
            0,
            Duration::default(),
        ));
        return Ok(());
    }

    if let Some(existing) = existing_metadata.as_ref() {
        if should_skip_copy(
            source,
            metadata,
            destination,
            existing,
            metadata_options,
            size_only_enabled,
            checksum_enabled,
        ) {
            apply_file_metadata_with_options(destination, metadata, metadata_options)
                .map_err(map_metadata_error)?;
            #[cfg(feature = "xattr")]
            sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
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
    let mut guard = None;

    let mut writer = if inplace_enabled {
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?
    } else {
        let (new_guard, file) = DestinationWriteGuard::new(destination, partial_enabled)?;
        guard = Some(new_guard);
        file
    };
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

    if let Some(guard) = guard {
        guard.commit()?;
    }

    apply_file_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
    hard_links.record(metadata, destination);
    let elapsed = start.elapsed();
    context.summary_mut().record_file(file_size);
    context.summary_mut().record_elapsed(elapsed);
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

fn temporary_destination_path(destination: &Path, unique: usize) -> PathBuf {
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "temp".to_string());
    let temp_name = format!(".oc-rsync-tmp-{file_name}-{}-{}", process::id(), unique);
    destination.with_file_name(temp_name)
}

struct DestinationWriteGuard {
    final_path: PathBuf,
    temp_path: PathBuf,
    preserve_on_error: bool,
    committed: bool,
}

impl DestinationWriteGuard {
    fn new(destination: &Path, partial: bool) -> Result<(Self, fs::File), LocalCopyError> {
        if partial {
            let temp_path = partial_destination_path(destination);
            if let Err(error) = fs::remove_file(&temp_path) {
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(LocalCopyError::io(
                        "remove existing partial file",
                        temp_path.clone(),
                        error,
                    ));
                }
            }
            let file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)
                .map_err(|error| LocalCopyError::io("copy file", temp_path.clone(), error))?;
            Ok((
                Self {
                    final_path: destination.to_path_buf(),
                    temp_path,
                    preserve_on_error: true,
                    committed: false,
                },
                file,
            ))
        } else {
            loop {
                let unique = NEXT_TEMP_FILE_ID.fetch_add(1, AtomicOrdering::Relaxed);
                let temp_path = temporary_destination_path(destination, unique);
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp_path)
                {
                    Ok(file) => {
                        return Ok((
                            Self {
                                final_path: destination.to_path_buf(),
                                temp_path,
                                preserve_on_error: false,
                                committed: false,
                            },
                            file,
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        continue;
                    }
                    Err(error) => {
                        return Err(LocalCopyError::io("copy file", temp_path.clone(), error));
                    }
                }
            }
        }
    }

    fn commit(mut self) -> Result<(), LocalCopyError> {
        match fs::rename(&self.temp_path, &self.final_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_file(&self.final_path).map_err(|remove_error| {
                    LocalCopyError::io(
                        "remove existing destination",
                        self.final_path.clone(),
                        remove_error,
                    )
                })?;
                fs::rename(&self.temp_path, &self.final_path).map_err(|rename_error| {
                    LocalCopyError::io(self.finalise_action(), self.temp_path.clone(), rename_error)
                })?;
            }
            Err(error) => {
                return Err(LocalCopyError::io(
                    self.finalise_action(),
                    self.temp_path.clone(),
                    error,
                ));
            }
        }
        self.committed = true;
        Ok(())
    }

    fn finalise_action(&self) -> &'static str {
        if self.preserve_on_error {
            "finalise partial file"
        } else {
            "finalise temporary file"
        }
    }
}

impl Drop for DestinationWriteGuard {
    fn drop(&mut self) {
        if !self.committed && !self.preserve_on_error {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
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
        let chunk_len = if let Some(limiter) = limiter.as_ref() {
            limiter.recommended_read_size(buffer.len())
        } else {
            buffer.len()
        };

        let read = reader
            .read(&mut buffer[..chunk_len])
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
    size_only: bool,
    checksum: bool,
) -> bool {
    if destination.len() != source.len() {
        return false;
    }

    if checksum {
        return files_checksum_match(source_path, destination_path).unwrap_or(false);
    }

    if size_only {
        return true;
    }

    if options.times() {
        match (source.modified(), destination.modified()) {
            (Ok(src), Ok(dst)) if system_time_eq(src, dst) => {}
            _ => return false,
        }
    } else {
        return false;
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

fn files_checksum_match(source: &Path, destination: &Path) -> io::Result<bool> {
    let mut source_file = fs::File::open(source)?;
    let mut destination_file = fs::File::open(destination)?;

    let mut source_hasher = Md5::new();
    let mut destination_hasher = Md5::new();

    let mut source_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0u8; COPY_BUFFER_SIZE];

    loop {
        let source_read = source_file.read(&mut source_buffer)?;
        let destination_read = destination_file.read(&mut destination_buffer)?;

        if source_read != destination_read {
            return Ok(false);
        }

        if source_read == 0 {
            break;
        }

        source_hasher.update(&source_buffer[..source_read]);
        destination_hasher.update(&destination_buffer[..destination_read]);
    }

    Ok(source_hasher.finalize().as_ref() == destination_hasher.finalize().as_ref())
}

fn copy_fifo(
    context: &mut CopyContext,
    _source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    let mode = context.mode();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
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

        context.summary_mut().record_fifo();
        if let Some(path) = record_path {
            context.record(LocalCopyRecord::new(
                path,
                LocalCopyAction::FifoCopied,
                0,
                Duration::default(),
            ));
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
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(preserve_xattrs, mode, _source, destination, true)?;
    context.summary_mut().record_fifo();
    if let Some(path) = record_path {
        context.record(LocalCopyRecord::new(
            path,
            LocalCopyAction::FifoCopied,
            0,
            Duration::default(),
        ));
    }
    Ok(())
}

fn copy_device(
    context: &mut CopyContext,
    _source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    let mode = context.mode();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
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

        context.summary_mut().record_device();
        if let Some(path) = record_path {
            context.record(LocalCopyRecord::new(
                path,
                LocalCopyAction::DeviceCopied,
                0,
                Duration::default(),
            ));
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
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(preserve_xattrs, mode, _source, destination, true)?;
    context.summary_mut().record_device();
    if let Some(path) = record_path {
        context.record(LocalCopyRecord::new(
            path,
            LocalCopyAction::DeviceCopied,
            0,
            Duration::default(),
        ));
    }
    Ok(())
}

fn delete_extraneous_entries(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[OsString],
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
        let entry_relative = match relative {
            Some(base) => base.join(&name_path),
            None => name_path.clone(),
        };

        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io("inspect extraneous destination entry", path.clone(), error)
        })?;

        if !context.allows_deletion(entry_relative.as_path(), file_type.is_dir()) {
            continue;
        }

        if context.mode().is_dry_run() {
            context.summary_mut().record_deletion();
            context.record(LocalCopyRecord::new(
                entry_relative,
                LocalCopyAction::EntryDeleted,
                0,
                Duration::default(),
            ));
            continue;
        }

        remove_extraneous_path(path, file_type)?;
        context.summary_mut().record_deletion();
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
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    let mode = context.mode();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
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
        context.summary_mut().record_symlink();
        if let Some(path) = record_path {
            context.record(LocalCopyRecord::new(
                path,
                LocalCopyAction::SymlinkCopied,
                0,
                Duration::default(),
            ));
        }
        return Ok(());
    }

    create_symlink(&target, source, destination).map_err(|error| {
        LocalCopyError::io("create symbolic link", destination.to_path_buf(), error)
    })?;

    apply_symlink_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, false)?;

    context.summary_mut().record_symlink();
    if let Some(path) = record_path {
        context.record(LocalCopyRecord::new(
            path,
            LocalCopyAction::SymlinkCopied,
            0,
            Duration::default(),
        ));
    }
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
    use std::ffi::OsString;
    use std::io::{Seek, SeekFrom, Write};
    use std::num::NonZeroU64;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::tempdir;

    #[cfg(feature = "xattr")]
    use xattr;

    #[test]
    fn local_copy_options_numeric_ids_round_trip() {
        let options = LocalCopyOptions::default().numeric_ids(true);
        assert!(options.numeric_ids_enabled());
        assert!(!LocalCopyOptions::default().numeric_ids_enabled());
    }

    #[test]
    fn local_copy_options_delete_excluded_round_trip() {
        let options = LocalCopyOptions::default().delete_excluded(true);
        assert!(options.delete_excluded_enabled());
        assert!(!LocalCopyOptions::default().delete_excluded_enabled());
    }

    #[test]
    fn local_copy_options_checksum_round_trip() {
        let options = LocalCopyOptions::default().checksum(true);
        assert!(options.checksum_enabled());
        assert!(!LocalCopyOptions::default().checksum_enabled());
    }

    #[test]
    fn metadata_options_reflect_numeric_ids_setting() {
        let options = LocalCopyOptions::default().numeric_ids(true);
        let context = CopyContext::new(LocalCopyExecution::Apply, options, None);
        assert!(context.metadata_options().numeric_ids_enabled());
    }

    #[test]
    fn local_copy_options_sparse_round_trip() {
        let options = LocalCopyOptions::default().sparse(true);
        assert!(options.sparse_enabled());
        assert!(!LocalCopyOptions::default().sparse_enabled());
    }

    #[test]
    fn local_copy_options_size_only_round_trip() {
        let options = LocalCopyOptions::default().size_only(true);
        assert!(options.size_only_enabled());
        assert!(!LocalCopyOptions::default().size_only_enabled());
    }

    #[test]
    fn local_copy_options_devices_round_trip() {
        let options = LocalCopyOptions::default().devices(true);
        assert!(options.devices_enabled());
        assert!(!LocalCopyOptions::default().devices_enabled());
    }

    #[test]
    fn local_copy_options_specials_round_trip() {
        let options = LocalCopyOptions::default().specials(true);
        assert!(options.specials_enabled());
        assert!(!LocalCopyOptions::default().specials_enabled());
    }

    #[test]
    fn local_copy_options_relative_round_trip() {
        let options = LocalCopyOptions::default().relative_paths(true);
        assert!(options.relative_paths_enabled());
        assert!(!LocalCopyOptions::default().relative_paths_enabled());
    }

    #[test]
    fn relative_root_drops_absolute_prefix_without_marker() {
        let operand = OsString::from("/var/log/messages");
        let spec = SourceSpec::from_operand(&operand).expect("source spec");
        let expected = Path::new("var").join("log").join("messages");
        assert_eq!(spec.relative_root(), Some(expected));
    }

    #[test]
    fn relative_root_respects_marker_boundary() {
        let operand = OsString::from("/srv/./data/file.txt");
        let spec = SourceSpec::from_operand(&operand).expect("source spec");
        assert_eq!(
            spec.relative_root(),
            Some(Path::new("data/file.txt").to_path_buf())
        );
    }

    #[test]
    fn relative_root_keeps_relative_paths_without_marker() {
        let operand = OsString::from("nested/dir/file.txt");
        let spec = SourceSpec::from_operand(&operand).expect("source spec");
        assert_eq!(
            spec.relative_root(),
            Some(Path::new("nested/dir/file.txt").to_path_buf())
        );
    }

    #[test]
    fn relative_root_counts_parent_components_before_marker() {
        let operand = OsString::from("dir/.././trimmed/file.txt");
        let spec = SourceSpec::from_operand(&operand).expect("source spec");
        assert_eq!(
            spec.relative_root(),
            Some(Path::new("trimmed/file.txt").to_path_buf())
        );
    }

    #[cfg(windows)]
    #[test]
    fn relative_root_handles_windows_drive_prefix() {
        let operand = OsString::from(r"C:\\path\\.\\to\\file.txt");
        let spec = SourceSpec::from_operand(&operand).expect("source spec");
        assert_eq!(
            spec.relative_root(),
            Some(Path::new("to/file.txt").to_path_buf())
        );
    }

    #[cfg(feature = "xattr")]
    #[test]
    fn local_copy_options_xattrs_round_trip() {
        let options = LocalCopyOptions::default().xattrs(true);
        assert!(options.preserve_xattrs());
        assert!(
            !LocalCopyOptions::default()
                .xattrs(true)
                .xattrs(false)
                .preserve_xattrs()
        );
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

        let summary = plan.execute().expect("copy succeeds");

        let copied = dest_dir.join(source.file_name().expect("source name"));
        assert_eq!(fs::read(copied).expect("read copied"), b"payload");
        assert_eq!(summary.files_copied(), 1);
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

        let summary = plan.execute().expect("copy succeeds");

        assert_eq!(fs::read(destination).expect("read dest"), b"example");
        assert_eq!(summary.files_copied(), 1);
    }

    #[test]
    fn execute_with_relative_preserves_parent_directories() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let destination_root = temp.path().join("dest");
        fs::create_dir_all(source_root.join("foo/bar")).expect("create source tree");
        fs::create_dir_all(&destination_root).expect("create destination root");
        let source_file = source_root.join("foo").join("bar").join("nested.txt");
        fs::write(&source_file, b"relative").expect("write source");

        let operand = source_root
            .join(".")
            .join("foo")
            .join("bar")
            .join("nested.txt");

        let operands = vec![
            operand.into_os_string(),
            destination_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().relative_paths(true),
            )
            .expect("copy succeeds");

        let copied = destination_root.join("foo").join("bar").join("nested.txt");
        assert_eq!(fs::read(copied).expect("read copied"), b"relative");
        assert_eq!(summary.files_copied(), 1);
    }

    #[test]
    fn execute_with_relative_requires_directory_destination() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("src");
        fs::create_dir_all(source_root.join("dir")).expect("create source tree");
        let source_file = source_root.join("dir").join("file.txt");
        fs::write(&source_file, b"dir").expect("write source");

        let destination = temp.path().join("dest.txt");
        fs::write(&destination, b"target").expect("write destination");

        let operand = source_root.join(".").join("dir").join("file.txt");

        let operands = vec![
            operand.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let result = plan.execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().relative_paths(true),
        );

        let error = result.expect_err("relative paths require directory destination");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(LocalCopyArgumentError::DestinationMustBeDirectory)
        ));
        assert_eq!(fs::read(&destination).expect("read destination"), b"target");
    }

    #[cfg(feature = "xattr")]
    #[test]
    fn execute_copies_file_with_xattrs() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"attr").expect("write source");
        xattr::set(&source, "user.demo", b"value").expect("set xattr");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().xattrs(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);
        let copied = xattr::get(&destination, "user.demo")
            .expect("read dest xattr")
            .expect("xattr present");
        assert_eq!(copied, b"value");
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

        let summary = plan.execute().expect("copy succeeds");
        assert_eq!(
            fs::read(dest_root.join("nested").join("file.txt")).expect("read"),
            b"tree"
        );
        assert_eq!(summary.files_copied(), 1);
        assert!(summary.directories_created() >= 1);
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

        let summary = plan
            .execute_with_options(
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
        assert_eq!(summary.files_copied(), 0);
    }

    #[test]
    fn execute_without_times_rewrites_when_checksum_disabled() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"content").expect("write source");
        fs::write(&destination, b"content").expect("write destination");

        let original_mtime = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_mtime(&destination, original_mtime).expect("set mtime");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);
        let metadata = fs::metadata(&destination).expect("dest metadata");
        let new_mtime = FileTime::from_last_modification_time(&metadata);
        assert_ne!(new_mtime, original_mtime);
    }

    #[test]
    fn execute_without_times_skips_with_checksum() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"content").expect("write source");
        fs::write(&destination, b"content").expect("write destination");

        let preserved_mtime = FileTime::from_unix_time(1_700_100_000, 0);
        set_file_mtime(&destination, preserved_mtime).expect("set mtime");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().checksum(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 0);
        let metadata = fs::metadata(&destination).expect("dest metadata");
        let final_mtime = FileTime::from_last_modification_time(&metadata);
        assert_eq!(final_mtime, preserved_mtime);
    }

    #[test]
    fn execute_with_size_only_skips_same_size_different_content() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let target_root = temp.path().join("target");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&target_root).expect("create target root");

        let source_path = source_root.join("file.txt");
        let dest_path = target_root.join("file.txt");
        fs::write(&source_path, b"abc").expect("write source");
        fs::write(&dest_path, b"xyz").expect("write destination");

        let operands = vec![
            source_path.clone().into_os_string(),
            dest_path.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().size_only(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.bytes_copied(), 0);
        assert_eq!(fs::read(dest_path).expect("read destination"), b"xyz");
    }

    #[test]
    fn execute_with_report_dry_run_records_file_event() {
        use std::fs;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        fs::write(&source, b"dry-run").expect("write source");
        let destination = temp.path().join("dest.txt");

        let operands = vec![
            source.clone().into_os_string(),
            destination.into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().collect_events(true);
        let report = plan
            .execute_with_report(LocalCopyExecution::DryRun, options)
            .expect("dry run succeeds");

        let records = report.records();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.action(), &LocalCopyAction::DataCopied);
        assert_eq!(record.relative_path(), Path::new("source.txt"));
        assert_eq!(record.bytes_transferred(), 7);
    }

    #[test]
    fn execute_with_report_dry_run_records_directory_event() {
        use std::fs;

        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("tree");
        fs::create_dir(&source_dir).expect("create source dir");
        fs::write(source_dir.join("file.txt"), b"data").expect("write nested file");
        let destination = temp.path().join("target");

        let operands = vec![
            source_dir.clone().into_os_string(),
            destination.into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().collect_events(true);
        let report = plan
            .execute_with_report(LocalCopyExecution::DryRun, options)
            .expect("dry run succeeds");

        let records = report.records();
        assert!(records.iter().any(|record| {
            record.action() == &LocalCopyAction::DirectoryCreated
                && record.relative_path() == Path::new("tree")
        }));
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

        let summary = plan.execute().expect("copy succeeds");
        let copied = fs::read_link(dest_link).expect("read copied link");
        assert_eq!(copied, target);
        assert_eq!(summary.symlinks_copied(), 1);
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
        let summary = plan.execute().expect("copy succeeds");

        let metadata = fs::metadata(&destination).expect("dest metadata");
        assert_ne!(metadata.permissions().mode() & 0o777, 0o640);
        let dest_atime = FileTime::from_last_access_time(&metadata);
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_ne!(dest_atime, atime);
        assert_ne!(dest_mtime, mtime);
        assert_eq!(summary.files_copied(), 1);
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
        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
        let dest_atime = FileTime::from_last_access_time(&metadata);
        let dest_mtime = FileTime::from_last_modification_time(&metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
        assert_eq!(summary.files_copied(), 1);
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

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().owner(true).group(true),
            )
            .expect("copy succeeds");

        let metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(metadata.uid(), owner);
        assert_eq!(metadata.gid(), group);
        assert_eq!(summary.files_copied(), 1);
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

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .permissions(true)
                    .times(true)
                    .specials(true),
            )
            .expect("fifo copy succeeds");

        let dest_metadata = fs::symlink_metadata(&destination_fifo).expect("dest metadata");
        assert!(dest_metadata.file_type().is_fifo());
        assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o640);
        let dest_atime = FileTime::from_last_access_time(&dest_metadata);
        let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);
        assert_eq!(summary.fifos_created(), 1);
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

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .permissions(true)
                    .times(true)
                    .specials(true),
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
        assert_eq!(summary.fifos_created(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn execute_without_specials_skips_fifo() {
        use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};

        let temp = tempdir().expect("tempdir");
        let source_fifo = temp.path().join("source.pipe");
        mknodat(
            CWD,
            &source_fifo,
            FileType::Fifo,
            Mode::from_bits_truncate(0o600),
            makedev(0, 0),
        )
        .expect("mkfifo");

        let destination_fifo = temp.path().join("dest.pipe");
        let operands = vec![
            source_fifo.into_os_string(),
            destination_fifo.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
            .expect("copy succeeds without specials");

        assert_eq!(summary.fifos_created(), 0);
        assert!(fs::symlink_metadata(&destination_fifo).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn execute_without_specials_records_skip_event() {
        use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};

        let temp = tempdir().expect("tempdir");
        let source_fifo = temp.path().join("skip.pipe");
        mknodat(
            CWD,
            &source_fifo,
            FileType::Fifo,
            Mode::from_bits_truncate(0o600),
            makedev(0, 0),
        )
        .expect("mkfifo");

        let destination = temp.path().join("dest.pipe");
        let operands = vec![
            source_fifo.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let report = plan
            .execute_with_report(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().collect_events(true),
            )
            .expect("copy executes");

        assert!(fs::symlink_metadata(&destination).is_err());
        assert!(report.records().iter().any(|record| {
            record.action() == &LocalCopyAction::SkippedNonRegular
                && record.relative_path() == Path::new("skip.pipe")
        }));
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

        let summary = plan.execute().expect("copy succeeds");
        assert!(dest_root.join("nested").exists());
        assert!(!dest_root.join("source").exists());
        assert!(summary.files_copied() >= 1);
    }

    #[test]
    fn execute_into_child_directory_succeeds_without_recursing() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let nested_dir = source_root.join("dir");
        fs::create_dir_all(&nested_dir).expect("create nested dir");
        fs::write(source_root.join("root.txt"), b"root").expect("write root");
        fs::write(nested_dir.join("child.txt"), b"child").expect("write nested");

        let dest_root = source_root.join("child");
        let operands = vec![
            source_root.clone().into_os_string(),
            dest_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan.execute().expect("copy into child succeeds");

        assert_eq!(
            fs::read(dest_root.join("root.txt")).expect("read root copy"),
            b"root"
        );
        assert_eq!(
            fs::read(dest_root.join("dir").join("child.txt")).expect("read nested copy"),
            b"child"
        );
        assert!(
            !dest_root.join("child").exists(),
            "destination recursion detected at {}",
            dest_root.join("child").display()
        );
        assert!(summary.files_copied() >= 2);
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

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert_eq!(
            fs::read(dest_root.join("keep.txt")).expect("read keep"),
            b"fresh"
        );
        assert!(!dest_root.join("extra.txt").exists());
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.items_deleted(), 1);
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

        let mut source_operand = source_root.into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());
        let operands = vec![source_operand, dest_root.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().delete(true);

        let summary = plan
            .execute_with_options(LocalCopyExecution::DryRun, options)
            .expect("dry-run succeeds");

        assert_eq!(
            fs::read(dest_root.join("keep.txt")).expect("read keep"),
            b"stale"
        );
        assert!(dest_root.join("extra.txt").exists());
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.items_deleted(), 1);
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

        let summary = plan.execute().expect("copy succeeds");

        let dest_a = dest_root.join("file-a");
        let dest_b = dest_root.join("file-b");
        let metadata_a = fs::metadata(&dest_a).expect("metadata a");
        let metadata_b = fs::metadata(&dest_b).expect("metadata b");

        assert_eq!(metadata_a.ino(), metadata_b.ino());
        assert_eq!(metadata_a.nlink(), 2);
        assert_eq!(metadata_b.nlink(), 2);
        assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
        assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
        assert!(summary.hard_links_created() >= 1);
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

    #[cfg(unix)]
    #[test]
    fn execute_without_inplace_replaces_destination_file() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        fs::write(&source, b"updated").expect("write source");

        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");
        let destination = dest_dir.join("target.txt");
        fs::write(&destination, b"original").expect("write destination");

        let original_inode = fs::metadata(&destination)
            .expect("destination metadata")
            .ino();

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan.execute().expect("copy succeeds");
        assert_eq!(summary.files_copied(), 1);

        let updated_metadata = fs::metadata(&destination).expect("destination metadata");
        assert_ne!(updated_metadata.ino(), original_inode);
        assert_eq!(
            fs::read(&destination).expect("read destination"),
            b"updated"
        );

        let mut entries = fs::read_dir(&dest_dir).expect("list dest dir");
        assert!(entries.all(|entry| {
            let name = entry.expect("dir entry").file_name();
            !name.to_string_lossy().starts_with(".oc-rsync-tmp-")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn execute_inplace_succeeds_with_read_only_directory() {
        use rustix::fs::{Mode, chmod};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        fs::write(&source, b"replacement").expect("write source");

        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");
        let destination = dest_dir.join("target.txt");
        fs::write(&destination, b"original").expect("write destination");
        fs::set_permissions(&destination, PermissionsExt::from_mode(0o644))
            .expect("make destination writable");

        let original_inode = fs::metadata(&destination)
            .expect("destination metadata")
            .ino();

        let readonly = Mode::from_bits_truncate(0o555);
        chmod(&dest_dir, readonly).expect("restrict directory permissions");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().inplace(true),
            )
            .expect("in-place copy succeeds");

        let contents = fs::read(&destination).expect("read destination");
        assert_eq!(contents, b"replacement");
        assert_eq!(summary.files_copied(), 1);

        let updated_inode = fs::metadata(&destination)
            .expect("destination metadata")
            .ino();
        assert_eq!(updated_inode, original_inode);

        let restore = Mode::from_bits_truncate(0o755);
        chmod(&dest_dir, restore).expect("restore directory permissions");
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
        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
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
        assert_eq!(summary.files_copied(), 1);
    }

    #[test]
    fn bandwidth_limiter_limits_chunk_size_for_slow_rates() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(1024).unwrap());
        assert_eq!(limiter.recommended_read_size(COPY_BUFFER_SIZE), 512);
        assert_eq!(limiter.recommended_read_size(256), 256);
    }

    #[test]
    fn bandwidth_limiter_preserves_buffer_for_fast_rates() {
        let limiter = BandwidthLimiter::new(NonZeroU64::new(8 * 1024 * 1024).unwrap());
        assert_eq!(
            limiter.recommended_read_size(COPY_BUFFER_SIZE),
            COPY_BUFFER_SIZE
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
        let summary = plan.execute().expect("copy succeeds");

        assert_eq!(fs::read(destination).expect("read dest"), b"no limit");
        let recorded = super::take_recorded_sleeps();
        assert!(recorded.is_empty(), "unexpected sleep durations recorded");
        assert_eq!(summary.files_copied(), 1);
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

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert!(!target_root.join("skip.tmp").exists());
        assert!(summary.files_copied() >= 1);
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

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.tmp").exists());
        assert!(!target_root.join("skip.tmp").exists());
        assert!(summary.files_copied() >= 1);
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

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert!(!target_root.join("extra.txt").exists());
        let skip_path = target_root.join("skip.tmp");
        assert!(skip_path.exists());
        assert_eq!(fs::read(skip_path).expect("read skip"), b"dest skip");
        assert!(summary.files_copied() >= 1);
        assert_eq!(summary.items_deleted(), 1);
    }

    #[test]
    fn delete_excluded_removes_excluded_entries() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(&dest).expect("create dest");
        fs::write(source.join("keep.txt"), b"keep").expect("write keep");

        let target_root = dest.join("source");
        fs::create_dir_all(&target_root).expect("create target root");
        fs::write(target_root.join("skip.tmp"), b"dest skip").expect("write existing skip");

        let operands = vec![
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let filters = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
            .expect("compile filters");
        let options = LocalCopyOptions::default()
            .delete(true)
            .delete_excluded(true)
            .filters(Some(filters));

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert!(!target_root.join("skip.tmp").exists());
        assert_eq!(summary.items_deleted(), 1);
    }

    #[test]
    fn delete_respects_protect_filters() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(&dest).expect("create dest");

        let target_root = dest.join("source");
        fs::create_dir_all(&target_root).expect("create target root");
        fs::write(target_root.join("keep.txt"), b"keep").expect("write keep");

        let operands = vec![
            source.clone().into_os_string(),
            dest.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let filters = FilterSet::from_rules([rsync_filters::FilterRule::protect("keep.txt")])
            .expect("compile filters");
        let options = LocalCopyOptions::default()
            .delete(true)
            .filters(Some(filters));

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert_eq!(summary.items_deleted(), 0);
    }
}
