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

/// Which end of the transfer is consulting the xattr chain.
///
/// This is upstream's `am_sender`. Upstream expresses the same distinction
/// indirectly: `send_rules()` stamps each rule's `elide` field from its side
/// flags and `am_sender` (exclude.c:1903-1911), and `rule_matches()` then skips
/// any rule whose `elide` equals `cur_elide_value` (exclude.c:1010). Working
/// the four combinations through, that reduces to one rule - a side-flagged
/// rule participates only on its own side - which is what this enum selects.
/// The `LOCAL_RULE`/`REMOTE_RULE` encoding exists upstream because `elide` also
/// doubles as "do not transmit this rule to the peer" (exclude.c:1913); oc has
/// no such double duty here, so the side is named directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XattrSide {
    /// Upstream's `am_sender != 0`: the file-list builder reading source xattrs.
    Sender,
    /// Upstream's `am_sender == 0`: the generator and every receiver-side read.
    Receiver,
}

/// A compiled `x`-modifier filter rule matched against xattr names only.
///
/// The action is normalised to include/exclude on construction via
/// [`FilterAction::xattr_decision`] - the single owner of upstream's
/// `exclude.c:1345-1358` table, shared with the engine's local-copy chain so
/// the two cannot drift. The meta (clear/merge) actions never reach this list.
#[derive(Debug)]
pub(crate) struct CompiledXattrRule {
    action: FilterAction,
    matchers: Vec<CompiledPattern>,
    negate: bool,
    /// Mirrors `FILTRULE_SENDER_SIDE` / `FILTRULE_RECEIVER_SIDE`. A rule with
    /// no side prefix carries both, so it participates on either end exactly as
    /// an unflagged upstream rule does (its `elide` stays 0, which never equals
    /// `cur_elide_value`).
    applies_to_sender: bool,
    applies_to_receiver: bool,
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
    /// Returns `Ok(None)` for a meta action, which carries no xattr decision.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] if the pattern is not a valid glob.
    pub(crate) fn new(rule: FilterRule) -> Result<Option<Self>, FilterError> {
        debug_assert!(rule.xattr_only, "non-xattr rule compiled as xattr rule");
        let Some(action) = rule.action.xattr_decision() else {
            return Ok(None);
        };
        let mut patterns = std::collections::HashSet::with_capacity(1);
        patterns.insert(rule.pattern.clone());
        let matchers = compile_patterns(patterns, false)?;
        Ok(Some(Self {
            action,
            matchers,
            negate: rule.negate,
            applies_to_sender: rule.applies_to_sender,
            applies_to_receiver: rule.applies_to_receiver,
        }))
    }

    /// Returns the rule's action, used to resolve the first-match-wins decision.
    pub(crate) const fn action(&self) -> FilterAction {
        self.action
    }

    /// Whether this rule participates on `side`.
    ///
    /// upstream: exclude.c:1010 - `if (!*name || ex->elide == cur_elide_value)
    /// return 0;`. A rule carrying the opposite side's flag is elided before
    /// its pattern is ever consulted.
    pub(crate) const fn applies_to(&self, side: XattrSide) -> bool {
        match side {
            XattrSide::Sender => self.applies_to_sender,
            XattrSide::Receiver => self.applies_to_receiver,
        }
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
