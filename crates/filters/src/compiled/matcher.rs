//! The public face of the compiled rule matcher.
//!
//! [`FilterSet`](crate::FilterSet) and the local copy's filter program both
//! evaluate rules through these types, so one port of upstream
//! `exclude.c:rule_matches()` decides every path and xattr match.

use std::path::Path;

use super::{CompiledRule, CompiledXattrRule, XattrSide};
use crate::{FilterAction, FilterError, FilterRule, OwnedRuleSource};

/// One path rule compiled for matching.
///
/// upstream: exclude.c:1002-1081 rule_matches(). Matching uses the crate's
/// port of `lib/wildmatch.c`, so `*`, `**`, `?`, `[...]` and `\` behave as
/// upstream's do.
#[derive(Clone, Debug)]
pub struct RuleMatcher(CompiledRule);

impl RuleMatcher {
    /// Compiles `rule`.
    ///
    /// # Errors
    ///
    /// Never fails in practice: upstream rejects no pattern, and neither does
    /// the wildmatch port. The signature matches
    /// [`FilterSet::from_rules`](crate::FilterSet::from_rules).
    pub fn new(rule: FilterRule) -> Result<Self, FilterError> {
        CompiledRule::new(rule).map(Self)
    }

    /// Whether the rule fires for `path`, a name relative to the transfer
    /// root, on the sender walk.
    ///
    /// upstream: rule_matches() has no descendant matching; the walk prunes an
    /// excluded directory instead, so descendants are not consulted.
    #[must_use]
    pub fn matches(&self, path: &Path, is_dir: bool) -> bool {
        self.0.matches(path, is_dir, false)
    }

    /// Whether the rule fires for `path` on the receiver's deletion scan,
    /// where an excluded directory also covers its contents.
    #[must_use]
    pub fn matches_for_deletion(&self, path: &Path, is_dir: bool) -> bool {
        self.0.matches_for_deletion(path, is_dir, false)
    }

    /// The rule's action.
    #[must_use]
    pub const fn action(&self) -> FilterAction {
        self.0.action
    }

    /// Whether the rule applies on the sending side.
    #[must_use]
    pub const fn applies_to_sender(&self) -> bool {
        self.0.applies_to_sender
    }

    /// Whether the rule applies on the receiving side.
    #[must_use]
    pub const fn applies_to_receiver(&self) -> bool {
        self.0.applies_to_receiver
    }

    /// Whether the rule was written in directory form (a trailing `/`).
    #[must_use]
    pub const fn is_directory_only(&self) -> bool {
        self.0.is_directory_only()
    }

    /// The pattern as written, for `--debug=FILTER` reporting.
    #[must_use]
    pub fn pattern(&self) -> &[u8] {
        &self.0.pattern
    }

    /// Where the rule's text came from, which decides whether a trace may show
    /// it (exclude.c:88-131 rule_text, rule_detail).
    #[must_use]
    pub const fn source(&self) -> &OwnedRuleSource {
        &self.0.source
    }
}

/// One xattr-name rule compiled for matching.
///
/// upstream: exclude.c:1013 - an `x` rule matches only xattr names, through
/// the same rule_matches() as a path rule.
#[derive(Clone, Debug)]
pub struct XattrMatcher(CompiledXattrRule);

impl XattrMatcher {
    /// Compiles `rule`, an xattr-only rule. Returns `Ok(None)` for an action
    /// that carries no xattr decision.
    ///
    /// # Errors
    ///
    /// As [`RuleMatcher::new`].
    pub fn new(rule: FilterRule) -> Result<Option<Self>, FilterError> {
        CompiledXattrRule::new(rule).map(|compiled| compiled.map(Self))
    }

    /// The include or exclude decision the rule makes.
    #[must_use]
    pub const fn action(&self) -> FilterAction {
        self.0.action()
    }

    /// Whether the rule participates on `side` (exclude.c:1010).
    #[must_use]
    pub const fn applies_to(&self, side: XattrSide) -> bool {
        self.0.applies_to(side)
    }

    /// Whether the rule fires for the xattr `name`, `!` applied.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        self.0.matches(name)
    }
}
