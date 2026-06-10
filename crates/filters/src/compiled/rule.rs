use std::path::Path;

use globset::GlobMatcher;
use logging::debug_log;

use crate::FilterAction;

/// A compiled filter rule with pre-built glob matchers for efficient matching.
///
/// This struct holds the compiled representation of a [`crate::FilterRule`], with
/// glob patterns pre-compiled into matchers for fast path evaluation.
///
/// # Negation
///
/// When `negate` is true, the match result is inverted. This mirrors upstream
/// rsync's `!` modifier behavior from `exclude.c` line 906:
/// ```c
/// int ret_match = ex->rflags & FILTRULE_NEGATE ? 0 : 1;
/// ```
#[derive(Debug)]
pub(crate) struct CompiledRule {
    pub(crate) action: FilterAction,
    pub(super) directory_only: bool,
    pub(super) direct_matchers: Vec<GlobMatcher>,
    pub(super) descendant_matchers: Vec<GlobMatcher>,
    pub(crate) applies_to_sender: bool,
    pub(crate) applies_to_receiver: bool,
    pub(crate) perishable: bool,
    pub(crate) negate: bool,
}

impl CompiledRule {
    /// Tests whether a path matches this rule's pattern.
    ///
    /// When `negate` is true, the match result is inverted: returns true when
    /// the pattern does NOT match. This mirrors upstream rsync's `!` modifier
    /// behavior from `exclude.c` line 906.
    pub(crate) fn matches(&self, path: &Path, is_dir: bool) -> bool {
        let pattern_matched = self.pattern_matches_impl(path, is_dir);

        // upstream: exclude.c:906 - ret_match = ex->rflags & FILTRULE_NEGATE ? 0 : 1
        if self.negate {
            debug_log!(
                Filter,
                2,
                "negated rule: pattern_matched={}, returning {}",
                pattern_matched,
                !pattern_matched
            );
            !pattern_matched
        } else {
            pattern_matched
        }
    }

    /// Internal pattern matching without negate logic.
    fn pattern_matches_impl(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) && (!self.directory_only || is_dir) {
                debug_log!(Filter, 2, "direct pattern matched: {:?}", path);
                return true;
            }
        }

        if !self.descendant_matchers.is_empty() {
            for matcher in &self.descendant_matchers {
                if matcher.is_match(path) {
                    debug_log!(Filter, 2, "descendant pattern matched: {:?}", path);
                    return true;
                }
            }
        }

        debug_log!(Filter, 3, "no pattern match for: {:?}", path);
        false
    }

    /// Returns `true` if this rule was compiled from a directory-only pattern
    /// (one with a trailing `/`).
    pub(crate) const fn is_directory_only(&self) -> bool {
        self.directory_only
    }

    /// Clears applicability flags for this rule based on context.
    ///
    /// When a `!` (clear) rule is processed, it removes matching rules from
    /// either the sender side, receiver side, or both. This method handles
    /// the flag clearing and returns whether the rule should be retained.
    ///
    /// # Arguments
    ///
    /// * `sender` - If true, clear the sender applicability flag
    /// * `receiver` - If true, clear the receiver applicability flag
    ///
    /// # Returns
    ///
    /// `true` if the rule still applies to at least one side (should be kept),
    /// `false` if the rule no longer applies to any side (should be removed).
    pub(crate) const fn clear_sides(&mut self, sender: bool, receiver: bool) -> bool {
        if sender {
            self.applies_to_sender = false;
        }
        if receiver {
            self.applies_to_receiver = false;
        }
        self.applies_to_sender || self.applies_to_receiver
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::compiled::CompiledRule;
    use crate::{FilterAction, FilterRule};

    #[test]
    fn compiled_rule_matches_simple() {
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
        assert!(compiled.matches(Path::new("file.bak"), false));
        assert!(compiled.matches(Path::new("dir/file.bak"), false));
        assert!(!compiled.matches(Path::new("file.txt"), false));
    }

    #[test]
    fn compiled_rule_matches_anchored() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "/build".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build"), false));
        assert!(!compiled.matches(Path::new("src/build"), false));
    }

    #[test]
    fn compiled_rule_matches_directory_only() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "node_modules/".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("node_modules"), true));
        assert!(!compiled.matches(Path::new("node_modules"), false));
    }

    #[test]
    fn compiled_rule_matches_descendant() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "build/".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build"), true));
        assert!(compiled.matches(Path::new("build/output.o"), false));
        assert!(compiled.matches(Path::new("build/subdir/file.txt"), false));
    }

    #[test]
    fn compiled_rule_protect_action() {
        let rule = FilterRule {
            action: FilterAction::Protect,
            pattern: "important.dat".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Protect);
        assert!(compiled.matches(Path::new("important.dat"), false));
    }

    #[test]
    fn compiled_rule_risk_action() {
        let rule = FilterRule {
            action: FilterAction::Risk,
            pattern: "temp.dat".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Risk);
    }

    #[test]
    fn compiled_rule_include_matches() {
        let rule = FilterRule {
            action: FilterAction::Include,
            pattern: "*.txt".to_owned(),
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
        assert!(compiled.matches(Path::new("readme.txt"), false));
    }

    #[test]
    fn compiled_rule_complex_glob() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "**/*.o".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build/main.o"), false));
        assert!(compiled.matches(Path::new("src/lib/util.o"), false));
    }

    /// Verifies `--exclude='/*'` matches root-level items but NOT nested paths.
    ///
    /// upstream: exclude.c:rule_matches - an anchored wildcard `/*` matches
    /// single-component root-level names only. `down/file.txt` must NOT match
    /// because the pattern is anchored and `*` does not cross `/` boundaries.
    /// Regression test for #5421.
    #[test]
    fn anchored_wildcard_exclude_does_not_match_nested_paths() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "/*".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        // Root-level items match the anchored `*` pattern.
        assert!(compiled.matches(Path::new("file.txt"), false));
        assert!(compiled.matches(Path::new("down"), true));

        // Nested paths must NOT match - this was the bug in #5421 where
        // descendant matchers (`*/**`) incorrectly matched any nested path.
        assert!(!compiled.matches(Path::new("down/file.txt"), false));
        assert!(!compiled.matches(Path::new("down/sub/deep.txt"), false));
    }

    /// Verifies `--exclude=/build` matches the directory and its descendants.
    ///
    /// Anchored literal excludes generate descendant matchers (`build/**`) so
    /// that paths like `build/output.o` are excluded when checked individually
    /// (e.g., by the receiver which does not perform traversal-skip).
    #[test]
    fn anchored_literal_exclude_matches_descendants() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "/build".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        assert!(compiled.matches(Path::new("build"), false));
        assert!(compiled.matches(Path::new("build/output.o"), false));
        assert!(!compiled.matches(Path::new("src/build"), false));
    }

    #[test]
    fn compiled_rule_negate_inverts_match() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.txt".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("file.txt"), false));
        assert!(!compiled.matches(Path::new("file.log"), false));

        let rule_negated = FilterRule {
            action: FilterAction::Exclude,
            pattern: "*.txt".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: true,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled_negated = CompiledRule::new(rule_negated).unwrap();
        assert!(!compiled_negated.matches(Path::new("file.txt"), false));
        assert!(compiled_negated.matches(Path::new("file.log"), false));
    }

    #[test]
    fn compiled_rule_negate_with_directory_only() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "cache/".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: true,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        assert!(!compiled.matches(Path::new("cache"), true));
        assert!(compiled.matches(Path::new("build"), true));
        // directory_only means a file named "cache" never matches the pattern,
        // so the negated rule returns true for it.
        assert!(compiled.matches(Path::new("cache"), false));
    }

    #[test]
    fn compiled_rule_negate_with_anchored() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "/important".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: true,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        assert!(!compiled.matches(Path::new("important"), false));
        assert!(compiled.matches(Path::new("other"), false));
        // Anchored pattern does not match a nested path, so negation gives true.
        assert!(compiled.matches(Path::new("dir/important"), false));
    }

    /// Verifies `--include '*/'` matches directories but NOT files inside them.
    ///
    /// upstream: `--include '*/' --exclude '*'` should only include directory
    /// entries, not their file contents. Files must match their own rules.
    #[test]
    fn include_dir_wildcard_does_not_match_file_descendants() {
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

        assert!(compiled.matches(Path::new("subdir"), true));
        assert!(compiled.matches(Path::new("deep/nested"), true));

        // Files inside matched directories still need their own rules to match;
        // an include of `*/` alone does not pull file descendants in.
        assert!(!compiled.matches(Path::new("file.txt"), false));
        assert!(!compiled.matches(Path::new("subdir/debug.log"), false));
        assert!(!compiled.matches(Path::new("subdir/report.csv"), false));
    }

    /// Verifies that an `exclude = ?` daemon-config rule does not block
    /// deletion of multi-character filenames. The upstream daemon-delete-stats
    /// test ships with a global `exclude = ? foobar.baz` directive; the single-
    /// character glob `?` must only match a single-character filename, never
    /// `delete.txt` or any longer name.
    ///
    /// Regression test for the upstream-testsuite `daemon-delete-stats` failure.
    #[test]
    fn single_char_wildcard_does_not_match_multichar_names() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "?".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        // The bare ? glob matches single-character names only.
        assert!(compiled.matches(Path::new("a"), false));
        // Multi-character names must not match either at the root or at depth.
        assert!(!compiled.matches(Path::new("delete.txt"), false));
        assert!(!compiled.matches(Path::new("keep.txt"), false));
        assert!(!compiled.matches(Path::new("subdir/delete.txt"), false));
    }
}
