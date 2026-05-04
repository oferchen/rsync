//! Compiled filter rule representation and matching.
//!
//! Splits [`CompiledRule`] construction, pattern matching, and clear-rule
//! processing into focused submodules following single-responsibility:
//!
//! - `pattern` - pattern normalisation and glob compilation
//! - `rule` - the `CompiledRule` struct with matching and side-clearing logic
//! - `clear` - bulk clear-rule application over rule vectors

mod clear;
mod pattern;
mod rule;

use std::collections::HashSet;

use crate::{FilterAction, FilterError, FilterRule};

pub(crate) use clear::apply_clear_rule;
use pattern::{compile_patterns, normalise_pattern};
pub(crate) use rule::CompiledRule;

impl CompiledRule {
    /// Compiles a [`FilterRule`] into optimised glob matchers.
    ///
    /// The pattern is normalised (anchored/directory flags extracted), then
    /// expanded into direct matchers (for the pattern itself) and descendant
    /// matchers (for `pattern/**` to cover directory contents). Unanchored
    /// patterns additionally get `**/pattern` variants for matching at any
    /// depth.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] if the pattern cannot be compiled into a valid
    /// glob matcher.
    pub(crate) fn new(rule: FilterRule) -> Result<Self, FilterError> {
        let FilterRule {
            action,
            pattern,
            applies_to_sender,
            applies_to_receiver,
            perishable,
            xattr_only,
            negate,
            exclude_only: _,
            no_inherit: _,
        } = rule;
        debug_assert!(
            !xattr_only,
            "xattr-only rules should be filtered before compilation"
        );
        let (anchored, directory_only, core_pattern) = normalise_pattern(&pattern);
        let mut direct_patterns = HashSet::new();
        direct_patterns.insert(core_pattern.to_string());
        if !anchored {
            direct_patterns.insert(format!("**/{core_pattern}"));
        }

        let mut descendant_patterns = HashSet::new();
        // upstream: exclude.c - excluding a directory excludes its contents,
        // but including a directory does NOT include its contents (they must
        // match their own rules). Only Exclude/Protect/Risk get descendants.
        if matches!(
            action,
            FilterAction::Exclude | FilterAction::Protect | FilterAction::Risk
        ) {
            descendant_patterns.insert(format!("{core_pattern}/**"));
            if !anchored {
                descendant_patterns.insert(format!("**/{core_pattern}/**"));
            }
        }

        let direct_matchers = compile_patterns(direct_patterns, &pattern)?;
        let descendant_matchers = compile_patterns(descendant_patterns, &pattern)?;

        Ok(Self {
            action,
            directory_only,
            direct_matchers,
            descendant_matchers,
            applies_to_sender,
            applies_to_receiver,
            perishable,
            negate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_rule_new_simple_exclude() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.bak".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Exclude);
        assert!(compiled.applies_to_sender);
        assert!(compiled.applies_to_receiver);
        assert!(!compiled.perishable);
    }

    #[test]
    fn compiled_rule_new_include() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "*.rs".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Include);
    }

    #[test]
    fn compiled_rule_perishable() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.log".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: true,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.perishable);
    }

    /// Verifies that `--include '*/'` does NOT generate descendant matchers.
    ///
    /// upstream: Including a directory means "include the directory entry" -
    /// it does NOT mean "include everything inside it". Files inside must
    /// match their own rules. Only Exclude/Protect/Risk get descendants.
    #[test]
    fn include_directory_only_has_no_descendant_matchers() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "*/".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.directory_only);
        // Include dirs should NOT match descendants - files inside must
        // match their own include/exclude rules independently.
        assert!(
            compiled.descendant_matchers.is_empty(),
            "include directory-only rules must not have descendant matchers"
        );
    }

    /// Verifies that `--exclude '*/'` DOES generate descendant matchers.
    ///
    /// upstream: Excluding a directory excludes all of its contents.
    #[test]
    fn exclude_directory_only_has_descendant_matchers() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*/".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.directory_only);
        assert!(
            !compiled.descendant_matchers.is_empty(),
            "exclude directory-only rules must have descendant matchers"
        );
    }

    #[test]
    fn compiled_rule_negate_flag_preserved() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: true,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.negate);

        let rule2 = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.tmp".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled2 = CompiledRule::new(rule2).unwrap();
        assert!(!compiled2.negate);
    }
}
