use std::ffi::{OsStr, OsString};

use core::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};

/// Represents a parsed filter directive from the CLI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FilterDirective {
    /// A concrete filter rule.
    Rule(FilterRuleSpec),
    /// A merge directive that loads additional filter rules.
    Merge(MergeDirective),
    /// Clears all existing filter rules.
    Clear,
}

/// Describes a filter merge directive including its source and options.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MergeDirective {
    source: OsString,
    options: DirMergeOptions,
}

impl MergeDirective {
    pub(crate) fn new(source: OsString, enforced_kind: Option<FilterRuleKind>) -> Self {
        let mut options = DirMergeOptions::default();
        options = match enforced_kind {
            Some(FilterRuleKind::Include) => {
                options.with_enforced_kind(Some(DirMergeEnforcedKind::Include))
            }
            Some(FilterRuleKind::Exclude) => {
                options.with_enforced_kind(Some(DirMergeEnforcedKind::Exclude))
            }
            _ => options,
        };

        Self { source, options }
    }

    pub(crate) fn with_options(mut self, options: DirMergeOptions) -> Self {
        self.options = options;
        self
    }

    pub(crate) fn source(&self) -> &OsStr {
        self.source.as_os_str()
    }

    pub(crate) fn options(&self) -> &DirMergeOptions {
        &self.options
    }
}

pub(crate) fn merge_directive_options(
    base: &DirMergeOptions,
    directive: &MergeDirective,
) -> DirMergeOptions {
    let defaults = DirMergeOptions::default();
    let current = directive.options();

    let inherit = if current.inherit_rules() != defaults.inherit_rules() {
        current.inherit_rules()
    } else {
        base.inherit_rules()
    };

    let exclude_self = if current.excludes_self() != defaults.excludes_self() {
        current.excludes_self()
    } else {
        base.excludes_self()
    };

    let allow_list_clear = if current.list_clear_allowed() != defaults.list_clear_allowed() {
        current.list_clear_allowed()
    } else {
        base.list_clear_allowed()
    };

    let uses_whitespace = if current.uses_whitespace() != defaults.uses_whitespace() {
        current.uses_whitespace()
    } else {
        base.uses_whitespace()
    };

    let allows_comments = if current.allows_comments() != defaults.allows_comments() {
        current.allows_comments()
    } else {
        base.allows_comments()
    };

    let enforced_kind = if current.enforced_kind() != defaults.enforced_kind() {
        current.enforced_kind()
    } else {
        base.enforced_kind()
    };

    let sender_override = current
        .sender_side_override()
        .or_else(|| base.sender_side_override());
    let receiver_override = current
        .receiver_side_override()
        .or_else(|| base.receiver_side_override());

    let anchor_root = if current.anchor_root_enabled() != defaults.anchor_root_enabled() {
        current.anchor_root_enabled()
    } else {
        base.anchor_root_enabled()
    };

    let mut merged = DirMergeOptions::default()
        .inherit(inherit)
        .exclude_filter_file(exclude_self)
        .allow_list_clearing(allow_list_clear)
        .anchor_root(anchor_root)
        .with_side_overrides(sender_override, receiver_override)
        .with_enforced_kind(enforced_kind);

    if uses_whitespace {
        merged = merged.use_whitespace();
    }

    if !allows_comments {
        merged = merged.allow_comments(false);
    }

    merged
}

pub(crate) fn os_string_to_pattern(value: OsString) -> String {
    match value.into_string() {
        Ok(text) => text,
        Err(value) => value.to_string_lossy().into_owned(),
    }
}
