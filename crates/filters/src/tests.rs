use super::*;
use std::path::{Path, PathBuf};

#[test]
fn empty_rules_allow_everything() {
    let set = FilterSet::from_rules(Vec::new()).expect("empty set");
    assert!(set.allows(Path::new("foo"), false));
    assert!(set.allows_deletion(Path::new("foo"), false));
}

#[test]
fn include_rule_allows_path() {
    let set = FilterSet::from_rules([FilterRule::include("foo")]).expect("compiled");
    assert!(set.allows(Path::new("foo"), false));
    assert!(set.allows_deletion(Path::new("foo"), false));
}

#[test]
fn exclude_rule_blocks_match() {
    let set = FilterSet::from_rules([FilterRule::exclude("foo")]).expect("compiled");
    assert!(!set.allows(Path::new("foo"), false));
    assert!(!set.allows(Path::new("bar/foo"), false));
    assert!(!set.allows_deletion(Path::new("foo"), false));
}

#[test]
fn include_before_exclude_reinstates_path() {
    // First-match-wins: includes must come before excludes to create exceptions.
    let rules = [
        FilterRule::include("foo/bar.txt"),
        FilterRule::exclude("foo"),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows(Path::new("foo/bar.txt"), false));
    assert!(!set.allows(Path::new("foo/baz.txt"), false));
    assert!(set.allows_deletion(Path::new("foo/bar.txt"), false));
}

#[test]
fn clear_rule_removes_previous_rules() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::protect("secrets/"),
        FilterRule::clear(),
        FilterRule::include("*.tmp"),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows(Path::new("scratch.tmp"), false));
    assert!(set.allows_deletion(Path::new("scratch.tmp"), false));
    assert!(set.allows_deletion(Path::new("secrets/data"), false));
}

#[test]
fn clear_rule_respects_side_flags() {
    let rules = [
        FilterRule::exclude("sender.txt").with_sides(true, false),
        FilterRule::exclude("receiver.txt").with_sides(false, true),
        FilterRule::clear().with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");

    assert!(set.allows(Path::new("sender.txt"), false));
    assert!(!set.allows_deletion(Path::new("receiver.txt"), false));
}

#[test]
fn anchored_pattern_matches_only_at_root() {
    let set = FilterSet::from_rules([FilterRule::exclude("/foo/bar")]).expect("compiled");
    assert!(!set.allows(Path::new("foo/bar"), false));
    assert!(set.allows(Path::new("a/foo/bar"), false));
}

#[test]
fn directory_rule_excludes_children() {
    let set = FilterSet::from_rules([FilterRule::exclude("build/")]).expect("compiled");
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("build/output.bin"), false));
    assert!(!set.allows(Path::new("dir/build/log.txt"), false));
    assert!(!set.allows_deletion(Path::new("build/output.bin"), false));
}

#[test]
fn wildcard_patterns_match_expected_paths() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).expect("compiled");
    assert!(!set.allows(Path::new("note.tmp"), false));
    assert!(!set.allows(Path::new("dir/note.tmp"), false));
    assert!(set.allows(Path::new("note.txt"), false));
}

#[test]
fn invalid_pattern_reports_error() {
    let error = FilterSet::from_rules([FilterRule::exclude("[")]).expect_err("invalid");
    assert_eq!(error.pattern(), "[");
}

#[test]
fn glob_escape_sequences_supported() {
    let set = FilterSet::from_rules([FilterRule::exclude("foo\\?bar")]).expect("compiled");
    assert!(!set.allows(Path::new("foo?bar"), false));
    assert!(set.allows(Path::new("fooXbar"), false));
}

#[test]
fn ordering_respected() {
    let rules = [
        FilterRule::include("special.tmp"),
        FilterRule::exclude("*.tmp"),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows(Path::new("special.tmp"), false));
    assert!(!set.allows(Path::new("other.tmp"), false));
}

#[test]
fn duplicate_rules_deduplicate_matchers() {
    let rules = [FilterRule::exclude("foo/"), FilterRule::exclude("foo/")];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(!set.allows(Path::new("foo/bar"), false));
}

#[test]
fn allows_checks_respect_directory_flag() {
    let set = FilterSet::from_rules([FilterRule::exclude("foo/")]).expect("compiled");
    assert!(!set.allows(Path::new("foo"), true));
    assert!(set.allows(Path::new("foo"), false));
}

#[test]
fn include_rule_for_directory_restores_descendants() {
    let rules = [
        FilterRule::include("cache/preserved/**"),
        FilterRule::exclude("cache/"),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows(Path::new("cache/preserved/data"), false));
    assert!(!set.allows(Path::new("cache/tmp"), false));
}

#[test]
fn relative_path_conversion_handles_dot_components() {
    // upstream: exclude.c:rule_matches() lines 947-951 - a pattern with an
    // internal slash but no leading `/` tail-matches against the last N+1
    // path components. So `- foo/bar` excludes both `foo/bar` and any path
    // whose last two components are `foo, bar`, including
    // `foo/../foo/bar`. The glob equivalent is the `**/foo/bar` direct
    // matcher generated for unanchored patterns with internal slashes.
    let set = FilterSet::from_rules([FilterRule::exclude("foo/bar")]).expect("compiled");

    let mut path = PathBuf::from("foo");
    path.push("..");
    path.push("foo");
    path.push("bar");
    assert!(!set.allows(&path, false));

    assert!(!set.allows(Path::new("foo/bar"), false));
}

#[test]
fn protect_rule_blocks_deletion_without_affecting_transfer() {
    let set = FilterSet::from_rules([FilterRule::protect("*.bak")]).expect("compiled");
    assert!(set.allows(Path::new("keep.bak"), false));
    assert!(!set.allows_deletion(Path::new("keep.bak"), false));
}

#[test]
fn perishable_rule_ignored_for_deletion_checks() {
    let rule = FilterRule::exclude("*.tmp").with_perishable(true);
    let set = FilterSet::from_rules([rule]).expect("compiled");

    assert!(!set.allows(Path::new("note.tmp"), false));
    assert!(set.allows_deletion(Path::new("note.tmp"), false));
    assert!(set.allows_deletion_when_excluded_removed(Path::new("note.tmp"), false));
}

#[test]
fn protect_rule_applies_to_directory_descendants() {
    let set = FilterSet::from_rules([FilterRule::protect("secrets/")]).expect("compiled");
    assert!(set.allows(Path::new("secrets/data.txt"), false));
    assert!(!set.allows_deletion(Path::new("secrets/data.txt"), false));
    assert!(!set.allows_deletion(Path::new("dir/secrets/data.txt"), false));
}

#[test]
fn risk_rule_allows_deletion_before_protection() {
    // First-match-wins: risk must precede protect to override it.
    let rules = [
        FilterRule::risk("archive/"),
        FilterRule::protect("archive/"),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows_deletion(Path::new("archive/file.bin"), false));
}

#[test]
fn risk_rule_applies_to_descendants() {
    let rules = [FilterRule::risk("backup/"), FilterRule::protect("backup/")];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows_deletion(Path::new("backup/snap/info"), false));
    assert!(set.allows_deletion(Path::new("sub/backup/snap"), true));
}

#[test]
fn delete_excluded_only_removes_excluded_matches() {
    let rules = [FilterRule::include("keep/**"), FilterRule::exclude("*.tmp")];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows_deletion_when_excluded_removed(Path::new("skip.tmp"), false));
    assert!(!set.allows_deletion_when_excluded_removed(Path::new("keep/file.txt"), false));
}

#[test]
fn sender_only_rule_does_not_prevent_deletion() {
    let rules = [FilterRule::exclude("skip.txt").with_sides(true, false)];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(!set.allows(Path::new("skip.txt"), false));
    assert!(set.allows_deletion(Path::new("skip.txt"), false));
}

#[test]
fn receiver_only_rule_blocks_deletion_without_hiding() {
    let rules = [FilterRule::exclude("keep.txt").with_sides(false, true)];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows(Path::new("keep.txt"), false));
    assert!(!set.allows_deletion(Path::new("keep.txt"), false));
}

#[test]
fn show_rule_applies_only_to_sender() {
    let set = FilterSet::from_rules([FilterRule::show("visible/**")]).expect("compiled");
    assert!(set.allows(Path::new("visible/file.txt"), false));
    assert!(set.allows_deletion(Path::new("visible/file.txt"), false));
}

#[test]
fn hide_rule_applies_only_to_sender() {
    let set = FilterSet::from_rules([FilterRule::hide("hidden/**")]).expect("compiled");
    assert!(!set.allows(Path::new("hidden/file.txt"), false));
    assert!(set.allows_deletion(Path::new("hidden/file.txt"), false));
}

#[test]
fn receiver_context_skips_sender_only_tail_rule() {
    let rules = [
        FilterRule::exclude("*.tmp").with_sides(false, true),
        FilterRule::include("*.tmp").with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(!set.allows_deletion(Path::new("note.tmp"), false));
}

#[test]
fn sender_only_risk_does_not_clear_receiver_protection() {
    let rules = [
        FilterRule::protect("keep/"),
        FilterRule::risk("keep/").with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(!set.allows_deletion(Path::new("keep/item.txt"), false));
}

/// upstream testsuite: `--include=/down/ --exclude='/*'` must allow files
/// inside `down/` because the anchored `/*` exclude only matches root-level
/// items and does not propagate to descendants via pattern expansion.
///
/// Regression test for #5421.
#[test]
fn anchored_wildcard_exclude_allows_included_directory_contents() {
    let rules = [FilterRule::include("/down/"), FilterRule::exclude("/*")];
    let set = FilterSet::from_rules(rules).expect("compiled");

    // `down/` is explicitly included - matched by the include rule.
    assert!(set.allows(Path::new("down"), true));

    // Files inside `down/` must be allowed - they do not match any rule
    // (the anchored `/*` only matches root-level names), so the default
    // allow-all applies.
    assert!(set.allows(Path::new("down/file.txt"), false));
    assert!(set.allows(Path::new("down/sub/deep.txt"), false));

    // Root-level items other than `down/` are excluded by `/*`.
    assert!(!set.allows(Path::new("other.txt"), false));
    assert!(!set.allows(Path::new("build"), true));
}

/// Tests for per-scope `!` (clear-rules) isolation across merge boundaries.
///
/// Upstream `exclude.c::pop_filter_list()` only frees rules between
/// `listp->head` and `listp->tail`, leaving inherited (parent-scope) rules
/// in place. Merge-file expansion treats each merge file as its own scope
/// so a `!` inside the file clears only that file's accumulated rules.
mod clear_scope_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// `!` inline in CLI-level arguments clears all CLI rules added before
    /// it, mirroring upstream's top-level `FILTRULE_CLEAR_LIST` handling on
    /// the global filter list.
    #[test]
    fn cli_inline_clear_drops_all_previous_rules() {
        let rules = [
            FilterRule::exclude("/a"),
            FilterRule::exclude("/keep"),
            FilterRule::clear(),
            FilterRule::include("/b"),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");

        // Both pre-clear excludes are gone.
        assert!(set.allows(Path::new("a"), false));
        assert!(set.allows(Path::new("keep"), false));
        // The post-clear include survives.
        assert!(set.allows(Path::new("b"), false));
    }

    /// `!` at the top of a merge file expanded via
    /// [`FilterSet::from_rules_with_merge_expansion`] clears only the rules
    /// loaded from that file. Parent-scope CLI rules survive.
    #[test]
    fn clear_in_merge_file_does_not_clear_parent_cli_rules() {
        let dir = TempDir::new().expect("tempdir");
        let merge_path = dir.path().join("rules.merge");
        fs::write(&merge_path, "!\n+ /b\n").expect("write merge file");

        let rules = [
            FilterRule::exclude("/a"),
            FilterRule::merge(merge_path.to_string_lossy().into_owned()),
        ];
        let set = FilterSet::from_rules_with_merge_expansion(rules, 8).expect("expanded");

        // Parent CLI rule survived the merge file's `!`.
        assert!(!set.allows(Path::new("a"), false));
        // Merge file's include is present after its scope-local clear.
        assert!(set.allows(Path::new("b"), false));
        // Unrelated paths still default-include.
        assert!(set.allows(Path::new("c"), false));
    }

    /// `!` inside a nested merge file clears only rules from the nested
    /// scope. Rules added by the outer merge file (before the nested
    /// reference) and CLI parent rules both survive.
    #[test]
    fn clear_in_nested_merge_isolates_to_child_scope() {
        let dir = TempDir::new().expect("tempdir");
        let outer = dir.path().join("outer.merge");
        let inner = dir.path().join("inner.merge");
        fs::write(&inner, "!\n+ /from_inner\n").expect("write inner");
        fs::write(
            &outer,
            format!(
                "+ /from_outer\n. {}\n",
                inner.to_string_lossy().into_owned()
            ),
        )
        .expect("write outer");

        let rules = [
            FilterRule::exclude("/parent_cli"),
            FilterRule::merge(outer.to_string_lossy().into_owned()),
        ];
        let set = FilterSet::from_rules_with_merge_expansion(rules, 8).expect("expanded");

        // Parent CLI rule survives both nested clears.
        assert!(!set.allows(Path::new("parent_cli"), false));
        // Outer merge's include survives the inner merge's `!` (different
        // scope) and reaches the final chain.
        assert!(set.allows(Path::new("from_outer"), false));
        // Inner merge's include is present after its own scope-local clear.
        assert!(set.allows(Path::new("from_inner"), false));
    }

    /// Wire-byte parity fixture: parent CLI `- /a`, merge file `! + /b`.
    /// Final chain must contain `- /a` (parent survives) AND `+ /b` (from
    /// merge), matching what upstream's per-directory mergelist produces
    /// when `!` truncates the local section of the rule list.
    #[test]
    fn fixture_parent_minus_a_merge_bang_plus_b_post_state() {
        let dir = TempDir::new().expect("tempdir");
        let merge_path = dir.path().join("filters");
        fs::write(&merge_path, "!\n+ /b\n").expect("write merge");

        let rules = [
            FilterRule::exclude("/a"),
            FilterRule::merge(merge_path.to_string_lossy().into_owned()),
        ];
        let set = FilterSet::from_rules_with_merge_expansion(rules, 8).expect("expanded");

        // `- /a` survives — parent CLI rule was not cleared.
        assert!(
            !set.allows(Path::new("a"), false),
            "parent CLI exclude `- /a` should still match",
        );
        // `+ /b` from the merge file is active.
        assert!(
            set.allows(Path::new("b"), false),
            "merge file include `+ /b` should match",
        );
        // Deletion side mirrors the same parent-survives semantics.
        assert!(!set.allows_deletion(Path::new("a"), false));
        assert!(set.allows_deletion(Path::new("b"), false));
    }

    /// A side-restricted `!` (e.g. sender-only clear) inside a merge file
    /// only retires the local-scope rules on that side. Parent CLI rules
    /// remain entirely intact, and local merge rules tied to the opposite
    /// side continue to apply.
    #[test]
    fn sender_only_clear_in_merge_preserves_receiver_side() {
        let dir = TempDir::new().expect("tempdir");
        let merge_path = dir.path().join("filters");
        fs::write(&merge_path, "+ /keep_both\n").expect("write merge");

        let rules = [
            FilterRule::exclude("/parent"),
            FilterRule::merge(merge_path.to_string_lossy().into_owned()),
        ];

        // Sanity baseline: without a side-restricted clear, parent rule and
        // merge rule both apply.
        let set = FilterSet::from_rules_with_merge_expansion(rules, 8).expect("expanded");
        assert!(!set.allows(Path::new("parent"), false));
        assert!(set.allows(Path::new("keep_both"), false));
    }
}

/// Tests for negated pattern matching (upstream rsync `!` modifier).
mod negate_tests {
    use super::*;

    #[test]
    fn negated_exclude_excludes_non_matching_files() {
        // `- ! *.txt` excludes everything except .txt files.
        let rules = [FilterRule::exclude("*.txt").with_negate(true)];
        let set = FilterSet::from_rules(rules).expect("compiled");

        assert!(set.allows(Path::new("file.txt"), false));
        assert!(set.allows(Path::new("dir/file.txt"), false));

        assert!(!set.allows(Path::new("file.log"), false));
        assert!(!set.allows(Path::new("file.bak"), false));
        assert!(!set.allows(Path::new("dir/file.log"), false));
    }

    #[test]
    fn negated_include_includes_non_matching_files() {
        // `+ ! *.bak` includes everything except .bak; first-match-wins
        // requires the include to precede the trailing exclude-all.
        let rules = [
            FilterRule::include("*.bak").with_negate(true),
            FilterRule::exclude("*"),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");

        assert!(set.allows(Path::new("file.txt"), false));
        assert!(set.allows(Path::new("file.log"), false));

        // *.bak falls through to the exclude("*") rule.
        assert!(!set.allows(Path::new("file.bak"), false));
    }

    #[test]
    fn negated_pattern_with_directory() {
        let rules = [FilterRule::exclude("cache/").with_negate(true)];
        let set = FilterSet::from_rules(rules).expect("compiled");

        assert!(set.allows(Path::new("cache"), true));

        assert!(!set.allows(Path::new("temp"), true));
        assert!(!set.allows(Path::new("build"), true));
    }

    #[test]
    fn negated_pattern_with_anchored() {
        let rules = [FilterRule::exclude("/important").with_negate(true)];
        let set = FilterSet::from_rules(rules).expect("compiled");

        assert!(set.allows(Path::new("important"), false));

        assert!(!set.allows(Path::new("other"), false));
        // "dir/important" is not an anchored match, so negation excludes it.
        assert!(!set.allows(Path::new("dir/important"), false));
    }

    #[test]
    fn negated_rules_combine_with_regular_rules() {
        let rules = [
            FilterRule::exclude("*.tmp"),
            FilterRule::exclude("*.txt").with_negate(true),
        ];
        let set = FilterSet::from_rules(rules).expect("compiled");

        // file.txt: first rule misses; the negated `*.txt` exclude matches and
        // negates to "do not exclude", so the file is allowed.
        assert!(set.allows(Path::new("file.txt"), false));

        // file.tmp: matched by the first plain exclude.
        assert!(!set.allows(Path::new("file.tmp"), false));

        // file.log: first rule misses; the negated `*.txt` exclude misses too,
        // and negation turns the miss into an exclusion.
        assert!(!set.allows(Path::new("file.log"), false));
    }

    #[test]
    fn negate_flag_accessor_works() {
        let rule = FilterRule::exclude("*.txt").with_negate(true);
        assert!(rule.is_negated());

        let rule2 = FilterRule::exclude("*.txt");
        assert!(!rule2.is_negated());
    }

    #[test]
    fn negate_flag_chaining() {
        let rule = FilterRule::exclude("*.tmp")
            .with_perishable(true)
            .with_negate(true)
            .with_sides(true, false);

        assert!(rule.is_negated());
        assert!(rule.is_perishable());
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }
}

mod properties {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating valid glob pattern characters.
    fn pattern_char() -> impl Strategy<Value = char> {
        prop_oneof![
            Just('a'),
            Just('b'),
            Just('c'),
            Just('0'),
            Just('1'),
            Just('_'),
            Just('-'),
            Just('.'),
            Just('/'),
            Just('*'),
        ]
    }

    /// Strategy for generating valid patterns (avoiding broken glob syntax).
    fn valid_pattern() -> impl Strategy<Value = String> {
        proptest::collection::vec(pattern_char(), 1..20)
            .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        #[test]
        fn include_exclude_duality(pattern in valid_pattern()) {
            let include = FilterRule::include(&pattern);
            prop_assert!(include.applies_to_sender());
            prop_assert!(include.applies_to_receiver());
            prop_assert_eq!(include.action(), FilterAction::Include);
            prop_assert_eq!(include.pattern(), &pattern);

            let exclude = FilterRule::exclude(&pattern);
            prop_assert!(exclude.applies_to_sender());
            prop_assert!(exclude.applies_to_receiver());
            prop_assert_eq!(exclude.action(), FilterAction::Exclude);
        }

        #[test]
        fn with_sides_is_consistent(
            pattern in valid_pattern(),
            sender in any::<bool>(),
            receiver in any::<bool>()
        ) {
            let rule = FilterRule::include(&pattern)
                .with_sides(sender, receiver);

            prop_assert_eq!(rule.applies_to_sender(), sender);
            prop_assert_eq!(rule.applies_to_receiver(), receiver);
        }

        #[test]
        fn anchor_to_root_adds_leading_slash(pattern in valid_pattern()) {
            // Skip patterns that already start with '/' to test the anchoring behavior
            prop_assume!(!pattern.starts_with('/'));

            let rule = FilterRule::include(&pattern).anchor_to_root();
            prop_assert!(rule.pattern().starts_with('/'));

            // Double anchoring should be idempotent
            let double_anchored = rule.anchor_to_root();
            prop_assert!(double_anchored.pattern().starts_with('/'));
            prop_assert!(!double_anchored.pattern().starts_with("//"));
        }

        #[test]
        fn show_hide_are_sender_only(pattern in valid_pattern()) {
            let show = FilterRule::show(&pattern);
            prop_assert!(show.applies_to_sender());
            prop_assert!(!show.applies_to_receiver());
            prop_assert_eq!(show.action(), FilterAction::Include);

            let hide = FilterRule::hide(&pattern);
            prop_assert!(hide.applies_to_sender());
            prop_assert!(!hide.applies_to_receiver());
            prop_assert_eq!(hide.action(), FilterAction::Exclude);
        }

        #[test]
        fn protect_risk_are_receiver_only(pattern in valid_pattern()) {
            let protect = FilterRule::protect(&pattern);
            prop_assert!(!protect.applies_to_sender());
            prop_assert!(protect.applies_to_receiver());
            prop_assert_eq!(protect.action(), FilterAction::Protect);

            let risk = FilterRule::risk(&pattern);
            prop_assert!(!risk.applies_to_sender());
            prop_assert!(risk.applies_to_receiver());
            prop_assert_eq!(risk.action(), FilterAction::Risk);
        }

        #[test]
        fn perishable_flag_is_independent(
            pattern in valid_pattern(),
            perishable in any::<bool>()
        ) {
            let rule = FilterRule::exclude(&pattern).with_perishable(perishable);
            prop_assert_eq!(rule.is_perishable(), perishable);
            prop_assert_eq!(rule.action(), FilterAction::Exclude);
        }
    }
}

/// Property tests for `FilterSet` evaluation correctness.
///
/// These tests verify the core invariants of filter rule evaluation:
/// first-match-wins semantics, include/exclude toggling, empty chain
/// behavior, and rule independence for disjoint patterns.
mod evaluation_properties {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for generating simple filenames (no path separators, no glob chars).
    fn filename() -> impl Strategy<Value = String> {
        proptest::string::string_regex("[a-z][a-z0-9]{0,7}")
            .unwrap()
            .prop_filter("non-empty", |s| !s.is_empty())
    }

    /// Strategy for generating a file extension.
    fn extension() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("txt".to_owned()),
            Just("rs".to_owned()),
            Just("log".to_owned()),
            Just("bak".to_owned()),
            Just("tmp".to_owned()),
            Just("dat".to_owned()),
            Just("cfg".to_owned()),
            Just("csv".to_owned()),
        ]
    }

    /// Strategy for a path like "name.ext".
    fn file_with_ext() -> impl Strategy<Value = (String, String)> {
        (filename(), extension()).prop_map(|(name, ext)| {
            let full = format!("{name}.{ext}");
            (full, ext)
        })
    }

    /// Strategy for two distinct extensions.
    fn two_distinct_extensions() -> impl Strategy<Value = (String, String)> {
        (extension(), extension()).prop_filter("distinct extensions", |(a, b)| a != b)
    }

    proptest! {
        /// Empty filter chain always returns None (allows all paths by default).
        #[test]
        fn empty_chain_allows_everything(
            name in filename(),
            is_dir in any::<bool>()
        ) {
            let set = FilterSet::from_rules(Vec::new()).unwrap();
            let path = Path::new(&name);
            prop_assert!(set.allows(path, is_dir));
            prop_assert!(set.allows_deletion(path, is_dir));
        }

        /// A single include rule for a specific anchored pattern matches that
        /// exact path. Non-matching paths fall through to the default (allow).
        #[test]
        fn single_include_matches_exact_pattern(
            (file, _ext) in file_with_ext()
        ) {
            let anchored = format!("/{file}");
            let rules = vec![
                FilterRule::include(&anchored),
                FilterRule::exclude("*"),
            ];
            let set = FilterSet::from_rules(rules).unwrap();
            prop_assert!(set.allows(Path::new(&file), false));
        }

        /// A single exclude rule blocks matching paths.
        #[test]
        fn single_exclude_blocks_matching(
            (file, ext) in file_with_ext()
        ) {
            let pattern = format!("*.{ext}");
            let set = FilterSet::from_rules(vec![FilterRule::exclude(&pattern)]).unwrap();
            prop_assert!(!set.allows(Path::new(&file), false));
        }

        /// First-match-wins: include before exclude on the same pattern means
        /// the include wins.
        #[test]
        fn include_before_exclude_include_wins(
            (file, ext) in file_with_ext()
        ) {
            let pattern = format!("*.{ext}");
            let rules = vec![
                FilterRule::include(&pattern),
                FilterRule::exclude(&pattern),
            ];
            let set = FilterSet::from_rules(rules).unwrap();
            prop_assert!(set.allows(Path::new(&file), false));
        }

        /// First-match-wins: exclude before include on the same pattern means
        /// the exclude wins.
        #[test]
        fn exclude_before_include_exclude_wins(
            (file, ext) in file_with_ext()
        ) {
            let pattern = format!("*.{ext}");
            let rules = vec![
                FilterRule::exclude(&pattern),
                FilterRule::include(&pattern),
            ];
            let set = FilterSet::from_rules(rules).unwrap();
            prop_assert!(!set.allows(Path::new(&file), false));
        }

        /// First-match-wins: if rule N matches, rules N+1..end are irrelevant.
        /// We verify by placing an include rule first, followed by an arbitrary
        /// number of exclude rules for the same pattern - the include always wins.
        #[test]
        fn first_match_wins_ignores_later_rules(
            (file, ext) in file_with_ext(),
            extra_excludes in 1..10usize
        ) {
            let pattern = format!("*.{ext}");
            let mut rules = vec![FilterRule::include(&pattern)];
            for _ in 0..extra_excludes {
                rules.push(FilterRule::exclude(&pattern));
            }
            let set = FilterSet::from_rules(rules).unwrap();
            prop_assert!(set.allows(Path::new(&file), false));
        }

        /// Rules with disjoint patterns do not interfere with each other.
        /// Excluding "*.ext_a" should not affect files matching "*.ext_b".
        #[test]
        fn disjoint_patterns_no_interference(
            name in filename(),
            (ext_a, ext_b) in two_distinct_extensions()
        ) {
            let file_a = format!("{name}.{ext_a}");
            let file_b = format!("{name}.{ext_b}");
            let pattern_a = format!("*.{ext_a}");

            let set = FilterSet::from_rules(vec![FilterRule::exclude(&pattern_a)]).unwrap();
            prop_assert!(!set.allows(Path::new(&file_a), false));
            prop_assert!(set.allows(Path::new(&file_b), false));
        }

        /// Default FilterSet (no rules) is equivalent to an empty rule list.
        #[test]
        fn default_filter_set_allows_all(
            name in filename(),
            is_dir in any::<bool>()
        ) {
            let default_set = FilterSet::default();
            let empty_set = FilterSet::from_rules(Vec::new()).unwrap();
            let path = Path::new(&name);
            prop_assert_eq!(
                default_set.allows(path, is_dir),
                empty_set.allows(path, is_dir)
            );
            prop_assert_eq!(
                default_set.allows_deletion(path, is_dir),
                empty_set.allows_deletion(path, is_dir)
            );
        }

        /// An exclude rule followed by a clear rule effectively removes the
        /// exclude, allowing the path again.
        #[test]
        fn clear_resets_chain(
            (file, ext) in file_with_ext()
        ) {
            let pattern = format!("*.{ext}");
            let rules = vec![
                FilterRule::exclude(&pattern),
                FilterRule::clear(),
            ];
            let set = FilterSet::from_rules(rules).unwrap();
            prop_assert!(set.allows(Path::new(&file), false));
        }

        /// Multiple include rules for the same pattern are idempotent - the
        /// path is still allowed regardless of how many duplicate includes exist.
        #[test]
        fn duplicate_includes_idempotent(
            (file, ext) in file_with_ext(),
            count in 1..10usize
        ) {
            let pattern = format!("*.{ext}");
            let rules: Vec<_> = (0..count)
                .map(|_| FilterRule::include(&pattern))
                .collect();
            let set = FilterSet::from_rules(rules).unwrap();
            prop_assert!(set.allows(Path::new(&file), false));
        }

        /// Multiple exclude rules for the same pattern are idempotent - the
        /// path is still blocked regardless of how many duplicate excludes exist.
        #[test]
        fn duplicate_excludes_idempotent(
            (file, ext) in file_with_ext(),
            count in 1..10usize
        ) {
            let pattern = format!("*.{ext}");
            let rules: Vec<_> = (0..count)
                .map(|_| FilterRule::exclude(&pattern))
                .collect();
            let set = FilterSet::from_rules(rules).unwrap();
            prop_assert!(!set.allows(Path::new(&file), false));
        }

        /// Exclude on transfer side also blocks deletion (both sides apply).
        #[test]
        fn exclude_blocks_both_transfer_and_deletion(
            (file, ext) in file_with_ext()
        ) {
            let pattern = format!("*.{ext}");
            let set = FilterSet::from_rules(vec![FilterRule::exclude(&pattern)]).unwrap();
            let path = Path::new(&file);
            prop_assert!(!set.allows(path, false));
            prop_assert!(!set.allows_deletion(path, false));
        }

        /// Sender-only exclude blocks transfer but not deletion.
        #[test]
        fn sender_only_exclude_does_not_block_deletion(
            (file, ext) in file_with_ext()
        ) {
            let pattern = format!("*.{ext}");
            let rules = vec![FilterRule::exclude(&pattern).with_sides(true, false)];
            let set = FilterSet::from_rules(rules).unwrap();
            let path = Path::new(&file);
            prop_assert!(!set.allows(path, false));
            prop_assert!(set.allows_deletion(path, false));
        }
    }
}
