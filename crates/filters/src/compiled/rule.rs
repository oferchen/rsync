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
    /// Descendant matchers (`{core}/**`) that must fire ONLY on the deletion
    /// (receiver) path. Populated for directory-only unanchored wildcard
    /// excludes such as `foo/*/`. Upstream's sender walk prunes the excluded
    /// directory and never emits a `foo/*/**` transfer rule (exclude.c:938-939
    /// FILTRULE_DIRECTORY returns "no match" for a non-dir candidate), so these
    /// must stay invisible to the Transfer path to preserve the `foo/*/`
    /// per-directory wildcard semantics that #6015 fixed. The receiver's
    /// per-candidate deletion scan has no traversal-pruning side effect, so it
    /// needs these descendants live to protect children of an excluded
    /// directory from over-deletion.
    pub(super) deletion_descendant_matchers: Vec<GlobMatcher>,
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
    /// Tests whether a path matches this rule's pattern.
    ///
    /// When `check_descendants` is false, only direct matchers are evaluated -
    /// descendant matchers are skipped. This matches upstream rsync's
    /// `rule_matches()` in exclude.c which has NO descendant matching at all;
    /// descendant exclusion is a side-effect of the sender walk not descending
    /// into excluded directories. The receiver deletion path needs descendants
    /// because it evaluates paths individually without traversal context.
    pub(crate) fn matches(&self, path: &Path, is_dir: bool, check_descendants: bool) -> bool {
        let pattern_matched = self.pattern_matches_impl(path, is_dir, check_descendants);

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

    /// Like [`Self::matches`] but for the receiver's deletion scan.
    ///
    /// Adds the `deletion_descendant_matchers` set on top of the regular
    /// matchers. Those descendants encode the
    /// upstream invariant that an excluded directory protects its descendants
    /// from deletion (the generator never descends into it). They are
    /// consulted independently of `check_descendants` because the deletion
    /// scan evaluates each candidate in isolation - it has no traversal-pruning
    /// side effect to suppress, and the local-copy delete pass calls with
    /// `check_descendants = false`. Keeping them out of [`Self::matches`]
    /// preserves the `foo/*/` per-directory wildcard transfer semantics fixed
    /// in #6015.
    ///
    /// upstream: exclude.c:rule_matches() / name_is_excluded() subtree pruning
    pub(crate) fn matches_for_deletion(
        &self,
        path: &Path,
        is_dir: bool,
        check_descendants: bool,
    ) -> bool {
        let pattern_matched = self.pattern_matches_impl(path, is_dir, check_descendants)
            || self.deletion_descendant_matches(path);

        if self.negate {
            !pattern_matched
        } else {
            pattern_matched
        }
    }

    /// Returns `true` when a deletion-only descendant matcher fires for `path`.
    fn deletion_descendant_matches(&self, path: &Path) -> bool {
        self.deletion_descendant_matchers
            .iter()
            .any(|matcher| matcher.is_match(path))
    }

    /// Internal pattern matching without negate logic.
    fn pattern_matches_impl(&self, path: &Path, is_dir: bool, check_descendants: bool) -> bool {
        for matcher in &self.direct_matchers {
            if matcher.is_match(path) && (!self.directory_only || is_dir) {
                debug_log!(Filter, 2, "direct pattern matched: {:?}", path);
                return true;
            }
        }

        // upstream: exclude.c:rule_matches() has no descendant matching.
        // Sender-side (Transfer context) skips descendants because the walk
        // implicitly handles them by not descending into excluded directories.
        // Receiver-side (Deletion context) needs descendants because it
        // evaluates paths individually without traversal.
        if check_descendants && !self.descendant_matchers.is_empty() {
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("file.bak"), false, true));
        assert!(compiled.matches(Path::new("dir/file.bak"), false, true));
        assert!(!compiled.matches(Path::new("file.txt"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build"), false, true));
        assert!(!compiled.matches(Path::new("src/build"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("node_modules"), true, true));
        assert!(!compiled.matches(Path::new("node_modules"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build"), true, true));
        assert!(compiled.matches(Path::new("build/output.o"), false, true));
        assert!(compiled.matches(Path::new("build/subdir/file.txt"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Protect);
        assert!(compiled.matches(Path::new("important.dat"), false, true));
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
            cvs_mode: false,
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert_eq!(compiled.action, FilterAction::Include);
        assert!(compiled.matches(Path::new("readme.txt"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("build/main.o"), false, true));
        assert!(compiled.matches(Path::new("src/lib/util.o"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        // Root-level items match the anchored `*` pattern.
        assert!(compiled.matches(Path::new("file.txt"), false, true));
        assert!(compiled.matches(Path::new("down"), true, true));

        // Nested paths must NOT match - this was the bug in #5421 where
        // descendant matchers (`*/**`) incorrectly matched any nested path.
        assert!(!compiled.matches(Path::new("down/file.txt"), false, true));
        assert!(!compiled.matches(Path::new("down/sub/deep.txt"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        assert!(compiled.matches(Path::new("build"), false, true));
        assert!(compiled.matches(Path::new("build/output.o"), false, true));
        assert!(!compiled.matches(Path::new("src/build"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();
        assert!(compiled.matches(Path::new("file.txt"), false, true));
        assert!(!compiled.matches(Path::new("file.log"), false, true));

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
            cvs_mode: false,
        };
        let compiled_negated = CompiledRule::new(rule_negated).unwrap();
        assert!(!compiled_negated.matches(Path::new("file.txt"), false, true));
        assert!(compiled_negated.matches(Path::new("file.log"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        assert!(!compiled.matches(Path::new("cache"), true, true));
        assert!(compiled.matches(Path::new("build"), true, true));
        // directory_only means a file named "cache" never matches the pattern,
        // so the negated rule returns true for it.
        assert!(compiled.matches(Path::new("cache"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        assert!(!compiled.matches(Path::new("important"), false, true));
        assert!(compiled.matches(Path::new("other"), false, true));
        // Anchored pattern does not match a nested path, so negation gives true.
        assert!(compiled.matches(Path::new("dir/important"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        assert!(compiled.matches(Path::new("subdir"), true, true));
        assert!(compiled.matches(Path::new("deep/nested"), true, true));

        // Files inside matched directories still need their own rules to match;
        // an include of `*/` alone does not pull file descendants in.
        assert!(!compiled.matches(Path::new("file.txt"), false, true));
        assert!(!compiled.matches(Path::new("subdir/debug.log"), false, true));
        assert!(!compiled.matches(Path::new("subdir/report.csv"), false, true));
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
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        // The bare ? glob matches single-character names only.
        assert!(compiled.matches(Path::new("a"), false, true));
        // Multi-character names must not match either at the root or at depth.
        assert!(!compiled.matches(Path::new("delete.txt"), false, true));
        assert!(!compiled.matches(Path::new("keep.txt"), false, true));
        assert!(!compiled.matches(Path::new("subdir/delete.txt"), false, true));
    }

    /// upstream: exclude.c:936-937 - `FILTRULE_WILD3_SUFFIX` causes `dir/***`
    /// to match both the directory itself (when `is_dir=true`) and everything
    /// inside it. `dir/**` (double star) only matches contents, not the
    /// directory entry itself.
    #[test]
    fn wild3_suffix_matches_dir_and_contents() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "new/lose/***".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        // The directory itself is excluded (directory-only match on stem).
        assert!(compiled.matches(Path::new("new/lose"), true, true));
        // Contents are excluded via descendant matchers.
        assert!(compiled.matches(Path::new("new/lose/this"), false, true));
        assert!(compiled.matches(Path::new("new/lose/this"), true, true));
        // A file named "new/lose" is NOT excluded (directory-only).
        assert!(!compiled.matches(Path::new("new/lose"), false, true));
        // Sibling entries are not affected.
        assert!(!compiled.matches(Path::new("new/keep"), true, true));
    }

    /// Anchored literal exclude `/bar` generates descendant pattern `bar/**`.
    /// With `check_descendants = false` (Transfer/sender context), children
    /// like `bar/.filt` must NOT match - matching upstream rsync's
    /// `rule_matches()` in exclude.c which has no descendant matching.
    /// With `check_descendants = true` (Deletion/receiver context), children
    /// do match via the descendant pattern.
    #[test]
    fn check_descendants_false_skips_descendant_matchers() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "/bar".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        // The directory itself matches via direct matcher in both contexts.
        assert!(compiled.matches(Path::new("bar"), true, true));
        assert!(compiled.matches(Path::new("bar"), true, false));

        // Descendant `bar/.filt` matches only when check_descendants = true
        // (Deletion/receiver context).
        assert!(compiled.matches(Path::new("bar/.filt"), false, true));

        // With check_descendants = false (Transfer/sender context), the
        // descendant pattern `bar/**` is skipped - upstream parity.
        assert!(!compiled.matches(Path::new("bar/.filt"), false, false));
        assert!(!compiled.matches(Path::new("bar/subdir/file"), false, false));
    }

    /// `dir/**` (double star without trailing `*`) should exclude contents
    /// but NOT the directory itself - contrast with `dir/***`.
    #[test]
    fn double_star_suffix_does_not_match_dir_itself() {
        let rule = FilterRule {
            action: FilterAction::Exclude,
            pattern: "new/keep/**".to_owned(),
            applies_to_sender: true,
            applies_to_receiver: true,
            perishable: false,
            xattr_only: false,
            negate: false,
            exclude_only: false,
            no_inherit: false,
            cvs_mode: false,
        };
        let compiled = CompiledRule::new(rule).unwrap();

        // The directory entry itself is NOT excluded.
        assert!(!compiled.matches(Path::new("new/keep"), true, true));
        // Contents are excluded.
        assert!(compiled.matches(Path::new("new/keep/this"), false, true));
        assert!(compiled.matches(Path::new("new/keep/this"), true, true));
    }
}
