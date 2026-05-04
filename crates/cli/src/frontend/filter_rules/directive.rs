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

    pub(crate) const fn with_options(mut self, options: DirMergeOptions) -> Self {
        self.options = options;
        self
    }

    pub(crate) fn source(&self) -> &OsStr {
        self.source.as_os_str()
    }

    pub(crate) const fn options(&self) -> &DirMergeOptions {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_directive_new_no_enforced() {
        let directive = MergeDirective::new(OsString::from("filter.txt"), None);
        assert_eq!(directive.source(), OsStr::new("filter.txt"));
        assert_eq!(directive.options().enforced_kind(), None);
    }

    #[test]
    fn merge_directive_new_include_enforced() {
        let directive =
            MergeDirective::new(OsString::from("includes"), Some(FilterRuleKind::Include));
        assert_eq!(directive.source(), OsStr::new("includes"));
        assert_eq!(
            directive.options().enforced_kind(),
            Some(DirMergeEnforcedKind::Include)
        );
    }

    #[test]
    fn merge_directive_new_exclude_enforced() {
        let directive =
            MergeDirective::new(OsString::from("excludes"), Some(FilterRuleKind::Exclude));
        assert_eq!(directive.source(), OsStr::new("excludes"));
        assert_eq!(
            directive.options().enforced_kind(),
            Some(DirMergeEnforcedKind::Exclude)
        );
    }

    #[test]
    fn merge_directive_with_options() {
        let directive = MergeDirective::new(OsString::from("filter"), None);
        let new_options = DirMergeOptions::default().inherit(true);
        let updated = directive.with_options(new_options);
        assert!(updated.options().inherit_rules());
    }

    #[test]
    fn merge_directive_options_returns_reference() {
        let directive = MergeDirective::new(OsString::from("rules.txt"), None);
        let options = directive.options();
        // Verify options are accessible via the reference
        let _ = options.inherit_rules();
    }

    #[test]
    fn filter_directive_eq() {
        let a = FilterDirective::Clear;
        let b = FilterDirective::Clear;
        assert_eq!(a, b);
    }

    #[test]
    fn filter_directive_merge_eq() {
        let a = FilterDirective::Merge(MergeDirective::new(OsString::from("file"), None));
        let b = FilterDirective::Merge(MergeDirective::new(OsString::from("file"), None));
        assert_eq!(a, b);
    }

    #[test]
    fn merge_directive_options_inherit() {
        let base = DirMergeOptions::default().inherit(true);
        let directive = MergeDirective::new(OsString::from("rules"), None);
        let merged = merge_directive_options(&base, &directive);
        assert!(merged.inherit_rules());
    }

    #[test]
    fn merge_directive_options_override_inherit() {
        let base = DirMergeOptions::default().inherit(true);
        let directive_options = DirMergeOptions::default().inherit(false);
        let directive =
            MergeDirective::new(OsString::from("rules"), None).with_options(directive_options);
        let merged = merge_directive_options(&base, &directive);
        // Directive overrides base since it differs from default
        assert!(!merged.inherit_rules());
    }

    #[test]
    fn merge_directive_options_exclude_self() {
        let base = DirMergeOptions::default().exclude_filter_file(true);
        let directive = MergeDirective::new(OsString::from("rules"), None);
        let merged = merge_directive_options(&base, &directive);
        assert!(merged.excludes_self());
    }

    #[test]
    fn merge_directive_options_allow_list_clear() {
        let base = DirMergeOptions::default().allow_list_clearing(true);
        let directive = MergeDirective::new(OsString::from("rules"), None);
        let merged = merge_directive_options(&base, &directive);
        assert!(merged.list_clear_allowed());
    }

    #[test]
    fn merge_directive_options_uses_whitespace() {
        let base = DirMergeOptions::default().use_whitespace();
        let directive = MergeDirective::new(OsString::from("rules"), None);
        let merged = merge_directive_options(&base, &directive);
        assert!(merged.uses_whitespace());
    }

    #[test]
    fn merge_directive_options_allows_comments() {
        let base = DirMergeOptions::default().allow_comments(false);
        let directive = MergeDirective::new(OsString::from("rules"), None);
        let merged = merge_directive_options(&base, &directive);
        assert!(!merged.allows_comments());
    }

    #[test]
    fn merge_directive_options_enforced_kind_from_base() {
        let base =
            DirMergeOptions::default().with_enforced_kind(Some(DirMergeEnforcedKind::Include));
        let directive = MergeDirective::new(OsString::from("rules"), None);
        let merged = merge_directive_options(&base, &directive);
        assert_eq!(merged.enforced_kind(), Some(DirMergeEnforcedKind::Include));
    }

    #[test]
    fn merge_directive_options_enforced_kind_override() {
        let base =
            DirMergeOptions::default().with_enforced_kind(Some(DirMergeEnforcedKind::Include));
        let directive = MergeDirective::new(OsString::from("rules"), Some(FilterRuleKind::Exclude));
        let merged = merge_directive_options(&base, &directive);
        // Directive's enforced_kind should override base
        assert_eq!(merged.enforced_kind(), Some(DirMergeEnforcedKind::Exclude));
    }

    #[test]
    fn merge_directive_options_anchor_root() {
        let base = DirMergeOptions::default().anchor_root(true);
        let directive = MergeDirective::new(OsString::from("rules"), None);
        let merged = merge_directive_options(&base, &directive);
        assert!(merged.anchor_root_enabled());
    }

    #[test]
    fn os_string_to_pattern_valid_utf8() {
        let value = OsString::from("hello.txt");
        assert_eq!(os_string_to_pattern(value), "hello.txt");
    }

    #[test]
    fn os_string_to_pattern_empty() {
        let value = OsString::from("");
        assert_eq!(os_string_to_pattern(value), "");
    }

    #[test]
    fn os_string_to_pattern_unicode() {
        let value = OsString::from("日本語.txt");
        assert_eq!(os_string_to_pattern(value), "日本語.txt");
    }

    #[test]
    fn os_string_to_pattern_special_chars() {
        let value = OsString::from("file with spaces.txt");
        assert_eq!(os_string_to_pattern(value), "file with spaces.txt");
    }

    #[test]
    fn merge_directive_source_returns_os_str() {
        let directive = MergeDirective::new(OsString::from("/path/to/filter"), None);
        assert_eq!(directive.source(), OsStr::new("/path/to/filter"));
    }

    #[test]
    fn merge_directive_clone() {
        let directive = MergeDirective::new(OsString::from("filter.txt"), None);
        let cloned = directive.clone();
        assert_eq!(directive, cloned);
    }

    #[test]
    fn filter_directive_clone() {
        let directive = FilterDirective::Clear;
        let cloned = directive.clone();
        assert_eq!(directive, cloned);
    }

    #[test]
    fn filter_directive_debug() {
        let directive = FilterDirective::Clear;
        let debug_str = format!("{directive:?}");
        assert!(debug_str.contains("Clear"));
    }
}
