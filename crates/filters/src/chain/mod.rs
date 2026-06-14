//! Per-directory scoped filter chain with push/pop semantics.
//!
//! [`FilterChain`] extends [`FilterSet`] with per-directory merge file support,
//! implementing upstream rsync's `push_local_filters()` / `pop_local_filters()`
//! pattern from `exclude.c`. When rsync enters a directory, it reads any
//! per-directory merge files (e.g., `.rsync-filter`) and pushes their rules
//! onto a scoped stack. When leaving, the rules are popped. Per-directory rules
//! take priority over global rules (first-match-wins within each layer,
//! innermost directory first).
//!
//! # Chain of Responsibility
//!
//! Evaluation proceeds from the innermost (most recently pushed) directory
//! scope outward to the global base rules. Within each scope, rules use
//! first-match-wins semantics. The first matching rule across all scopes
//! determines the outcome. If no rule matches anywhere, the default is to
//! include the path.
//!
//! # Submodules
//!
//! - `config` - [`DirMergeConfig`] for per-directory merge file behaviour
//! - `scope` - [`DirFilterGuard`] and internal per-directory scope handling
//! - `error` - [`FilterChainError`] for I/O and parse failures
//!
//! # Upstream References
//!
//! - `exclude.c:push_local_filters()` - enter directory, read merge files
//! - `exclude.c:pop_local_filters()` - leave directory, restore state
//! - `exclude.c:change_local_filter_dir()` - depth-tracked push/pop

mod config;
mod error;
mod scope;

#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::Path;

use crate::merge::parse::parse_rules;
use crate::{FilterRule, FilterSet};

pub use config::DirMergeConfig;
pub use error::FilterChainError;
pub use scope::DirFilterGuard;

use scope::{DirScope, has_matching_rule};

/// Per-directory scoped filter chain with push/pop semantics.
///
/// `FilterChain` manages a stack of per-directory filter scopes on top of a
/// global base [`FilterSet`]. When evaluating a path, scopes are checked from
/// innermost (most recently pushed) to outermost, then the global rules.
/// The first matching rule across all layers wins.
///
/// This mirrors upstream rsync's `push_local_filters()` / `pop_local_filters()`
/// from `exclude.c`, which maintains a stack of per-directory merge file rules
/// that are pushed when entering directories and popped when leaving.
///
/// # Examples
///
/// ```
/// use filters::{FilterChain, FilterRule, FilterSet, DirMergeConfig};
/// use std::path::Path;
///
/// let global = FilterSet::from_rules([
///     FilterRule::exclude("*.bak"),
/// ]).unwrap();
///
/// let mut chain = FilterChain::new(global);
/// chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));
///
/// // Global rules apply everywhere
/// assert!(!chain.allows(Path::new("file.bak"), false));
/// assert!(chain.allows(Path::new("file.txt"), false));
/// ```
#[derive(Clone, Debug)]
pub struct FilterChain {
    global: FilterSet,
    merge_configs: Vec<DirMergeConfig>,
    /// Ordered from outermost to innermost; evaluation iterates in reverse.
    scopes: Vec<DirScope>,
    current_depth: usize,
}

impl FilterChain {
    /// Creates a new filter chain with the given global rules.
    ///
    /// The global rules serve as the base layer. Per-directory scopes are
    /// pushed on top during traversal.
    #[must_use]
    pub fn new(global: FilterSet) -> Self {
        Self {
            global,
            merge_configs: Vec::new(),
            scopes: Vec::new(),
            current_depth: 0,
        }
    }

    /// Creates an empty filter chain with no global rules.
    #[must_use]
    pub fn empty() -> Self {
        Self::new(FilterSet::default())
    }

    /// Adds a per-directory merge file configuration.
    ///
    /// When [`enter_directory`](Self::enter_directory) is called, the chain
    /// will look for files matching this configuration's filename and parse
    /// their rules into scoped filter sets.
    pub fn add_merge_config(&mut self, config: DirMergeConfig) {
        self.merge_configs.push(config);
    }

    /// Returns `true` if the path should be included in the transfer.
    ///
    /// Evaluates per-directory scopes from innermost to outermost, then
    /// global rules. First matching rule wins. Paths that match no rule
    /// are included by default.
    ///
    /// This is the traversal-driven sender entry point: synthetic
    /// `pattern/**` descendant matchers (produced for anchored excludes
    /// like `- /bar`) are skipped because the walk itself handles
    /// descendant exclusion by not descending into excluded directories.
    /// Mirrors upstream `exclude.c:rule_matches()` which has no descendant
    /// matching at all.
    ///
    /// `is_dir` should be `true` when the path refers to a directory.
    #[must_use]
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool {
        for scope in self.scopes.iter().rev() {
            if !scope.filter_set.is_empty() && has_matching_rule(&scope.filter_set, path, is_dir) {
                return scope.filter_set.allows_during_traversal(path, is_dir);
            }
        }

        self.global.allows_during_traversal(path, is_dir)
    }

    /// Returns `true` if deleting the path on the receiver is permitted.
    ///
    /// Evaluates per-directory scopes from innermost to outermost, then
    /// global rules. A path may be deleted when it is included by
    /// receiver-side rules and no protect rule matches.
    #[must_use]
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool {
        for scope in self.scopes.iter().rev() {
            if !scope.filter_set.is_empty() && has_matching_rule(&scope.filter_set, path, is_dir) {
                return scope.filter_set.allows_deletion(path, is_dir);
            }
        }

        self.global.allows_deletion(path, is_dir)
    }

    /// Enters a directory, reading any per-directory merge files and pushing
    /// their rules onto the scope stack.
    ///
    /// For each configured merge file, checks whether the file exists in the
    /// given directory. If found, parses its rules and pushes a new scope.
    /// Returns a [`DirFilterGuard`] that pops the scopes when dropped.
    ///
    /// # Arguments
    ///
    /// * `directory` - Absolute path to the directory being entered
    ///
    /// # Errors
    ///
    /// Returns [`FilterChainError`] if a merge file exists but cannot be read
    /// or contains invalid syntax.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `exclude.c:push_local_filters()` which reads each registered
    /// per-dir merge file from the current directory and pushes its rules.
    pub fn enter_directory(
        &mut self,
        directory: &Path,
    ) -> Result<DirFilterGuard, FilterChainError> {
        self.current_depth += 1;
        let depth = self.current_depth;
        let mut pushed_count = 0;

        for config_index in 0..self.merge_configs.len() {
            let config = &self.merge_configs[config_index];
            let merge_path = directory.join(config.filename());

            // upstream: exclude.c:push_local_filters() - parse_filter_file()
            // silently skips missing files
            let content = match fs::read_to_string(&merge_path) {
                Ok(content) => content,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => continue,
                Err(e) => {
                    self.pop_scopes_at_depth(depth);
                    self.current_depth -= 1;
                    return Err(FilterChainError::Io {
                        path: merge_path,
                        source: e,
                    });
                }
            };

            let rules = match parse_rules(&content, &merge_path) {
                Ok(rules) => rules,
                Err(e) => {
                    self.pop_scopes_at_depth(depth);
                    self.current_depth -= 1;
                    return Err(FilterChainError::Parse {
                        path: merge_path,
                        message: e.to_string(),
                    });
                }
            };

            if rules.is_empty() && !config.excludes_self() {
                continue;
            }

            let config = &self.merge_configs[config_index];

            let mut modified_rules: Vec<FilterRule> = rules
                .into_iter()
                .map(|rule| config.apply_modifiers(rule))
                .collect();

            // upstream: exclude.c - FILTRULE_EXCLUDE_SELF handling
            if config.excludes_self() {
                modified_rules.push(FilterRule::exclude(config.filename().to_owned()));
            }

            let filter_set = match FilterSet::from_rules(modified_rules) {
                Ok(set) => set,
                Err(e) => {
                    self.pop_scopes_at_depth(depth);
                    self.current_depth -= 1;
                    return Err(FilterChainError::Filter(e));
                }
            };

            if !filter_set.is_empty() {
                self.scopes.push(DirScope { depth, filter_set });
                pushed_count += 1;
            }
        }

        Ok(DirFilterGuard {
            depth,
            pushed_count,
        })
    }

    /// Leaves a directory, removing all scopes pushed at the given depth.
    ///
    /// This is called when leaving a directory to restore filter state.
    ///
    /// # Upstream Reference
    ///
    /// Mirrors `exclude.c:pop_local_filters()` which restores the filter list
    /// state from before entering the directory.
    pub fn leave_directory(&mut self, guard: DirFilterGuard) {
        self.pop_scopes_at_depth(guard.depth);
        self.current_depth = self.current_depth.saturating_sub(1);
    }

    /// Returns `true` if the chain has no rules at all (global or per-directory).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.global.is_empty() && self.scopes.is_empty()
    }

    /// Number of active per-directory filter scopes on the stack.
    #[must_use]
    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Directory nesting level relative to the transfer root.
    #[must_use]
    pub fn current_depth(&self) -> usize {
        self.current_depth
    }

    /// Base filter set applied before any per-directory rules.
    #[must_use]
    pub fn global(&self) -> &FilterSet {
        &self.global
    }

    /// Removes all scopes at the specified depth.
    fn pop_scopes_at_depth(&mut self, depth: usize) {
        self.scopes.retain(|scope| scope.depth != depth);
    }

    /// Pushes a pre-built filter set as a per-directory scope.
    ///
    /// This is useful for testing or when rules are obtained from sources
    /// other than merge files (e.g., received over the wire from a remote
    /// sender).
    pub fn push_scope(&mut self, filter_set: FilterSet) -> DirFilterGuard {
        self.current_depth += 1;
        let depth = self.current_depth;
        let is_empty = filter_set.is_empty();
        if !is_empty {
            self.scopes.push(DirScope { depth, filter_set });
        }
        DirFilterGuard {
            depth,
            pushed_count: if is_empty { 0 } else { 1 },
        }
    }
}
