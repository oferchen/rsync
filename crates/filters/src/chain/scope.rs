//! Per-directory filter scopes and RAII guard.
//!
//! Each [`DirScope`] holds the compiled rules for one directory. Scopes are
//! pushed onto a stack as the traversal enters directories and popped when
//! leaving via the [`DirFilterGuard`] returned by
//! [`FilterChain::enter_directory`](super::FilterChain::enter_directory).
//!
//! [`has_matching_rule`] determines whether any rule inside a scope actually
//! matches a given path, which lets the chain fall through to outer scopes
//! when the innermost scope is silent on a path.

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
}

/// Checks whether a `FilterSet` has any rule that matches the given path.
///
/// This is used to distinguish "no rules matched" (fall through to next scope)
/// from "a rule matched and said include" (stop evaluation).
///
/// We detect a match by checking allows under traversal semantics and
/// allows_deletion: if the path is excluded by either, a rule matched.
/// Using traversal semantics (descendants disabled) for the transfer check
/// mirrors upstream `exclude.c:rule_matches()` which has no descendant
/// matching - the sender walk handles descendant exclusion by not
/// descending into excluded directories. Without this, a `- /bar` rule
/// in an outer scope would synthesize a `bar/**` descendant matcher that
/// makes the scope appear to match every path under `bar/` and short-
/// circuits inner-scope evaluation incorrectly.
pub(super) fn has_matching_rule(filter_set: &FilterSet, path: &Path, is_dir: bool) -> bool {
    if !filter_set.allows_during_traversal(path, is_dir) {
        return true;
    }

    if !filter_set.allows_deletion(path, is_dir) {
        return true;
    }

    // Include-only scopes cannot be detected as "matched" because the default
    // is already allow-all. Return false to fall through to the next scope,
    // matching upstream rsync's per-directory rule prepend semantics.
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
