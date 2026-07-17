//! Per-directory filter scopes and RAII guard.
//!
//! Each [`DirScope`] holds the compiled rules for one directory. Scopes are
//! pushed onto a stack as the traversal enters directories and popped when
//! leaving via the [`DirFilterGuard`] returned by
//! [`FilterChain::enter_directory`](super::FilterChain::enter_directory).
//!
//! [`scope_has_transfer_match`] and [`scope_has_deletion_match`] determine
//! whether any user-written rule inside a scope matches a given path. They
//! let the chain fall through to outer scopes when the innermost scope is
//! silent on a path, matching upstream `exclude.c:check_filter()` which only
//! returns from a per-directory mergelist when a rule actually matched.

use std::path::Path;

use crate::FilterSet;

/// A per-directory scope containing compiled filter rules.
///
/// Each scope corresponds to one directory's merge file contents. Scopes
/// are stacked during traversal and popped when leaving directories.
#[derive(Clone, Debug)]
pub(super) struct DirScope {
    pub(super) depth: usize,
    pub(super) filter_set: FilterSet,
    /// Whether the merge config that produced this scope inherits to
    /// deeper directories. When `false`, the scope is only consulted at
    /// its own depth, mirroring upstream `FILTRULE_NO_INHERIT`.
    pub(super) inherits: bool,
    /// Index into `FilterChain::merge_configs` of the per-directory merge
    /// config that produced this scope, or `None` for scopes not tied to a
    /// persistent config (e.g. `push_scope`). Scopes sharing a `config_index`
    /// form a single logical mergelist across directories, matching upstream's
    /// per-`dir-merge` `filter_rule_list`. A `!` (clear-list) rule clears only
    /// the scopes of the same `config_index` (`exclude.c:pop_filter_list()`
    /// operates on one `listp`).
    pub(super) config_index: Option<usize>,
}

/// An ancestor scope removed from the stack by a `!` (clear-list) rule.
///
/// Upstream's `!` drops the mergelist's inherited ancestor rules for the
/// duration of the clearing directory and its descendants, then the state is
/// restored when leaving that directory (`exclude.c:pop_local_filters()`
/// rebuilds the pre-descent mergelist). `depth` records the directory depth at
/// which the clear fired so the scope is re-inserted when that directory is
/// left, keeping sibling directories' inherited rules intact.
#[derive(Clone, Debug)]
pub(super) struct ClearedScope {
    pub(super) depth: usize,
    pub(super) scope: DirScope,
}

/// Checks whether a `FilterSet` has any sender-side rule that matches the
/// path, for use in the Transfer (sender) evaluation path.
///
/// Descendant matchers are skipped so the predicate reflects only real
/// user-written rules. Mirrors upstream `exclude.c:rule_matches()` which
/// has no descendant matching at all - descendant exclusion in the walk
/// is a side effect of not descending into excluded directories. Without
/// this guard, a synthetic `bar/**` descendant matcher (compiled for a
/// `- /bar` rule) would short-circuit fall-through to outer scopes even
/// when this scope contains no rule actually applicable to the path.
pub(super) fn scope_has_transfer_match(filter_set: &FilterSet, path: &Path, is_dir: bool) -> bool {
    filter_set.has_transfer_rule_match(path, is_dir)
}

/// Checks whether a `FilterSet` has any receiver-side rule that matches
/// the path, for use in the Deletion (receiver) evaluation path.
///
/// Descendant matchers are skipped for the same reason as
/// [`scope_has_transfer_match`]: synthetic descendant patterns are not
/// user-written rules and must not short-circuit scope fall-through.
pub(super) fn scope_has_deletion_match(filter_set: &FilterSet, path: &Path, is_dir: bool) -> bool {
    filter_set.has_deletion_rule_match(path, is_dir)
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
///
/// [`FilterChain::enter_directory`]: super::FilterChain::enter_directory
/// [`FilterChain::leave_directory`]: super::FilterChain::leave_directory
#[derive(Debug)]
pub struct DirFilterGuard {
    pub(super) depth: usize,
    pub(super) pushed_count: usize,
}

impl DirFilterGuard {
    /// Directory depth at which this guard was pushed.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Number of per-directory scopes introduced by this guard.
    #[must_use]
    pub const fn pushed_count(&self) -> usize {
        self.pushed_count
    }
}
