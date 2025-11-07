use std::collections::HashSet;
use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};
use rsync_filters::{FilterAction, FilterRule};

/// Compiled list of rules evaluated sequentially.
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterSegment {
    include_exclude: Vec<CompiledRule>,
    protect_risk: Vec<CompiledRule>,
}

impl FilterSegment {
    pub(crate) fn push_rule(&mut self, rule: FilterRule) -> Result<(), super::FilterProgramError> {
        match rule.action() {
            FilterAction::Include | FilterAction::Exclude => {
                self.include_exclude.push(CompiledRule::new(rule)?);
            }
            FilterAction::Protect | FilterAction::Risk => {
                self.protect_risk.push(CompiledRule::new(rule)?);
            }
            FilterAction::Clear => {
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

    pub(crate) fn apply(
        &self,
        path: &Path,
        is_dir: bool,
        outcome: &mut FilterOutcome,
        context: FilterContext,
    ) {
        for rule in &self.include_exclude {
            if rule.matches(path, is_dir) {
                if matches!(context, FilterContext::Deletion) && rule.applies_to_receiver {
                    outcome.set_delete_excluded(matches!(rule.action, FilterAction::Exclude));
                }
                if matches!(context, FilterContext::Deletion) && rule.perishable {
                    continue;
                }
                match context {
                    FilterContext::Transfer => {
                        if rule.applies_to_sender {
                            outcome
                                .set_transfer_allowed(matches!(rule.action, FilterAction::Include));
                        }
                    }
                    FilterContext::Deletion => {
                        if rule.applies_to_receiver {
                            outcome
                                .set_transfer_allowed(matches!(rule.action, FilterAction::Include));
                        }
                    }
                }
            }
        }

        for rule in &self.protect_risk {
            if matches!(context, FilterContext::Deletion) && rule.perishable {
                continue;
            }
            if rule.matches(path, is_dir) {
                let applies = match context {
                    FilterContext::Transfer => rule.applies_to_sender,
                    FilterContext::Deletion => rule.applies_to_receiver,
                };
                if applies {
                    match rule.action {
                        FilterAction::Protect => outcome.protect(),
                        FilterAction::Risk => outcome.unprotect(),
                        FilterAction::Include | FilterAction::Exclude => {}
                        FilterAction::Clear => debug_assert!(
                            false,
                            "clear directives should be converted into FilterProgramEntry::Clear before compilation",
                        ),
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum FilterInstruction {
    Segment(FilterSegment),
    DirMerge { index: usize },
    ExcludeIfPresent { index: usize },
}

pub(crate) type FilterSegmentLayers = Vec<Vec<FilterSegment>>;
pub(crate) type FilterSegmentStack = Vec<Vec<(usize, FilterSegment)>>;
pub(crate) type ExcludeIfPresentLayers = Vec<Vec<super::ExcludeIfPresentRule>>;
pub(crate) type ExcludeIfPresentStack = Vec<Vec<(usize, Vec<super::ExcludeIfPresentRule>)>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FilterContext {
    Transfer,
    Deletion,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterOutcome {
    transfer_allowed: bool,
    protected: bool,
    excluded_for_delete_excluded: bool,
}

impl FilterOutcome {
    fn new() -> Self {
        Self {
            transfer_allowed: true,
            protected: false,
            excluded_for_delete_excluded: false,
        }
    }

    pub(crate) fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    pub(crate) fn allows_deletion(self) -> bool {
        self.transfer_allowed && !self.protected
    }

    pub(crate) fn allows_deletion_when_excluded_removed(self) -> bool {
        self.excluded_for_delete_excluded && !self.protected
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

    fn set_delete_excluded(&mut self, excluded: bool) {
        self.excluded_for_delete_excluded = excluded;
    }
}

impl Default for FilterOutcome {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
struct CompiledRule {
    action: FilterAction,
    directory_only: bool,
    direct_matchers: Vec<GlobMatcher>,
    descendant_matchers: Vec<GlobMatcher>,
    applies_to_sender: bool,
    applies_to_receiver: bool,
    perishable: bool,
}

impl CompiledRule {
    fn new(rule: FilterRule) -> Result<Self, super::FilterProgramError> {
        let action = rule.action();
        let applies_to_sender = rule.applies_to_sender();
        let applies_to_receiver = rule.applies_to_receiver();
        let pattern = rule.pattern().to_string();
        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);

        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.clone());
        if !anchored {
            direct_patterns.insert(format!("**/{core_pattern}"));
        }

        let mut descendant_patterns = HashSet::new();
        if directory_only
            || matches!(
                action,
                FilterAction::Exclude | FilterAction::Protect | FilterAction::Risk
            )
        {
            descendant_patterns.insert(format!("{core_pattern}/**"));
            if !anchored {
                descendant_patterns.insert(format!("**/{core_pattern}/**"));
            }
        }

        Ok(Self {
            action,
            directory_only,
            direct_matchers: compile_patterns(direct_patterns, &pattern)?,
            descendant_matchers: compile_patterns(descendant_patterns, &pattern)?,
            applies_to_sender,
            applies_to_receiver,
            perishable: rule.is_perishable(),
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
) -> Result<Vec<GlobMatcher>, super::FilterProgramError> {
    let mut unique: Vec<_> = patterns.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| super::FilterProgramError::new(original.to_string(), error))?;
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
