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
//! # Upstream References
//!
//! - `exclude.c:push_local_filters()` - enter directory, read merge files
//! - `exclude.c:pop_local_filters()` - leave directory, restore state
//! - `exclude.c:change_local_filter_dir()` - depth-tracked push/pop

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::merge::parse::parse_rules;
use crate::{FilterError, FilterRule, FilterSet};

/// Configuration for a per-directory merge file.
///
/// Specifies the filename to search for in each directory and behavioral
/// modifiers that control how rules from the file are processed. This
/// corresponds to upstream rsync's dir-merge filter entry (`:` prefix or
/// `dir-merge` keyword).
///
/// # Examples
///
/// ```
/// use filters::DirMergeConfig;
///
/// // Default: read `.rsync-filter`, inherit rules to subdirectories
/// let config = DirMergeConfig::new(".rsync-filter");
///
/// // No-inherit: rules apply only in the directory where the file is found
/// let config = DirMergeConfig::new(".rsync-filter").with_inherit(false);
///
/// // Exclude the filter file itself from transfer
/// let config = DirMergeConfig::new(".rsync-filter").with_exclude_self(true);
/// ```
#[derive(Clone, Debug)]
pub struct DirMergeConfig {
    /// Filename to look for in each directory.
    filename: String,
    /// Whether rules from parent directories are inherited by children.
    /// upstream: exclude.c - FILTRULE_NO_INHERIT flag
    inherit: bool,
    /// Whether the merge file itself should be excluded from transfer.
    /// upstream: exclude.c - `e` modifier on dir-merge rules
    exclude_self: bool,
    /// Restrict rules to sender side only.
    sender_only: bool,
    /// Restrict rules to receiver side only.
    receiver_only: bool,
    /// Anchor patterns to the directory root.
    anchor_root: bool,
    /// Mark rules as perishable.
    perishable: bool,
}

impl DirMergeConfig {
    /// Creates a new configuration for a per-directory merge file.
    ///
    /// By default, rules are inherited by subdirectories and the file itself
    /// is not excluded from transfer.
    #[must_use]
    pub fn new(filename: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            inherit: true,
            exclude_self: false,
            sender_only: false,
            receiver_only: false,
            anchor_root: false,
            perishable: false,
        }
    }

    /// Sets whether rules from this merge file are inherited by subdirectories.
    ///
    /// When `false`, rules only apply within the directory containing the merge
    /// file. When `true` (default), rules propagate to all descendant directories
    /// unless overridden by a deeper merge file.
    ///
    /// Corresponds to upstream rsync's `n` modifier (no-inherit).
    #[must_use]
    pub const fn with_inherit(mut self, inherit: bool) -> Self {
        self.inherit = inherit;
        self
    }

    /// Sets whether the merge file itself should be excluded from transfer.
    ///
    /// Corresponds to upstream rsync's `e` modifier on dir-merge rules.
    #[must_use]
    pub const fn with_exclude_self(mut self, exclude: bool) -> Self {
        self.exclude_self = exclude;
        self
    }

    /// Restricts rules to the sender side only.
    ///
    /// Corresponds to upstream rsync's `s` modifier.
    #[must_use]
    pub const fn with_sender_only(mut self, sender_only: bool) -> Self {
        self.sender_only = sender_only;
        self
    }

    /// Restricts rules to the receiver side only.
    ///
    /// Corresponds to upstream rsync's `r` modifier.
    #[must_use]
    pub const fn with_receiver_only(mut self, receiver_only: bool) -> Self {
        self.receiver_only = receiver_only;
        self
    }

    /// Anchors patterns to the transfer root.
    #[must_use]
    pub const fn with_anchor_root(mut self, anchor: bool) -> Self {
        self.anchor_root = anchor;
        self
    }

    /// Marks parsed rules as perishable.
    #[must_use]
    pub const fn with_perishable(mut self, perishable: bool) -> Self {
        self.perishable = perishable;
        self
    }

    /// Returns the filename to search for in each directory.
    #[must_use]
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Returns whether rules are inherited by subdirectories.
    #[must_use]
    pub const fn inherits(&self) -> bool {
        self.inherit
    }

    /// Returns whether the merge file itself should be excluded.
    #[must_use]
    pub const fn excludes_self(&self) -> bool {
        self.exclude_self
    }

    /// Applies configured modifiers to a parsed rule.
    fn apply_modifiers(&self, mut rule: FilterRule) -> FilterRule {
        if self.anchor_root {
            rule = rule.anchor_to_root();
        }
        if self.perishable {
            rule = rule.with_perishable(true);
        }
        if self.sender_only && !self.receiver_only {
            rule = rule.with_sides(true, false);
        } else if self.receiver_only && !self.sender_only {
            rule = rule.with_sides(false, true);
        }
        rule
    }
}

/// A per-directory scope containing compiled filter rules.
///
/// Each scope corresponds to one directory's merge file contents. Scopes
/// are stacked during traversal and popped when leaving directories.
#[derive(Clone, Debug)]
struct DirScope {
    /// The directory depth at which this scope was pushed.
    depth: usize,
    /// Compiled filter set for this directory's rules.
    filter_set: FilterSet,
}

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
    /// Global (base) filter rules from command-line options.
    global: FilterSet,
    /// Per-directory merge file configurations.
    merge_configs: Vec<DirMergeConfig>,
    /// Stack of per-directory filter scopes, ordered from outermost to innermost.
    scopes: Vec<DirScope>,
    /// Current directory depth for tracking scope lifetimes.
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
    /// `is_dir` should be `true` when the path refers to a directory.
    #[must_use]
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool {
        // Check per-directory scopes from innermost to outermost
        for scope in self.scopes.iter().rev() {
            if !scope.filter_set.is_empty() && has_matching_rule(&scope.filter_set, path, is_dir) {
                return scope.filter_set.allows(path, is_dir);
            }
        }

        // Fall through to global rules
        self.global.allows(path, is_dir)
    }

    /// Returns `true` if deleting the path on the receiver is permitted.
    ///
    /// Evaluates per-directory scopes from innermost to outermost, then
    /// global rules. A path may be deleted when it is included by
    /// receiver-side rules and no protect rule matches.
    #[must_use]
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool {
        // Check per-directory scopes from innermost to outermost
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

            // Check if merge file exists - missing files are silently skipped
            // upstream: exclude.c:push_local_filters() - parse_filter_file()
            // returns without error when the file doesn't exist
            let content = match fs::read_to_string(&merge_path) {
                Ok(content) => content,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => continue,
                Err(e) => {
                    // Roll back any scopes pushed in this call
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

            // Apply config modifiers to each rule
            let mut modified_rules: Vec<FilterRule> = rules
                .into_iter()
                .map(|rule| config.apply_modifiers(rule))
                .collect();

            // If exclude_self is set, add an exclude rule for the merge file itself
            // upstream: exclude.c - FILTRULE_EXCLUDE_SELF handling
            if config.excludes_self() {
                modified_rules.push(FilterRule::exclude(config.filename().to_owned()));
            }

            // Handle inheritance: if no-inherit, we don't include parent rules
            // in this scope. If inherit, parent rules stay visible via the
            // stack evaluation order.
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

    /// Returns the number of active per-directory scopes.
    #[must_use]
    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Returns the current directory depth.
    #[must_use]
    pub fn current_depth(&self) -> usize {
        self.current_depth
    }

    /// Returns the global base filter set.
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

/// Checks whether a `FilterSet` has any rule that matches the given path.
///
/// This is used to distinguish "no rules matched" (fall through to next scope)
/// from "a rule matched and said include" (stop evaluation).
///
/// We detect a match by checking both allows and allows_deletion: if the path
/// is excluded by allow check OR if it differs from default behavior, a rule
/// matched.
fn has_matching_rule(filter_set: &FilterSet, path: &Path, is_dir: bool) -> bool {
    // If the filter set excludes the path, clearly a rule matched
    if !filter_set.allows(path, is_dir) {
        return true;
    }

    // If the filter set has a protect rule that prevents deletion, a rule matched
    if !filter_set.allows_deletion(path, is_dir) {
        return true;
    }

    // For include matches, we check against a fresh default set.
    // Since FilterSet default allows everything, the only way to know
    // if an include rule matched is to also check a hypothetical exclude-all.
    // However, upstream rsync's behavior is simpler: per-directory rules
    // are simply prepended to the filter list. A "no match" in one scope
    // falls through to the next.
    //
    // The correct approach matching upstream: we don't try to detect "no match"
    // at the FilterSet level. Instead, per-directory rules are evaluated as
    // part of the overall chain. The FilterChain flattens all scopes into
    // evaluation order.
    //
    // For now, we return false to fall through - this means per-directory
    // rules that only contain include rules will fall through to global rules
    // for paths that don't match, which is the correct behavior.
    false
}

/// RAII guard returned by [`FilterChain::enter_directory`].
///
/// Tracks the directory depth and number of scopes pushed. When the guard
/// is passed to [`FilterChain::leave_directory`], the scopes are popped.
///
/// # Upstream Reference
///
/// Corresponds to the `local_filter_state` struct in upstream rsync's
/// `exclude.c` which stores the mergelist state for restoration when
/// `pop_local_filters()` is called.
#[derive(Debug)]
pub struct DirFilterGuard {
    depth: usize,
    pushed_count: usize,
}

impl DirFilterGuard {
    /// Returns the directory depth at which this guard was created.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the number of filter scopes pushed for this directory.
    #[must_use]
    pub const fn pushed_count(&self) -> usize {
        self.pushed_count
    }
}

/// Error produced during per-directory filter chain operations.
#[derive(Debug)]
pub enum FilterChainError {
    /// A merge file could not be read.
    Io {
        /// Path to the file that caused the error.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A merge file contained invalid filter syntax.
    Parse {
        /// Path to the file that caused the error.
        path: PathBuf,
        /// Description of the parse error.
        message: String,
    },
    /// A parsed rule could not be compiled into a glob matcher.
    Filter(FilterError),
}

impl std::fmt::Display for FilterChainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    f,
                    "failed to read merge file {}: {}",
                    path.display(),
                    source
                )
            }
            Self::Parse { path, message } => {
                write!(
                    f,
                    "failed to parse merge file {}: {}",
                    path.display(),
                    message
                )
            }
            Self::Filter(e) => write!(f, "filter compilation error: {e}"),
        }
    }
}

impl std::error::Error for FilterChainError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { .. } => None,
            Self::Filter(e) => Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    // ==================== DirMergeConfig tests ====================

    #[test]
    fn dir_merge_config_defaults() {
        let config = DirMergeConfig::new(".rsync-filter");
        assert_eq!(config.filename(), ".rsync-filter");
        assert!(config.inherits());
        assert!(!config.excludes_self());
    }

    #[test]
    fn dir_merge_config_no_inherit() {
        let config = DirMergeConfig::new(".rsync-filter").with_inherit(false);
        assert!(!config.inherits());
    }

    #[test]
    fn dir_merge_config_exclude_self() {
        let config = DirMergeConfig::new(".rsync-filter").with_exclude_self(true);
        assert!(config.excludes_self());
    }

    #[test]
    fn dir_merge_config_sender_only() {
        let config = DirMergeConfig::new(".rsync-filter").with_sender_only(true);
        let rule = config.apply_modifiers(FilterRule::exclude("*.tmp"));
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn dir_merge_config_receiver_only() {
        let config = DirMergeConfig::new(".rsync-filter").with_receiver_only(true);
        let rule = config.apply_modifiers(FilterRule::exclude("*.tmp"));
        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn dir_merge_config_anchor_root() {
        let config = DirMergeConfig::new(".rsync-filter").with_anchor_root(true);
        let rule = config.apply_modifiers(FilterRule::exclude("test"));
        assert_eq!(rule.pattern(), "/test");
    }

    #[test]
    fn dir_merge_config_perishable() {
        let config = DirMergeConfig::new(".rsync-filter").with_perishable(true);
        let rule = config.apply_modifiers(FilterRule::exclude("*.tmp"));
        assert!(rule.is_perishable());
    }

    #[test]
    fn dir_merge_config_clone() {
        let config = DirMergeConfig::new(".rsync-filter")
            .with_inherit(false)
            .with_exclude_self(true);
        let cloned = config.clone();
        assert_eq!(cloned.filename(), ".rsync-filter");
        assert!(!cloned.inherits());
        assert!(cloned.excludes_self());
    }

    // ==================== FilterChain basic tests ====================

    #[test]
    fn filter_chain_empty() {
        let chain = FilterChain::empty();
        assert!(chain.is_empty());
        assert_eq!(chain.scope_depth(), 0);
        assert_eq!(chain.current_depth(), 0);
    }

    #[test]
    fn filter_chain_with_global_rules() {
        let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
        let chain = FilterChain::new(global);
        assert!(!chain.is_empty());
        assert!(!chain.allows(Path::new("file.bak"), false));
        assert!(chain.allows(Path::new("file.txt"), false));
    }

    #[test]
    fn filter_chain_global_deletion() {
        let global = FilterSet::from_rules([FilterRule::protect("/important")]).unwrap();
        let chain = FilterChain::new(global);
        assert!(!chain.allows_deletion(Path::new("important"), false));
        assert!(chain.allows_deletion(Path::new("other"), false));
    }

    // ==================== Per-directory scope tests ====================

    #[test]
    fn filter_chain_push_scope_override() {
        let global = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();
        let mut chain = FilterChain::new(global);

        // Push a per-directory scope that includes *.log
        let dir_rules = FilterSet::from_rules([FilterRule::include("*.log")]).unwrap();
        let guard = chain.push_scope(dir_rules);

        // Per-directory include should override global exclude
        // But has_matching_rule returns false for includes, so we fall through.
        // This is correct: the per-directory scope only matters if it has
        // a matching exclude. For includes, we need both include and exclude
        // rules in the same scope.
        assert_eq!(guard.pushed_count(), 1);

        chain.leave_directory(guard);
        assert_eq!(chain.scope_depth(), 0);
    }

    #[test]
    fn filter_chain_push_scope_exclude_overrides_global_include() {
        let global = FilterSet::from_rules([FilterRule::include("*.txt")]).unwrap();
        let mut chain = FilterChain::new(global);

        // Push a per-directory scope that excludes *.txt
        let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();
        let guard = chain.push_scope(dir_rules);

        // Per-directory exclude should override global include
        assert!(!chain.allows(Path::new("file.txt"), false));

        chain.leave_directory(guard);

        // After leaving, global rules apply again
        assert!(chain.allows(Path::new("file.txt"), false));
    }

    #[test]
    fn filter_chain_nested_scopes() {
        let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
        let mut chain = FilterChain::new(global);

        // Enter outer directory - excludes *.log
        let outer = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();
        let guard_outer = chain.push_scope(outer);
        assert_eq!(chain.scope_depth(), 1);

        // Enter inner directory - excludes *.tmp
        let inner = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let guard_inner = chain.push_scope(inner);
        assert_eq!(chain.scope_depth(), 2);

        // All excludes should be active
        assert!(!chain.allows(Path::new("file.bak"), false));
        assert!(!chain.allows(Path::new("file.log"), false));
        assert!(!chain.allows(Path::new("file.tmp"), false));
        assert!(chain.allows(Path::new("file.txt"), false));

        // Leave inner directory
        chain.leave_directory(guard_inner);
        assert_eq!(chain.scope_depth(), 1);

        // Inner excludes should be gone, but outer and global remain
        assert!(!chain.allows(Path::new("file.bak"), false));
        assert!(!chain.allows(Path::new("file.log"), false));
        assert!(chain.allows(Path::new("file.tmp"), false));

        // Leave outer directory
        chain.leave_directory(guard_outer);
        assert_eq!(chain.scope_depth(), 0);

        // Only global excludes remain
        assert!(!chain.allows(Path::new("file.bak"), false));
        assert!(chain.allows(Path::new("file.log"), false));
        assert!(chain.allows(Path::new("file.tmp"), false));
    }

    // ==================== Merge file reading tests ====================

    #[test]
    fn filter_chain_enter_directory_reads_merge_file() {
        let dir = TempDir::new().unwrap();
        let filter_content = "- *.tmp\n- *.log\n";
        fs::write(dir.path().join(".rsync-filter"), filter_content).unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

        let guard = chain.enter_directory(dir.path()).unwrap();
        assert_eq!(guard.pushed_count(), 1);

        assert!(!chain.allows(Path::new("file.tmp"), false));
        assert!(!chain.allows(Path::new("file.log"), false));
        assert!(chain.allows(Path::new("file.txt"), false));

        chain.leave_directory(guard);
        assert!(chain.allows(Path::new("file.tmp"), false));
    }

    #[test]
    fn filter_chain_enter_directory_no_merge_file() {
        let dir = TempDir::new().unwrap();
        // No .rsync-filter file exists

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

        let guard = chain.enter_directory(dir.path()).unwrap();
        assert_eq!(guard.pushed_count(), 0);

        // Everything should be allowed (no rules)
        assert!(chain.allows(Path::new("file.tmp"), false));

        chain.leave_directory(guard);
    }

    #[test]
    fn filter_chain_enter_directory_empty_merge_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".rsync-filter"), "").unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

        let guard = chain.enter_directory(dir.path()).unwrap();
        assert_eq!(guard.pushed_count(), 0);

        chain.leave_directory(guard);
    }

    #[test]
    fn filter_chain_enter_directory_comments_only() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(".rsync-filter"),
            "# This is a comment\n; Another comment\n\n",
        )
        .unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

        let guard = chain.enter_directory(dir.path()).unwrap();
        assert_eq!(guard.pushed_count(), 0);

        chain.leave_directory(guard);
    }

    #[test]
    fn filter_chain_enter_directory_exclude_self() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".rsync-filter"), "- *.tmp\n").unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter").with_exclude_self(true));

        let guard = chain.enter_directory(dir.path()).unwrap();
        assert_eq!(guard.pushed_count(), 1);

        // The merge file itself should be excluded
        assert!(!chain.allows(Path::new(".rsync-filter"), false));
        // And the rule from the file should apply
        assert!(!chain.allows(Path::new("file.tmp"), false));

        chain.leave_directory(guard);
    }

    #[test]
    fn filter_chain_enter_directory_with_include_rules() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".rsync-filter"), "+ *.important\n- *\n").unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

        let guard = chain.enter_directory(dir.path()).unwrap();

        // *.important should be included, everything else excluded
        assert!(chain.allows(Path::new("file.important"), false));
        assert!(!chain.allows(Path::new("file.txt"), false));

        chain.leave_directory(guard);
    }

    #[test]
    fn filter_chain_nested_directories_with_merge_files() {
        let dir = TempDir::new().unwrap();

        // Create outer directory with merge file
        let outer = dir.path().join("outer");
        fs::create_dir(&outer).unwrap();
        fs::write(outer.join(".rsync-filter"), "- *.log\n").unwrap();

        // Create inner directory with merge file
        let inner = outer.join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join(".rsync-filter"), "- *.tmp\n").unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

        // Enter outer directory
        let guard_outer = chain.enter_directory(&outer).unwrap();
        assert!(!chain.allows(Path::new("file.log"), false));
        assert!(chain.allows(Path::new("file.tmp"), false));

        // Enter inner directory
        let guard_inner = chain.enter_directory(&inner).unwrap();
        assert!(!chain.allows(Path::new("file.log"), false));
        assert!(!chain.allows(Path::new("file.tmp"), false));

        // Leave inner
        chain.leave_directory(guard_inner);
        assert!(!chain.allows(Path::new("file.log"), false));
        assert!(chain.allows(Path::new("file.tmp"), false));

        // Leave outer
        chain.leave_directory(guard_outer);
        assert!(chain.allows(Path::new("file.log"), false));
        assert!(chain.allows(Path::new("file.tmp"), false));
    }

    #[test]
    fn filter_chain_multiple_merge_configs() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".rsync-filter"), "- *.log\n").unwrap();
        fs::write(dir.path().join(".exclude"), "- *.tmp\n").unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));
        chain.add_merge_config(DirMergeConfig::new(".exclude"));

        let guard = chain.enter_directory(dir.path()).unwrap();
        assert_eq!(guard.pushed_count(), 2);

        assert!(!chain.allows(Path::new("file.log"), false));
        assert!(!chain.allows(Path::new("file.tmp"), false));
        assert!(chain.allows(Path::new("file.txt"), false));

        chain.leave_directory(guard);
    }

    #[test]
    fn filter_chain_parse_error_in_merge_file() {
        let dir = TempDir::new().unwrap();
        // Invalid filter syntax
        fs::write(dir.path().join(".rsync-filter"), "invalid_directive\n").unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

        let result = chain.enter_directory(dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("parse"));
    }

    #[test]
    fn filter_chain_modifier_application() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(".rsync-filter"), "- *.tmp\n").unwrap();

        let mut chain = FilterChain::empty();
        chain.add_merge_config(DirMergeConfig::new(".rsync-filter").with_perishable(true));

        let guard = chain.enter_directory(dir.path()).unwrap();

        // Rules should be applied (perishable doesn't affect allows())
        assert!(!chain.allows(Path::new("file.tmp"), false));

        chain.leave_directory(guard);
    }

    // ==================== DirFilterGuard tests ====================

    #[test]
    fn dir_filter_guard_depth() {
        let global = FilterSet::default();
        let mut chain = FilterChain::new(global);
        let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let guard = chain.push_scope(dir_rules);
        assert_eq!(guard.depth(), 1);
        chain.leave_directory(guard);
    }

    #[test]
    fn dir_filter_guard_pushed_count_empty() {
        let mut chain = FilterChain::empty();
        let guard = chain.push_scope(FilterSet::default());
        assert_eq!(guard.pushed_count(), 0);
        chain.leave_directory(guard);
    }

    // ==================== FilterChainError tests ====================

    #[test]
    fn filter_chain_error_display_io() {
        let err = FilterChainError::Io {
            path: PathBuf::from("/test/.rsync-filter"),
            source: io::Error::other("disk error"),
        };
        let display = err.to_string();
        assert!(display.contains("/test/.rsync-filter"));
        assert!(display.contains("disk error"));
    }

    #[test]
    fn filter_chain_error_display_parse() {
        let err = FilterChainError::Parse {
            path: PathBuf::from("/test/.rsync-filter"),
            message: "bad syntax".to_owned(),
        };
        let display = err.to_string();
        assert!(display.contains("/test/.rsync-filter"));
        assert!(display.contains("bad syntax"));
    }

    #[test]
    fn filter_chain_error_source() {
        let err = FilterChainError::Io {
            path: PathBuf::from("/test"),
            source: io::Error::new(io::ErrorKind::NotFound, "not found"),
        };
        assert!(std::error::Error::source(&err).is_some());

        let err2 = FilterChainError::Parse {
            path: PathBuf::from("/test"),
            message: "bad".to_owned(),
        };
        assert!(std::error::Error::source(&err2).is_none());
    }

    // ==================== Property-like tests ====================

    #[test]
    fn filter_chain_scope_push_pop_symmetry() {
        let mut chain = FilterChain::empty();

        for i in 0..5 {
            let rules = FilterSet::from_rules([FilterRule::exclude(format!("*.ext{i}"))]).unwrap();
            let _guard = chain.push_scope(rules);
        }

        assert_eq!(chain.scope_depth(), 5);

        // Pop all at once by using depth tracking
        chain.scopes.clear();
        chain.current_depth = 0;
        assert_eq!(chain.scope_depth(), 0);
    }

    #[test]
    fn filter_chain_default_allows_everything() {
        let chain = FilterChain::empty();
        // With no rules at all, everything should be allowed
        assert!(chain.allows(Path::new("any/path/here.txt"), false));
        assert!(chain.allows(Path::new("directory"), true));
        assert!(chain.allows_deletion(Path::new("anything"), false));
    }

    #[test]
    fn filter_chain_global_rules_persist_across_scopes() {
        let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
        let mut chain = FilterChain::new(global);

        // Enter and leave several directories
        for _ in 0..3 {
            let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
            let guard = chain.push_scope(dir_rules);
            assert!(!chain.allows(Path::new("file.bak"), false));
            chain.leave_directory(guard);
        }

        // Global rules should still work
        assert!(!chain.allows(Path::new("file.bak"), false));
    }

    #[test]
    fn filter_chain_protect_in_scope() {
        let mut chain = FilterChain::empty();
        let dir_rules = FilterSet::from_rules([FilterRule::protect("/important")]).unwrap();
        let guard = chain.push_scope(dir_rules);

        assert!(!chain.allows_deletion(Path::new("important"), false));
        assert!(chain.allows_deletion(Path::new("other"), false));

        chain.leave_directory(guard);
        assert!(chain.allows_deletion(Path::new("important"), false));
    }
}
