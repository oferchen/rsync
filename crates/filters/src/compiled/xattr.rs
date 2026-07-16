//! Compiled xattr-name filter rules (the `x` rule modifier).
//!
//! upstream: exclude.c:914 rule_matches() gates every rule on
//! `!(name_flags & NAME_IS_XATTR) ^ !(ex->rflags & FILTRULE_XATTR)`: a rule
//! carrying the `x` modifier (`FILTRULE_XATTR`) matches ONLY when the candidate
//! is an xattr name (`NAME_IS_XATTR`), and a rule WITHOUT `x` never participates
//! in xattr-name matching. These rules are therefore kept out of the ordinary
//! path include/exclude chain and evaluated separately, first-match-wins,
//! against xattr names alone (upstream: xattrs.c:250 rsync_xal_get() consults
//! `name_is_excluded(name, NAME_IS_XATTR, ALL_FILTERS)`).

use std::path::Path;

use super::pattern::{CompiledPattern, compile_patterns};
use crate::{FilterAction, FilterError, FilterRule};

/// A compiled `x`-modifier filter rule matched against xattr names only.
///
/// Only include/exclude actions carry meaning for xattr-name filtering; the
/// deletion (protect/risk) and meta (clear/merge) actions never reach this list
/// because upstream's xattr filter dispatch is a plain include/exclude decision.
#[derive(Debug)]
pub(crate) struct CompiledXattrRule {
    action: FilterAction,
    matchers: Vec<CompiledPattern>,
    negate: bool,
}

impl CompiledXattrRule {
    /// Compiles an `x`-modifier [`FilterRule`] into an xattr-name matcher.
    ///
    /// The pattern is matched exactly as upstream matches an xattr name: the
    /// name has no path separators, so no anchoring or descendant expansion is
    /// applied - only the interior-`**` normalisation shared with ordinary
    /// rules (via [`compile_patterns`]) is used so wildcard semantics stay in
    /// lockstep with the path chain.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] if the pattern is not a valid glob.
    pub(crate) fn new(rule: FilterRule) -> Result<Self, FilterError> {
        debug_assert!(rule.xattr_only, "non-xattr rule compiled as xattr rule");
        let mut patterns = std::collections::HashSet::with_capacity(1);
        patterns.insert(rule.pattern.clone());
        let matchers = compile_patterns(patterns, &rule.pattern, false)?;
        Ok(Self {
            action: rule.action,
            matchers,
            negate: rule.negate,
        })
    }

    /// Returns the rule's action, used to resolve the first-match-wins decision.
    pub(crate) const fn action(&self) -> FilterAction {
        self.action
    }

    /// Tests whether `name` matches this rule, honouring the `!` negate modifier.
    ///
    /// upstream: exclude.c:906 - `ret_match = FILTRULE_NEGATE ? 0 : 1`.
    pub(crate) fn matches(&self, name: &str) -> bool {
        let candidate = Path::new(name);
        let matched = self.matchers.iter().any(|m| m.is_match(candidate));
        matched ^ self.negate
    }
}
