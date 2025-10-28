use std::path::Path;

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
            DecisionContext::Transfer => {
                last_matching_rule(&self.include_exclude, path, is_dir, |rule| {
                    rule.applies_to_sender
                })
            }
            DecisionContext::Deletion => {
                last_matching_rule(&self.include_exclude, path, is_dir, |rule| {
                    rule.applies_to_receiver
                })
            }
        };

        if let Some(rule) = transfer_rule {
            decision.transfer_allowed = matches!(rule.action, FilterAction::Include);
        }

        let protection_rule = match context {
            DecisionContext::Transfer => {
                last_matching_rule(&self.protect_risk, path, is_dir, |rule| {
                    rule.applies_to_sender
                })
            }
            DecisionContext::Deletion => {
                last_matching_rule(&self.protect_risk, path, is_dir, |rule| {
                    rule.applies_to_receiver
                })
            }
        };

        if let Some(rule) = protection_rule {
            match rule.action {
                FilterAction::Protect => decision.protect(),
                FilterAction::Risk => decision.unprotect(),
                FilterAction::Include | FilterAction::Exclude | FilterAction::Clear => {}
            }
        }

        decision
    }
}

fn last_matching_rule<'a, F>(
    rules: &'a [CompiledRule],
    path: &Path,
    is_dir: bool,
    mut applies: F,
) -> Option<&'a CompiledRule>
where
    F: FnMut(&CompiledRule) -> bool,
{
    rules
        .iter()
        .rev()
        .find(|rule| applies(rule) && rule.matches(path, is_dir))
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
}

impl FilterDecision {
    pub(crate) const fn allows_transfer(self) -> bool {
        self.transfer_allowed
    }

    pub(crate) const fn allows_deletion(self) -> bool {
        self.transfer_allowed && !self.protected
    }

    pub(crate) const fn allows_deletion_when_excluded_removed(self) -> bool {
        !self.transfer_allowed && !self.protected
    }

    pub(crate) fn protect(&mut self) {
        self.protected = true;
    }

    pub(crate) fn unprotect(&mut self) {
        self.protected = false;
    }
}

impl Default for FilterDecision {
    fn default() -> Self {
        Self {
            transfer_allowed: true,
            protected: false,
        }
    }
}
