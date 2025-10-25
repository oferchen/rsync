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

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::num::{NonZeroU8, NonZeroU64};
use std::path::{Path, PathBuf};
use std::process;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant, SystemTime};

#[cfg(unix)]
use rustix::{
    fd::AsFd,
    fs::{FallocateFlags, fallocate},
    io::Errno,
};

use globset::{GlobBuilder, GlobMatcher};
use rsync_bandwidth::BandwidthLimiter;
use rsync_checksums::RollingChecksum;
use rsync_checksums::strong::Md5;
use rsync_compress::zlib::{CompressionLevel, CountingZlibEncoder};
use rsync_filters::{FilterRule, FilterSet};
#[cfg(feature = "acl")]
use rsync_meta::sync_acls;
#[cfg(feature = "xattr")]
use rsync_meta::sync_xattrs;
use rsync_meta::{
    ChmodModifiers, MetadataError, MetadataOptions, apply_directory_metadata_with_options,
    apply_file_metadata_with_options, apply_symlink_metadata_with_options, create_device_node,
    create_fifo,
};
use rsync_protocol::ProtocolVersion;

use crate::delta::{DeltaSignatureIndex, SignatureLayoutParams, calculate_signature_layout};
use crate::signature::{
    SignatureAlgorithm, SignatureBlock, SignatureError, generate_file_signature,
};

const COPY_BUFFER_SIZE: usize = 128 * 1024;
static NEXT_TEMP_FILE_ID: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
thread_local! {
    static HARD_LINK_OVERRIDE: RefCell<Option<Box<dyn Fn(&Path, &Path) -> io::Result<()> + 'static>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
fn with_hard_link_override<F, R>(override_fn: F, action: impl FnOnce() -> R) -> R
where
    F: Fn(&Path, &Path) -> io::Result<()> + 'static,
{
    struct ResetGuard;

    impl Drop for ResetGuard {
        fn drop(&mut self) {
            HARD_LINK_OVERRIDE.with(|cell| {
                cell.replace(None);
            });
        }
    }

    HARD_LINK_OVERRIDE.with(|cell| {
        cell.replace(Some(Box::new(override_fn)));
    });
    let guard = ResetGuard;
    let result = action();
    drop(guard);
    result
}

fn create_hard_link(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = HARD_LINK_OVERRIDE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|override_fn| override_fn(source, destination))
    }) {
        return result;
    }

    fs::hard_link(source, destination)
}

#[cfg(unix)]
const CROSS_DEVICE_ERROR_CODE: i32 = 18;

#[cfg(windows)]
const CROSS_DEVICE_ERROR_CODE: i32 = 17;

#[cfg(not(any(unix, windows)))]
const CROSS_DEVICE_ERROR_CODE: i32 = 18;

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
    sender_side: SideState,
    receiver_side: SideState,
    anchor_root: bool,
}

impl DirMergeOptions {
    /// Creates default merge options: inherited rules, line-based parsing,
    /// comment support, and permission for list-clearing directives.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inherit: true,
            exclude_self: false,
            parser: DirMergeParser::Lines {
                enforce_kind: None,
                allow_comments: true,
            },
            allow_list_clear: true,
            sender_side: SideState::Unspecified,
            receiver_side: SideState::Unspecified,
            anchor_root: false,
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

    /// Applies the sender-side modifier to rules loaded from the filter file.
    #[must_use]
    pub fn sender_modifier(mut self) -> Self {
        self.sender_side = SideState::Enabled;
        if matches!(self.receiver_side, SideState::Unspecified) {
            self.receiver_side = SideState::Disabled;
        }
        self
    }

    /// Applies the receiver-side modifier to rules loaded from the filter file.
    #[must_use]
    pub fn receiver_modifier(mut self) -> Self {
        self.receiver_side = SideState::Enabled;
        if matches!(self.sender_side, SideState::Unspecified) {
            self.sender_side = SideState::Disabled;
        }
        self
    }

    /// Overrides the sender/receiver applicability flags without inferring defaults.
    #[must_use]
    pub fn with_side_overrides(mut self, sender: Option<bool>, receiver: Option<bool>) -> Self {
        self.sender_side = match sender {
            Some(true) => SideState::Enabled,
            Some(false) => SideState::Disabled,
            None => SideState::Unspecified,
        };
        self.receiver_side = match receiver {
            Some(true) => SideState::Enabled,
            Some(false) => SideState::Disabled,
            None => SideState::Unspecified,
        };
        self
    }

    /// Requests that patterns within the filter file be anchored to the transfer root.
    #[must_use]
    pub const fn anchor_root(mut self, anchor: bool) -> Self {
        self.anchor_root = anchor;
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

    /// Reports whether loaded rules should apply to the sending side.
    #[must_use]
    pub const fn applies_to_sender(&self) -> bool {
        !matches!(self.sender_side, SideState::Disabled)
    }

    /// Optional override for the sender side when explicitly requested by modifiers.
    #[must_use]
    pub const fn sender_side_override(&self) -> Option<bool> {
        match self.sender_side {
            SideState::Unspecified => None,
            SideState::Enabled => Some(true),
            SideState::Disabled => Some(false),
        }
    }

    /// Reports whether loaded rules should apply to the receiving side.
    #[must_use]
    pub const fn applies_to_receiver(&self) -> bool {
        !matches!(self.receiver_side, SideState::Disabled)
    }

    /// Optional override for the receiver side when explicitly requested by modifiers.
    #[must_use]
    pub const fn receiver_side_override(&self) -> Option<bool> {
        match self.receiver_side {
            SideState::Unspecified => None,
            SideState::Enabled => Some(true),
            SideState::Disabled => Some(false),
        }
    }

    /// Reports whether patterns should be anchored to the transfer root.
    #[must_use]
    pub const fn anchor_root_enabled(&self) -> bool {
        self.anchor_root
    }
}

impl Default for DirMergeOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SideState {
    Unspecified,
    Enabled,
    Disabled,
}

/// Exit code returned when operand validation fails.
const INVALID_OPERAND_EXIT_CODE: i32 = 23;
/// Exit code returned when no transfer operands are supplied.
const MISSING_OPERANDS_EXIT_CODE: i32 = 1;
/// Exit code returned when the transfer exceeds the configured timeout.
const TIMEOUT_EXIT_CODE: i32 = 30;

/// Ordered list of filter rules and per-directory merge directives.
#[derive(Clone, Debug, Default)]
pub struct FilterProgram {
    instructions: Vec<FilterInstruction>,
    dir_merge_rules: Vec<DirMergeRule>,
    exclude_if_present_rules: Vec<ExcludeIfPresentRule>,
}

impl FilterProgram {
    /// Builds a [`FilterProgram`] from the supplied entries.
    pub fn new<I>(entries: I) -> Result<Self, FilterProgramError>
    where
        I: IntoIterator<Item = FilterProgramEntry>,
    {
        let mut instructions = Vec::new();
        let mut dir_merge_rules = Vec::new();
        let mut exclude_if_present_rules = Vec::new();
        let mut current_segment = FilterSegment::default();

        for entry in entries {
            match entry {
                FilterProgramEntry::Rule(rule) => {
                    current_segment.push_rule(rule)?;
                }
                FilterProgramEntry::Clear => {
                    current_segment = FilterSegment::default();
                    instructions.clear();
                    dir_merge_rules.clear();
                    exclude_if_present_rules.clear();
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
                FilterProgramEntry::ExcludeIfPresent(rule) => {
                    if !current_segment.is_empty() || instructions.is_empty() {
                        instructions.push(FilterInstruction::Segment(current_segment));
                        current_segment = FilterSegment::default();
                    }
                    let index = exclude_if_present_rules.len();
                    exclude_if_present_rules.push(rule);
                    instructions.push(FilterInstruction::ExcludeIfPresent { index });
                }
            }
        }

        if !current_segment.is_empty() || instructions.is_empty() {
            instructions.push(FilterInstruction::Segment(current_segment));
        }

        Ok(Self {
            instructions,
            dir_merge_rules,
            exclude_if_present_rules,
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
            .all(|instruction| match instruction {
                FilterInstruction::Segment(segment) => segment.is_empty(),
                FilterInstruction::DirMerge { .. } | FilterInstruction::ExcludeIfPresent { .. } => {
                    false
                }
            })
    }

    /// Evaluates the program for the provided path.
    fn evaluate(
        &self,
        path: &Path,
        is_dir: bool,
        dir_merge_layers: &[Vec<FilterSegment>],
        ephemeral_layers: Option<&[(usize, FilterSegment)]>,
        context: FilterContext,
    ) -> FilterOutcome {
        let mut outcome = FilterOutcome::default();

        for instruction in &self.instructions {
            match instruction {
                FilterInstruction::Segment(segment) => {
                    segment.apply(path, is_dir, &mut outcome, context)
                }
                FilterInstruction::DirMerge { index } => {
                    if let Some(layers) = dir_merge_layers.get(*index) {
                        for layer in layers {
                            layer.apply(path, is_dir, &mut outcome, context);
                        }
                    }
                    if let Some(ephemeral) = ephemeral_layers {
                        for (rule_index, segment) in ephemeral {
                            if *rule_index == *index {
                                segment.apply(path, is_dir, &mut outcome, context);
                            }
                        }
                    }
                }
                FilterInstruction::ExcludeIfPresent { .. } => {}
            }
        }

        outcome
    }

    fn should_exclude_directory(&self, directory: &Path) -> Result<bool, LocalCopyError> {
        for instruction in &self.instructions {
            if let FilterInstruction::ExcludeIfPresent { index } = instruction {
                let rule = &self.exclude_if_present_rules[*index];
                match rule.marker_exists(directory) {
                    Ok(true) => return Ok(true),
                    Ok(false) => continue,
                    Err(error) => {
                        let path = rule.marker_path(directory);
                        return Err(LocalCopyError::io(
                            "inspect exclude-if-present marker",
                            path,
                            error,
                        ));
                    }
                }
            }
        }

        Ok(false)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FilterContext {
    Transfer,
    Deletion,
}

/// Entry used to construct a [`FilterProgram`].
#[derive(Clone, Debug)]
pub enum FilterProgramEntry {
    /// Static include/exclude/protect rule.
    Rule(FilterRule),
    /// Clears any rules accumulated so far, mirroring the `!` directive.
    Clear,
    /// Per-directory merge directive.
    DirMerge(DirMergeRule),
    /// Exclude a directory when the marker file is present.
    ExcludeIfPresent(ExcludeIfPresentRule),
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

/// Excludes directories that contain a particular marker file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExcludeIfPresentRule {
    raw_pattern: String,
    pattern: PathBuf,
}

impl ExcludeIfPresentRule {
    /// Creates a new rule that checks for the provided marker file.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        let raw_pattern = pattern.into();
        let pattern = PathBuf::from(&raw_pattern);
        Self {
            raw_pattern,
            pattern,
        }
    }

    fn marker_path(&self, directory: &Path) -> PathBuf {
        if self.pattern.is_absolute() {
            self.pattern.clone()
        } else {
            directory.join(&self.pattern)
        }
    }

    fn marker_exists(&self, directory: &Path) -> io::Result<bool> {
        let target = self.marker_path(directory);
        match fs::symlink_metadata(&target) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[derive(Clone, Debug)]
enum FilterInstruction {
    Segment(FilterSegment),
    DirMerge { index: usize },
    ExcludeIfPresent { index: usize },
}

/// Compiled list of rules evaluated sequentially.
#[derive(Clone, Debug, Default)]
struct FilterSegment {
    include_exclude: Vec<CompiledRule>,
    protect_risk: Vec<CompiledRule>,
}

impl FilterSegment {
    fn push_rule(&mut self, rule: FilterRule) -> Result<(), FilterProgramError> {
        match rule.action() {
            rsync_filters::FilterAction::Include | rsync_filters::FilterAction::Exclude => {
                self.include_exclude.push(CompiledRule::new(rule)?);
            }
            rsync_filters::FilterAction::Protect | rsync_filters::FilterAction::Risk => {
                self.protect_risk.push(CompiledRule::new(rule)?);
            }
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.include_exclude.is_empty() && self.protect_risk.is_empty()
    }

    fn apply(
        &self,
        path: &Path,
        is_dir: bool,
        outcome: &mut FilterOutcome,
        context: FilterContext,
    ) {
        for rule in &self.include_exclude {
            if rule.matches(path, is_dir) {
                match context {
                    FilterContext::Transfer => {
                        if rule.applies_to_sender {
                            outcome.set_transfer_allowed(matches!(
                                rule.action,
                                rsync_filters::FilterAction::Include
                            ));
                        }
                    }
                    FilterContext::Deletion => {
                        if rule.applies_to_receiver {
                            outcome.set_transfer_allowed(matches!(
                                rule.action,
                                rsync_filters::FilterAction::Include
                            ));
                        }
                    }
                }
            }
        }

        for rule in &self.protect_risk {
            if rule.matches(path, is_dir) {
                let applies = match context {
                    FilterContext::Transfer => rule.applies_to_sender,
                    FilterContext::Deletion => rule.applies_to_receiver,
                };
                if applies {
                    match rule.action {
                        rsync_filters::FilterAction::Protect => outcome.protect(),
                        rsync_filters::FilterAction::Risk => outcome.unprotect(),
                        rsync_filters::FilterAction::Include
                        | rsync_filters::FilterAction::Exclude => {}
                    }
                }
            }
        }
    }
}

type FilterSegmentLayers = Vec<Vec<FilterSegment>>;
type FilterSegmentStack = Vec<Vec<(usize, FilterSegment)>>;

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

    fn unprotect(&mut self) {
        self.protected = false;
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
    applies_to_sender: bool,
    applies_to_receiver: bool,
}

impl CompiledRule {
    fn new(rule: FilterRule) -> Result<Self, FilterProgramError> {
        let action = rule.action();
        let applies_to_sender = rule.applies_to_sender();
        let applies_to_receiver = rule.applies_to_receiver();
        let pattern = rule.pattern().to_string();
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
                rsync_filters::FilterAction::Exclude
                    | rsync_filters::FilterAction::Protect
                    | rsync_filters::FilterAction::Risk
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
            applies_to_sender,
            applies_to_receiver,
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
    /// An existing destination file was left untouched due to `--ignore-existing`.
    SkippedExisting,
    /// An existing destination file was newer than the source and left untouched.
    SkippedNewerDestination,
    /// A non-regular file was skipped because support was disabled.
    SkippedNonRegular,
    /// An entry was removed due to `--delete`.
    EntryDeleted,
    /// A source entry was removed after a successful transfer.
    SourceRemoved,
}

/// Record describing a single filesystem action performed during local copy execution.
#[derive(Clone, Debug)]
pub struct LocalCopyRecord {
    relative_path: PathBuf,
    action: LocalCopyAction,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
    metadata: Option<LocalCopyMetadata>,
    created: bool,
}

impl LocalCopyRecord {
    /// Creates a new [`LocalCopyRecord`].
    fn new(
        relative_path: PathBuf,
        action: LocalCopyAction,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        elapsed: Duration,
        metadata: Option<LocalCopyMetadata>,
    ) -> Self {
        Self {
            relative_path,
            action,
            bytes_transferred,
            total_bytes,
            elapsed,
            metadata,
            created: false,
        }
    }

    /// Marks whether the record corresponds to the creation of a new destination entry.
    #[must_use]
    fn with_creation(mut self, created: bool) -> Self {
        self.created = created;
        self
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

    /// Returns the total number of bytes expected for this record, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent performing the action.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Returns the metadata snapshot associated with this record, when available.
    #[must_use]
    pub fn metadata(&self) -> Option<&LocalCopyMetadata> {
        self.metadata.as_ref()
    }

    /// Returns whether the record corresponds to a newly created destination entry.
    #[must_use]
    pub const fn was_created(&self) -> bool {
        self.created
    }
}

/// File type captured for [`LocalCopyMetadata`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyFileKind {
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
    /// Unknown or platform specific entry.
    Other,
}

impl LocalCopyFileKind {
    fn from_file_type(file_type: &fs::FileType) -> Self {
        if file_type.is_dir() {
            return Self::Directory;
        }
        if file_type.is_symlink() {
            return Self::Symlink;
        }
        if file_type.is_file() {
            return Self::File;
        }
        if is_fifo(file_type) {
            return Self::Fifo;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;

            if file_type.is_char_device() {
                return Self::CharDevice;
            }
            if file_type.is_block_device() {
                return Self::BlockDevice;
            }
            if file_type.is_socket() {
                return Self::Socket;
            }
        }
        Self::Other
    }

    /// Returns whether the kind represents a directory.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

/// Metadata snapshot recorded for events emitted by [`LocalCopyRecord`].
#[derive(Clone, Debug)]
pub struct LocalCopyMetadata {
    kind: LocalCopyFileKind,
    len: u64,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

impl LocalCopyMetadata {
    fn from_metadata(metadata: &fs::Metadata, symlink_target: Option<PathBuf>) -> Self {
        let file_type = metadata.file_type();
        let kind = LocalCopyFileKind::from_file_type(&file_type);
        let len = metadata.len();
        let modified = metadata.modified().ok();

        #[cfg(unix)]
        let (mode, uid, gid, nlink) = {
            use std::os::unix::fs::MetadataExt;
            (
                Some(metadata.mode()),
                Some(metadata.uid()),
                Some(metadata.gid()),
                Some(metadata.nlink()),
            )
        };

        #[cfg(not(unix))]
        let (mode, uid, gid, nlink) = (None, None, None, None);

        let target = if matches!(kind, LocalCopyFileKind::Symlink) {
            symlink_target
        } else {
            None
        };

        Self {
            kind,
            len,
            modified,
            mode,
            uid,
            gid,
            nlink,
            symlink_target: target,
        }
    }

    /// Returns the entry kind associated with the metadata.
    #[must_use]
    pub const fn kind(&self) -> LocalCopyFileKind {
        self.kind
    }

    /// Returns the entry length in bytes.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns whether the metadata describes an empty entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the recorded modification time, when available.
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

    /// Returns the hard link count when available.
    #[must_use]
    pub const fn nlink(&self) -> Option<u64> {
        self.nlink
    }

    /// Returns the recorded symbolic link target when the metadata describes a symlink.
    #[must_use]
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }
}

/// Snapshot describing in-flight progress for a transfer action.
#[derive(Clone, Copy, Debug)]
pub struct LocalCopyProgress<'a> {
    relative_path: &'a Path,
    bytes_transferred: u64,
    total_bytes: Option<u64>,
    elapsed: Duration,
}

impl<'a> LocalCopyProgress<'a> {
    /// Creates a new [`LocalCopyProgress`] snapshot.
    #[must_use]
    pub const fn new(
        relative_path: &'a Path,
        bytes_transferred: u64,
        total_bytes: Option<u64>,
        elapsed: Duration,
    ) -> Self {
        Self {
            relative_path,
            bytes_transferred,
            total_bytes,
            elapsed,
        }
    }

    /// Returns the path associated with the progress snapshot.
    #[must_use]
    pub const fn relative_path(&self) -> &'a Path {
        self.relative_path
    }

    /// Returns the number of bytes transferred so far.
    #[must_use]
    pub const fn bytes_transferred(&self) -> u64 {
        self.bytes_transferred
    }

    /// Returns the total number of bytes expected for this action, when known.
    #[must_use]
    pub const fn total_bytes(&self) -> Option<u64> {
        self.total_bytes
    }

    /// Returns the elapsed time spent on this action.
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

    /// Handles an in-flight progress update for the current action.
    fn handle_progress(&mut self, _progress: LocalCopyProgress<'_>) {}
}

impl<F> LocalCopyRecordHandler for F
where
    F: FnMut(LocalCopyRecord),
{
    fn handle(&mut self, record: LocalCopyRecord) {
        self(record);
    }
}

/// Controls when deletion sweeps run relative to content transfers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteTiming {
    /// Remove extraneous entries before copying new content.
    Before,
    /// Remove extraneous entries as directories are processed.
    During,
    /// Record deletions during the walk and apply them after transfers finish.
    Delay,
    /// Remove extraneous entries after the full transfer completes.
    After,
}

impl DeleteTiming {
    const fn default() -> Self {
        Self::During
    }
}

/// Identifies how a reference directory should be treated when evaluating
/// `--compare-dest`, `--copy-dest`, and `--link-dest` semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceDirectoryKind {
    /// Skip creating the destination entry when the reference file matches.
    Compare,
    /// Copy the payload from the reference directory when the file matches.
    Copy,
    /// Create a hard link to the reference directory when the file matches.
    Link,
}

/// Reference directory consulted during copy execution.
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

    /// Returns the reference directory kind.
    #[must_use]
    pub const fn kind(&self) -> ReferenceDirectoryKind {
        self.kind
    }

    /// Returns the base directory path associated with the entry.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Options that influence how a [`LocalCopyPlan`] is executed.
#[derive(Clone, Debug)]
pub struct LocalCopyOptions {
    delete: bool,
    delete_timing: DeleteTiming,
    delete_excluded: bool,
    remove_source_files: bool,
    preallocate: bool,
    bandwidth_limit: Option<NonZeroU64>,
    compress: bool,
    compression_level_override: Option<CompressionLevel>,
    compression_level: CompressionLevel,
    whole_file: bool,
    copy_links: bool,
    copy_dirlinks: bool,
    keep_dirlinks: bool,
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
    owner_override: Option<u32>,
    group_override: Option<u32>,
    omit_dir_times: bool,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
    filters: Option<FilterSet>,
    filter_program: Option<FilterProgram>,
    numeric_ids: bool,
    sparse: bool,
    checksum: bool,
    size_only: bool,
    ignore_existing: bool,
    update: bool,
    partial: bool,
    partial_dir: Option<PathBuf>,
    delay_updates: bool,
    inplace: bool,
    append: bool,
    append_verify: bool,
    collect_events: bool,
    relative_paths: bool,
    devices: bool,
    specials: bool,
    implied_dirs: bool,
    mkpath: bool,
    prune_empty_dirs: bool,
    timeout: Option<Duration>,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
    link_dests: Vec<LinkDestEntry>,
    reference_directories: Vec<ReferenceDirectory>,
    chmod: Option<ChmodModifiers>,
}

impl LocalCopyOptions {
    /// Creates a new [`LocalCopyOptions`] value with defaults applied.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            delete: false,
            delete_timing: DeleteTiming::default(),
            delete_excluded: false,
            remove_source_files: false,
            preallocate: false,
            bandwidth_limit: None,
            compress: false,
            compression_level_override: None,
            compression_level: CompressionLevel::Default,
            whole_file: true,
            copy_links: false,
            copy_dirlinks: false,
            keep_dirlinks: false,
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: false,
            preserve_times: false,
            owner_override: None,
            group_override: None,
            omit_dir_times: false,
            #[cfg(feature = "acl")]
            preserve_acls: false,
            filters: None,
            filter_program: None,
            numeric_ids: false,
            sparse: false,
            checksum: false,
            size_only: false,
            ignore_existing: false,
            update: false,
            partial: false,
            partial_dir: None,
            delay_updates: false,
            inplace: false,
            append: false,
            append_verify: false,
            collect_events: false,
            relative_paths: false,
            devices: false,
            specials: false,
            implied_dirs: true,
            mkpath: false,
            prune_empty_dirs: false,
            timeout: None,
            #[cfg(feature = "xattr")]
            preserve_xattrs: false,
            link_dests: Vec::new(),
            reference_directories: Vec::new(),
            chmod: None,
        }
    }

    /// Adds a directory that should be consulted when creating hard links for matching files.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn with_link_dest(mut self, path: PathBuf) -> Self {
        if !path.as_os_str().is_empty() {
            self.link_dests.push(LinkDestEntry::new(path));
        }
        self
    }

    /// Extends the link-destination list with additional directories.
    #[must_use]
    #[doc(alias = "--link-dest")]
    pub fn extend_link_dests<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        for path in paths.into_iter() {
            let path = path.into();
            if !path.as_os_str().is_empty() {
                self.link_dests.push(LinkDestEntry::new(path));
            }
        }
        self
    }

    /// Configures chmod modifiers that should be applied after metadata preservation.
    #[must_use]
    pub fn with_chmod(mut self, modifiers: Option<ChmodModifiers>) -> Self {
        self.chmod = modifiers;
        self
    }

    /// Returns the configured link-destination entries.
    #[must_use]
    pub(crate) fn link_dest_entries(&self) -> &[LinkDestEntry] {
        &self.link_dests
    }

    /// Requests that destination files absent from the source be removed.
    #[must_use]
    #[doc(alias = "--delete")]
    pub const fn delete(mut self, delete: bool) -> Self {
        self.delete = delete;
        if delete {
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Requests that extraneous destination files be removed after the transfer completes.
    #[must_use]
    #[doc(alias = "--delete-after")]
    pub const fn delete_after(mut self, delete_after: bool) -> Self {
        if delete_after {
            self.delete = true;
            self.delete_timing = DeleteTiming::After;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::After) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Queues deletions discovered during the walk and applies them after transfers finish.
    #[must_use]
    #[doc(alias = "--delete-delay")]
    pub const fn delete_delay(mut self, delete_delay: bool) -> Self {
        if delete_delay {
            self.delete = true;
            self.delete_timing = DeleteTiming::Delay;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Delay) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Requests that extraneous destination files be removed before the transfer begins.
    #[must_use]
    #[doc(alias = "--delete-before")]
    pub const fn delete_before(mut self, delete_before: bool) -> Self {
        if delete_before {
            self.delete = true;
            self.delete_timing = DeleteTiming::Before;
        } else if self.delete && matches!(self.delete_timing, DeleteTiming::Before) {
            self.delete = false;
            self.delete_timing = DeleteTiming::default();
        }
        self
    }

    /// Requests that extraneous destination files be removed while processing directories.
    #[must_use]
    #[doc(alias = "--delete-during")]
    pub const fn delete_during(mut self) -> Self {
        if self.delete {
            self.delete_timing = DeleteTiming::During;
        } else {
            self.delete = true;
            self.delete_timing = DeleteTiming::During;
        }
        self
    }

    /// Requests that excluded destination entries be removed during deletion sweeps.
    #[must_use]
    #[doc(alias = "--delete-excluded")]
    pub const fn delete_excluded(mut self, delete: bool) -> Self {
        self.delete_excluded = delete;
        self
    }

    /// Requests that source files be removed after successful transfer.
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

    /// Applies an optional bandwidth limit expressed in bytes per second.
    #[must_use]
    #[doc(alias = "--bwlimit")]
    pub const fn bandwidth_limit(mut self, limit: Option<NonZeroU64>) -> Self {
        self.bandwidth_limit = limit;
        self
    }

    /// Controls whether whole-file transfers are forced even when delta mode is requested.
    #[must_use]
    #[doc(alias = "--whole-file")]
    #[doc(alias = "--no-whole-file")]
    pub const fn whole_file(mut self, whole: bool) -> Self {
        self.whole_file = whole;
        self
    }

    /// Requests that symlinks be followed and copied as their referents.
    #[must_use]
    #[doc(alias = "--copy-links")]
    #[doc(alias = "-L")]
    pub const fn copy_links(mut self, copy: bool) -> Self {
        self.copy_links = copy;
        self
    }

    /// Treats symlinks to directories as directories when traversing the source tree.
    #[must_use]
    #[doc(alias = "--copy-dirlinks")]
    pub const fn copy_dirlinks(mut self, copy: bool) -> Self {
        self.copy_dirlinks = copy;
        self
    }

    /// Keeps existing destination symlinks that point to directories.
    #[must_use]
    #[doc(alias = "--keep-dirlinks")]
    pub const fn keep_dirlinks(mut self, keep: bool) -> Self {
        self.keep_dirlinks = keep;
        self
    }

    /// Applies an inactivity timeout to the transfer.
    #[must_use]
    #[doc(alias = "--timeout")]
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Enables or disables compression during payload processing.
    #[must_use]
    #[doc(alias = "--compress")]
    pub const fn compress(mut self, compress: bool) -> Self {
        self.compress = compress;
        if !compress {
            self.compression_level_override = None;
        }
        self
    }

    /// Applies an explicit compression level override for payload processing.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn with_compression_level_override(
        mut self,
        level: Option<CompressionLevel>,
    ) -> Self {
        self.compression_level_override = level;
        self
    }

    /// Sets the default compression level used when compression is enabled.
    #[must_use]
    pub const fn with_default_compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level = level;
        self
    }

    /// Applies an explicit compression level override supplied by the user.
    #[must_use]
    #[doc(alias = "--compress-level")]
    pub const fn with_compression_level(mut self, level: CompressionLevel) -> Self {
        self.compression_level_override = Some(level);
        self
    }

    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--owner")]
    pub const fn owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    /// Applies an explicit ownership override to transferred entries.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn with_owner_override(mut self, owner: Option<u32>) -> Self {
        self.owner_override = owner;
        self
    }

    /// Requests that the group be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--group")]
    pub const fn group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Applies an explicit group override to transferred entries.
    #[must_use]
    #[doc(alias = "--chown")]
    pub const fn with_group_override(mut self, group: Option<u32>) -> Self {
        self.group_override = group;
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

    /// Skips preserving directory modification times even when [`Self::times`] is enabled.
    #[must_use]
    #[doc(alias = "--omit-dir-times")]
    pub const fn omit_dir_times(mut self, omit: bool) -> Self {
        self.omit_dir_times = omit;
        self
    }

    #[cfg(feature = "acl")]
    /// Requests that POSIX ACLs be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--acls")]
    pub const fn acls(mut self, preserve: bool) -> Self {
        self.preserve_acls = preserve;
        self
    }

    /// Applies a precompiled filter set to the execution.
    #[must_use]
    pub fn with_filters(mut self, filters: Option<FilterSet>) -> Self {
        self.filters = filters;
        self
    }

    /// Applies a filter set using the legacy builder name for compatibility.
    #[must_use]
    pub fn filters(self, filters: Option<FilterSet>) -> Self {
        self.with_filters(filters)
    }

    /// Applies an external filter program configuration.
    #[must_use]
    pub fn with_filter_program(mut self, program: Option<FilterProgram>) -> Self {
        self.filter_program = program;
        self
    }

    /// Requests numeric UID/GID preservation.
    #[must_use]
    #[doc(alias = "--numeric-ids")]
    pub const fn numeric_ids(mut self, numeric: bool) -> Self {
        self.numeric_ids = numeric;
        self
    }

    /// Enables sparse file handling during copies.
    #[must_use]
    #[doc(alias = "--sparse")]
    pub const fn sparse(mut self, sparse: bool) -> Self {
        self.sparse = sparse;
        self
    }

    /// Enables checksum-based change detection.
    #[must_use]
    #[doc(alias = "--checksum")]
    pub const fn checksum(mut self, checksum: bool) -> Self {
        self.checksum = checksum;
        self
    }

    /// Enables size-only change detection.
    #[must_use]
    #[doc(alias = "--size-only")]
    pub const fn size_only(mut self, size_only: bool) -> Self {
        self.size_only = size_only;
        self
    }

    /// Requests that existing destination files be skipped.
    #[must_use]
    #[doc(alias = "--ignore-existing")]
    pub const fn ignore_existing(mut self, ignore: bool) -> Self {
        self.ignore_existing = ignore;
        self
    }

    /// Requests that newer destination files be preserved.
    #[must_use]
    #[doc(alias = "--update")]
    pub const fn update(mut self, update: bool) -> Self {
        self.update = update;
        self
    }

    /// Requests that partial transfers leave temporary files.
    #[must_use]
    #[doc(alias = "--partial")]
    pub const fn partial(mut self, partial: bool) -> Self {
        self.partial = partial;
        self
    }

    /// Requests that updated files be renamed into place after the transfer completes.
    #[must_use]
    #[doc(alias = "--delay-updates")]
    pub const fn delay_updates(mut self, delay: bool) -> Self {
        self.delay_updates = delay;
        if delay {
            self.partial = true;
        }
        self
    }

    /// Selects the directory used to retain partial files when transfers fail.
    #[must_use]
    #[doc(alias = "--partial-dir")]
    pub fn with_partial_directory<P: Into<PathBuf>>(mut self, directory: Option<P>) -> Self {
        self.partial_dir = directory.map(Into::into);
        if self.partial_dir.is_some() {
            self.partial = true;
        }
        self
    }

    /// Appends a reference directory consulted for `--compare-dest`,
    /// `--copy-dest`, and `--link-dest` handling.
    #[must_use]
    pub fn push_reference_directory(mut self, reference: ReferenceDirectory) -> Self {
        self.reference_directories.push(reference);
        self
    }

    /// Extends the reference directory list with the provided entries.
    #[must_use]
    pub fn extend_reference_directories<I>(mut self, references: I) -> Self
    where
        I: IntoIterator<Item = ReferenceDirectory>,
    {
        self.reference_directories.extend(references);
        self
    }

    /// Requests in-place destination updates.
    #[must_use]
    #[doc(alias = "--inplace")]
    pub const fn inplace(mut self, inplace: bool) -> Self {
        self.inplace = inplace;
        self
    }

    /// Enables appending to existing destination files when they are shorter than the source.
    #[must_use]
    #[doc(alias = "--append")]
    pub const fn append(mut self, append: bool) -> Self {
        self.append = append;
        if !append {
            self.append_verify = false;
        }
        self
    }

    /// Enables append-with-verification semantics.
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

    /// Requests that relative source paths be preserved in the destination.
    #[must_use]
    #[doc(alias = "--relative")]
    pub const fn relative_paths(mut self, relative: bool) -> Self {
        self.relative_paths = relative;
        self
    }

    /// Controls whether parent directories implied by the source path are created.
    #[must_use]
    #[doc(alias = "--implied-dirs")]
    #[doc(alias = "--no-implied-dirs")]
    pub const fn implied_dirs(mut self, implied: bool) -> Self {
        self.implied_dirs = implied;
        self
    }

    /// Requests creation of missing destination path components prior to copying.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath(mut self, mkpath: bool) -> Self {
        self.mkpath = mkpath;
        self
    }

    /// Prunes directories that would otherwise be empty after filtering.
    #[must_use]
    #[doc(alias = "--prune-empty-dirs")]
    #[doc(alias = "-m")]
    pub const fn prune_empty_dirs(mut self, prune: bool) -> Self {
        self.prune_empty_dirs = prune;
        self
    }

    /// Requests that device nodes be copied.
    #[must_use]
    #[doc(alias = "--devices")]
    pub const fn devices(mut self, devices: bool) -> Self {
        self.devices = devices;
        self
    }

    /// Requests that special files such as FIFOs be copied.
    #[must_use]
    #[doc(alias = "--specials")]
    pub const fn specials(mut self, specials: bool) -> Self {
        self.specials = specials;
        self
    }

    /// Enables collection of transfer events that describe work performed by the engine.
    #[must_use]
    pub const fn collect_events(mut self, collect: bool) -> Self {
        self.collect_events = collect;
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

    /// Reports whether extraneous destination files should be removed.
    #[must_use]
    pub const fn delete_extraneous(&self) -> bool {
        self.delete
    }

    /// Returns the configured deletion timing when deletion sweeps are enabled.
    #[must_use]
    pub const fn delete_timing(&self) -> Option<DeleteTiming> {
        if self.delete {
            Some(self.delete_timing)
        } else {
            None
        }
    }

    /// Reports whether deletions should occur before content transfers.
    #[must_use]
    pub const fn delete_before_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::Before) && self.delete
    }

    /// Reports whether deletions should occur after transfers instead of immediately.
    #[must_use]
    pub const fn delete_after_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::After) && self.delete
    }

    /// Reports whether deletions are deferred until after transfers but determined during the walk.
    #[must_use]
    pub const fn delete_delay_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::Delay) && self.delete
    }

    /// Reports whether deletions should occur while processing directory entries.
    #[must_use]
    pub const fn delete_during_enabled(&self) -> bool {
        matches!(self.delete_timing, DeleteTiming::During) && self.delete
    }

    /// Reports whether excluded paths should also be removed during deletion sweeps.
    #[must_use]
    pub const fn delete_excluded_enabled(&self) -> bool {
        self.delete_excluded
    }

    /// Reports whether source files should be removed after transfer.
    #[must_use]
    pub const fn remove_source_files_enabled(&self) -> bool {
        self.remove_source_files
    }

    /// Returns the configured bandwidth limit, if any, in bytes per second.
    #[must_use]
    pub const fn bandwidth_limit_bytes(&self) -> Option<NonZeroU64> {
        self.bandwidth_limit
    }

    /// Returns whether compression is enabled for payload handling.
    #[must_use]
    pub const fn compress_enabled(&self) -> bool {
        self.compress
    }

    /// Returns the configured compression level override, if any.
    #[must_use]
    pub const fn compression_level_override(&self) -> Option<CompressionLevel> {
        self.compression_level_override
    }

    /// Returns the compression level that should be used when compression is enabled.
    #[must_use]
    pub const fn compression_level(&self) -> CompressionLevel {
        match self.compression_level_override {
            Some(level) => level,
            None => self.compression_level,
        }
    }

    /// Reports whether whole-file transfers are requested.
    #[must_use]
    pub const fn whole_file_enabled(&self) -> bool {
        self.whole_file
    }

    /// Returns whether symlinks should be materialised as their referents.
    #[must_use]
    pub const fn copy_links_enabled(&self) -> bool {
        self.copy_links
    }

    /// Reports whether symlinks to directories should be followed as directories.
    #[must_use]
    pub const fn copy_dirlinks_enabled(&self) -> bool {
        self.copy_dirlinks
    }

    /// Reports whether existing destination directory symlinks should be preserved.
    #[must_use]
    pub const fn keep_dirlinks_enabled(&self) -> bool {
        self.keep_dirlinks
    }

    /// Returns the effective compression level when compression is enabled.
    #[must_use]
    pub const fn effective_compression_level(&self) -> Option<CompressionLevel> {
        if self.compress {
            Some(self.compression_level())
        } else {
            None
        }
    }

    /// Reports whether ownership preservation has been requested.
    #[must_use]
    pub const fn preserve_owner(&self) -> bool {
        self.preserve_owner
    }

    /// Returns the configured ownership override, if any.
    #[must_use]
    pub const fn owner_override(&self) -> Option<u32> {
        self.owner_override
    }

    /// Reports whether group preservation has been requested.
    #[must_use]
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
    pub fn chmod(&self) -> Option<&ChmodModifiers> {
        self.chmod.as_ref()
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

    /// Reports whether directory modification times should be skipped during metadata preservation.
    #[must_use]
    pub const fn omit_dir_times_enabled(&self) -> bool {
        self.omit_dir_times
    }

    #[cfg(feature = "acl")]
    /// Returns whether POSIX ACLs should be preserved.
    #[must_use]
    pub const fn preserve_acls(&self) -> bool {
        self.preserve_acls
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

    /// Reports whether numeric UID/GID preservation has been requested.
    #[must_use]
    pub const fn numeric_ids_enabled(&self) -> bool {
        self.numeric_ids
    }

    /// Returns whether destination files are preallocated before writing.
    #[must_use]
    pub const fn preallocate_enabled(&self) -> bool {
        self.preallocate
    }

    #[cfg(feature = "acl")]
    /// Reports whether ACL preservation is enabled.
    #[must_use]
    pub const fn acls_enabled(&self) -> bool {
        self.preserve_acls
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

    /// Reports whether existing destination files should be skipped.
    #[must_use]
    pub const fn ignore_existing_enabled(&self) -> bool {
        self.ignore_existing
    }

    /// Reports whether newer destination files should be preserved.
    #[must_use]
    pub const fn update_enabled(&self) -> bool {
        self.update
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

    /// Reports whether implied parent directories should be created automatically.
    #[must_use]
    pub const fn implied_dirs_enabled(&self) -> bool {
        self.implied_dirs
    }

    /// Reports whether `--mkpath` style directory creation is enabled.
    #[must_use]
    #[doc(alias = "--mkpath")]
    pub const fn mkpath_enabled(&self) -> bool {
        self.mkpath
    }

    /// Returns whether empty directories should be pruned after filtering.
    #[must_use]
    pub const fn prune_empty_dirs_enabled(&self) -> bool {
        self.prune_empty_dirs
    }

    /// Reports whether partial transfer handling has been requested.
    #[must_use]
    pub const fn partial_enabled(&self) -> bool {
        self.partial || self.partial_dir.is_some()
    }

    /// Returns the configured partial directory when present.
    #[must_use]
    pub fn partial_directory_path(&self) -> Option<&Path> {
        self.partial_dir.as_deref()
    }

    /// Reports whether destination updates should be delayed until the end of the transfer.
    #[must_use]
    pub const fn delay_updates_enabled(&self) -> bool {
        self.delay_updates
    }

    /// Returns the ordered list of reference directories consulted during copy
    /// execution.
    pub fn reference_directories(&self) -> &[ReferenceDirectory] {
        &self.reference_directories
    }

    /// Reports whether in-place destination updates have been requested.
    #[must_use]
    pub const fn inplace_enabled(&self) -> bool {
        self.inplace
    }

    /// Returns `true` when appending to existing destinations is enabled.
    #[must_use]
    pub const fn append_enabled(&self) -> bool {
        self.append
    }

    /// Returns `true` when append verification is requested.
    #[must_use]
    pub const fn append_verify_enabled(&self) -> bool {
        self.append_verify
    }

    /// Reports whether the execution should record transfer events.
    #[must_use]
    pub const fn events_enabled(&self) -> bool {
        self.collect_events
    }

    /// Reports whether extended attribute preservation has been requested.
    #[cfg(feature = "xattr")]
    #[must_use]
    pub const fn preserve_xattrs(&self) -> bool {
        self.preserve_xattrs
    }

    /// Returns the configured inactivity timeout, if any.
    #[must_use]
    pub const fn timeout(&self) -> Option<Duration> {
        self.timeout
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LinkDestEntry {
    path: PathBuf,
    is_relative: bool,
}

impl LinkDestEntry {
    fn new(path: PathBuf) -> Self {
        let is_relative = !path.is_absolute();
        Self { path, is_relative }
    }

    fn resolve(&self, destination_root: &Path, relative: &Path) -> PathBuf {
        let base = if self.is_relative {
            destination_root.join(&self.path)
        } else {
            self.path.clone()
        };
        base.join(relative)
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
    regular_files_total: u64,
    regular_files_matched: u64,
    regular_files_ignored_existing: u64,
    regular_files_skipped_newer: u64,
    directories_total: u64,
    symlinks_total: u64,
    devices_total: u64,
    fifos_total: u64,
    files_copied: u64,
    directories_created: u64,
    symlinks_copied: u64,
    hard_links_created: u64,
    devices_created: u64,
    fifos_created: u64,
    items_deleted: u64,
    sources_removed: u64,
    transferred_file_size: u64,
    bytes_copied: u64,
    bytes_received: u64,
    compressed_bytes: u64,
    compression_used: bool,
    total_source_bytes: u64,
    total_elapsed: Duration,
    file_list_size: u64,
    file_list_generation: Duration,
    file_list_transfer: Duration,
}

impl LocalCopySummary {
    /// Returns the number of regular files copied or updated.
    #[must_use]
    pub const fn files_copied(&self) -> u64 {
        self.files_copied
    }

    /// Returns the number of regular files encountered during the transfer.
    #[must_use]
    pub const fn regular_files_total(&self) -> u64 {
        self.regular_files_total
    }

    /// Returns the number of regular files that already matched the destination state.
    #[must_use]
    pub const fn regular_files_matched(&self) -> u64 {
        self.regular_files_matched
    }

    /// Returns the number of regular files skipped due to `--ignore-existing`.
    #[must_use]
    pub const fn regular_files_ignored_existing(&self) -> u64 {
        self.regular_files_ignored_existing
    }

    /// Returns the number of regular files skipped because the destination was newer.
    #[must_use]
    pub const fn regular_files_skipped_newer(&self) -> u64 {
        self.regular_files_skipped_newer
    }

    /// Returns the number of directories created during the transfer.
    #[must_use]
    pub const fn directories_created(&self) -> u64 {
        self.directories_created
    }

    /// Returns the number of directories encountered in the source set.
    #[must_use]
    pub const fn directories_total(&self) -> u64 {
        self.directories_total
    }

    /// Returns the number of symbolic links copied.
    #[must_use]
    pub const fn symlinks_copied(&self) -> u64 {
        self.symlinks_copied
    }

    /// Returns the number of symbolic links encountered in the source set.
    #[must_use]
    pub const fn symlinks_total(&self) -> u64 {
        self.symlinks_total
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

    /// Returns the number of device nodes encountered in the source set.
    #[must_use]
    pub const fn devices_total(&self) -> u64 {
        self.devices_total
    }

    /// Returns the number of FIFOs created.
    #[must_use]
    pub const fn fifos_created(&self) -> u64 {
        self.fifos_created
    }

    /// Returns the number of FIFOs encountered in the source set.
    #[must_use]
    pub const fn fifos_total(&self) -> u64 {
        self.fifos_total
    }

    /// Returns the number of entries removed because of `--delete`.
    #[must_use]
    pub const fn items_deleted(&self) -> u64 {
        self.items_deleted
    }

    /// Returns the number of source entries removed due to `--remove-source-files`.
    #[must_use]
    pub const fn sources_removed(&self) -> u64 {
        self.sources_removed
    }

    /// Returns the aggregate number of literal bytes written for copied files.
    #[must_use]
    pub const fn bytes_copied(&self) -> u64 {
        self.bytes_copied
    }

    /// Returns the aggregate number of bytes received during the transfer.
    #[must_use]
    pub const fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    /// Returns the aggregate size of files that were rewritten or created.
    #[must_use]
    pub const fn transferred_file_size(&self) -> u64 {
        self.transferred_file_size
    }

    /// Returns the aggregate number of compressed bytes that would be sent when compression is enabled.
    #[must_use]
    pub const fn compressed_bytes(&self) -> u64 {
        self.compressed_bytes
    }

    /// Reports whether compression was applied during the transfer.
    #[must_use]
    pub const fn compression_used(&self) -> bool {
        self.compression_used
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

    /// Returns the number of bytes that would be transmitted for the file list.
    #[must_use]
    pub const fn file_list_size(&self) -> u64 {
        self.file_list_size
    }

    /// Returns the time spent enumerating the file list.
    #[must_use]
    pub const fn file_list_generation_time(&self) -> Duration {
        self.file_list_generation
    }

    /// Returns the time spent sending the file list to a peer.
    #[must_use]
    pub const fn file_list_transfer_time(&self) -> Duration {
        self.file_list_transfer
    }

    fn record_file(&mut self, file_size: u64, literal_bytes: u64, compressed: Option<u64>) {
        self.files_copied = self.files_copied.saturating_add(1);
        self.transferred_file_size = self.transferred_file_size.saturating_add(file_size);
        self.bytes_copied = self.bytes_copied.saturating_add(literal_bytes);
        let received = compressed.unwrap_or(literal_bytes);
        self.bytes_received = self.bytes_received.saturating_add(received);
        if let Some(compressed_bytes) = compressed {
            self.compression_used = true;
            self.compressed_bytes = self.compressed_bytes.saturating_add(compressed_bytes);
        }
    }

    fn record_regular_file_total(&mut self) {
        self.regular_files_total = self.regular_files_total.saturating_add(1);
    }

    fn record_regular_file_matched(&mut self) {
        self.regular_files_matched = self.regular_files_matched.saturating_add(1);
    }

    fn record_regular_file_ignored_existing(&mut self) {
        self.regular_files_ignored_existing = self.regular_files_ignored_existing.saturating_add(1);
    }

    fn record_regular_file_skipped_newer(&mut self) {
        self.regular_files_skipped_newer = self.regular_files_skipped_newer.saturating_add(1);
    }

    fn record_total_bytes(&mut self, bytes: u64) {
        self.total_source_bytes = self.total_source_bytes.saturating_add(bytes);
    }

    fn record_elapsed(&mut self, elapsed: Duration) {
        self.total_elapsed = self.total_elapsed.saturating_add(elapsed);
    }

    fn record_file_list_generation(&mut self, elapsed: Duration) {
        self.file_list_generation = self.file_list_generation.saturating_add(elapsed);
    }

    fn record_file_list_transfer(&mut self, elapsed: Duration) {
        self.file_list_transfer = self.file_list_transfer.saturating_add(elapsed);
    }

    fn record_directory(&mut self) {
        self.directories_created = self.directories_created.saturating_add(1);
    }

    fn record_directory_total(&mut self) {
        self.directories_total = self.directories_total.saturating_add(1);
    }

    fn record_symlink(&mut self) {
        self.symlinks_copied = self.symlinks_copied.saturating_add(1);
    }

    fn record_symlink_total(&mut self) {
        self.symlinks_total = self.symlinks_total.saturating_add(1);
    }

    fn record_hard_link(&mut self) {
        self.hard_links_created = self.hard_links_created.saturating_add(1);
    }

    fn record_device(&mut self) {
        self.devices_created = self.devices_created.saturating_add(1);
    }

    fn record_device_total(&mut self) {
        self.devices_total = self.devices_total.saturating_add(1);
    }

    fn record_fifo(&mut self) {
        self.fifos_created = self.fifos_created.saturating_add(1);
    }

    fn record_fifo_total(&mut self) {
        self.fifos_total = self.fifos_total.saturating_add(1);
    }

    fn record_deletion(&mut self) {
        self.items_deleted = self.items_deleted.saturating_add(1);
    }

    fn record_source_removed(&mut self) {
        self.sources_removed = self.sources_removed.saturating_add(1);
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
    dir_merge_layers: Rc<RefCell<FilterSegmentLayers>>,
    observer: Option<&'a mut dyn LocalCopyRecordHandler>,
    dir_merge_ephemeral: Rc<RefCell<FilterSegmentStack>>,
    deferred_deletions: Vec<DeferredDeletion>,
    deferred_updates: Vec<DeferredUpdate>,
    timeout: Option<Duration>,
    last_progress: Instant,
    created_entries: Vec<CreatedEntry>,
    destination_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct FileCopyOutcome {
    literal_bytes: u64,
    compressed_bytes: Option<u64>,
}

impl FileCopyOutcome {
    fn new(literal_bytes: u64, compressed_bytes: Option<u64>) -> Self {
        Self {
            literal_bytes,
            compressed_bytes,
        }
    }

    fn literal_bytes(self) -> u64 {
        self.literal_bytes
    }

    fn compressed_bytes(self) -> Option<u64> {
        self.compressed_bytes
    }
}

/// Describes a block matched against the existing destination during delta copy.
#[derive(Clone, Copy, Debug)]
struct MatchedBlock<'a> {
    descriptor: &'a SignatureBlock,
    canonical_length: usize,
}

impl<'a> MatchedBlock<'a> {
    /// Creates a matched block descriptor from a [`SignatureBlock`] and its canonical length.
    fn new(descriptor: &'a SignatureBlock, canonical_length: usize) -> Self {
        Self {
            descriptor,
            canonical_length,
        }
    }

    /// Returns the matched [`SignatureBlock`].
    fn descriptor(&self) -> &'a SignatureBlock {
        self.descriptor
    }

    /// Calculates the byte offset of the block within the destination file.
    fn offset(&self) -> u64 {
        self.descriptor
            .index()
            .saturating_mul(self.canonical_length as u64)
    }
}

struct DeferredDeletion {
    destination: PathBuf,
    relative: Option<PathBuf>,
    keep: Vec<OsString>,
}

struct DeferredUpdate {
    guard: DestinationWriteGuard,
    metadata: fs::Metadata,
    metadata_options: MetadataOptions,
    mode: LocalCopyExecution,
    source: PathBuf,
    relative: Option<PathBuf>,
    destination: PathBuf,
    file_type: fs::FileType,
    destination_previously_existed: bool,
    #[cfg(feature = "xattr")]
    preserve_xattrs: bool,
    #[cfg(feature = "acl")]
    preserve_acls: bool,
}

#[derive(Clone, Debug)]
struct CreatedEntry {
    path: PathBuf,
    kind: CreatedEntryKind,
}

#[derive(Clone, Copy, Debug)]
enum CreatedEntryKind {
    File,
    Directory,
    Symlink,
    Fifo,
    Device,
    HardLink,
}

impl<'a> CopyContext<'a> {
    fn new(
        mode: LocalCopyExecution,
        options: LocalCopyOptions,
        observer: Option<&'a mut dyn LocalCopyRecordHandler>,
        destination_root: PathBuf,
    ) -> Self {
        let limiter = options.bandwidth_limit_bytes().map(BandwidthLimiter::new);
        let collect_events = options.events_enabled();
        let filter_program = options.filter_program().cloned();
        let dir_merge_layers = filter_program
            .as_ref()
            .map(|program| vec![Vec::new(); program.dir_merge_rules().len()])
            .unwrap_or_default();
        let dir_merge_ephemeral = Vec::new();
        let timeout = options.timeout();
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
            deferred_deletions: Vec::new(),
            deferred_updates: Vec::new(),
            timeout,
            last_progress: Instant::now(),
            created_entries: Vec::new(),
            destination_root,
        }
    }

    fn register_progress(&mut self) {
        self.last_progress = Instant::now();
    }

    fn enforce_timeout(&mut self) -> Result<(), LocalCopyError> {
        if let Some(limit) = self.timeout {
            if self.last_progress.elapsed() > limit {
                return Err(LocalCopyError::timeout(limit));
            }
        }
        Ok(())
    }

    fn mode(&self) -> LocalCopyExecution {
        self.mode
    }

    fn options(&self) -> &LocalCopyOptions {
        &self.options
    }

    fn delay_updates_enabled(&self) -> bool {
        self.options.delay_updates_enabled()
    }

    fn destination_root(&self) -> &Path {
        &self.destination_root
    }

    fn apply_metadata_and_finalize(
        &mut self,
        destination: &Path,
        metadata: &fs::Metadata,
        metadata_options: MetadataOptions,
        mode: LocalCopyExecution,
        source: &Path,
        relative: Option<&Path>,
        file_type: fs::FileType,
        destination_previously_existed: bool,
        #[cfg(feature = "xattr")] preserve_xattrs: bool,
        #[cfg(feature = "acl")] preserve_acls: bool,
    ) -> Result<(), LocalCopyError> {
        self.register_created_path(
            destination,
            CreatedEntryKind::File,
            destination_previously_existed,
        );
        apply_file_metadata_with_options(destination, metadata, metadata_options)
            .map_err(map_metadata_error)?;
        #[cfg(feature = "xattr")]
        {
            sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
        }
        #[cfg(feature = "acl")]
        {
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
        }
        #[cfg(not(any(feature = "xattr", feature = "acl")))]
        let _ = mode;
        self.hard_links.record(metadata, destination);
        remove_source_entry_if_requested(self, source, relative, file_type)?;
        Ok(())
    }

    fn link_dest_target(
        &self,
        relative: &Path,
        source: &Path,
        metadata: &fs::Metadata,
        metadata_options: &MetadataOptions,
        size_only: bool,
        checksum: bool,
    ) -> Result<Option<PathBuf>, LocalCopyError> {
        if self.options.link_dest_entries().is_empty() {
            return Ok(None);
        }

        for entry in self.options.link_dest_entries() {
            let candidate = entry.resolve(self.destination_root(), relative);
            let candidate_metadata = match fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "inspect link-dest candidate",
                        candidate,
                        error,
                    ));
                }
            };

            if !candidate_metadata.file_type().is_file() {
                continue;
            }

            if should_skip_copy(
                source,
                metadata,
                candidate.as_path(),
                &candidate_metadata,
                metadata_options,
                size_only,
                checksum,
            ) {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
    }

    fn reference_directories(&self) -> &[ReferenceDirectory] {
        self.options.reference_directories()
    }

    fn register_deferred_update(&mut self, update: DeferredUpdate) {
        let metadata = update.metadata.clone();
        let destination = update.destination.clone();
        self.hard_links.record(&metadata, destination.as_path());
        self.deferred_updates.push(update);
    }

    fn commit_deferred_update_for(&mut self, destination: &Path) -> Result<(), LocalCopyError> {
        if let Some(index) = self
            .deferred_updates
            .iter()
            .position(|update| update.destination.as_path() == destination)
        {
            let update = self.deferred_updates.swap_remove(index);
            self.finalize_deferred_update(update)?;
        }
        Ok(())
    }

    fn flush_deferred_updates(&mut self) -> Result<(), LocalCopyError> {
        if self.deferred_updates.is_empty() {
            return Ok(());
        }

        let updates = std::mem::take(&mut self.deferred_updates);
        for update in updates {
            self.finalize_deferred_update(update)?;
        }
        Ok(())
    }

    fn finalize_deferred_update(&mut self, update: DeferredUpdate) -> Result<(), LocalCopyError> {
        let DeferredUpdate {
            guard,
            metadata,
            metadata_options,
            mode,
            source,
            relative,
            destination,
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        } = update;

        #[cfg(not(any(feature = "xattr", feature = "acl")))]
        let _ = &source;

        guard.commit()?;

        self.apply_metadata_and_finalize(
            destination.as_path(),
            &metadata,
            metadata_options,
            mode,
            source.as_path(),
            relative.as_deref(),
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        )
    }

    fn delete_timing(&self) -> Option<DeleteTiming> {
        self.options.delete_timing()
    }

    fn metadata_options(&self) -> MetadataOptions {
        MetadataOptions::new()
            .preserve_owner(self.options.preserve_owner())
            .preserve_group(self.options.preserve_group())
            .preserve_permissions(self.options.preserve_permissions())
            .preserve_times(self.options.preserve_times())
            .numeric_ids(self.options.numeric_ids_enabled())
            .with_owner_override(self.options.owner_override())
            .with_group_override(self.options.group_override())
            .with_chmod(self.options.chmod().cloned())
    }

    fn copy_links_enabled(&self) -> bool {
        self.options.copy_links_enabled()
    }

    fn copy_dirlinks_enabled(&self) -> bool {
        self.options.copy_dirlinks_enabled()
    }

    fn keep_dirlinks_enabled(&self) -> bool {
        self.options.keep_dirlinks_enabled()
    }

    fn whole_file_enabled(&self) -> bool {
        self.options.whole_file_enabled()
    }

    fn sparse_enabled(&self) -> bool {
        self.options.sparse_enabled()
    }

    fn append_enabled(&self) -> bool {
        self.options.append_enabled()
    }

    fn append_verify_enabled(&self) -> bool {
        self.options.append_verify_enabled()
    }

    fn preallocate_enabled(&self) -> bool {
        self.options.preallocate_enabled()
    }

    fn devices_enabled(&self) -> bool {
        self.options.devices_enabled()
    }

    fn specials_enabled(&self) -> bool {
        self.options.specials_enabled()
    }

    #[cfg(feature = "acl")]
    fn acls_enabled(&self) -> bool {
        self.options.acls_enabled()
    }

    fn relative_paths_enabled(&self) -> bool {
        self.options.relative_paths_enabled()
    }

    fn implied_dirs_enabled(&self) -> bool {
        self.options.implied_dirs_enabled()
    }

    fn mkpath_enabled(&self) -> bool {
        self.options.mkpath_enabled()
    }

    fn prune_empty_dirs_enabled(&self) -> bool {
        self.options.prune_empty_dirs_enabled()
    }

    fn omit_dir_times_enabled(&self) -> bool {
        self.options.omit_dir_times_enabled()
    }

    fn prepare_parent_directory(&mut self, parent: &Path) -> Result<(), LocalCopyError> {
        if parent.as_os_str().is_empty() {
            return Ok(());
        }

        let allow_creation = self.implied_dirs_enabled() || self.mkpath_enabled();
        let keep_dirlinks = self.keep_dirlinks_enabled();

        if self.mode.is_dry_run() {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        follow_symlink_metadata(parent).and_then(|metadata| {
                            if metadata.file_type().is_dir() {
                                Ok(())
                            } else {
                                Err(LocalCopyError::invalid_argument(
                                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                                ))
                            }
                        })
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    if allow_creation {
                        Ok(())
                    } else {
                        Err(LocalCopyError::io(
                            "create parent directory",
                            parent.to_path_buf(),
                            error,
                        ))
                    }
                }
                Err(error) => Err(LocalCopyError::io(
                    "inspect existing destination",
                    parent.to_path_buf(),
                    error,
                )),
            }
        } else if allow_creation {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        let metadata = follow_symlink_metadata(parent)?;
                        if metadata.file_type().is_dir() {
                            Ok(())
                        } else {
                            Err(LocalCopyError::invalid_argument(
                                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                            ))
                        }
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(parent).map_err(|error| {
                        LocalCopyError::io("create parent directory", parent.to_path_buf(), error)
                    })?;
                    self.register_progress();
                    Ok(())
                }
                Err(error) => Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    error,
                )),
            }
        } else {
            match fs::symlink_metadata(parent) {
                Ok(existing) => {
                    let ty = existing.file_type();
                    if ty.is_dir() {
                        Ok(())
                    } else if keep_dirlinks && ty.is_symlink() {
                        let metadata = follow_symlink_metadata(parent)?;
                        if metadata.file_type().is_dir() {
                            Ok(())
                        } else {
                            Err(LocalCopyError::invalid_argument(
                                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                            ))
                        }
                    } else {
                        Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ))
                    }
                }
                Err(error) => Err(LocalCopyError::io(
                    "create parent directory",
                    parent.to_path_buf(),
                    error,
                )),
            }
        }
    }

    fn remove_source_files_enabled(&self) -> bool {
        self.options.remove_source_files_enabled()
    }

    fn compress_enabled(&self) -> bool {
        self.options.compress_enabled()
    }

    fn compression_level(&self) -> CompressionLevel {
        self.options.compression_level()
    }

    fn checksum_enabled(&self) -> bool {
        self.options.checksum_enabled()
    }

    fn size_only_enabled(&self) -> bool {
        self.options.size_only_enabled()
    }

    fn ignore_existing_enabled(&self) -> bool {
        self.options.ignore_existing_enabled()
    }

    fn update_enabled(&self) -> bool {
        self.options.update_enabled()
    }

    fn partial_enabled(&self) -> bool {
        self.options.partial_enabled()
    }

    fn partial_directory_path(&self) -> Option<&Path> {
        self.options.partial_directory_path()
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
                .evaluate(
                    relative,
                    is_dir,
                    layers.as_slice(),
                    temp_layers,
                    FilterContext::Transfer,
                )
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
            let outcome = program.evaluate(
                relative,
                is_dir,
                layers.as_slice(),
                temp_layers,
                FilterContext::Deletion,
            );
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

        let excluded = program.should_exclude_directory(source)?;

        Ok(DirectoryFilterGuard::new(
            Rc::clone(&self.dir_merge_layers),
            Rc::clone(&self.dir_merge_ephemeral),
            added_indices,
            true,
            excluded,
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

    fn notify_progress(
        &mut self,
        relative: &Path,
        total_bytes: Option<u64>,
        transferred: u64,
        elapsed: Duration,
    ) {
        self.register_progress();
        if self.observer.is_none() {
            return;
        }

        if let Some(observer) = &mut self.observer {
            observer.handle_progress(LocalCopyProgress::new(
                relative,
                transferred,
                total_bytes,
                elapsed,
            ));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_file_contents(
        &mut self,
        reader: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        sparse: bool,
        compress: bool,
        source: &Path,
        destination: &Path,
        relative: &Path,
        delta: Option<&DeltaSignatureIndex>,
        total_size: u64,
        initial_bytes: u64,
        start: Instant,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        if let Some(index) = delta {
            return self.copy_file_contents_with_delta(
                reader,
                writer,
                buffer,
                sparse,
                compress,
                source,
                destination,
                relative,
                index,
                total_size,
                initial_bytes,
                start,
            );
        }

        let mut total_bytes: u64 = 0;
        let mut literal_bytes: u64 = 0;
        let mut compressor = if compress {
            Some(CountingZlibEncoder::new(self.compression_level()))
        } else {
            None
        };
        let mut compressed_progress: u64 = 0;

        loop {
            self.enforce_timeout()?;
            let chunk_len = if let Some(limiter) = self.limiter.as_ref() {
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

            let written = if sparse {
                write_sparse_chunk(writer, &buffer[..read], destination)?
            } else {
                writer.write_all(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("copy file", destination.to_path_buf(), error)
                })?;
                read
            };

            self.register_progress();

            let mut compressed_delta = None;
            if let Some(encoder) = compressor.as_mut() {
                encoder.write(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("compress file", source.to_path_buf(), error)
                })?;
                let total = encoder.bytes_written();
                let delta = total.saturating_sub(compressed_progress);
                compressed_progress = total;
                compressed_delta = Some(delta);
            }

            if let Some(limiter) = self.limiter.as_mut() {
                if let Some(delta) = compressed_delta {
                    if delta > 0 {
                        let bounded = delta.min(usize::MAX as u64) as usize;
                        limiter.register(bounded);
                    }
                } else {
                    limiter.register(read);
                }
            }

            total_bytes = total_bytes.saturating_add(read as u64);
            literal_bytes = literal_bytes.saturating_add(written as u64);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
        }

        if sparse {
            let final_len = initial_bytes.saturating_add(total_bytes);
            writer.set_len(final_len).map_err(|error| {
                LocalCopyError::io(
                    "truncate destination file",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            self.register_progress();
        }

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            self.register_progress();
            if let Some(limiter) = self.limiter.as_mut() {
                let delta = compressed_total.saturating_sub(compressed_progress);
                if delta > 0 {
                    let bounded = delta.min(usize::MAX as u64) as usize;
                    limiter.register(bounded);
                }
            }
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_file_contents_with_delta(
        &mut self,
        reader: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        sparse: bool,
        compress: bool,
        source: &Path,
        destination: &Path,
        relative: &Path,
        index: &DeltaSignatureIndex,
        total_size: u64,
        initial_bytes: u64,
        start: Instant,
    ) -> Result<FileCopyOutcome, LocalCopyError> {
        let mut destination_reader = fs::File::open(destination).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?;
        let mut compressor = if compress {
            Some(CountingZlibEncoder::new(self.compression_level()))
        } else {
            None
        };
        let mut compressed_progress = 0u64;
        let mut total_bytes = 0u64;
        let mut literal_bytes = 0u64;
        let mut window: VecDeque<u8> = VecDeque::with_capacity(index.block_length());
        let mut pending_literals = Vec::with_capacity(index.block_length());
        let mut scratch = Vec::with_capacity(index.block_length());
        let mut rolling = RollingChecksum::new();
        let mut outgoing: Option<u8> = None;
        let mut read_buffer = vec![0u8; buffer.len().max(index.block_length())];
        let mut buffer_len = 0usize;
        let mut buffer_pos = 0usize;

        loop {
            self.enforce_timeout()?;
            if buffer_pos == buffer_len {
                buffer_len = reader.read(&mut read_buffer).map_err(|error| {
                    LocalCopyError::io("copy file", source.to_path_buf(), error)
                })?;
                buffer_pos = 0;
                if buffer_len == 0 {
                    break;
                }
            }

            let byte = read_buffer[buffer_pos];
            buffer_pos += 1;

            window.push_back(byte);
            if let Some(outgoing_byte) = outgoing.take() {
                debug_assert!(window.len() <= index.block_length());
                rolling.roll_many(&[outgoing_byte], &[byte]).map_err(|_| {
                    LocalCopyError::invalid_argument(LocalCopyArgumentError::UnsupportedFileType)
                })?;
            } else {
                rolling.update(&[byte]);
            }

            if window.len() < index.block_length() {
                continue;
            }

            let digest = rolling.digest();
            if let Some(block_index) = index.find_match_window(digest, &window, &mut scratch) {
                if !pending_literals.is_empty() {
                    let flushed_len = pending_literals.len();
                    let flushed = self.flush_literal_chunk(
                        writer,
                        pending_literals.as_slice(),
                        sparse,
                        compressor.as_mut(),
                        &mut compressed_progress,
                        source,
                        destination,
                    )?;
                    literal_bytes = literal_bytes.saturating_add(flushed as u64);
                    total_bytes = total_bytes.saturating_add(flushed_len as u64);
                    let progressed = initial_bytes.saturating_add(total_bytes);
                    self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
                    pending_literals.clear();
                }

                let block = index.block(block_index);
                let block_len = block.len();
                let matched = MatchedBlock::new(block, index.block_length());
                self.copy_matched_block(
                    &mut destination_reader,
                    writer,
                    buffer,
                    destination,
                    matched,
                    sparse,
                )?;
                total_bytes = total_bytes.saturating_add(block_len as u64);
                let progressed = initial_bytes.saturating_add(total_bytes);
                self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
                window.clear();
                rolling.reset();
                outgoing = None;
                continue;
            }

            if let Some(front) = window.pop_front() {
                pending_literals.push(front);
                outgoing = Some(front);
            }
        }

        while let Some(byte) = window.pop_front() {
            pending_literals.push(byte);
        }

        if !pending_literals.is_empty() {
            let flushed_len = pending_literals.len();
            let flushed = self.flush_literal_chunk(
                writer,
                pending_literals.as_slice(),
                sparse,
                compressor.as_mut(),
                &mut compressed_progress,
                source,
                destination,
            )?;
            total_bytes = total_bytes.saturating_add(flushed_len as u64);
            literal_bytes = literal_bytes.saturating_add(flushed as u64);
            let progressed = initial_bytes.saturating_add(total_bytes);
            self.notify_progress(relative, Some(total_size), progressed, start.elapsed());
        }

        if sparse {
            let final_len = initial_bytes.saturating_add(total_bytes);
            writer.set_len(final_len).map_err(|error| {
                LocalCopyError::io(
                    "truncate destination file",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            self.register_progress();
        }

        let outcome = if let Some(encoder) = compressor {
            let compressed_total = encoder.finish().map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            if let Some(limiter) = self.limiter.as_mut() {
                let delta = compressed_total.saturating_sub(compressed_progress);
                if delta > 0 {
                    let bounded = delta.min(usize::MAX as u64) as usize;
                    limiter.register(bounded);
                }
            }
            FileCopyOutcome::new(literal_bytes, Some(compressed_total))
        } else {
            FileCopyOutcome::new(literal_bytes, None)
        };

        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_literal_chunk(
        &mut self,
        writer: &mut fs::File,
        chunk: &[u8],
        sparse: bool,
        compressor: Option<&mut CountingZlibEncoder>,
        compressed_progress: &mut u64,
        source: &Path,
        destination: &Path,
    ) -> Result<usize, LocalCopyError> {
        if chunk.is_empty() {
            return Ok(0);
        }
        self.enforce_timeout()?;
        let written = if sparse {
            write_sparse_chunk(writer, chunk, destination)?
        } else {
            writer.write_all(chunk).map_err(|error| {
                LocalCopyError::io("copy file", destination.to_path_buf(), error)
            })?;
            chunk.len()
        };

        if let Some(encoder) = compressor {
            encoder.write(chunk).map_err(|error| {
                LocalCopyError::io("compress file", source.to_path_buf(), error)
            })?;
            let total = encoder.bytes_written();
            let delta = total.saturating_sub(*compressed_progress);
            *compressed_progress = total;
            if let Some(limiter) = self.limiter.as_mut() {
                if delta > 0 {
                    let bounded = delta.min(usize::MAX as u64) as usize;
                    limiter.register(bounded);
                }
            }
        } else if let Some(limiter) = self.limiter.as_mut() {
            limiter.register(chunk.len());
        }

        Ok(written)
    }

    fn copy_matched_block(
        &mut self,
        existing: &mut fs::File,
        writer: &mut fs::File,
        buffer: &mut [u8],
        destination: &Path,
        matched: MatchedBlock<'_>,
        sparse: bool,
    ) -> Result<(), LocalCopyError> {
        let offset = matched.offset();
        existing.seek(SeekFrom::Start(offset)).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?;

        let mut remaining = matched.descriptor().len();
        while remaining > 0 {
            self.enforce_timeout()?;
            let chunk_len = remaining.min(buffer.len());
            let read = existing.read(&mut buffer[..chunk_len]).map_err(|error| {
                LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?;
            if read == 0 {
                let eof = io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading existing block",
                );
                return Err(LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    eof,
                ));
            }

            if sparse {
                let _ = write_sparse_chunk(writer, &buffer[..read], destination)?;
            } else {
                writer.write_all(&buffer[..read]).map_err(|error| {
                    LocalCopyError::io("copy file", destination.to_path_buf(), error)
                })?;
            }

            remaining -= read;
        }

        Ok(())
    }

    fn record_skipped_non_regular(&mut self, relative: Option<&Path>) {
        if let Some(path) = relative {
            self.record(LocalCopyRecord::new(
                path.to_path_buf(),
                LocalCopyAction::SkippedNonRegular,
                0,
                None,
                Duration::default(),
                None,
            ));
        }
    }

    fn record_file_list_generation(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_generation(elapsed);
        }
    }

    #[allow(dead_code)]
    fn record_file_list_transfer(&mut self, elapsed: Duration) {
        if !elapsed.is_zero() {
            self.summary.record_file_list_transfer(elapsed);
        }
    }

    fn into_outcome(self) -> CopyOutcome {
        CopyOutcome {
            summary: self.summary,
            events: self.events,
        }
    }

    fn defer_deletion(
        &mut self,
        destination: PathBuf,
        relative: Option<PathBuf>,
        keep: Vec<OsString>,
    ) {
        self.deferred_deletions.push(DeferredDeletion {
            destination,
            relative,
            keep,
        });
    }

    fn flush_deferred_deletions(&mut self) -> Result<(), LocalCopyError> {
        let pending = std::mem::take(&mut self.deferred_deletions);
        for entry in pending {
            self.enforce_timeout()?;
            let relative = entry.relative.as_deref();
            delete_extraneous_entries(self, entry.destination.as_path(), relative, &entry.keep)?;
        }
        Ok(())
    }

    fn register_created_path(&mut self, path: &Path, kind: CreatedEntryKind, existed_before: bool) {
        if self.mode.is_dry_run() || existed_before {
            return;
        }
        self.created_entries.push(CreatedEntry {
            path: path.to_path_buf(),
            kind,
        });
    }

    fn rollback_on_error(&mut self, error: &LocalCopyError) {
        if matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }) {
            self.rollback_created_entries();
        }
    }

    fn rollback_created_entries(&mut self) {
        while let Some(entry) = self.created_entries.pop() {
            match entry.kind {
                CreatedEntryKind::Directory => {
                    let _ = fs::remove_dir(&entry.path);
                }
                CreatedEntryKind::File
                | CreatedEntryKind::Symlink
                | CreatedEntryKind::Fifo
                | CreatedEntryKind::Device
                | CreatedEntryKind::HardLink => {
                    let _ = fs::remove_file(&entry.path);
                }
            }
        }
    }
}

struct DirectoryFilterGuard {
    layers: Rc<RefCell<FilterSegmentLayers>>,
    ephemeral: Rc<RefCell<FilterSegmentStack>>,
    indices: Vec<usize>,
    ephemeral_active: bool,
    excluded: bool,
}

impl DirectoryFilterGuard {
    fn new(
        layers: Rc<RefCell<FilterSegmentLayers>>,
        ephemeral: Rc<RefCell<FilterSegmentStack>>,
        indices: Vec<usize>,
        ephemeral_active: bool,
        excluded: bool,
    ) -> Self {
        Self {
            layers,
            ephemeral,
            indices,
            ephemeral_active,
            excluded,
        }
    }

    fn is_excluded(&self) -> bool {
        self.excluded
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

fn apply_dir_merge_rule_defaults(mut rule: FilterRule, options: &DirMergeOptions) -> FilterRule {
    if options.anchor_root_enabled() {
        rule = rule.anchor_to_root();
    }

    if let Some(sender) = options.sender_side_override() {
        rule = rule.with_sender(sender);
    }

    if let Some(receiver) = options.receiver_side_override() {
        rule = rule.with_receiver(receiver);
    }

    rule
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
                    let rule = match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(token.to_string()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(token.to_string()),
                    };
                    rules.push(apply_dir_merge_rule_defaults(rule, options));
                    continue;
                }

                let mut directive = token.to_string();
                let lower = directive.to_ascii_lowercase();
                let needs_argument = matches!(
                    lower.as_str(),
                    "merge" | "include" | "exclude" | "show" | "hide" | "protect"
                ) || lower.starts_with("dir-merge");

                if needs_argument {
                    if let Some(next) = iter.next() {
                        directive.push(' ');
                        directive.push_str(next);
                    }
                }

                match parse_filter_directive_line(&directive) {
                    Ok(Some(ParsedFilterDirective::Rule(rule))) => {
                        rules.push(apply_dir_merge_rule_defaults(rule, options));
                    }
                    Ok(Some(ParsedFilterDirective::Merge {
                        path: merge_path,
                        options: merge_options,
                    })) => {
                        let nested = if merge_path.is_absolute() {
                            merge_path
                        } else {
                            let parent = path.parent().unwrap_or_else(|| Path::new("."));
                            parent.join(merge_path)
                        };
                        if let Some(options_override) = merge_options {
                            let nested_rules = load_dir_merge_rules_recursive(
                                &nested,
                                &options_override,
                                visited,
                            )?;
                            rules.extend(nested_rules);
                        } else {
                            let nested_rules =
                                load_dir_merge_rules_recursive(&nested, options, visited)?;
                            rules.extend(nested_rules);
                        }
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
                    let rule = match kind {
                        DirMergeEnforcedKind::Include => FilterRule::include(trimmed.to_string()),
                        DirMergeEnforcedKind::Exclude => FilterRule::exclude(trimmed.to_string()),
                    };
                    rules.push(apply_dir_merge_rule_defaults(rule, options));
                    continue;
                }

                match parse_filter_directive_line(trimmed) {
                    Ok(Some(ParsedFilterDirective::Rule(rule))) => {
                        rules.push(apply_dir_merge_rule_defaults(rule, options));
                    }
                    Ok(Some(ParsedFilterDirective::Merge {
                        path: merge_path,
                        options: merge_options,
                    })) => {
                        let nested = if merge_path.is_absolute() {
                            merge_path
                        } else {
                            let parent = path.parent().unwrap_or_else(|| Path::new("."));
                            parent.join(merge_path)
                        };
                        if let Some(options_override) = merge_options {
                            let nested_rules = load_dir_merge_rules_recursive(
                                &nested,
                                &options_override,
                                visited,
                            )?;
                            rules.extend(nested_rules);
                        } else {
                            let nested_rules =
                                load_dir_merge_rules_recursive(&nested, options, visited)?;
                            rules.extend(nested_rules);
                        }
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

#[derive(Debug)]
enum ParsedFilterDirective {
    Rule(FilterRule),
    Merge {
        path: PathBuf,
        options: Option<DirMergeOptions>,
    },
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

    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Some(directive) = parse_short_merge_directive_line(trimmed)? {
        return Ok(Some(directive));
    }

    if let Some(directive) = parse_merge_directive(trimmed)? {
        return Ok(Some(directive));
    }

    if let Some(directive) = parse_dir_merge_directive(trimmed)? {
        return Ok(Some(directive));
    }

    if let Some(remainder) = trimmed.strip_prefix('+') {
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '+' requires a pattern"));
        }
        return Ok(Some(ParsedFilterDirective::Rule(FilterRule::include(
            pattern.to_string(),
        ))));
    }

    if let Some(remainder) = trimmed.strip_prefix('-') {
        let pattern = remainder.trim_start();
        if pattern.is_empty() {
            return Err(FilterParseError::new("filter rule '-' requires a pattern"));
        }
        return Ok(Some(ParsedFilterDirective::Rule(FilterRule::exclude(
            pattern.to_string(),
        ))));
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
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

    if keyword.len() == 1 {
        let shorthand = keyword.chars().next().unwrap().to_ascii_lowercase();
        match shorthand {
            'p' => {
                return handle_keyword(remainder, FilterRule::protect);
            }
            'r' => {
                return handle_keyword(remainder, FilterRule::risk);
            }
            's' => {
                if remainder.is_empty() {
                    return Err(FilterParseError::new("filter directive missing pattern"));
                }
                let rule = FilterRule::show(remainder.to_string());
                return Ok(Some(ParsedFilterDirective::Rule(rule)));
            }
            'h' => {
                if remainder.is_empty() {
                    return Err(FilterParseError::new("filter directive missing pattern"));
                }
                let rule = FilterRule::hide(remainder.to_string());
                return Ok(Some(ParsedFilterDirective::Rule(rule)));
            }
            _ => {}
        }
    }

    if keyword.eq_ignore_ascii_case("include") {
        return handle_keyword(remainder, FilterRule::include);
    }

    if keyword.eq_ignore_ascii_case("exclude") {
        return handle_keyword(remainder, FilterRule::exclude);
    }

    if keyword.eq_ignore_ascii_case("show") {
        if remainder.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        let rule = FilterRule::show(remainder.to_string());
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    if keyword.eq_ignore_ascii_case("hide") {
        if remainder.is_empty() {
            return Err(FilterParseError::new("filter directive missing pattern"));
        }
        let rule = FilterRule::hide(remainder.to_string());
        return Ok(Some(ParsedFilterDirective::Rule(rule)));
    }

    if keyword.eq_ignore_ascii_case("protect") {
        return handle_keyword(remainder, FilterRule::protect);
    }

    if keyword.eq_ignore_ascii_case("risk") {
        return handle_keyword(remainder, FilterRule::risk);
    }

    Err(FilterParseError::new(format!(
        "unsupported filter directive '{}'",
        trimmed
    )))
}

fn parse_merge_directive(text: &str) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    const MERGE_PREFIX: &str = "merge";

    if text.len() < MERGE_PREFIX.len() {
        return Ok(None);
    }

    let (prefix, rest) = text.split_at(MERGE_PREFIX.len());
    if !prefix.eq_ignore_ascii_case(MERGE_PREFIX) {
        return Ok(None);
    }

    let mut remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    let mut modifiers = "";
    if let Some(next) = remainder.strip_prefix(',') {
        let mut split = next.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        modifiers = split.next().unwrap_or("");
        remainder = split
            .next()
            .unwrap_or("")
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }

    let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, text, false)?;

    if remainder == "-" {
        return Err(FilterParseError::new(
            "merge from standard input is not supported in .rsync-filter files",
        ));
    }

    let path_text = remainder.trim_end();
    let path_text = if path_text.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else {
            return Err(FilterParseError::new(
                "merge directive requires a file path",
            ));
        }
    } else {
        path_text
    };

    let options = if modifiers.is_empty() && !assume_cvsignore {
        None
    } else {
        Some(options)
    };

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(path_text),
        options,
    }))
}

fn parse_short_merge_directive_line(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    let mut chars = text.chars();
    let first = match chars.next() {
        Some(first) => first,
        None => return Ok(None),
    };

    let allow_extended = match first {
        '.' => false,
        ':' => true,
        _ => return Ok(None),
    };

    let remainder = chars.as_str();
    let (modifiers, rest) = split_short_rule_modifiers(remainder);
    let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, text, allow_extended)?;

    let pattern = rest.trim();
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else if allow_extended {
            return Err(FilterParseError::new(format!(
                "dir-merge directive '{}' is missing a file name",
                text
            )));
        } else {
            return Err(FilterParseError::new(format!(
                "merge directive '{}' is missing a file path",
                text
            )));
        }
    } else {
        pattern
    };

    if allow_extended {
        return Ok(Some(ParsedFilterDirective::Merge {
            path: PathBuf::from(pattern),
            options: Some(options),
        }));
    }

    let options = if modifiers.is_empty() && !assume_cvsignore {
        None
    } else {
        Some(options)
    };

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(pattern),
        options,
    }))
}

fn split_short_rule_modifiers(text: &str) -> (&str, &str) {
    if text.is_empty() {
        return ("", "");
    }

    if let Some(rest) = text.strip_prefix(',') {
        let mut parts = rest.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        let modifiers = parts.next().unwrap_or("");
        let remainder = parts.next().unwrap_or("");
        let remainder =
            remainder.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
        return (modifiers, remainder);
    }

    let mut chars = text.chars();
    match chars.next() {
        None => ("", ""),
        Some(first) if first.is_ascii_whitespace() || first == '_' => {
            let remainder =
                text.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            ("", remainder)
        }
        Some(_) => {
            let mut len = 0;
            for ch in text.chars() {
                if ch.is_ascii_whitespace() || ch == '_' {
                    break;
                }
                len += ch.len_utf8();
            }
            let modifiers = &text[..len];
            let remainder =
                text[len..].trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
            (modifiers, remainder)
        }
    }
}

fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
    allow_extended: bool,
) -> Result<(DirMergeOptions, bool), FilterParseError> {
    let label = if allow_extended { "dir-merge" } else { "merge" };
    let mut options = if allow_extended {
        DirMergeOptions::default()
    } else {
        DirMergeOptions::default().allow_list_clearing(true)
    };
    let mut enforced: Option<DirMergeEnforcedKind> = None;
    let mut saw_include = false;
    let mut saw_exclude = false;
    let mut assume_cvsignore = false;

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '-' => {
                if saw_include {
                    let message = format!(
                        "{label} directive '{}' cannot combine '+' and '-' modifiers",
                        directive
                    );

                    return Err(FilterParseError::new(message));
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
            }
            '+' => {
                if saw_exclude {
                    let message = format!(
                        "{label} directive '{}' cannot combine '+' and '-' modifiers",
                        directive
                    );
                    return Err(FilterParseError::new(message));
                }
                saw_include = true;
                enforced = Some(DirMergeEnforcedKind::Include);
            }
            'c' => {
                if saw_include {
                    let message = format!(
                        "{label} directive '{}' cannot combine 'C' with '+' or '-'",
                        directive
                    );
                    return Err(FilterParseError::new(message));
                }
                saw_exclude = true;
                enforced = Some(DirMergeEnforcedKind::Exclude);
                options = options
                    .use_whitespace()
                    .allow_comments(false)
                    .allow_list_clearing(true)
                    .inherit(false);
                assume_cvsignore = true;
            }
            'e' => {
                if allow_extended {
                    options = options.exclude_filter_file(true);
                } else {
                    let message = format!(
                        "merge directive '{}' uses unsupported modifier '{}'",
                        directive, modifier
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            'n' => {
                if allow_extended {
                    options = options.inherit(false);
                } else {
                    let message = format!(
                        "merge directive '{}' uses unsupported modifier '{}'",
                        directive, modifier
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            'w' => {
                if allow_extended {
                    options = options.use_whitespace().allow_comments(false);
                } else {
                    let message = format!(
                        "merge directive '{}' uses unsupported modifier '{}'",
                        directive, modifier
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            's' => {
                if allow_extended {
                    options = options.sender_modifier();
                } else {
                    let message = format!(
                        "merge directive '{}' uses unsupported modifier '{}'",
                        directive, modifier
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            'r' => {
                if allow_extended {
                    options = options.receiver_modifier();
                } else {
                    let message = format!(
                        "merge directive '{}' uses unsupported modifier '{}'",
                        directive, modifier
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            '/' => {
                if allow_extended {
                    options = options.anchor_root(true);
                } else {
                    let message = format!(
                        "merge directive '{}' uses unsupported modifier '{}'",
                        directive, modifier
                    );
                    return Err(FilterParseError::new(message));
                }
            }
            _ => {
                let message = format!(
                    "{label} directive '{}' uses unsupported modifier '{}'",
                    directive, modifier
                );
                return Err(FilterParseError::new(message));
            }
        }
    }

    options = options.with_enforced_kind(enforced);
    if !allow_extended && !options.list_clear_allowed() {
        options = options.allow_list_clearing(true);
    }

    Ok((options, assume_cvsignore))
}

fn parse_dir_merge_directive(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    const DIR_MERGE_PREFIX: &str = "dir-merge";

    if text.len() < DIR_MERGE_PREFIX.len() {
        return Ok(None);
    }

    let (prefix, mut remainder) = text.split_at(DIR_MERGE_PREFIX.len());
    if !prefix.eq_ignore_ascii_case(DIR_MERGE_PREFIX) {
        return Ok(None);
    }

    if let Some(ch) = remainder.chars().next() {
        if ch != ',' && !ch.is_ascii_whitespace() {
            return Ok(None);
        }
    }

    remainder = remainder.trim_start();

    let mut modifiers = "";
    if let Some(rest) = remainder.strip_prefix(',') {
        let mut split = rest.splitn(2, char::is_whitespace);
        modifiers = split.next().unwrap_or("");
        remainder = split.next().unwrap_or("").trim_start();
    }

    let mut options = DirMergeOptions::default();
    let mut saw_plus = false;
    let mut saw_minus = false;
    let mut used_cvs_default = false;

    for modifier in modifiers.chars() {
        let lower = modifier.to_ascii_lowercase();
        match lower {
            '-' => {
                if saw_plus {
                    let message = format!(
                        "dir-merge directive '{}' cannot combine '+' and '-' modifiers",
                        text
                    );
                    return Err(FilterParseError::new(message));
                }
                saw_minus = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
            }
            '+' => {
                if saw_minus {
                    let message = format!(
                        "dir-merge directive '{}' cannot combine '+' and '-' modifiers",
                        text
                    );
                    return Err(FilterParseError::new(message));
                }
                saw_plus = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Include));
            }
            'n' => {
                options = options.inherit(false);
            }
            'e' => {
                options = options.exclude_filter_file(true);
            }
            'w' => {
                options = options.use_whitespace();
                options = options.allow_comments(false);
            }
            's' => {
                options = options.sender_modifier();
            }
            'r' => {
                options = options.receiver_modifier();
            }
            '/' => {
                options = options.anchor_root(true);
            }
            'c' => {
                used_cvs_default = true;
                options = options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude));
                options = options.use_whitespace();
                options = options.allow_comments(false);
                options = options.inherit(false);
                options = options.allow_list_clearing(true);
            }
            _ => {
                let message = format!(
                    "dir-merge directive '{}' uses unsupported modifier '{}'",
                    text, modifier
                );
                return Err(FilterParseError::new(message));
            }
        }
    }

    let path_text = if remainder.is_empty() {
        if used_cvs_default {
            ".cvsignore"
        } else {
            let message = format!("dir-merge directive '{}' is missing a file name", text);
            return Err(FilterParseError::new(message));
        }
    } else {
        remainder
    };

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(path_text),
        options: Some(options),
    }))
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

#[cfg(feature = "acl")]
fn sync_acls_if_requested(
    preserve_acls: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), LocalCopyError> {
    if preserve_acls && !mode.is_dry_run() {
        sync_acls(source, destination, follow_symlinks).map_err(map_metadata_error)?;
    }
    Ok(())
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

    /// Constructs an error representing an inactivity timeout.
    #[must_use]
    pub fn timeout(duration: Duration) -> Self {
        Self::new(LocalCopyErrorKind::Timeout { duration })
    }

    /// Returns the exit code that mirrors upstream rsync's behaviour.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self.kind {
            LocalCopyErrorKind::MissingSourceOperands => MISSING_OPERANDS_EXIT_CODE,
            LocalCopyErrorKind::InvalidArgument(_) | LocalCopyErrorKind::Io { .. } => {
                INVALID_OPERAND_EXIT_CODE
            }
            LocalCopyErrorKind::Timeout { .. } => TIMEOUT_EXIT_CODE,
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
            LocalCopyErrorKind::Timeout { duration } => {
                write!(
                    f,
                    "transfer timed out after {:.3} seconds without progress",
                    duration.as_secs_f64()
                )
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
    /// The transfer exceeded the configured inactivity timeout.
    Timeout {
        /// Duration of inactivity that triggered the timeout.
        duration: Duration,
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
            Self::Timeout { .. } => None,
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
            Self::RemoteOperandUnsupported => concat!(
                "remote operands are not supported: this build handles local filesystem copies only; ",
                "set OC_RSYNC_FALLBACK to point to an upstream rsync binary for remote transfers",
            ),
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
    symlink_to_dir: bool,
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

fn follow_symlink_metadata(path: &Path) -> Result<fs::Metadata, LocalCopyError> {
    fs::metadata(path)
        .map_err(|error| LocalCopyError::io("inspect symlink target", path.to_path_buf(), error))
}

fn copy_sources(
    plan: &LocalCopyPlan,
    mode: LocalCopyExecution,
    options: LocalCopyOptions,
    handler: Option<&mut dyn LocalCopyRecordHandler>,
) -> Result<CopyOutcome, LocalCopyError> {
    let destination_root = plan.destination.path().to_path_buf();
    let mut context = CopyContext::new(mode, options, handler, destination_root);
    let result = {
        let context = &mut context;
        (|| -> Result<(), LocalCopyError> {
            let multiple_sources = plan.sources.len() > 1;
            let destination_path = plan.destination.path();
            let mut destination_state = query_destination_state(destination_path)?;
            if context.keep_dirlinks_enabled() && destination_state.symlink_to_dir {
                destination_state.is_dir = true;
            }

            if plan.destination.force_directory() {
                ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                )?;
            }

            if multiple_sources {
                ensure_destination_directory(
                    destination_path,
                    &mut destination_state,
                    context.mode(),
                )?;
            }

            let destination_behaves_like_directory =
                destination_state.is_dir || plan.destination.force_directory();

            let relative_enabled = context.relative_paths_enabled();

            for source in &plan.sources {
                context.enforce_timeout()?;
                let source_path = source.path();
                let metadata_start = Instant::now();
                let metadata = fs::symlink_metadata(source_path).map_err(|error| {
                    LocalCopyError::io("access source", source_path.to_path_buf(), error)
                })?;
                context.record_file_list_generation(metadata_start.elapsed());
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
                            context,
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
                        LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::DirectoryNameUnavailable,
                        )
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
                        context,
                        source_path,
                        &target,
                        &metadata,
                        non_empty_path(relative.as_path()),
                    )?;
                } else {
                    let name = source_path.file_name().ok_or_else(|| {
                        LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::FileNameUnavailable,
                        )
                    })?;
                    let relative = relative_root
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(Path::new(name)));
                    let followed_metadata = if file_type.is_symlink()
                        && (context.copy_links_enabled() || context.copy_dirlinks_enabled())
                    {
                        match follow_symlink_metadata(source_path) {
                            Ok(target_metadata) => Some(target_metadata),
                            Err(error) => {
                                if context.copy_links_enabled() {
                                    return Err(error);
                                }
                                None
                            }
                        }
                    } else {
                        None
                    };

                    let (effective_metadata, effective_type) =
                        if let Some(ref target_metadata) = followed_metadata {
                            let ty = target_metadata.file_type();
                            if context.copy_links_enabled()
                                || (context.copy_dirlinks_enabled() && ty.is_dir())
                            {
                                (target_metadata, ty)
                            } else {
                                (&metadata, file_type)
                            }
                        } else {
                            (&metadata, file_type)
                        };

                    if !context.allows(&relative, effective_type.is_dir()) {
                        continue;
                    }

                    let target = if destination_behaves_like_directory {
                        destination_base.join(name)
                    } else {
                        destination_path.to_path_buf()
                    };

                    let record_path = non_empty_path(relative.as_path());
                    if effective_type.is_file() {
                        copy_file(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            record_path,
                        )?;
                    } else if effective_type.is_dir() {
                        copy_directory_recursive(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            non_empty_path(relative.as_path()),
                        )?;
                    } else if file_type.is_symlink() && !context.copy_links_enabled() {
                        copy_symlink(
                            context,
                            source_path,
                            &target,
                            &metadata,
                            &metadata_options,
                            record_path,
                        )?;
                    } else if is_fifo(&effective_type) {
                        if !context.specials_enabled() {
                            context.record_skipped_non_regular(record_path);
                            continue;
                        }
                        copy_fifo(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            &metadata_options,
                            record_path,
                        )?;
                    } else if is_device(&effective_type) {
                        if !context.devices_enabled() {
                            context.record_skipped_non_regular(record_path);
                            continue;
                        }
                        copy_device(
                            context,
                            source_path,
                            &target,
                            effective_metadata,
                            &metadata_options,
                            record_path,
                        )?;
                    } else {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::UnsupportedFileType,
                        ));
                    }
                }

                context.enforce_timeout()?;
            }

            context.flush_deferred_updates()?;
            context.flush_deferred_deletions()?;
            context.enforce_timeout()?;
            Ok(())
        })()
    };

    match result {
        Ok(()) => Ok(context.into_outcome()),
        Err(error) => {
            context.rollback_on_error(&error);
            Err(error)
        }
    }
}
fn query_destination_state(path: &Path) -> Result<DestinationState, LocalCopyError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            let symlink_to_dir = if file_type.is_symlink() {
                follow_symlink_metadata(path)
                    .map(|target| target.file_type().is_dir())
                    .unwrap_or(false)
            } else {
                false
            };

            Ok(DestinationState {
                exists: true,
                is_dir: file_type.is_dir(),
                symlink_to_dir,
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
) -> Result<bool, LocalCopyError> {
    #[cfg(any(feature = "acl", feature = "xattr"))]
    let mode = context.mode();
    #[cfg(not(any(feature = "acl", feature = "xattr")))]
    let _mode = context.mode();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(feature = "acl")]
    let preserve_acls = context.acls_enabled();
    let prune_enabled = context.prune_empty_dirs_enabled();

    let mut destination_missing = false;

    let keep_dirlinks = context.keep_dirlinks_enabled();

    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            let file_type = existing.file_type();
            if file_type.is_dir() {
                // Directory already present; nothing to do.
            } else if file_type.is_symlink() && keep_dirlinks {
                let target_metadata = follow_symlink_metadata(destination)?;
                if !target_metadata.file_type().is_dir() {
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                    ));
                }
            } else {
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

    let list_start = Instant::now();
    let entries = read_directory_entries_sorted(source)?;
    context.record_file_list_generation(list_start.elapsed());
    context.register_progress();

    let dir_merge_guard = context.enter_directory(source)?;
    if dir_merge_guard.is_excluded() {
        return Ok(false);
    }
    let _dir_merge_guard = dir_merge_guard;

    let directory_ready = Cell::new(!destination_missing);
    let mut created_directory_on_disk = false;
    let creation_record_pending = destination_missing && relative.is_some();
    let mut pending_record: Option<LocalCopyRecord> = None;
    let metadata_record = relative.map(|rel| {
        (
            rel.to_path_buf(),
            LocalCopyMetadata::from_metadata(metadata, None),
        )
    });

    let mut kept_any = !prune_enabled;

    let mut ensure_directory = |context: &mut CopyContext| -> Result<(), LocalCopyError> {
        if directory_ready.get() {
            return Ok(());
        }

        if context.mode().is_dry_run() {
            if !context.implied_dirs_enabled() {
                if let Some(parent) = destination.parent() {
                    context.prepare_parent_directory(parent)?;
                }
            }
            directory_ready.set(true);
        } else {
            if let Some(parent) = destination.parent() {
                context.prepare_parent_directory(parent)?;
            }
            if context.implied_dirs_enabled() {
                fs::create_dir_all(destination).map_err(|error| {
                    LocalCopyError::io("create directory", destination.to_path_buf(), error)
                })?;
            } else {
                fs::create_dir(destination).map_err(|error| {
                    LocalCopyError::io("create directory", destination.to_path_buf(), error)
                })?;
            }
            context.register_progress();
            context.register_created_path(destination, CreatedEntryKind::Directory, false);
            directory_ready.set(true);
            created_directory_on_disk = true;
        }

        if pending_record.is_none() {
            if let Some((ref rel_path, ref snapshot)) = metadata_record {
                pending_record = Some(LocalCopyRecord::new(
                    rel_path.clone(),
                    LocalCopyAction::DirectoryCreated,
                    0,
                    Some(snapshot.len()),
                    Duration::default(),
                    Some(snapshot.clone()),
                ));
            }
        }

        Ok(())
    };

    if !directory_ready.get() && !prune_enabled {
        ensure_directory(context)?;
    }

    #[derive(Clone, Copy)]
    enum EntryAction {
        SkipExcluded,
        SkipNonRegular,
        CopyDirectory,
        CopyFile,
        CopySymlink,
        CopyFifo,
        CopyDevice,
    }

    struct PlannedEntry<'a> {
        entry: &'a DirectoryEntry,
        relative: PathBuf,
        action: EntryAction,
        metadata_override: Option<fs::Metadata>,
    }

    impl<'a> PlannedEntry<'a> {
        fn metadata(&self) -> &fs::Metadata {
            self.metadata_override
                .as_ref()
                .unwrap_or(&self.entry.metadata)
        }
    }

    let deletion_enabled = context.options().delete_extraneous();
    let delete_timing = context.delete_timing();
    let mut keep_names = if deletion_enabled {
        Vec::with_capacity(entries.len())
    } else {
        Vec::new()
    };
    let mut planned_entries = Vec::with_capacity(entries.len());

    for entry in entries.iter() {
        context.enforce_timeout()?;
        context.register_progress();

        let file_name = entry.file_name.clone();
        let entry_metadata = &entry.metadata;
        let entry_type = entry_metadata.file_type();
        let mut metadata_override = None;
        let mut effective_type = entry_type;
        if entry_type.is_symlink()
            && (context.copy_links_enabled() || context.copy_dirlinks_enabled())
        {
            match follow_symlink_metadata(entry.path.as_path()) {
                Ok(target_metadata) => {
                    let target_type = target_metadata.file_type();
                    if context.copy_links_enabled()
                        || (context.copy_dirlinks_enabled() && target_type.is_dir())
                    {
                        effective_type = target_type;
                        metadata_override = Some(target_metadata);
                    }
                }
                Err(error) => {
                    if context.copy_links_enabled() {
                        return Err(error);
                    }
                }
            }
        }
        let relative_path = match relative {
            Some(base) => base.join(Path::new(&file_name)),
            None => PathBuf::from(Path::new(&file_name)),
        };

        let mut keep_name = true;

        let action = if !context.allows(&relative_path, effective_type.is_dir()) {
            // Skip excluded entries while optionally allowing deletion sweeps to remove them.
            if context.options().delete_excluded_enabled() {
                keep_name = false;
            }
            EntryAction::SkipExcluded
        } else if entry_type.is_dir() {
            EntryAction::CopyDirectory
        } else if effective_type.is_file() {
            EntryAction::CopyFile
        } else if effective_type.is_dir() {
            EntryAction::CopyDirectory
        } else if entry_type.is_symlink() && !context.copy_links_enabled() {
            EntryAction::CopySymlink
        } else if is_fifo(&effective_type) {
            if context.specials_enabled() {
                EntryAction::CopyFifo
            } else {
                keep_name = false;
                EntryAction::SkipNonRegular
            }
        } else if is_device(&effective_type) {
            if context.devices_enabled() {
                EntryAction::CopyDevice
            } else {
                keep_name = false;
                EntryAction::SkipNonRegular
            }
        } else {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::UnsupportedFileType,
            ));
        };

        if deletion_enabled && keep_name {
            let preserve_name = match delete_timing {
                Some(DeleteTiming::Before) => matches!(
                    action,
                    EntryAction::CopyDirectory | EntryAction::SkipExcluded
                ),
                _ => true,
            };

            if preserve_name {
                keep_names.push(file_name.clone());
            }
        }

        planned_entries.push(PlannedEntry {
            entry,
            relative: relative_path,
            action,
            metadata_override,
        });
    }

    if deletion_enabled && matches!(delete_timing, Some(DeleteTiming::Before)) {
        delete_extraneous_entries(context, destination, relative, &keep_names)?;
    }

    for planned in planned_entries {
        let file_name = &planned.entry.file_name;
        let target_path = destination.join(Path::new(file_name));
        let entry_metadata = planned.metadata();
        let record_relative = non_empty_path(planned.relative.as_path());

        match planned.action {
            EntryAction::SkipExcluded => {}
            EntryAction::SkipNonRegular => {
                context.record_skipped_non_regular(record_relative);
            }
            EntryAction::CopyDirectory => {
                ensure_directory(context)?;
                let child_kept = copy_directory_recursive(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    Some(planned.relative.as_path()),
                )?;
                if child_kept {
                    kept_any = true;
                }
            }
            EntryAction::CopyFile => {
                ensure_directory(context)?;
                copy_file(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
            EntryAction::CopySymlink => {
                ensure_directory(context)?;
                let metadata_options = context.metadata_options();
                copy_symlink(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    &metadata_options,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
            EntryAction::CopyFifo => {
                ensure_directory(context)?;
                let metadata_options = context.metadata_options();
                copy_fifo(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    &metadata_options,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
            EntryAction::CopyDevice => {
                ensure_directory(context)?;
                let metadata_options = context.metadata_options();
                copy_device(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    &metadata_options,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
        }
    }

    if deletion_enabled {
        match delete_timing.unwrap_or(DeleteTiming::During) {
            DeleteTiming::Before => {}
            DeleteTiming::During => {
                delete_extraneous_entries(context, destination, relative, &keep_names)?;
            }
            DeleteTiming::Delay | DeleteTiming::After => {
                let relative_owned = relative.map(Path::to_path_buf);
                context.defer_deletion(destination.to_path_buf(), relative_owned, keep_names);
            }
        }
    }

    if prune_enabled && !kept_any {
        if created_directory_on_disk {
            fs::remove_dir(destination).map_err(|error| {
                LocalCopyError::io("remove empty directory", destination.to_path_buf(), error)
            })?;
            if let Some(last) = context.created_entries.last() {
                if last.path == destination {
                    context.created_entries.pop();
                }
            }
        }
        return Ok(false);
    }

    context.summary_mut().record_directory_total();
    if creation_record_pending {
        context.summary_mut().record_directory();
    }
    if let Some(record) = pending_record {
        context.record(record);
    }

    if !context.mode().is_dry_run() {
        let metadata_options = if context.omit_dir_times_enabled() {
            context.metadata_options().preserve_times(false)
        } else {
            context.metadata_options()
        };
        apply_directory_metadata_with_options(destination, metadata, metadata_options)
            .map_err(map_metadata_error)?;
        #[cfg(feature = "xattr")]
        sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
        #[cfg(feature = "acl")]
        sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
    }

    Ok(true)
}

#[derive(Debug)]
enum ReferenceDecision {
    Skip,
    Copy(PathBuf),
    Link(PathBuf),
}

fn resolve_reference_candidate(base: &Path, relative: &Path, destination: &Path) -> PathBuf {
    if base.is_absolute() {
        base.join(relative)
    } else {
        let mut ancestor = destination.to_path_buf();
        let depth = relative.components().count();
        for _ in 0..depth {
            if !ancestor.pop() {
                break;
            }
        }
        ancestor.join(base).join(relative)
    }
}

fn find_reference_action(
    context: &CopyContext<'_>,
    destination: &Path,
    relative: &Path,
    source: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    size_only: bool,
    checksum: bool,
) -> Result<Option<ReferenceDecision>, LocalCopyError> {
    for reference in context.reference_directories() {
        let candidate = resolve_reference_candidate(reference.path(), relative, destination);
        let candidate_metadata = match fs::symlink_metadata(&candidate) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect reference file",
                    candidate,
                    error,
                ));
            }
        };

        if !candidate_metadata.file_type().is_file() {
            continue;
        }

        if should_skip_copy(
            source,
            metadata,
            &candidate,
            &candidate_metadata,
            metadata_options,
            size_only,
            checksum,
        ) {
            return Ok(Some(match reference.kind() {
                ReferenceDirectoryKind::Compare => ReferenceDecision::Skip,
                ReferenceDirectoryKind::Copy => ReferenceDecision::Copy(candidate),
                ReferenceDirectoryKind::Link => ReferenceDecision::Link(candidate),
            }));
        }
    }

    Ok(None)
}

fn maybe_preallocate_destination(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
    existing_bytes: u64,
    enabled: bool,
) -> Result<(), LocalCopyError> {
    if !enabled || total_len == 0 || total_len <= existing_bytes {
        return Ok(());
    }

    preallocate_destination_file(file, path, total_len)
}

fn preallocate_destination_file(
    file: &mut fs::File,
    path: &Path,
    total_len: u64,
) -> Result<(), LocalCopyError> {
    #[cfg(unix)]
    {
        if total_len == 0 {
            return Ok(());
        }

        if total_len > i64::MAX as u64 {
            return Err(LocalCopyError::io(
                "preallocate destination file",
                path.to_path_buf(),
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "preallocation size exceeds platform limit",
                ),
            ));
        }

        let fd = file.as_fd();
        match fallocate(&fd, FallocateFlags::empty(), 0, total_len) {
            Ok(()) => Ok(()),
            Err(errno) if matches!(errno, Errno::OPNOTSUPP | Errno::NOSYS | Errno::INVAL) => {
                file.set_len(total_len).map_err(|error| {
                    LocalCopyError::io("preallocate destination file", path.to_path_buf(), error)
                })
            }
            Err(errno) => Err(LocalCopyError::io(
                "preallocate destination file",
                path.to_path_buf(),
                io::Error::from_raw_os_error(errno.raw_os_error()),
            )),
        }
    }

    #[cfg(not(unix))]
    {
        if total_len == 0 {
            return Ok(());
        }

        file.set_len(total_len).map_err(|error| {
            LocalCopyError::io("preallocate destination file", path.to_path_buf(), error)
        })
    }
}

fn copy_file(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    context.enforce_timeout()?;
    let metadata_options = context.metadata_options();
    let mode = context.mode();
    let file_type = metadata.file_type();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(feature = "acl")]
    let preserve_acls = context.acls_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| source.file_name().map(PathBuf::from))
        .unwrap_or_else(|| {
            destination
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_default()
        });
    let file_size = metadata.len();
    context.summary_mut().record_regular_file_total();
    context.summary_mut().record_total_bytes(file_size);
    if let Some(parent) = destination.parent() {
        context.prepare_parent_directory(parent)?;
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

    let destination_previously_existed = existing_metadata.is_some();

    if mode.is_dry_run() {
        if context.update_enabled() {
            if let Some(existing) = existing_metadata.as_ref() {
                if destination_is_newer(metadata, existing) {
                    context.summary_mut().record_regular_file_skipped_newer();
                    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                    let total_bytes = Some(metadata_snapshot.len());
                    context.record(LocalCopyRecord::new(
                        record_path.clone(),
                        LocalCopyAction::SkippedNewerDestination,
                        0,
                        total_bytes,
                        Duration::default(),
                        Some(metadata_snapshot),
                    ));
                    return Ok(());
                }
            }
        }

        if context.ignore_existing_enabled() && existing_metadata.is_some() {
            context.summary_mut().record_regular_file_ignored_existing();
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                record_path.clone(),
                LocalCopyAction::SkippedExisting,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
            return Ok(());
        }

        let mut reader = fs::File::open(source)
            .map_err(|error| LocalCopyError::io("open source file", source.to_path_buf(), error))?;

        let append_mode = determine_append_mode(
            context.append_enabled(),
            context.append_verify_enabled(),
            &mut reader,
            source,
            destination,
            existing_metadata.as_ref(),
            file_size,
        )?;
        let append_offset = match append_mode {
            AppendMode::Append(offset) => offset,
            AppendMode::Disabled => 0,
        };
        let bytes_transferred = file_size.saturating_sub(append_offset);

        context
            .summary_mut()
            .record_file(file_size, bytes_transferred, None);
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(
            LocalCopyRecord::new(
                record_path.clone(),
                LocalCopyAction::DataCopied,
                bytes_transferred,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            )
            .with_creation(!destination_previously_existed),
        );
        remove_source_entry_if_requested(context, source, Some(record_path.as_path()), file_type)?;
        return Ok(());
    }

    if context.update_enabled() {
        if let Some(existing) = existing_metadata.as_ref() {
            if destination_is_newer(metadata, existing) {
                context.summary_mut().record_regular_file_skipped_newer();
                context.hard_links.record(metadata, destination);
                let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                let total_bytes = Some(metadata_snapshot.len());
                context.record(LocalCopyRecord::new(
                    record_path.clone(),
                    LocalCopyAction::SkippedNewerDestination,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(metadata_snapshot),
                ));
                return Ok(());
            }
        }
    }

    if context.ignore_existing_enabled() && existing_metadata.is_some() {
        context.summary_mut().record_regular_file_ignored_existing();
        context.hard_links.record(metadata, destination);
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.clone(),
            LocalCopyAction::SkippedExisting,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        return Ok(());
    }

    let use_sparse_writes = context.sparse_enabled();
    let partial_enabled = context.partial_enabled();
    let inplace_enabled = context.inplace_enabled();
    let checksum_enabled = context.checksum_enabled();
    let size_only_enabled = context.size_only_enabled();
    let append_allowed = context.append_enabled();
    let append_verify = context.append_verify_enabled();
    let whole_file_enabled = context.whole_file_enabled();
    let compress_enabled = context.compress_enabled();
    let relative_for_link = relative.unwrap_or(record_path.as_path());

    if let Some(link_target) = context.link_dest_target(
        relative_for_link,
        source,
        metadata,
        &metadata_options,
        size_only_enabled,
        checksum_enabled,
    )? {
        let mut attempted_commit = false;
        loop {
            match fs::hard_link(&link_target, destination) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    fs::remove_file(destination).map_err(|remove_error| {
                        LocalCopyError::io(
                            "remove existing destination",
                            destination.to_path_buf(),
                            remove_error,
                        )
                    })?;
                    fs::hard_link(&link_target, destination).map_err(|link_error| {
                        LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            link_error,
                        )
                    })?;
                    break;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && context.delay_updates_enabled()
                        && !attempted_commit =>
                {
                    context.commit_deferred_update_for(&link_target)?;
                    attempted_commit = true;
                    continue;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "create hard link",
                        destination.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        context.hard_links.record(metadata, destination);
        context.summary_mut().record_hard_link();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.clone(),
            LocalCopyAction::HardLink,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        context.register_created_path(
            destination,
            CreatedEntryKind::HardLink,
            destination_previously_existed,
        );
        remove_source_entry_if_requested(context, source, Some(record_path.as_path()), file_type)?;
        return Ok(());
    }
    let mut copy_source_override: Option<PathBuf> = None;

    if let Some(existing_target) = context.hard_links.existing_target(metadata) {
        let mut attempted_commit = false;
        loop {
            match create_hard_link(&existing_target, destination) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    fs::remove_file(destination).map_err(|remove_error| {
                        LocalCopyError::io(
                            "remove existing destination",
                            destination.to_path_buf(),
                            remove_error,
                        )
                    })?;
                    create_hard_link(&existing_target, destination).map_err(|link_error| {
                        LocalCopyError::io(
                            "create hard link",
                            destination.to_path_buf(),
                            link_error,
                        )
                    })?;
                    break;
                }
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && context.delay_updates_enabled()
                        && !attempted_commit =>
                {
                    context.commit_deferred_update_for(&existing_target)?;
                    attempted_commit = true;
                    continue;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "create hard link",
                        destination.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        context.hard_links.record(metadata, destination);
        context.summary_mut().record_hard_link();
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            record_path.clone(),
            LocalCopyAction::HardLink,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
        context.register_created_path(
            destination,
            CreatedEntryKind::HardLink,
            destination_previously_existed,
        );
        remove_source_entry_if_requested(context, source, Some(record_path.as_path()), file_type)?;
        return Ok(());
    }

    if !context.reference_directories().is_empty() && !record_path.as_os_str().is_empty() {
        if let Some(decision) = find_reference_action(
            context,
            destination,
            record_path.as_path(),
            source,
            metadata,
            &metadata_options,
            size_only_enabled,
            checksum_enabled,
        )? {
            match decision {
                ReferenceDecision::Skip => {
                    context.summary_mut().record_regular_file_matched();
                    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                    let total_bytes = Some(metadata_snapshot.len());
                    context.record(LocalCopyRecord::new(
                        record_path.clone(),
                        LocalCopyAction::MetadataReused,
                        0,
                        total_bytes,
                        Duration::default(),
                        Some(metadata_snapshot),
                    ));
                    context.register_progress();
                    remove_source_entry_if_requested(
                        context,
                        source,
                        Some(record_path.as_path()),
                        file_type,
                    )?;
                    return Ok(());
                }
                ReferenceDecision::Copy(path) => {
                    copy_source_override = Some(path);
                }
                ReferenceDecision::Link(path) => {
                    if existing_metadata.is_some() {
                        fs::remove_file(destination).map_err(|error| {
                            LocalCopyError::io(
                                "remove existing destination",
                                destination.to_path_buf(),
                                error,
                            )
                        })?;
                    }

                    let link_result = create_hard_link(&path, destination);
                    let mut degrade_to_copy = false;
                    match link_result {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                            fs::remove_file(destination).map_err(|remove_error| {
                                LocalCopyError::io(
                                    "remove existing destination",
                                    destination.to_path_buf(),
                                    remove_error,
                                )
                            })?;
                            create_hard_link(&path, destination).map_err(|link_error| {
                                LocalCopyError::io(
                                    "create hard link",
                                    destination.to_path_buf(),
                                    link_error,
                                )
                            })?;
                        }
                        Err(error)
                            if matches!(
                                error.raw_os_error(),
                                Some(code) if code == CROSS_DEVICE_ERROR_CODE
                            ) =>
                        {
                            degrade_to_copy = true;
                        }
                        Err(error) => {
                            return Err(LocalCopyError::io(
                                "create hard link",
                                destination.to_path_buf(),
                                error,
                            ));
                        }
                    }

                    if degrade_to_copy {
                        copy_source_override = Some(path);
                    } else if copy_source_override.is_none() {
                        apply_file_metadata_with_options(
                            destination,
                            metadata,
                            metadata_options.clone(),
                        )
                        .map_err(map_metadata_error)?;
                        #[cfg(feature = "xattr")]
                        sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
                        #[cfg(feature = "acl")]
                        sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
                        context.hard_links.record(metadata, destination);
                        context.summary_mut().record_hard_link();
                        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
                        let total_bytes = Some(metadata_snapshot.len());
                        context.record(LocalCopyRecord::new(
                            record_path.clone(),
                            LocalCopyAction::HardLink,
                            0,
                            total_bytes,
                            Duration::default(),
                            Some(metadata_snapshot),
                        ));
                        context.register_created_path(
                            destination,
                            CreatedEntryKind::HardLink,
                            destination_previously_existed,
                        );
                        context.register_progress();
                        remove_source_entry_if_requested(
                            context,
                            source,
                            Some(record_path.as_path()),
                            file_type,
                        )?;
                        return Ok(());
                    }
                }
            }
        }
    }

    if let Some(existing) = existing_metadata.as_ref() {
        if should_skip_copy(
            source,
            metadata,
            destination,
            existing,
            &metadata_options,
            size_only_enabled,
            checksum_enabled,
        ) {
            apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
                .map_err(map_metadata_error)?;
            #[cfg(feature = "xattr")]
            sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
            #[cfg(feature = "acl")]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
            context.hard_links.record(metadata, destination);
            context.summary_mut().record_regular_file_matched();
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                record_path.clone(),
                LocalCopyAction::MetadataReused,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
            return Ok(());
        }
    }

    let mut reader = fs::File::open(source)
        .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    let append_mode = determine_append_mode(
        append_allowed,
        append_verify,
        &mut reader,
        source,
        destination,
        existing_metadata.as_ref(),
        file_size,
    )?;
    let append_offset = match append_mode {
        AppendMode::Append(offset) => offset,
        AppendMode::Disabled => 0,
    };
    reader
        .seek(SeekFrom::Start(append_offset))
        .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    let delta_signature = if append_offset == 0 && !whole_file_enabled && !inplace_enabled {
        match existing_metadata.as_ref() {
            Some(existing) if existing.is_file() => build_delta_signature(destination, existing)?,
            _ => None,
        }
    } else {
        None
    };

    let copy_source = copy_source_override.as_deref().unwrap_or(source);
    let mut reader = fs::File::open(copy_source)
        .map_err(|error| LocalCopyError::io("copy file", copy_source.to_path_buf(), error))?;
    if append_offset > 0 {
        reader
            .seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", copy_source.to_path_buf(), error))?;
    }
    let mut guard = None;
    let mut staging_path: Option<PathBuf> = None;

    let mut writer = if append_offset > 0 {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?;
        file.seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?;
        file
    } else if inplace_enabled {
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?
    } else {
        let (new_guard, file) = DestinationWriteGuard::new(
            destination,
            partial_enabled,
            context.partial_directory_path(),
        )?;
        staging_path = Some(new_guard.staging_path().to_path_buf());
        guard = Some(new_guard);
        file
    };
    let preallocate_target = guard
        .as_ref()
        .map(|existing_guard| existing_guard.staging_path())
        .unwrap_or(destination);
    maybe_preallocate_destination(
        &mut writer,
        preallocate_target,
        file_size,
        append_offset,
        context.preallocate_enabled(),
    )?;
    let mut buffer = vec![0u8; COPY_BUFFER_SIZE];

    let start = Instant::now();

    let copy_result = context.copy_file_contents(
        &mut reader,
        &mut writer,
        &mut buffer,
        use_sparse_writes,
        compress_enabled,
        source,
        destination,
        record_path.as_path(),
        delta_signature.as_ref(),
        file_size,
        append_offset,
        start,
    );

    drop(writer);

    let staging_path_for_links = guard
        .as_ref()
        .map(|existing_guard| existing_guard.staging_path().to_path_buf())
        .or_else(|| staging_path.take());
    let delay_updates_enabled = context.delay_updates_enabled();

    let outcome = match copy_result {
        Ok(outcome) => {
            if let Err(timeout_error) = context.enforce_timeout() {
                if let Some(guard) = guard.take() {
                    guard.discard();
                }

                if existing_metadata.is_none() {
                    remove_incomplete_destination(destination);
                }

                return Err(timeout_error);
            }
            outcome
        }
        Err(error) => {
            if let Some(guard) = guard.take() {
                guard.discard();
            }

            if existing_metadata.is_none() {
                remove_incomplete_destination(destination);
            }

            return Err(error);
        }
    };

    context.register_created_path(
        destination,
        CreatedEntryKind::File,
        destination_previously_existed,
    );

    let hard_link_path = if delay_updates_enabled {
        staging_path_for_links.as_deref().unwrap_or(destination)
    } else {
        destination
    };
    context.hard_links.record(metadata, hard_link_path);
    let elapsed = start.elapsed();
    let compressed_bytes = outcome.compressed_bytes();
    context
        .summary_mut()
        .record_file(file_size, outcome.literal_bytes(), compressed_bytes);
    context.summary_mut().record_elapsed(elapsed);
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    let total_bytes = Some(metadata_snapshot.len());
    context.record(
        LocalCopyRecord::new(
            record_path.clone(),
            LocalCopyAction::DataCopied,
            outcome.literal_bytes(),
            total_bytes,
            elapsed,
            Some(metadata_snapshot),
        )
        .with_creation(!destination_previously_existed),
    );

    if let Err(timeout_error) = context.enforce_timeout() {
        if existing_metadata.is_none() {
            remove_incomplete_destination(destination);
        }

        return Err(timeout_error);
    }

    let relative_for_removal = Some(record_path.clone());
    if let Some(guard) = guard {
        if delay_updates_enabled {
            let destination_path = guard.final_path().to_path_buf();
            let update = DeferredUpdate {
                guard,
                metadata: metadata.clone(),
                metadata_options,
                mode,
                source: source.to_path_buf(),
                relative: relative_for_removal.clone(),
                destination: destination_path,
                file_type,
                destination_previously_existed,
                #[cfg(feature = "xattr")]
                preserve_xattrs,
                #[cfg(feature = "acl")]
                preserve_acls,
            };
            context.register_deferred_update(update);
        } else {
            let destination_path = guard.final_path().to_path_buf();
            guard.commit()?;
            context.apply_metadata_and_finalize(
                destination_path.as_path(),
                metadata,
                metadata_options,
                mode,
                source,
                relative_for_removal.as_deref(),
                file_type,
                destination_previously_existed,
                #[cfg(feature = "xattr")]
                preserve_xattrs,
                #[cfg(feature = "acl")]
                preserve_acls,
            )?;
        }
    } else {
        context.apply_metadata_and_finalize(
            destination,
            metadata,
            metadata_options,
            mode,
            source,
            relative_for_removal.as_deref(),
            file_type,
            destination_previously_existed,
            #[cfg(feature = "xattr")]
            preserve_xattrs,
            #[cfg(feature = "acl")]
            preserve_acls,
        )?;
    }

    Ok(())
}

enum AppendMode {
    Disabled,
    Append(u64),
}

fn determine_append_mode(
    append_allowed: bool,
    append_verify: bool,
    reader: &mut fs::File,
    source: &Path,
    destination: &Path,
    existing_metadata: Option<&fs::Metadata>,
    file_size: u64,
) -> Result<AppendMode, LocalCopyError> {
    if !append_allowed {
        return Ok(AppendMode::Disabled);
    }

    let existing = match existing_metadata {
        Some(meta) if meta.is_file() => meta,
        _ => return Ok(AppendMode::Disabled),
    };

    let existing_len = existing.len();
    if existing_len == 0 || existing_len >= file_size {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
        return Ok(AppendMode::Disabled);
    }

    if append_verify {
        let matches = verify_append_prefix(reader, source, destination, existing_len)?;
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
        if !matches {
            return Ok(AppendMode::Disabled);
        }
    } else {
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    }

    Ok(AppendMode::Append(existing_len))
}

fn verify_append_prefix(
    reader: &mut fs::File,
    source: &Path,
    destination: &Path,
    existing_len: u64,
) -> Result<bool, LocalCopyError> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    let mut destination_file = fs::File::open(destination).map_err(|error| {
        LocalCopyError::io(
            "read existing destination",
            destination.to_path_buf(),
            error,
        )
    })?;
    let mut remaining = existing_len;
    let mut source_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0u8; COPY_BUFFER_SIZE];

    while remaining > 0 {
        let chunk = remaining.min(COPY_BUFFER_SIZE as u64) as usize;
        let source_read = reader
            .read(&mut source_buffer[..chunk])
            .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
        let destination_read = destination_file
            .read(&mut destination_buffer[..chunk])
            .map_err(|error| {
                LocalCopyError::io(
                    "read existing destination",
                    destination.to_path_buf(),
                    error,
                )
            })?;

        if source_read == 0 || destination_read == 0 || source_read != destination_read {
            return Ok(false);
        }

        if source_buffer[..source_read] != destination_buffer[..destination_read] {
            return Ok(false);
        }

        remaining = remaining.saturating_sub(source_read as u64);
    }

    Ok(true)
}

fn partial_destination_path(destination: &Path) -> PathBuf {
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "partial".to_string());
    let partial_name = format!(".oc-rsync-partial-{}", file_name);
    destination.with_file_name(partial_name)
}

fn partial_directory_destination_path(
    destination: &Path,
    partial_dir: &Path,
) -> Result<PathBuf, LocalCopyError> {
    let base_dir = if partial_dir.is_absolute() {
        partial_dir.to_path_buf()
    } else {
        let parent = destination
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        parent.join(partial_dir)
    };
    fs::create_dir_all(&base_dir)
        .map_err(|error| LocalCopyError::io("create partial directory", base_dir.clone(), error))?;
    let file_name = destination
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| OsStr::new("partial").to_os_string());
    Ok(base_dir.join(file_name))
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
    fn new(
        destination: &Path,
        partial: bool,
        partial_dir: Option<&Path>,
    ) -> Result<(Self, fs::File), LocalCopyError> {
        if partial {
            let temp_path = if let Some(dir) = partial_dir {
                partial_directory_destination_path(destination, dir)?
            } else {
                partial_destination_path(destination)
            };
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

    fn staging_path(&self) -> &Path {
        &self.temp_path
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
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                fs::copy(&self.temp_path, &self.final_path).map_err(|copy_error| {
                    LocalCopyError::io(self.finalise_action(), self.final_path.clone(), copy_error)
                })?;
                fs::remove_file(&self.temp_path).map_err(|remove_error| {
                    LocalCopyError::io(self.finalise_action(), self.temp_path.clone(), remove_error)
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

    fn final_path(&self) -> &Path {
        &self.final_path
    }

    fn discard(mut self) {
        if self.preserve_on_error {
            self.committed = true;
            return;
        }

        if let Err(error) = fs::remove_file(&self.temp_path) {
            if error.kind() != io::ErrorKind::NotFound {
                // Best-effort cleanup: the file may have been removed concurrently.
            }
        }

        self.committed = true;
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

fn remove_incomplete_destination(destination: &Path) {
    if let Err(error) = fs::remove_file(destination) {
        if error.kind() != io::ErrorKind::NotFound {
            // Preserve the original error from the transfer attempt.
        }
    }
}

fn write_sparse_chunk(
    writer: &mut fs::File,
    chunk: &[u8],
    destination: &Path,
) -> Result<usize, LocalCopyError> {
    let mut index = 0usize;
    let mut written = 0usize;

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
            written = written.saturating_add(index - start);
        }
    }

    Ok(written)
}

fn destination_is_newer(source: &fs::Metadata, destination: &fs::Metadata) -> bool {
    match (source.modified(), destination.modified()) {
        (Ok(src), Ok(dst)) => dst > src,
        _ => false,
    }
}

fn build_delta_signature(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<Option<DeltaSignatureIndex>, LocalCopyError> {
    let length = metadata.len();
    if length == 0 {
        return Ok(None);
    }

    let checksum_len = NonZeroU8::new(16).expect("strong checksum length must be non-zero");
    let params = SignatureLayoutParams::new(length, None, ProtocolVersion::NEWEST, checksum_len);
    let layout = match calculate_signature_layout(params) {
        Ok(layout) => layout,
        Err(_) => return Ok(None),
    };

    let signature = match generate_file_signature(
        fs::File::open(destination).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?,
        layout,
        SignatureAlgorithm::Md4,
    ) {
        Ok(signature) => signature,
        Err(SignatureError::Io(error)) => {
            return Err(LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
        Err(_) => return Ok(None),
    };

    match DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4) {
        Some(index) => Ok(Some(index)),
        None => Ok(None),
    }
}

fn should_skip_copy(
    source_path: &Path,
    source: &fs::Metadata,
    destination_path: &Path,
    destination: &fs::Metadata,
    options: &MetadataOptions,
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
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    context.enforce_timeout()?;
    let mode = context.mode();
    let file_type = metadata.file_type();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_fifo_total();
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
                context.register_progress();
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
        if let Some(path) = &record_path {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                path.clone(),
                LocalCopyAction::FifoCopied,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
        context.register_progress();
        remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
        return Ok(());
    }

    let mut destination_previously_existed = false;
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            destination_previously_existed = true;
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
    context.register_created_path(
        destination,
        CreatedEntryKind::Fifo,
        destination_previously_existed,
    );
    apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
    #[cfg(feature = "acl")]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
    context.summary_mut().record_fifo();
    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::FifoCopied,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }
    context.register_progress();
    remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
    Ok(())
}

fn copy_device(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    context.enforce_timeout()?;
    let mode = context.mode();
    let file_type = metadata.file_type();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_device_total();
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
                context.register_progress();
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
        if let Some(path) = &record_path {
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                path.clone(),
                LocalCopyAction::DeviceCopied,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
        context.register_progress();
        remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
        return Ok(());
    }

    let mut destination_previously_existed = false;
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            destination_previously_existed = true;
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
    context.register_created_path(
        destination,
        CreatedEntryKind::Device,
        destination_previously_existed,
    );
    apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
    #[cfg(feature = "acl")]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
    context.summary_mut().record_device();
    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::DeviceCopied,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }
    context.register_progress();
    remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
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
        context.enforce_timeout()?;
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
                None,
                Duration::default(),
                None,
            ));
            context.register_progress();
            continue;
        }

        remove_extraneous_path(path, file_type)?;
        context.summary_mut().record_deletion();
        context.record(LocalCopyRecord::new(
            entry_relative,
            LocalCopyAction::EntryDeleted,
            0,
            None,
            Duration::default(),
            None,
        ));
        context.register_progress();
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

fn remove_source_entry_if_requested(
    context: &mut CopyContext,
    source: &Path,
    record_path: Option<&Path>,
    file_type: fs::FileType,
) -> Result<(), LocalCopyError> {
    if !context.remove_source_files_enabled() || context.mode().is_dry_run() {
        return Ok(());
    }

    let source_type = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata.file_type(),
        Err(_) => file_type,
    };

    if source_type.is_dir() {
        return Ok(());
    }

    match fs::remove_file(source) {
        Ok(()) => {
            context.summary_mut().record_source_removed();
            if let Some(path) = record_path {
                context.record(LocalCopyRecord::new(
                    path.to_path_buf(),
                    LocalCopyAction::SourceRemoved,
                    0,
                    None,
                    Duration::default(),
                    None,
                ));
            }
            context.register_progress();
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalCopyError::io(
            "remove source entry",
            source.to_path_buf(),
            error,
        )),
    }
}

fn copy_symlink(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    relative: Option<&Path>,
) -> Result<(), LocalCopyError> {
    context.enforce_timeout()?;
    let mode = context.mode();
    let file_type = metadata.file_type();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    let record_path = relative
        .map(Path::to_path_buf)
        .or_else(|| destination.file_name().map(PathBuf::from));
    context.summary_mut().record_symlink_total();
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
                context.register_progress();
            }
        }
    }

    let mut destination_previously_existed = false;
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            destination_previously_existed = true;
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
        if let Some(path) = &record_path {
            let metadata_snapshot =
                LocalCopyMetadata::from_metadata(metadata, Some(target.clone()));
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                path.clone(),
                LocalCopyAction::SymlinkCopied,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
        }
        context.register_progress();
        remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
        return Ok(());
    }

    create_symlink(&target, source, destination).map_err(|error| {
        LocalCopyError::io("create symbolic link", destination.to_path_buf(), error)
    })?;

    context.register_created_path(
        destination,
        CreatedEntryKind::Symlink,
        destination_previously_existed,
    );

    apply_symlink_metadata_with_options(destination, metadata, metadata_options.clone())
        .map_err(map_metadata_error)?;
    #[cfg(feature = "xattr")]
    sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, false)?;
    #[cfg(feature = "acl")]
    sync_acls_if_requested(preserve_acls, mode, source, destination, false)?;

    context.summary_mut().record_symlink();
    if let Some(path) = &record_path {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, Some(target.clone()));
        let total_bytes = Some(metadata_snapshot.len());
        context.record(LocalCopyRecord::new(
            path.clone(),
            LocalCopyAction::SymlinkCopied,
            0,
            total_bytes,
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }
    context.register_progress();
    remove_source_entry_if_requested(context, source, record_path.as_deref(), file_type)?;
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

        left.as_bytes().cmp(right.as_bytes())
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let left_wide: Vec<u16> = left.encode_wide().collect();
        let right_wide: Vec<u16> = right.encode_wide().collect();
        left_wide.cmp(&right_wide)
    }

    #[cfg(not(any(unix, windows)))]
    {
        left.to_string_lossy().cmp(&right.to_string_lossy())
    }
}

fn has_trailing_separator(path: &OsStr) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        let bytes = path.as_bytes();
        !bytes.is_empty() && bytes.ends_with(b"/")
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

        file_type.is_fifo()
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

        file_type.is_char_device() || file_type.is_block_device()
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
    use filetime::{FileTime, set_file_mtime, set_file_times};
    use std::ffi::OsString;
    use std::io::{self, Seek, SeekFrom, Write};
    use std::num::{NonZeroU8, NonZeroU64};
    use std::path::Path;
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    #[cfg(feature = "xattr")]
    use xattr;

    #[cfg(all(unix, feature = "acl"))]
    use std::os::unix::ffi::OsStrExt;

    #[cfg(all(unix, feature = "acl"))]
    fn acl_to_text(path: &Path, ty: libc::acl_type_t) -> Option<String> {
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("cstring");
        let acl = unsafe { libc::acl_get_file(c_path.as_ptr(), ty) };
        if acl.is_null() {
            return None;
        }
        let mut len = 0;
        let text_ptr = unsafe { libc::acl_to_text(acl, &mut len) };
        if text_ptr.is_null() {
            unsafe { libc::acl_free(acl as *mut _) };
            return None;
        }
        let slice = unsafe { std::slice::from_raw_parts(text_ptr.cast::<u8>(), len as usize) };
        let text = String::from_utf8_lossy(slice).trim().to_string();
        unsafe {
            libc::acl_free(text_ptr as *mut _);
            libc::acl_free(acl as *mut _);
        }
        Some(text)
    }

    #[cfg(all(unix, feature = "acl"))]
    fn set_acl_from_text(path: &Path, text: &str, ty: libc::acl_type_t) {
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("cstring");
        let c_text = std::ffi::CString::new(text).expect("text");
        let acl = unsafe { libc::acl_from_text(c_text.as_ptr()) };
        assert!(!acl.is_null(), "acl_from_text");
        let result = unsafe { libc::acl_set_file(c_path.as_ptr(), ty, acl) };
        unsafe {
            libc::acl_free(acl as *mut _);
        }
        assert_eq!(result, 0, "acl_set_file");
    }

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
    fn local_copy_options_delete_delay_round_trip() {
        let options = LocalCopyOptions::default().delete_delay(true);
        assert!(options.delete_delay_enabled());
        assert!(!LocalCopyOptions::default().delete_delay_enabled());
    }

    #[test]
    fn local_copy_options_checksum_round_trip() {
        let options = LocalCopyOptions::default().checksum(true);
        assert!(options.checksum_enabled());
        assert!(!LocalCopyOptions::default().checksum_enabled());
    }

    #[test]
    fn local_copy_options_omit_dir_times_round_trip() {
        let options = LocalCopyOptions::default().omit_dir_times(true);
        assert!(options.omit_dir_times_enabled());
        assert!(!LocalCopyOptions::default().omit_dir_times_enabled());
    }

    #[test]
    fn metadata_options_reflect_numeric_ids_setting() {
        let options = LocalCopyOptions::default().numeric_ids(true);
        let context =
            CopyContext::new(LocalCopyExecution::Apply, options, None, PathBuf::from("."));
        assert!(context.metadata_options().numeric_ids_enabled());
    }

    #[test]
    fn metadata_options_carry_chmod_modifiers() {
        let modifiers = ChmodModifiers::parse("a+rw").expect("chmod parses");
        let options = LocalCopyOptions::default().with_chmod(Some(modifiers.clone()));
        let context =
            CopyContext::new(LocalCopyExecution::Apply, options, None, PathBuf::from("."));

        assert_eq!(context.metadata_options().chmod(), Some(&modifiers));
    }

    #[test]
    fn local_copy_options_sparse_round_trip() {
        let options = LocalCopyOptions::default().sparse(true);
        assert!(options.sparse_enabled());
        assert!(!LocalCopyOptions::default().sparse_enabled());
    }

    #[test]
    fn local_copy_options_append_round_trip() {
        let options = LocalCopyOptions::default().append(true);
        assert!(options.append_enabled());
        assert!(!options.append_verify_enabled());
        assert!(!LocalCopyOptions::default().append_enabled());
    }

    #[test]
    fn local_copy_options_append_verify_round_trip() {
        let options = LocalCopyOptions::default().append_verify(true);
        assert!(options.append_enabled());
        assert!(options.append_verify_enabled());
        let disabled = options.append(false);
        assert!(!disabled.append_enabled());
        assert!(!disabled.append_verify_enabled());
    }

    #[test]
    fn local_copy_options_preallocate_round_trip() {
        let options = LocalCopyOptions::default().preallocate(true);
        assert!(options.preallocate_enabled());
        assert!(!LocalCopyOptions::default().preallocate_enabled());
    }

    #[test]
    fn preallocate_destination_reserves_space() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("prealloc.bin");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&path)
            .expect("create file");

        maybe_preallocate_destination(&mut file, &path, 4096, 0, true).expect("preallocate file");

        let metadata = fs::metadata(&path).expect("metadata");
        assert_eq!(metadata.len(), 4096);
    }

    #[test]
    fn local_copy_options_compress_round_trip() {
        let options = LocalCopyOptions::default().compress(true);
        assert!(options.compress_enabled());
        assert!(!LocalCopyOptions::default().compress_enabled());
    }

    #[test]
    fn local_copy_options_whole_file_round_trip() {
        assert!(LocalCopyOptions::default().whole_file_enabled());
        let delta = LocalCopyOptions::default().whole_file(false);
        assert!(!delta.whole_file_enabled());
        let restored = delta.whole_file(true);
        assert!(restored.whole_file_enabled());
    }

    #[test]
    fn local_copy_options_copy_links_round_trip() {
        let options = LocalCopyOptions::default().copy_links(true);
        assert!(options.copy_links_enabled());
        assert!(!LocalCopyOptions::default().copy_links_enabled());
    }

    #[test]
    fn local_copy_options_timeout_round_trip() {
        let timeout = Duration::from_secs(5);
        let options = LocalCopyOptions::default().with_timeout(Some(timeout));
        assert_eq!(options.timeout(), Some(timeout));
        assert!(LocalCopyOptions::default().timeout().is_none());
    }

    #[test]
    fn local_copy_options_compression_override_round_trip() {
        let level = NonZeroU8::new(5).expect("level");
        let override_level = CompressionLevel::precise(level);
        let options = LocalCopyOptions::default()
            .compress(true)
            .with_compression_level_override(Some(override_level));
        assert_eq!(options.compression_level_override(), Some(override_level));
        assert_eq!(options.compression_level(), override_level);
        assert_eq!(options.effective_compression_level(), Some(override_level));

        let disabled = LocalCopyOptions::default()
            .with_compression_level_override(Some(override_level))
            .compress(false);
        assert_eq!(disabled.compression_level_override(), None);
        assert_eq!(disabled.compression_level(), CompressionLevel::Default);
        assert_eq!(disabled.effective_compression_level(), None);
    }

    #[test]
    fn local_copy_options_compression_level_round_trip() {
        let options =
            LocalCopyOptions::default().with_default_compression_level(CompressionLevel::Best);
        assert_eq!(options.compression_level(), CompressionLevel::Best);
        assert_eq!(
            LocalCopyOptions::default().compression_level(),
            CompressionLevel::Default
        );
    }

    #[test]
    fn local_copy_options_size_only_round_trip() {
        let options = LocalCopyOptions::default().size_only(true);
        assert!(options.size_only_enabled());
        assert!(!LocalCopyOptions::default().size_only_enabled());
    }

    #[test]
    fn local_copy_options_ignore_existing_round_trip() {
        let options = LocalCopyOptions::default().ignore_existing(true);
        assert!(options.ignore_existing_enabled());
        assert!(!LocalCopyOptions::default().ignore_existing_enabled());
    }

    #[test]
    fn local_copy_options_update_round_trip() {
        let options = LocalCopyOptions::default().update(true);
        assert!(options.update_enabled());
        assert!(!LocalCopyOptions::default().update_enabled());
    }

    #[test]
    fn local_copy_options_delay_updates_round_trip() {
        let options = LocalCopyOptions::default().delay_updates(true);
        assert!(options.delay_updates_enabled());
        assert!(!LocalCopyOptions::default().delay_updates_enabled());
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
    fn local_copy_options_implied_dirs_round_trip() {
        let options = LocalCopyOptions::default();
        assert!(options.implied_dirs_enabled());

        let disabled = LocalCopyOptions::default().implied_dirs(false);
        assert!(!disabled.implied_dirs_enabled());

        let reenabled = disabled.implied_dirs(true);
        assert!(reenabled.implied_dirs_enabled());
    }

    #[test]
    fn local_copy_options_mkpath_round_trip() {
        let options = LocalCopyOptions::default();
        assert!(!options.mkpath_enabled());

        let enabled = LocalCopyOptions::default().mkpath(true);
        assert!(enabled.mkpath_enabled());

        let disabled = enabled.mkpath(false);
        assert!(!disabled.mkpath_enabled());
    }

    #[test]
    fn parse_filter_directive_show_is_sender_only() {
        let rule = match parse_filter_directive_line("show images/**").expect("parse") {
            Some(ParsedFilterDirective::Rule(rule)) => rule,
            other => panic!("expected rule, got {:?}", other),
        };

        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_hide_is_sender_only() {
        let rule = match parse_filter_directive_line("hide *.tmp").expect("parse") {
            Some(ParsedFilterDirective::Rule(rule)) => rule,
            other => panic!("expected rule, got {:?}", other),
        };

        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_shorthand_show_is_sender_only() {
        let rule = match parse_filter_directive_line("S logs/**").expect("parse") {
            Some(ParsedFilterDirective::Rule(rule)) => rule,
            other => panic!("expected rule, got {:?}", other),
        };

        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_shorthand_hide_is_sender_only() {
        let rule = match parse_filter_directive_line("H *.bak").expect("parse") {
            Some(ParsedFilterDirective::Rule(rule)) => rule,
            other => panic!("expected rule, got {:?}", other),
        };

        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_shorthand_protect_requires_receiver() {
        let rule = match parse_filter_directive_line("P cache/**").expect("parse") {
            Some(ParsedFilterDirective::Rule(rule)) => rule,
            other => panic!("expected rule, got {:?}", other),
        };

        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_risk_requires_receiver() {
        let rule = match parse_filter_directive_line("risk cache/**").expect("parse") {
            Some(ParsedFilterDirective::Rule(rule)) => rule,
            other => panic!("expected rule, got {:?}", other),
        };

        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_shorthand_risk_requires_receiver() {
        let rule = match parse_filter_directive_line("R cache/**").expect("parse") {
            Some(ParsedFilterDirective::Rule(rule)) => rule,
            other => panic!("expected rule, got {:?}", other),
        };

        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn parse_filter_directive_dir_merge_without_modifiers() {
        let directive = parse_filter_directive_line("dir-merge .rsync-filter")
            .expect("parse")
            .expect("directive");

        let (path, options) = match directive {
            ParsedFilterDirective::Merge { path, options } => (path, options),
            other => panic!("expected dir-merge directive, got {:?}", other),
        };

        assert_eq!(path, PathBuf::from(".rsync-filter"));
        let opts = options.expect("options");
        assert!(opts.inherit_rules());
        assert!(opts.allows_comments());
        assert!(!opts.uses_whitespace());
        assert_eq!(opts.enforced_kind(), None);
    }

    #[test]
    fn parse_filter_directive_dir_merge_with_modifiers() {
        let directive = parse_filter_directive_line("dir-merge,+ne rules/filter.txt")
            .expect("parse")
            .expect("directive");

        let (path, options) = match directive {
            ParsedFilterDirective::Merge { path, options } => (path, options),
            other => panic!("expected dir-merge directive, got {:?}", other),
        };

        assert_eq!(path, PathBuf::from("rules/filter.txt"));
        let opts = options.expect("options");
        assert!(!opts.inherit_rules());
        assert!(opts.excludes_self());
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn parse_filter_directive_dir_merge_cvs_default_path() {
        let directive = parse_filter_directive_line("dir-merge,c")
            .expect("parse")
            .expect("directive");

        let (path, options) = match directive {
            ParsedFilterDirective::Merge { path, options } => (path, options),
            other => panic!("expected dir-merge directive, got {:?}", other),
        };

        assert_eq!(path, PathBuf::from(".cvsignore"));
        let opts = options.expect("options");
        assert!(!opts.inherit_rules());
        assert!(opts.list_clear_allowed());
        assert!(opts.uses_whitespace());
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    }

    #[test]
    fn parse_filter_directive_short_merge_inherits_context() {
        let directive = parse_filter_directive_line(". per-dir")
            .expect("parse")
            .expect("directive");

        let (path, options) = match directive {
            ParsedFilterDirective::Merge { path, options } => (path, options),
            other => panic!("expected merge directive, got {:?}", other),
        };

        assert_eq!(path, PathBuf::from("per-dir"));
        assert!(options.is_none());
    }

    #[test]
    fn parse_filter_directive_short_merge_cvs_defaults() {
        let directive = parse_filter_directive_line(".C")
            .expect("parse")
            .expect("directive");

        let (path, options) = match directive {
            ParsedFilterDirective::Merge { path, options } => (path, options),
            other => panic!("expected merge directive, got {:?}", other),
        };

        assert_eq!(path, PathBuf::from(".cvsignore"));
        let opts = options.expect("options");
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
        assert!(opts.uses_whitespace());
        assert!(!opts.inherit_rules());
    }

    #[test]
    fn parse_filter_directive_short_dir_merge_with_modifiers() {
        let directive = parse_filter_directive_line(":- per-dir")
            .expect("parse")
            .expect("directive");

        let (path, options) = match directive {
            ParsedFilterDirective::Merge { path, options } => (path, options),
            other => panic!("expected dir-merge directive, got {:?}", other),
        };

        assert_eq!(path, PathBuf::from("per-dir"));
        let opts = options.expect("options");
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    }

    #[test]
    fn parse_filter_directive_merge_with_modifiers() {
        let directive = parse_filter_directive_line("merge,+ rules")
            .expect("parse")
            .expect("directive");

        let (path, options) = match directive {
            ParsedFilterDirective::Merge { path, options } => (path, options),
            other => panic!("expected merge directive, got {:?}", other),
        };

        assert_eq!(path, PathBuf::from("rules"));
        let opts = options.expect("options");
        assert_eq!(opts.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn parse_filter_directive_dir_merge_conflicting_modifiers_error() {
        let error = parse_filter_directive_line("dir-merge,+- rules").expect_err("conflict");
        assert!(
            error
                .to_string()
                .contains("cannot combine '+' and '-' modifiers")
        );
    }

    #[test]
    fn deferred_updates_flush_commits_pending_files() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        fs::write(&source, b"payload").expect("write source");
        let destination_root = temp.path().join("dest");
        fs::create_dir_all(&destination_root).expect("create dest root");
        let destination = destination_root.join("file.txt");

        let options = LocalCopyOptions::default()
            .partial(true)
            .delay_updates(true);
        let mut context = CopyContext::new(
            LocalCopyExecution::Apply,
            options,
            None,
            destination_root.clone(),
        );

        let (guard, mut file) =
            DestinationWriteGuard::new(destination.as_path(), true, None).expect("guard");
        file.write_all(b"payload").expect("write temp");
        drop(file);

        let metadata = fs::metadata(&source).expect("metadata");
        let metadata_options = context.metadata_options();
        let partial_path = partial_destination_path(&destination);
        let final_path = guard.final_path().to_path_buf();
        let update = DeferredUpdate {
            guard,
            metadata: metadata.clone(),
            metadata_options,
            mode: LocalCopyExecution::Apply,
            source: source.clone(),
            relative: Some(std::path::PathBuf::from("file.txt")),
            destination: final_path,
            file_type: metadata.file_type(),
            destination_previously_existed: false,
            #[cfg(feature = "xattr")]
            preserve_xattrs: context.xattrs_enabled(),
            #[cfg(feature = "acl")]
            preserve_acls: context.acls_enabled(),
        };

        context.register_deferred_update(update);

        assert!(!destination.exists());
        assert!(partial_path.exists());

        context
            .flush_deferred_updates()
            .expect("deferred updates committed");

        assert!(destination.exists());
        assert_eq!(fs::read(&destination).expect("read dest"), b"payload");
        assert!(!partial_path.exists());
    }

    #[test]
    fn dir_merge_defaults_preserve_rule_side_overrides() {
        let options = DirMergeOptions::default();
        let rule = FilterRule::exclude("*.tmp").with_receiver(false);
        let adjusted = apply_dir_merge_rule_defaults(rule, &options);

        assert!(adjusted.applies_to_sender());
        assert!(!adjusted.applies_to_receiver());
    }

    #[test]
    fn dir_merge_modifiers_override_rule_side_overrides() {
        let sender_only_options = DirMergeOptions::default().sender_modifier();
        let receiver_only_options = DirMergeOptions::default().receiver_modifier();

        let rule = FilterRule::include("logs/**").with_receiver(false);
        let sender_adjusted = apply_dir_merge_rule_defaults(rule.clone(), &sender_only_options);
        assert!(sender_adjusted.applies_to_sender());
        assert!(!sender_adjusted.applies_to_receiver());

        let receiver_adjusted = apply_dir_merge_rule_defaults(rule, &receiver_only_options);
        assert!(!receiver_adjusted.applies_to_sender());
        assert!(receiver_adjusted.applies_to_receiver());
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
        assert_eq!(summary.regular_files_total(), 1);
        assert_eq!(summary.regular_files_matched(), 0);
    }

    struct SleepyHandler {
        slept: bool,
        delay: Duration,
    }

    impl LocalCopyRecordHandler for SleepyHandler {
        fn handle(&mut self, _record: LocalCopyRecord) {
            if !self.slept {
                self.slept = true;
                thread::sleep(self.delay);
            }
        }
    }

    #[test]
    fn local_copy_timeout_enforced() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        fs::write(&source, vec![0u8; 1024]).expect("write source");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let mut handler = SleepyHandler {
            slept: false,
            delay: Duration::from_millis(50),
        };

        let options = LocalCopyOptions::default().with_timeout(Some(Duration::from_millis(5)));
        let error = plan
            .execute_with_options_and_handler(
                LocalCopyExecution::Apply,
                options,
                Some(&mut handler),
            )
            .expect_err("timeout should fail copy");

        assert!(matches!(error.kind(), LocalCopyErrorKind::Timeout { .. }));
        assert!(
            !destination.exists(),
            "destination should not be created on timeout"
        );
    }

    #[test]
    fn execute_with_remove_source_files_deletes_source() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"move me").expect("write source");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().remove_source_files(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.sources_removed(), 1);
        assert!(!source.exists(), "source should be removed");
        assert_eq!(fs::read(destination).expect("read dest"), b"move me");
    }

    #[test]
    fn execute_with_remove_source_files_preserves_unchanged_source() {
        use filetime::{FileTime, set_file_times};

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        let payload = b"stable";
        fs::write(&source, payload).expect("write source");
        fs::write(&destination, payload).expect("write destination");

        let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_times(&source, timestamp, timestamp).expect("set source times");
        set_file_times(&destination, timestamp, timestamp).expect("set dest times");

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .remove_source_files(true)
                    .times(true),
            )
            .expect("execution succeeds");

        assert_eq!(summary.sources_removed(), 0, "unchanged sources remain");
        assert!(source.exists(), "source should remain when unchanged");
        assert!(destination.exists(), "destination remains present");
        assert_eq!(summary.files_copied(), 0);
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

    #[cfg(all(unix, feature = "acl"))]
    #[test]
    fn execute_copies_file_with_acls() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"acl").expect("write source");
        let acl_text = "user::rw-\ngroup::r--\nother::r--\n";
        set_acl_from_text(&source, acl_text, libc::ACL_TYPE_ACCESS);

        let operands = vec![
            source.clone().into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().acls(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 1);
        let copied = acl_to_text(&destination, libc::ACL_TYPE_ACCESS).expect("dest acl");
        assert!(copied.contains("user::rw-"));
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
        assert_eq!(summary.regular_files_total(), 1);
        assert_eq!(summary.regular_files_matched(), 1);
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
        assert_eq!(summary.regular_files_total(), 1);
        assert_eq!(summary.regular_files_matched(), 0);
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
        assert_eq!(summary.regular_files_total(), 1);
        assert_eq!(summary.regular_files_matched(), 1);
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
    fn execute_with_ignore_existing_skips_existing_destination() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let target_root = temp.path().join("target");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&target_root).expect("create target root");

        let source_path = source_root.join("file.txt");
        let dest_path = target_root.join("file.txt");
        fs::write(&source_path, b"updated").expect("write source");
        fs::write(&dest_path, b"original").expect("write destination");

        let operands = vec![
            source_path.clone().into_os_string(),
            dest_path.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().ignore_existing(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.regular_files_total(), 1);
        assert_eq!(summary.regular_files_matched(), 0);
        assert_eq!(summary.regular_files_ignored_existing(), 1);
        assert_eq!(fs::read(dest_path).expect("read destination"), b"original");
    }

    #[test]
    fn execute_with_update_skips_newer_destination() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let target_root = temp.path().join("target");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&target_root).expect("create target root");

        let source_path = source_root.join("file.txt");
        let dest_path = target_root.join("file.txt");
        fs::write(&source_path, b"updated").expect("write source");
        fs::write(&dest_path, b"existing").expect("write destination");

        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let newer = FileTime::from_unix_time(1_700_000_100, 0);
        set_file_times(&source_path, older, older).expect("set source times");
        set_file_times(&dest_path, newer, newer).expect("set dest times");

        let operands = vec![
            source_path.clone().into_os_string(),
            dest_path.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().update(true),
            )
            .expect("copy succeeds");

        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.regular_files_total(), 1);
        assert_eq!(summary.regular_files_skipped_newer(), 1);
        assert_eq!(fs::read(dest_path).expect("read destination"), b"existing");
    }

    #[test]
    fn execute_delta_copy_reuses_existing_blocks() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let target_root = temp.path().join("target");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&target_root).expect("create target root");

        let source_path = source_root.join("file.bin");
        let dest_path = target_root.join("file.bin");

        let mut prefix = vec![b'A'; 700];
        let mut suffix = vec![b'B'; 700];
        let mut replacement = vec![b'C'; 700];

        let mut initial = Vec::new();
        initial.append(&mut prefix.clone());
        initial.append(&mut suffix);
        fs::write(&dest_path, &initial).expect("write initial destination");

        let mut updated = Vec::new();
        updated.append(&mut prefix);
        updated.append(&mut replacement);
        fs::write(&source_path, &updated).expect("write updated source");

        let operands = vec![
            source_path.clone().into_os_string(),
            dest_path.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().whole_file(false),
            )
            .expect("delta copy succeeds");

        assert_eq!(summary.files_copied(), 1);
        assert_eq!(fs::read(&dest_path).expect("read destination"), updated);
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
    fn execute_with_copy_links_materialises_symlink_to_file() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        fs::write(&target, b"payload").expect("write target");

        let link = temp.path().join("link-file");
        symlink(&target, &link).expect("create link");
        let dest = temp.path().join("dest-file");

        let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default().copy_links(true);
        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let metadata = fs::symlink_metadata(&dest).expect("dest metadata");
        assert!(metadata.file_type().is_file());
        assert_eq!(fs::read(&dest).expect("read dest"), b"payload");
        assert_eq!(summary.symlinks_copied(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn execute_with_copy_links_materialises_symlink_to_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target_dir = temp.path().join("target-dir");
        fs::create_dir(&target_dir).expect("create target dir");
        fs::write(target_dir.join("inner.txt"), b"dir data").expect("write inner");

        let link = temp.path().join("link-dir");
        symlink(&target_dir, &link).expect("create dir link");
        let dest_dir = temp.path().join("dest-dir");

        let operands = vec![link.into_os_string(), dest_dir.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default().copy_links(true);
        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let metadata = fs::symlink_metadata(&dest_dir).expect("dest metadata");
        assert!(metadata.file_type().is_dir());
        let inner = dest_dir.join("inner.txt");
        assert_eq!(fs::read(&inner).expect("read inner"), b"dir data");
    }

    #[cfg(unix)]
    #[test]
    fn execute_with_copy_dirlinks_follows_directory_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target_dir = temp.path().join("referenced-dir");
        fs::create_dir(&target_dir).expect("create target dir");
        fs::write(target_dir.join("inner.txt"), b"dir data").expect("write inner");

        let link = temp.path().join("dir-link");
        symlink(&target_dir, &link).expect("create dir link");
        let dest_dir = temp.path().join("dest-dir");

        let operands = vec![link.into_os_string(), dest_dir.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default().copy_dirlinks(true);
        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let metadata = fs::symlink_metadata(&dest_dir).expect("dest metadata");
        assert!(metadata.file_type().is_dir());
        let inner = dest_dir.join("inner.txt");
        assert_eq!(fs::read(&inner).expect("read inner"), b"dir data");
    }

    #[cfg(unix)]
    #[test]
    fn execute_with_copy_dirlinks_preserves_file_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target_file = temp.path().join("target.txt");
        fs::write(&target_file, b"payload").expect("write target");

        let link = temp.path().join("file-link");
        symlink(&target_file, &link).expect("create file link");
        let dest = temp.path().join("dest-link");

        let operands = vec![link.into_os_string(), dest.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default().copy_dirlinks(true);
        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert_eq!(summary.symlinks_copied(), 1);
        let copied = fs::read_link(&dest).expect("read link");
        assert_eq!(copied, target_file);
    }

    #[cfg(unix)]
    #[test]
    fn execute_with_keep_dirlinks_allows_destination_directory_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("src-dir");
        fs::create_dir(&source_dir).expect("create source dir");
        fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

        let actual_destination = temp.path().join("actual-destination");
        fs::create_dir(&actual_destination).expect("create destination dir");
        let destination_link = temp.path().join("dest-link");
        symlink(&actual_destination, &destination_link).expect("create destination link");

        let operands = vec![
            source_dir.clone().into_os_string(),
            destination_link.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().keep_dirlinks(true),
            )
            .expect("copy succeeds");

        let copied_file = actual_destination.join("src-dir").join("file.txt");
        assert_eq!(
            fs::read(&copied_file).expect("read copied file"),
            b"payload"
        );
        assert!(
            fs::symlink_metadata(&destination_link)
                .expect("destination link metadata")
                .file_type()
                .is_symlink()
        );
        assert!(summary.directories_created() >= 1);
    }

    #[cfg(unix)]
    #[test]
    fn execute_without_keep_dirlinks_rejects_destination_directory_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("src-dir");
        fs::create_dir(&source_dir).expect("create source dir");
        fs::write(source_dir.join("file.txt"), b"payload").expect("write source file");

        let actual_destination = temp.path().join("actual-destination");
        fs::create_dir(&actual_destination).expect("create destination dir");
        let destination_link = temp.path().join("dest-link");
        symlink(&actual_destination, &destination_link).expect("create destination link");

        let operands = vec![
            source_dir.into_os_string(),
            destination_link.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let result =
            plan.execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default());

        let error = result.expect_err("keep-dirlinks disabled should reject destination symlink");
        assert!(matches!(
            error.kind(),
            LocalCopyErrorKind::InvalidArgument(
                LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory
            )
        ));
        assert!(
            fs::symlink_metadata(&destination_link)
                .expect("destination link metadata")
                .file_type()
                .is_symlink()
        );
        assert!(!actual_destination.join("src-dir").join("file.txt").exists());
    }

    #[test]
    fn reference_compare_destination_skips_matching_file() {
        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let reference_dir = temp.path().join("reference");
        let destination_dir = temp.path().join("dest");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::create_dir_all(&reference_dir).expect("create reference dir");
        fs::create_dir_all(&destination_dir).expect("create dest dir");

        let source_file = source_dir.join("file.txt");
        let reference_file = reference_dir.join("file.txt");
        fs::write(&source_file, b"payload").expect("write source");
        fs::write(&reference_file, b"payload").expect("write reference");

        let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_mtime(&source_file, timestamp).expect("source mtime");
        set_file_mtime(&reference_file, timestamp).expect("reference mtime");

        let destination_file = destination_dir.join("file.txt");
        let operands = vec![
            source_file.into_os_string(),
            destination_file.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default()
            .times(true)
            .extend_reference_directories([ReferenceDirectory::new(
                ReferenceDirectoryKind::Compare,
                &reference_dir,
            )]);

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert!(!destination_file.exists());
        assert_eq!(summary.files_copied(), 0);
        assert_eq!(summary.regular_files_matched(), 1);
    }

    #[test]
    fn reference_copy_destination_reuses_reference_payload() {
        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let reference_dir = temp.path().join("reference");
        let destination_dir = temp.path().join("dest");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::create_dir_all(&reference_dir).expect("create reference dir");
        fs::create_dir_all(&destination_dir).expect("create dest dir");

        let source_file = source_dir.join("file.txt");
        let reference_file = reference_dir.join("file.txt");
        fs::write(&source_file, b"payload").expect("write source");
        fs::write(&reference_file, b"payload").expect("write reference");

        let timestamp = FileTime::from_unix_time(1_700_000_500, 0);
        set_file_mtime(&source_file, timestamp).expect("source mtime");
        set_file_mtime(&reference_file, timestamp).expect("reference mtime");

        let destination_file = destination_dir.join("file.txt");
        let operands = vec![
            source_file.into_os_string(),
            destination_file.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default()
            .times(true)
            .extend_reference_directories([ReferenceDirectory::new(
                ReferenceDirectoryKind::Copy,
                &reference_dir,
            )]);

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert!(destination_file.exists());
        assert_eq!(fs::read(&destination_file).expect("read dest"), b"payload");
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.regular_files_matched(), 0);
    }

    #[test]
    fn reference_link_destination_degrades_to_copy_on_cross_device_error() {
        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let reference_dir = temp.path().join("reference");
        let destination_dir = temp.path().join("dest");
        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::create_dir_all(&reference_dir).expect("create reference dir");
        fs::create_dir_all(&destination_dir).expect("create dest dir");

        let source_file = source_dir.join("file.txt");
        let reference_file = reference_dir.join("file.txt");
        fs::write(&source_file, b"payload").expect("write source");
        fs::write(&reference_file, b"payload").expect("write reference");

        let timestamp = FileTime::from_unix_time(1_700_001_000, 0);
        set_file_mtime(&source_file, timestamp).expect("source mtime");
        set_file_mtime(&reference_file, timestamp).expect("reference mtime");

        let destination_file = destination_dir.join("file.txt");
        let operands = vec![
            source_file.into_os_string(),
            destination_file.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default()
            .times(true)
            .extend_reference_directories([ReferenceDirectory::new(
                ReferenceDirectoryKind::Link,
                &reference_dir,
            )]);

        let summary = super::with_hard_link_override(
            |_, _| Err(io::Error::from_raw_os_error(super::CROSS_DEVICE_ERROR_CODE)),
            || {
                plan.execute_with_options(LocalCopyExecution::Apply, options)
                    .expect("execution succeeds")
            },
        );

        assert!(destination_file.exists());
        assert_eq!(fs::read(&destination_file).expect("read dest"), b"payload");
        assert_eq!(summary.files_copied(), 1);
        assert_eq!(summary.hard_links_created(), 0);
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
    fn execute_applies_chmod_modifiers() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"payload").expect("write source");
        fs::set_permissions(&source, PermissionsExt::from_mode(0o666)).expect("set perms");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let modifiers = ChmodModifiers::parse("Fgo-w").expect("chmod parses");
        let options = LocalCopyOptions::default().with_chmod(Some(modifiers));
        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let metadata = fs::metadata(&destination).expect("dest metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
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
    fn execute_with_delete_after_removes_extraneous_entries() {
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
        let options = LocalCopyOptions::default().delete_after(true);

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
    fn execute_with_delete_delay_removes_extraneous_entries() {
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
        let options = LocalCopyOptions::default().delete_delay(true);

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
    fn execute_with_delete_before_removes_conflicting_entries() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::write(source_root.join("file"), b"fresh").expect("write source file");

        let dest_root = temp.path().join("dest");
        fs::create_dir_all(dest_root.join("file")).expect("create conflicting directory");

        let mut source_operand = source_root.clone().into_os_string();
        source_operand.push(std::path::MAIN_SEPARATOR.to_string());
        let operands = vec![source_operand, dest_root.clone().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().delete_before(true);

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds with delete-before");

        let target = dest_root.join("file");
        assert_eq!(fs::read(&target).expect("read copied file"), b"fresh");
        assert!(target.is_file());
        assert_eq!(summary.files_copied(), 1);
        assert!(summary.items_deleted() >= 1);
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
    fn execute_without_implied_dirs_requires_existing_parent() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("missing").join("dest.txt");
        fs::write(&source, b"data").expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().implied_dirs(false);

        let error = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect_err("missing parent should error");

        match error.kind() {
            LocalCopyErrorKind::Io { action, path, .. } => {
                assert_eq!(*action, "create parent directory");
                assert_eq!(path, destination.parent().expect("parent"));
            }
            other => panic!("unexpected error kind: {other:?}"),
        }
        assert!(!destination.exists());
    }

    #[test]
    fn execute_dry_run_without_implied_dirs_requires_existing_parent() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("missing").join("dest.txt");
        fs::write(&source, b"data").expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().implied_dirs(false);

        let error = plan
            .execute_with_options(LocalCopyExecution::DryRun, options)
            .expect_err("dry-run should error");

        match error.kind() {
            LocalCopyErrorKind::Io { action, path, .. } => {
                assert_eq!(*action, "create parent directory");
                assert_eq!(path, destination.parent().expect("parent"));
            }
            other => panic!("unexpected error kind: {other:?}"),
        }
        assert!(!destination.exists());
    }

    #[test]
    fn execute_with_implied_dirs_creates_missing_parents() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("missing").join("dest.txt");
        fs::write(&source, b"data").expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        plan.execute().expect("copy succeeds");

        assert!(destination.exists());
        assert_eq!(fs::read(&destination).expect("read dest"), b"data");
    }

    #[test]
    fn execute_with_mkpath_creates_missing_parents_without_implied_dirs() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("missing").join("dest.txt");
        fs::write(&source, b"data").expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let options = LocalCopyOptions::default().implied_dirs(false).mkpath(true);

        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert!(destination.exists());
        assert_eq!(fs::read(&destination).expect("read dest"), b"data");
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
    fn execute_with_delay_updates_preserves_hard_links() {
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

        let options = LocalCopyOptions::default()
            .partial(true)
            .delay_updates(true);
        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

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

        for entry in fs::read_dir(&dest_root).expect("read dest") {
            let entry = entry.expect("dir entry");
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.starts_with(".oc-rsync-tmp-") && !name.starts_with(".oc-rsync-partial-"),
                "unexpected temporary file left behind: {}",
                name
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn execute_with_link_dest_uses_reference_inode() {
        use filetime::FileTime;
        use std::os::unix::fs::MetadataExt;

        let temp = tempdir().expect("tempdir");
        let source_dir = temp.path().join("src");
        fs::create_dir_all(&source_dir).expect("create source dir");
        let source_file = source_dir.join("data.txt");
        fs::write(&source_file, b"payload").expect("write source");

        let baseline_dir = temp.path().join("baseline");
        fs::create_dir_all(&baseline_dir).expect("create baseline dir");
        let baseline_file = baseline_dir.join("data.txt");
        fs::write(&baseline_file, b"payload").expect("write baseline");

        let source_metadata = fs::metadata(&source_file).expect("source metadata");
        let source_mtime = source_metadata.modified().expect("source modified time");
        let timestamp = FileTime::from_system_time(source_mtime);
        filetime::set_file_times(&baseline_file, timestamp, timestamp)
            .expect("synchronise baseline timestamps");

        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create destination dir");

        let operands = vec![
            source_file.clone().into_os_string(),
            dest_dir.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let options = LocalCopyOptions::default()
            .times(true)
            .extend_link_dests([baseline_dir.clone()]);
        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let dest_file = dest_dir.join("data.txt");
        let dest_metadata = fs::metadata(&dest_file).expect("dest metadata");
        let baseline_metadata = fs::metadata(&baseline_file).expect("baseline metadata");

        assert_eq!(dest_metadata.ino(), baseline_metadata.ino());
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
        let mut recorder = rsync_bandwidth::recorded_sleep_session();
        recorder.clear();

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

        let recorded = recorder.take();
        assert!(
            !recorded.is_empty(),
            "expected bandwidth limiter to schedule sleeps"
        );
        let total = recorded
            .into_iter()
            .fold(Duration::ZERO, |acc, duration| acc + duration);
        let expected = Duration::from_secs(4);
        let diff = total.abs_diff(expected);
        assert!(
            diff <= Duration::from_millis(50),
            "expected sleep duration near {:?}, got {:?}",
            expected,
            total
        );
        assert_eq!(summary.files_copied(), 1);
    }

    #[test]
    fn execute_with_append_appends_missing_bytes() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"abcdef").expect("write source");
        fs::write(&destination, b"abc").expect("write dest");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().append(true),
            )
            .expect("append succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), b"abcdef");
        assert_eq!(summary.bytes_copied(), 3);
    }

    #[test]
    fn execute_with_append_verify_rewrites_on_mismatch() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");
        fs::write(&source, b"abcdef").expect("write source");
        fs::write(&destination, b"abx").expect("write dest");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().append_verify(true),
            )
            .expect("append verify succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), b"abcdef");
        assert_eq!(summary.bytes_copied(), 6);
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
        let mut recorder = rsync_bandwidth::recorded_sleep_session();
        recorder.clear();

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
        assert!(
            recorder.take().is_empty(),
            "unexpected sleep durations recorded"
        );
        assert_eq!(summary.files_copied(), 1);
    }

    #[test]
    fn execute_with_compression_records_compressed_bytes() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        let content = vec![b'A'; 16 * 1024];
        fs::write(&source, &content).expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default().compress(true),
            )
            .expect("copy succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), content);
        assert!(summary.compression_used());
        let compressed = summary.compressed_bytes();
        assert!(compressed > 0);
        assert!(compressed <= summary.bytes_copied());
    }

    #[test]
    fn execute_with_compression_limits_post_compress_bandwidth() {
        let mut recorder = rsync_bandwidth::recorded_sleep_session();
        recorder.clear();

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        let mut content = Vec::new();
        for _ in 0..4096 {
            content
                .extend_from_slice(b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \n");
        }
        fs::write(&source, &content).expect("write source");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
        let limit = NonZeroU64::new(2 * 1024).expect("limit");
        let options = LocalCopyOptions::default()
            .compress(true)
            .bandwidth_limit(Some(limit));

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        assert_eq!(fs::read(&destination).expect("read dest"), content);
        assert!(summary.compression_used());

        let compressed = summary.compressed_bytes();
        assert!(compressed > 0);
        let transferred = summary.bytes_copied();

        let sleeps = recorder.take();
        assert!(
            !sleeps.is_empty(),
            "bandwidth limiter did not record sleeps"
        );
        let total_sleep_secs: f64 = sleeps.iter().map(|duration| duration.as_secs_f64()).sum();

        let expected_compressed = compressed as f64 / limit.get() as f64;
        let expected_uncompressed = transferred as f64 / limit.get() as f64;

        let tolerance = expected_compressed * 0.2 + 0.2;
        assert!(
            (total_sleep_secs - expected_compressed).abs() <= tolerance,
            "sleep {:?}s deviates too far from compressed expectation {:?}s",
            total_sleep_secs,
            expected_compressed,
        );
        assert!(
            (total_sleep_secs - expected_compressed).abs()
                < (total_sleep_secs - expected_uncompressed).abs(),
            "sleep {:?}s should align with compressed bytes ({:?}s) rather than uncompressed ({:?}s)",
            total_sleep_secs,
            expected_compressed,
            expected_uncompressed,
        );
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
    fn execute_prunes_empty_directories_when_enabled() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest_without_prune = temp.path().join("dest_without_prune");
        let dest_with_prune = temp.path().join("dest_with_prune");

        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(source.join("empty")).expect("create empty dir");
        fs::write(source.join("keep.txt"), b"payload").expect("write keep");
        fs::write(source.join("empty").join("skip.tmp"), b"skip").expect("write skip");

        fs::create_dir_all(&dest_without_prune).expect("create dest");
        fs::create_dir_all(&dest_with_prune).expect("create dest");

        let operands_without = vec![
            source.clone().into_os_string(),
            dest_without_prune.clone().into_os_string(),
        ];
        let plan_without = LocalCopyPlan::from_operands(&operands_without).expect("plan");
        let filters_without = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
            .expect("compile filters");
        let options_without = LocalCopyOptions::default().filters(Some(filters_without));
        let summary_without = plan_without
            .execute_with_options(LocalCopyExecution::Apply, options_without)
            .expect("copy succeeds");

        let operands_with = vec![
            source.into_os_string(),
            dest_with_prune.clone().into_os_string(),
        ];
        let plan_with = LocalCopyPlan::from_operands(&operands_with).expect("plan");
        let filters_with = FilterSet::from_rules([rsync_filters::FilterRule::exclude("*.tmp")])
            .expect("compile filters");
        let options_with = LocalCopyOptions::default()
            .filters(Some(filters_with))
            .prune_empty_dirs(true);
        let summary_with = plan_with
            .execute_with_options(LocalCopyExecution::Apply, options_with)
            .expect("copy succeeds");

        let target_without = dest_without_prune.join("source");
        let target_with = dest_with_prune.join("source");

        assert!(target_without.join("keep.txt").exists());
        assert!(target_with.join("keep.txt").exists());
        assert!(target_without.join("empty").is_dir());
        assert!(!target_with.join("empty").exists());
        assert!(summary_with.directories_created() < summary_without.directories_created());
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
    fn execute_skips_directories_with_exclude_if_present_marker() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let destination_root = temp.path().join("dest");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&destination_root).expect("create dest root");

        fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        let marker_dir = source_root.join("skip");
        fs::create_dir_all(&marker_dir).expect("create marker dir");
        fs::write(marker_dir.join(".rsyncignore"), b"marker").expect("write marker");
        fs::write(marker_dir.join("data.txt"), b"ignored").expect("write data");

        let operands = vec![
            source_root.clone().into_os_string(),
            destination_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let program = FilterProgram::new([FilterProgramEntry::ExcludeIfPresent(
            ExcludeIfPresentRule::new(".rsyncignore"),
        )])
        .expect("compile filter program");

        let options = LocalCopyOptions::default().with_filter_program(Some(program));

        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = destination_root.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert!(!target_root.join("skip").exists());
    }

    #[test]
    fn filter_program_clear_discards_previous_rules() {
        let temp = tempdir().expect("tempdir");
        let source_root = temp.path().join("source");
        let destination_root = temp.path().join("dest");
        fs::create_dir_all(&source_root).expect("create source");
        fs::create_dir_all(&destination_root).expect("create dest");

        fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");
        fs::write(source_root.join("skip.tmp"), b"tmp").expect("write tmp");
        fs::write(source_root.join("skip.bak"), b"bak").expect("write bak");

        let operands = vec![
            source_root.clone().into_os_string(),
            destination_root.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let program = FilterProgram::new([
            FilterProgramEntry::Rule(FilterRule::exclude("*.tmp")),
            FilterProgramEntry::Clear,
            FilterProgramEntry::Rule(FilterRule::exclude("*.bak")),
        ])
        .expect("compile filter program");

        let options = LocalCopyOptions::default().with_filter_program(Some(program));

        plan.execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = destination_root.join("source");
        assert!(target_root.join("keep.txt").exists());
        assert!(
            target_root.join("skip.tmp").exists(),
            "list-clearing rule should discard earlier excludes"
        );
        assert!(!target_root.join("skip.bak").exists());
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
    fn delete_excluded_removes_matching_source_files() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let dest = temp.path().join("dest");
        fs::create_dir_all(&source).expect("create source");
        fs::create_dir_all(&dest).expect("create dest");
        fs::write(source.join("keep.txt"), b"keep").expect("write keep");
        fs::write(source.join("skip.tmp"), b"skip source").expect("write skip source");

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

    #[test]
    fn delete_risk_rule_overrides_protection() {
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
        let filters = FilterSet::from_rules([
            rsync_filters::FilterRule::protect("keep.txt"),
            rsync_filters::FilterRule::risk("keep.txt"),
        ])
        .expect("compile filters");
        let options = LocalCopyOptions::default()
            .delete(true)
            .filters(Some(filters));

        let summary = plan
            .execute_with_options(LocalCopyExecution::Apply, options)
            .expect("copy succeeds");

        let target_root = dest.join("source");
        assert!(!target_root.join("keep.txt").exists());
        assert_eq!(summary.items_deleted(), 1);
    }

    #[test]
    fn destination_write_guard_uses_custom_partial_directory() {
        let temp = tempdir().expect("tempdir");
        let destination_dir = temp.path().join("dest");
        fs::create_dir_all(&destination_dir).expect("dest dir");
        let destination = destination_dir.join("file.txt");
        let partial_dir = Path::new(".custom-partial");

        let (guard, mut file) =
            DestinationWriteGuard::new(destination.as_path(), true, Some(partial_dir))
                .expect("guard");
        let temp_path = guard.temp_path.clone();
        file.write_all(b"partial payload").expect("write partial");
        drop(file);

        drop(guard);

        let expected_base = destination_dir.join(partial_dir);
        assert!(temp_path.starts_with(&expected_base));
        assert!(temp_path.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn destination_write_guard_commit_moves_from_partial_directory() {
        let temp = tempdir().expect("tempdir");
        let destination_dir = temp.path().join("dest");
        fs::create_dir_all(&destination_dir).expect("dest dir");
        let destination = destination_dir.join("file.txt");
        let partial_dir = temp.path().join("partials");

        let (guard, mut file) =
            DestinationWriteGuard::new(destination.as_path(), true, Some(partial_dir.as_path()))
                .expect("guard");
        let temp_path = guard.temp_path.clone();
        file.write_all(b"committed payload").expect("write payload");
        drop(file);

        guard.commit().expect("commit succeeds");

        assert!(!temp_path.exists());
        let committed = fs::read(&destination).expect("read committed file");
        assert_eq!(committed, b"committed payload");
    }
}
