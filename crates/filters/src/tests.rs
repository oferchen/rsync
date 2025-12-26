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
fn include_after_exclude_reinstates_path() {
    let rules = [
        FilterRule::exclude("foo"),
        FilterRule::include("foo/bar.txt"),
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
        FilterRule::exclude("*.tmp"),
        FilterRule::include("special.tmp"),
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
        FilterRule::exclude("cache/"),
        FilterRule::include("cache/preserved/**"),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows(Path::new("cache/preserved/data"), false));
    assert!(!set.allows(Path::new("cache/tmp"), false));
}

#[test]
fn relative_path_conversion_handles_dot_components() {
    let set = FilterSet::from_rules([FilterRule::exclude("foo/bar")]).expect("compiled");
    let mut path = PathBuf::from("foo");
    path.push("..");
    path.push("foo");
    path.push("bar");
    assert!(!set.allows(&path, false));
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
fn risk_rule_allows_deletion_after_protection() {
    let rules = [
        FilterRule::protect("archive/"),
        FilterRule::risk("archive/"),
    ];
    let set = FilterSet::from_rules(rules).expect("compiled");
    assert!(set.allows_deletion(Path::new("archive/file.bin"), false));
}

#[test]
fn risk_rule_applies_to_descendants() {
    let rules = [FilterRule::protect("backup/"), FilterRule::risk("backup/")];
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
            // An include rule should make applies_to_sender and applies_to_receiver true
            let include = FilterRule::include(&pattern);
            prop_assert!(include.applies_to_sender());
            prop_assert!(include.applies_to_receiver());
            prop_assert_eq!(include.action(), FilterAction::Include);
            prop_assert_eq!(include.pattern(), &pattern);

            // An exclude rule should also apply to both sides by default
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
            // Other properties should be unaffected
            prop_assert_eq!(rule.action(), FilterAction::Exclude);
        }
    }
}
