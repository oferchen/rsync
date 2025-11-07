use std::path::Path;
use std::sync::Arc;

use crate::{
    FilterAction, FilterError, FilterRule,
    compiled::{CompiledRule, apply_clear_rule},
    decision::{DecisionContext, FilterSetInner},
};

/// Ordered collection of filter rules.
#[derive(Clone, Debug, Default)]
pub struct FilterSet {
    inner: Arc<FilterSetInner>,
}

impl FilterSet {
    /// Builds a [`FilterSet`] from the supplied rules.
    pub fn from_rules<I>(rules: I) -> Result<Self, FilterError>
    where
        I: IntoIterator<Item = FilterRule>,
    {
        let mut include_exclude = Vec::new();
        let mut protect_risk = Vec::new();

        for rule in rules.into_iter() {
            if rule.is_xattr_only() {
                continue;
            }
            match rule.action {
                FilterAction::Include | FilterAction::Exclude => {
                    include_exclude.push(CompiledRule::new(rule)?);
                }
                FilterAction::Protect | FilterAction::Risk => {
                    protect_risk.push(CompiledRule::new(rule)?);
                }
                FilterAction::Clear => {
                    apply_clear_rule(
                        &mut include_exclude,
                        rule.applies_to_sender,
                        rule.applies_to_receiver,
                    );
                    apply_clear_rule(
                        &mut protect_risk,
                        rule.applies_to_sender,
                        rule.applies_to_receiver,
                    );
                }
            }
        }

        Ok(Self {
            inner: Arc::new(FilterSetInner {
                include_exclude,
                protect_risk,
            }),
        })
    }

    /// Reports whether the set contains any rules.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.include_exclude.is_empty() && self.inner.protect_risk.is_empty()
    }

    /// Determines whether the provided path is allowed.
    #[must_use]
    pub fn allows(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Transfer)
            .allows_transfer()
    }

    /// Determines whether deleting the provided path is permitted.
    ///
    /// Protect directives prevent deletion regardless of the include/exclude
    /// decision, matching upstream `--filter 'protect …'` semantics.
    #[must_use]
    pub fn allows_deletion(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Deletion)
            .allows_deletion()
    }

    /// Determines whether the path may be removed when excluded entries are purged.
    #[must_use]
    pub fn allows_deletion_when_excluded_removed(&self, path: &Path, is_dir: bool) -> bool {
        self.inner
            .decision(path, is_dir, DecisionContext::Deletion)
            .allows_deletion_when_excluded_removed()
    }
}
