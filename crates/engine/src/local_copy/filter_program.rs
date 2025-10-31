use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use rsync_filters::FilterRule;

use super::LocalCopyError;

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
pub(crate) enum DirMergeParser {
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
    pub(crate) const fn parser(&self) -> &DirMergeParser {
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
pub(crate) const INVALID_OPERAND_EXIT_CODE: i32 = 23;
/// Exit code returned when no transfer operands are supplied.
pub(crate) const MISSING_OPERANDS_EXIT_CODE: i32 = 1;
/// Exit code returned when the transfer exceeds the configured timeout.
pub(crate) const TIMEOUT_EXIT_CODE: i32 = 30;
/// Exit code returned when the `--max-delete` limit stops deletions.
pub(crate) const MAX_DELETE_EXIT_CODE: i32 = 25;

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
    pub(crate) fn evaluate(
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

    pub(crate) fn should_exclude_directory(
        &self,
        directory: &Path,
    ) -> Result<bool, LocalCopyError> {
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
pub(crate) enum FilterContext {
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

pub(crate) fn directory_has_marker(
    rules: &[ExcludeIfPresentRule],
    directory: &Path,
) -> Result<bool, LocalCopyError> {
    for rule in rules {
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

    Ok(false)
}

#[derive(Clone, Debug)]
enum FilterInstruction {
    Segment(FilterSegment),
    DirMerge { index: usize },
    ExcludeIfPresent { index: usize },
}

/// Compiled list of rules evaluated sequentially.
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterSegment {
    include_exclude: Vec<CompiledRule>,
    protect_risk: Vec<CompiledRule>,
}

impl FilterSegment {
    pub(crate) fn push_rule(&mut self, rule: FilterRule) -> Result<(), FilterProgramError> {
        match rule.action() {
            rsync_filters::FilterAction::Include | rsync_filters::FilterAction::Exclude => {
                self.include_exclude.push(CompiledRule::new(rule)?);
            }
            rsync_filters::FilterAction::Protect | rsync_filters::FilterAction::Risk => {
                self.protect_risk.push(CompiledRule::new(rule)?);
            }
            rsync_filters::FilterAction::Clear => {
                debug_assert!(
                    false,
                    "clear directives should be converted into FilterProgramEntry::Clear before compilation",
                );
            }
        }
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
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
                        rsync_filters::FilterAction::Clear => debug_assert!(
                            false,
                            "clear directives should be converted into FilterProgramEntry::Clear before compilation",
                        ),
                    }
                }
            }
        }
    }
}

pub(crate) type FilterSegmentLayers = Vec<Vec<FilterSegment>>;
pub(crate) type FilterSegmentStack = Vec<Vec<(usize, FilterSegment)>>;
pub(crate) type ExcludeIfPresentLayers = Vec<Vec<ExcludeIfPresentRule>>;
pub(crate) type ExcludeIfPresentStack = Vec<Vec<(usize, Vec<ExcludeIfPresentRule>)>>;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterOutcome {
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

    pub(crate) fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    pub(crate) fn allows_deletion(self) -> bool {
        self.transfer_allowed && !self.protected
    }

    pub(crate) fn allows_deletion_when_excluded_removed(self) -> bool {
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
