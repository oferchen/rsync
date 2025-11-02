use std::collections::HashSet;
use std::path::Path;

use globset::{GlobBuilder, GlobMatcher};

use crate::{FilterAction, FilterError, FilterRule};

#[derive(Debug)]
pub(crate) struct CompiledRule {
    pub(crate) action: FilterAction,
    directory_only: bool,
    direct_matchers: Vec<GlobMatcher>,
    descendant_matchers: Vec<GlobMatcher>,
    pub(crate) applies_to_sender: bool,
    pub(crate) applies_to_receiver: bool,
}

impl CompiledRule {
    pub(crate) fn new(rule: FilterRule) -> Result<Self, FilterError> {
        let FilterRule {
            action,
            pattern,
            applies_to_sender,
            applies_to_receiver,
        } = rule;
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

        let direct_matchers = compile_patterns(direct_patterns, &pattern)?;
        let descendant_matchers = compile_patterns(descendant_patterns, &pattern)?;

        Ok(Self {
            action,
            directory_only,
            direct_matchers,
            descendant_matchers,
            applies_to_sender,
            applies_to_receiver,
        })
    }

    pub(crate) fn matches(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) && (!self.directory_only || is_dir) {
                return true;
            }
        }

        if !self.descendant_matchers.is_empty() {
            for matcher in &self.descendant_matchers {
                if matcher.is_match(path) {
                    return true;
                }
            }
        }

        false
    }

    pub(crate) fn clear_sides(&mut self, sender: bool, receiver: bool) -> bool {
        if sender {
            self.applies_to_sender = false;
        }
        if receiver {
            self.applies_to_receiver = false;
        }
        self.applies_to_sender || self.applies_to_receiver
    }
}

pub(crate) fn apply_clear_rule(rules: &mut Vec<CompiledRule>, sender: bool, receiver: bool) {
    if !sender && !receiver {
        return;
    }

    rules.retain_mut(|rule| rule.clear_sides(sender, receiver));
}

fn compile_patterns(
    patterns: HashSet<String>,
    original: &str,
) -> Result<Vec<GlobMatcher>, FilterError> {
    let mut unique: Vec<_> = patterns.into_iter().collect();
    unique.sort();

    let mut matchers = Vec::with_capacity(unique.len());
    for pattern in unique {
        let glob = GlobBuilder::new(&pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|error| FilterError::new(original.to_string(), error))?;
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
