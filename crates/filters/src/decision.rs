use std::path::Path;

use logging::debug_log;

use crate::{FilterAction, compiled::CompiledRule};

#[derive(Debug, Default)]
pub(crate) struct FilterSetInner {
    pub(crate) include_exclude: Vec<CompiledRule>,
    pub(crate) protect_risk: Vec<CompiledRule>,
}

impl FilterSetInner {
    pub(crate) fn decision(
        &self,
        path: &Path,
        is_dir: bool,
        context: DecisionContext,
    ) -> FilterDecision {
        let mut decision = FilterDecision::default();

        let transfer_rule = match context {
            DecisionContext::Transfer => first_matching_rule(
                &self.include_exclude,
                path,
                is_dir,
                |rule| rule.applies_to_sender,
                true,
            ),
            DecisionContext::Deletion => first_matching_rule(
                &self.include_exclude,
                path,
                is_dir,
                |rule| rule.applies_to_receiver,
                false,
            ),
        };

        if matches!(context, DecisionContext::Deletion)
            && let Some(rule) = first_matching_rule(
                &self.include_exclude,
                path,
                is_dir,
                |rule| rule.applies_to_receiver,
                true,
            )
        {
            decision.excluded_for_delete_excluded = matches!(rule.action, FilterAction::Exclude);
        }

        if let Some(rule) = transfer_rule {
            let allowed = matches!(rule.action, FilterAction::Include);
            decision.transfer_allowed = allowed;

            if allowed {
                debug_log!(Filter, 1, "including {:?} (matched rule)", path);
            } else {
                debug_log!(Filter, 1, "excluding {:?} (matched rule)", path);
            }
        }

        let protection_rule = match context {
            DecisionContext::Transfer => first_matching_rule(
                &self.protect_risk,
                path,
                is_dir,
                |rule| rule.applies_to_sender,
                true,
            ),
            DecisionContext::Deletion => first_matching_rule(
                &self.protect_risk,
                path,
                is_dir,
                |rule| rule.applies_to_receiver,
                false,
            ),
        };

        if let Some(rule) = protection_rule {
            match rule.action {
                FilterAction::Protect => decision.protect(),
                FilterAction::Risk => decision.unprotect(),
                // Include/Exclude/Clear/Merge/DirMerge are not protection actions.
                // Merge and DirMerge are expanded before compilation, so they
                // should never appear in compiled rules.
                FilterAction::Include
                | FilterAction::Exclude
                | FilterAction::Clear
                | FilterAction::Merge
                | FilterAction::DirMerge => {}
            }
        }

        decision
    }
}

/// Finds the first matching rule in the list (first-match-wins semantics).
///
/// This matches upstream rsync's `check_filter()` in exclude.c which iterates
/// from the head of the filter list and returns on the first match.
///
/// # Arguments
///
/// * `rules` - Compiled rules to search, evaluated in order
/// * `path` - File path to match against rule patterns
/// * `is_dir` - Whether the path is a directory (affects trailing-slash patterns)
/// * `applies` - Predicate filtering which rules are considered (e.g., sender-only rules)
/// * `include_perishable` - Whether to consider perishable rules (marked with `p` modifier)
///
/// # Returns
///
/// The first rule where all conditions are met:
/// 1. `include_perishable` is true OR the rule is not perishable
/// 2. `applies(rule)` returns true
/// 3. The rule's pattern matches `path` considering `is_dir`
fn first_matching_rule<'a, F>(
    rules: &'a [CompiledRule],
    path: &Path,
    is_dir: bool,
    mut applies: F,
    include_perishable: bool,
) -> Option<&'a CompiledRule>
where
    F: FnMut(&CompiledRule) -> bool,
{
    rules.iter().find(|rule| {
        (include_perishable || !rule.perishable) && applies(rule) && rule.matches(path, is_dir)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecisionContext {
    Transfer,
    Deletion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FilterDecision {
    transfer_allowed: bool,
    protected: bool,
    excluded_for_delete_excluded: bool,
}

impl FilterDecision {
    pub(crate) const fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    pub(crate) const fn allows_deletion(self) -> bool {
        self.transfer_allowed && !self.protected
    }

    pub(crate) const fn allows_deletion_when_excluded_removed(self) -> bool {
        self.excluded_for_delete_excluded && !self.protected
    }

    pub(crate) const fn protect(&mut self) {
        self.protected = true;
    }

    pub(crate) const fn unprotect(&mut self) {
        self.protected = false;
    }
}

impl Default for FilterDecision {
    fn default() -> Self {
        Self {
            transfer_allowed: true,
            protected: false,
            excluded_for_delete_excluded: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_decision_default() {
        let decision = FilterDecision::default();
        assert!(decision.allows_transfer());
        assert!(decision.allows_deletion());
        assert!(!decision.allows_deletion_when_excluded_removed());
    }

    #[test]
    fn filter_decision_protect() {
        let mut decision = FilterDecision::default();
        decision.protect();
        assert!(decision.allows_transfer());
        assert!(!decision.allows_deletion());
    }

    #[test]
    fn filter_decision_unprotect() {
        let mut decision = FilterDecision::default();
        decision.protect();
        decision.unprotect();
        assert!(decision.allows_transfer());
        assert!(decision.allows_deletion());
    }

    #[test]
    fn filter_decision_transfer_not_allowed() {
        let decision = FilterDecision {
            transfer_allowed: false,
            protected: false,
            excluded_for_delete_excluded: false,
        };
        assert!(!decision.allows_transfer());
        assert!(!decision.allows_deletion());
    }

    #[test]
    fn filter_decision_excluded_for_delete_excluded() {
        let decision = FilterDecision {
            transfer_allowed: false,
            protected: false,
            excluded_for_delete_excluded: true,
        };
        assert!(decision.allows_deletion_when_excluded_removed());
    }

    #[test]
    fn filter_decision_protected_blocks_excluded_removal() {
        let decision = FilterDecision {
            transfer_allowed: false,
            protected: true,
            excluded_for_delete_excluded: true,
        };
        assert!(!decision.allows_deletion_when_excluded_removed());
    }

    #[test]
    fn decision_context_eq() {
        assert_eq!(DecisionContext::Transfer, DecisionContext::Transfer);
        assert_eq!(DecisionContext::Deletion, DecisionContext::Deletion);
        assert_ne!(DecisionContext::Transfer, DecisionContext::Deletion);
    }

    #[test]
    fn filter_set_inner_default_is_empty() {
        let inner = FilterSetInner::default();
        assert!(inner.include_exclude.is_empty());
        assert!(inner.protect_risk.is_empty());
    }
}
