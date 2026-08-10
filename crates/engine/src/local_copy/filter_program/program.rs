//! `FilterProgram` aggregate: ordered filter instructions plus per-directory
//! merge and exclude-if-present rules, along with the upstream-mapped exit
//! code constants surfaced when filter evaluation aborts a transfer.

use std::path::Path;

#[cfg(all(any(unix, windows), feature = "xattr"))]
use filters::FilterAction;
use filters::FilterRule;
#[cfg(all(any(unix, windows), feature = "xattr"))]
use globset::{GlobBuilder, GlobMatcher};
use thiserror::Error;

use super::super::LocalCopyError;
use super::rules::{DirMergeRule, ExcludeIfPresentRule};
use super::segments::{FilterContext, FilterInstruction, FilterOutcome, FilterSegment};

/// Exit code returned when operand validation fails.
///
/// Maps to upstream rsync's `RERR_PARTIAL` (23) and `core::exit_code::ExitCode::PartialTransfer`.
pub(crate) const INVALID_OPERAND_EXIT_CODE: i32 = 23;

/// Exit code returned when no transfer operands are supplied.
///
/// Maps to upstream rsync's `RERR_SYNTAX` (1) and `core::exit_code::ExitCode::Syntax`.
pub(crate) const MISSING_OPERANDS_EXIT_CODE: i32 = 1;

/// Exit code returned when the transfer is aborted by SIGINT/SIGTERM/SIGHUP.
///
/// Maps to upstream rsync's `RERR_SIGNAL` (20) and `core::exit_code::ExitCode::Signal`.
/// upstream: cleanup.c:_exit_cleanup exits with `RERR_SIGNAL` after finalising
/// partials when `sig_int` (rsync.c) requests a controlled shutdown.
pub(crate) const SIGNAL_EXIT_CODE: i32 = 20;

/// Exit code returned when the transfer exceeds the configured timeout.
///
/// Maps to upstream rsync's `RERR_TIMEOUT` (30) and `core::exit_code::ExitCode::Timeout`.
pub(crate) const TIMEOUT_EXIT_CODE: i32 = 30;

/// Exit code returned when the connection setup exceeds the configured timeout.
///
/// Maps to upstream rsync's `RERR_CONTIMEOUT` (35) and `core::exit_code::ExitCode::ConnectionTimeout`.
#[allow(dead_code)] // upstream exit code constant; reserved for connection-timeout reporting
pub(crate) const CONNECTION_TIMEOUT_EXIT_CODE: i32 = 35;

/// Exit code returned when source files vanished during transfer.
///
/// Maps to upstream rsync's `RERR_VANISHED` (24) and `core::exit_code::ExitCode::Vanished`.
pub(crate) const VANISHED_EXIT_CODE: i32 = 24;

/// Exit code returned when the `--max-delete` limit stops deletions.
///
/// Maps to upstream rsync's `RERR_DEL_LIMIT` (25) and `core::exit_code::ExitCode::DeleteLimit`.
pub(crate) const MAX_DELETE_EXIT_CODE: i32 = 25;

/// Ordered list of filter rules and per-directory merge directives.
#[derive(Clone, Debug, Default)]
pub struct FilterProgram {
    instructions: Vec<FilterInstruction>,
    dir_merge_rules: Vec<DirMergeRule>,
    exclude_if_present_rules: Vec<ExcludeIfPresentRule>,

    #[cfg(all(any(unix, windows), feature = "xattr"))]
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

        #[cfg(all(any(unix, windows), feature = "xattr"))]
        let mut xattr_rules = Vec::new();

        for entry in entries {
            match entry {
                FilterProgramEntry::Rule(rule) => {
                    #[cfg(all(any(unix, windows), feature = "xattr"))]
                    {
                        if rule.is_xattr_only() {
                            let compiled = XattrRule::new(&rule)?;
                            xattr_rules.push(compiled);
                        } else {
                            current_segment.push_rule(rule)?;
                        }
                    }
                    #[cfg(not(all(any(unix, windows), feature = "xattr")))]
                    {
                        current_segment.push_rule(rule)?;
                    }
                }
                FilterProgramEntry::Clear => {
                    current_segment = FilterSegment::default();
                    instructions.clear();
                    dir_merge_rules.clear();
                    exclude_if_present_rules.clear();
                    #[cfg(all(any(unix, windows), feature = "xattr"))]
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
            #[cfg(all(any(unix, windows), feature = "xattr"))]
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

        #[cfg(all(any(unix, windows), feature = "xattr"))]
        {
            filters_empty && self.xattr_rules.is_empty()
        }
        #[cfg(not(all(any(unix, windows), feature = "xattr")))]
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
                        // upstream: exclude.c:check_filter() - local (child) rules
                        // precede inherited (parent) rules in the linked list;
                        // head-first traversal gives child rules precedence.
                        // Our `layers` Vec is populated parent-first via
                        // `layers[index].push(segment)` in context_impl/transfer.rs,
                        // so iterate in reverse to match upstream semantics.
                        for layer in layers.iter().rev() {
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

    /// Checks whether a directory path is excluded by a non-directory-specific rule.
    ///
    /// Used by `--prune-empty-dirs` to determine whether an excluded directory
    /// should still be descended into. When the exclusion comes from a generic
    /// pattern (e.g., `*`) rather than a directory-specific one (e.g., `cache/`),
    /// the directory should be traversed so file-level include rules can be
    /// evaluated inside it.
    pub(crate) fn excluded_dir_by_non_dir_rule(
        &self,
        path: &Path,
        dir_merge_layers: &[Vec<FilterSegment>],
        ephemeral_layers: Option<&[(usize, FilterSegment)]>,
    ) -> bool {
        for instruction in &self.instructions {
            match instruction {
                FilterInstruction::Segment(segment) => {
                    if let Some(result) = segment.excluded_dir_by_non_dir_rule(path) {
                        return result;
                    }
                }
                FilterInstruction::DirMerge { index } => {
                    if let Some(layers) = dir_merge_layers.get(*index) {
                        // upstream: exclude.c:check_filter() - newest (child)
                        // rules win on first match, see the matching loop in
                        // `evaluate()` above.
                        for layer in layers.iter().rev() {
                            if let Some(result) = layer.excluded_dir_by_non_dir_rule(path) {
                                return result;
                            }
                        }
                    }
                    if let Some(ephemeral) = ephemeral_layers {
                        for (rule_index, segment) in ephemeral {
                            if *rule_index == *index {
                                if let Some(result) = segment.excluded_dir_by_non_dir_rule(path) {
                                    return result;
                                }
                            }
                        }
                    }
                }
                FilterInstruction::ExcludeIfPresent { .. } => {}
            }
        }
        false
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

    #[cfg(all(any(unix, windows), feature = "xattr"))]
    pub(crate) const fn has_xattr_rules(&self) -> bool {
        !self.xattr_rules.is_empty()
    }

    #[cfg(all(any(unix, windows), feature = "xattr"))]
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
            Some(
                FilterAction::Protect
                | FilterAction::Risk
                | FilterAction::Clear
                | FilterAction::Merge
                | FilterAction::DirMerge,
            ) => true,
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
#[derive(Debug, Error)]
#[error("failed to compile filter pattern '{pattern}': {source}")]
pub struct FilterProgramError {
    pattern: String,
    #[source]
    source: globset::Error,
}

impl FilterProgramError {
    pub(crate) const fn new(pattern: String, source: globset::Error) -> Self {
        Self { pattern, source }
    }

    /// Returns the pattern that failed to compile.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

#[cfg(all(any(unix, windows), feature = "xattr"))]
#[derive(Clone, Debug)]
struct XattrRule {
    action: FilterAction,
    matcher: GlobMatcher,
}

#[cfg(all(any(unix, windows), feature = "xattr"))]
impl XattrRule {
    fn new(rule: &FilterRule) -> Result<Self, FilterProgramError> {
        debug_assert!(rule.is_xattr_only());
        let pattern = rule.pattern().to_owned();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_program_is_empty() {
        let program = FilterProgram::new(std::iter::empty()).unwrap();
        assert!(program.is_empty());
    }

    #[test]
    fn program_with_rule_is_not_empty() {
        let rule = FilterRule::exclude("*.txt");
        let program = FilterProgram::new([FilterProgramEntry::Rule(rule)]).unwrap();
        assert!(!program.is_empty());
    }

    #[test]
    fn program_with_dir_merge_is_not_empty() {
        let rule = DirMergeRule::new(".rsync-filter".to_owned(), Default::default());
        let program = FilterProgram::new([FilterProgramEntry::DirMerge(rule)]).unwrap();
        assert!(!program.is_empty());
    }

    #[test]
    fn clear_entry_clears_rules() {
        let entries = [
            FilterProgramEntry::Rule(FilterRule::exclude("*.txt")),
            FilterProgramEntry::Clear,
        ];
        let program = FilterProgram::new(entries).unwrap();
        assert!(program.is_empty());
    }

    #[test]
    fn dir_merge_rules_accessor() {
        let rule1 = DirMergeRule::new(".rsync-filter".to_owned(), Default::default());
        let rule2 = DirMergeRule::new(".gitignore".to_owned(), Default::default());
        let entries = [
            FilterProgramEntry::DirMerge(rule1),
            FilterProgramEntry::DirMerge(rule2),
        ];
        let program = FilterProgram::new(entries).unwrap();
        assert_eq!(program.dir_merge_rules().len(), 2);
    }

    #[test]
    fn filter_program_error_pattern_accessor() {
        let error = FilterProgramError::new(
            "bad[pattern".to_owned(),
            globset::GlobBuilder::new("bad[pattern")
                .build()
                .unwrap_err(),
        );
        assert_eq!(error.pattern(), "bad[pattern");
    }

    #[test]
    fn evaluate_returns_default_for_empty_program() {
        let program = FilterProgram::new(std::iter::empty()).unwrap();
        let path = Path::new("test.txt");
        let outcome = program.evaluate(path, false, &[], None, FilterContext::Transfer);
        assert!(outcome.allows_transfer());
    }

    #[test]
    fn should_exclude_directory_returns_false_when_no_rules() {
        let program = FilterProgram::new(std::iter::empty()).unwrap();
        let result = program.should_exclude_directory(Path::new("/tmp"));
        assert!(!result.unwrap());
    }

    #[test]
    fn exit_code_constants_are_distinct() {
        let codes = [
            INVALID_OPERAND_EXIT_CODE,
            MISSING_OPERANDS_EXIT_CODE,
            TIMEOUT_EXIT_CODE,
            CONNECTION_TIMEOUT_EXIT_CODE,
            VANISHED_EXIT_CODE,
            MAX_DELETE_EXIT_CODE,
        ];
        let unique: std::collections::HashSet<_> = codes.iter().collect();
        assert_eq!(codes.len(), unique.len());
    }

    #[test]
    fn child_dir_merge_rules_override_parent_rules() {
        // upstream: exclude.c:check_filter() - local (child) rules are at the
        // head of the merged rule list, inherited (parent) rules at the tail;
        // head-first traversal gives child rules precedence on first match.
        let rule = DirMergeRule::new(".rsync-filter".to_owned(), Default::default());
        let program = FilterProgram::new([FilterProgramEntry::DirMerge(rule)]).unwrap();

        let mut parent_segment = FilterSegment::default();
        parent_segment
            .push_rule(FilterRule::exclude("debug.log"))
            .expect("add parent rule");

        let mut child_segment = FilterSegment::default();
        child_segment
            .push_rule(FilterRule::include("debug.log"))
            .expect("add child override");

        // Parent pushed first (layer index 0), child pushed second (layer
        // index 1) - mirrors how context_impl/transfer.rs populates the Vec
        // as directories are descended.
        let dir_layers = vec![vec![parent_segment, child_segment]];

        let outcome = program.evaluate(
            Path::new("debug.log"),
            false,
            &dir_layers,
            None,
            FilterContext::Transfer,
        );
        assert!(
            outcome.allows_transfer(),
            "child directory include must override parent directory exclude"
        );
    }
}
