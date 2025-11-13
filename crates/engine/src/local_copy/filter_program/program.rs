use std::error::Error;
use std::fmt;
use std::path::Path;

#[cfg(all(unix, feature = "xattr"))]
use filters::FilterAction;
use filters::FilterRule;
#[cfg(all(unix, feature = "xattr"))]
use globset::{GlobBuilder, GlobMatcher};

use super::super::LocalCopyError;
use super::rules::{DirMergeRule, ExcludeIfPresentRule};
use super::segments::{FilterContext, FilterInstruction, FilterOutcome, FilterSegment};

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

    // XAttr filter strategy – present only where meaningful.
    #[cfg(all(unix, feature = "xattr"))]
    xattr_rules: Vec<XattrRule>,
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

        #[cfg(all(unix, feature = "xattr"))]
        let mut xattr_rules = Vec::new();

        for entry in entries {
            match entry {
                FilterProgramEntry::Rule(rule) => {
                    #[cfg(all(unix, feature = "xattr"))]
                    {
                        if rule.is_xattr_only() {
                            let compiled = XattrRule::new(&rule)?;
                            xattr_rules.push(compiled);
                            continue;
                        }
                    }

                    current_segment.push_rule(rule)?;
                }
                FilterProgramEntry::Clear => {
                    current_segment = FilterSegment::default();
                    instructions.clear();
                    dir_merge_rules.clear();
                    exclude_if_present_rules.clear();
                    #[cfg(all(unix, feature = "xattr"))]
                    {
                        xattr_rules.clear();
                    }
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
            #[cfg(all(unix, feature = "xattr"))]
            xattr_rules,
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
        let filters_empty =
            self.instructions
                .iter()
                .all(|instruction| match instruction {
                    FilterInstruction::Segment(segment) => segment.is_empty(),
                    FilterInstruction::DirMerge { .. }
                    | FilterInstruction::ExcludeIfPresent { .. } => false,
                });

        #[cfg(all(unix, feature = "xattr"))]
        {
            filters_empty && self.xattr_rules.is_empty()
        }
        #[cfg(not(all(unix, feature = "xattr")))]
        {
            filters_empty
        }
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

    // XAttr filtering strategy – only compiled where supported.
    #[cfg(all(unix, feature = "xattr"))]
    pub(crate) fn has_xattr_rules(&self) -> bool {
        !self.xattr_rules.is_empty()
    }

    #[cfg(all(unix, feature = "xattr"))]
    pub(crate) fn allows_xattr(&self, name: &str) -> bool {
        if self.xattr_rules.is_empty() {
            return true;
        }

        let mut decision = None;
        for rule in &self.xattr_rules {
            if rule.matches(name) {
                decision = Some(rule.action);
            }
        }

        match decision {
            Some(FilterAction::Exclude) => false,
            Some(FilterAction::Include) | None => true,
            Some(FilterAction::Protect | FilterAction::Risk | FilterAction::Clear) => true,
        }
    }
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
    pub(crate) fn new(pattern: String, source: globset::Error) -> Self {
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

#[cfg(all(unix, feature = "xattr"))]
#[derive(Clone, Debug)]
struct XattrRule {
    action: FilterAction,
    matcher: GlobMatcher,
}

#[cfg(all(unix, feature = "xattr"))]
impl XattrRule {
    fn new(rule: &FilterRule) -> Result<Self, FilterProgramError> {
        debug_assert!(rule.is_xattr_only());
        let pattern = rule.pattern().to_string();
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| FilterProgramError::new(pattern.clone(), error))?;

        let action = rule.action();
        debug_assert!(matches!(
            action,
            FilterAction::Include | FilterAction::Exclude
        ));

        Ok(Self {
            action,
            matcher: glob.compile_matcher(),
        })
    }

    fn matches(&self, name: &str) -> bool {
        self.matcher.is_match(name)
    }
}

impl Error for FilterProgramError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
