//! Comprehensive tests for --filter rule syntax.
//!
//! These tests verify the parsing and behavior of rsync's --filter flag syntax,
//! including all rule types and modifiers. This covers:
//!
//! 1. Include rules (+ pattern)
//! 2. Exclude rules (- pattern)
//! 3. Clear rules (!)
//! 4. Dir-merge rules (: filename)
//! 5. Hide/show rules (H/S)
//! 6. Protect/risk rules (P/R)
//! 7. Combined filter types
//! 8. Rule ordering
//!
//! Reference: rsync 3.4.1 exclude.c for filter rule parsing.

use filters::{FilterAction, FilterRule, FilterSet, parse_rules};
use std::path::Path;

// ============================================================================
// 1. Include Rules (+ pattern)
// ============================================================================

mod include_rules {
    use super::*;

    #[test]
    fn short_form_include() {
        let rules = parse_rules("+ *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert_eq!(rules[0].pattern(), "*.txt");
    }

    #[test]
    fn short_form_include_no_space() {
        let rules = parse_rules("+*.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert_eq!(rules[0].pattern(), "*.txt");
    }

    #[test]
    fn long_form_include() {
        let rules = parse_rules("include *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert_eq!(rules[0].pattern(), "*.txt");
    }

    #[test]
    fn long_form_include_case_insensitive() {
        let rules = parse_rules("INCLUDE *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
    }

    #[test]
    fn include_with_wildcard() {
        let rules = parse_rules("+ **/*.rs", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(set.allows(Path::new("main.rs"), false));
        assert!(set.allows(Path::new("src/lib.rs"), false));
        assert!(set.allows(Path::new("src/nested/mod.rs"), false));
    }

    #[test]
    fn include_with_directory_pattern() {
        let rules = parse_rules("+ src/", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Directory matches
        assert!(set.allows(Path::new("src"), true));

        // Directory descendants are included
        assert!(set.allows(Path::new("src/main.rs"), false));
    }

    #[test]
    fn include_anchored_pattern() {
        let rules = parse_rules("+ /root_file.txt", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Anchored pattern matches only at root
        assert!(set.allows(Path::new("root_file.txt"), false));

        // Does not match nested paths
        assert!(set.allows(Path::new("dir/root_file.txt"), false)); // Default include
    }

    #[test]
    fn include_with_character_class() {
        let rules = parse_rules("+ file[0-9].txt", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(set.allows(Path::new("file1.txt"), false));
        assert!(set.allows(Path::new("file9.txt"), false));
    }

    #[test]
    fn include_with_question_mark() {
        let rules = parse_rules("+ file?.txt", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(set.allows(Path::new("fileA.txt"), false));
        assert!(set.allows(Path::new("file1.txt"), false));
    }

    #[test]
    fn include_with_negation_modifier() {
        let rules = parse_rules("+! *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert!(rules[0].is_negated());
    }

    #[test]
    fn include_with_perishable_modifier() {
        let rules = parse_rules("+p *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_perishable());
    }

    #[test]
    fn include_with_sender_modifier() {
        let rules = parse_rules("+s *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn include_with_receiver_modifier() {
        let rules = parse_rules("+r *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(!rules[0].applies_to_sender());
        assert!(rules[0].applies_to_receiver());
    }

    #[test]
    fn include_with_multiple_modifiers() {
        let rules = parse_rules("+!ps *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_negated());
        assert!(rules[0].is_perishable());
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn include_with_word_split() {
        let rules = parse_rules("+w *.rs *.toml *.md", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.rs");
        assert_eq!(rules[1].pattern(), "*.toml");
        assert_eq!(rules[2].pattern(), "*.md");
        for rule in &rules {
            assert_eq!(rule.action(), FilterAction::Include);
        }
    }
}

// ============================================================================
// 2. Exclude Rules (- pattern)
// ============================================================================

mod exclude_rules {
    use super::*;

    #[test]
    fn short_form_exclude() {
        let rules = parse_rules("- *.bak", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
        assert_eq!(rules[0].pattern(), "*.bak");
    }

    #[test]
    fn short_form_exclude_no_space() {
        let rules = parse_rules("-*.bak", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
        assert_eq!(rules[0].pattern(), "*.bak");
    }

    #[test]
    fn long_form_exclude() {
        let rules = parse_rules("exclude *.bak", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
        assert_eq!(rules[0].pattern(), "*.bak");
    }

    #[test]
    fn long_form_exclude_case_insensitive() {
        let rules = parse_rules("EXCLUDE *.bak", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
    }

    #[test]
    fn exclude_blocks_matching_files() {
        let rules = parse_rules("- *.tmp", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(!set.allows(Path::new("scratch.tmp"), false));
        assert!(!set.allows(Path::new("dir/file.tmp"), false));
        assert!(set.allows(Path::new("file.txt"), false));
    }

    #[test]
    fn exclude_directory_pattern() {
        let rules = parse_rules("- build/", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Directory itself
        assert!(!set.allows(Path::new("build"), true));

        // Directory contents
        assert!(!set.allows(Path::new("build/output.bin"), false));

        // File named build (not a directory)
        assert!(set.allows(Path::new("build"), false));
    }

    #[test]
    fn exclude_with_double_star() {
        let rules = parse_rules("- **/cache/**", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(!set.allows(Path::new("cache/data"), false));
        assert!(!set.allows(Path::new("app/cache/data"), false));
        assert!(!set.allows(Path::new("deep/nested/cache/file.txt"), false));
    }

    #[test]
    fn exclude_with_negation_excludes_non_matching() {
        // Negated exclude: excludes files that do NOT match the pattern
        let rules = parse_rules("-! *.txt", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Files matching *.txt are allowed (negated = inverted)
        assert!(set.allows(Path::new("readme.txt"), false));

        // Files NOT matching *.txt are excluded
        assert!(!set.allows(Path::new("image.png"), false));
        assert!(!set.allows(Path::new("data.json"), false));
    }

    #[test]
    fn exclude_with_perishable_modifier() {
        let rules = parse_rules("-p *.tmp", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_perishable());
    }

    #[test]
    fn exclude_with_xattr_modifier() {
        let rules = parse_rules("-x user.*", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_xattr_only());
    }

    #[test]
    fn exclude_with_exclude_only_modifier() {
        let rules = parse_rules("-e *.log", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_exclude_only());
    }

    #[test]
    fn exclude_with_no_inherit_modifier() {
        let rules = parse_rules("-n *.tmp", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_no_inherit());
    }

    #[test]
    fn exclude_with_word_split() {
        let rules = parse_rules("-w *.tmp *.bak *.swp", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].pattern(), "*.tmp");
        assert_eq!(rules[1].pattern(), "*.bak");
        assert_eq!(rules[2].pattern(), "*.swp");
        for rule in &rules {
            assert_eq!(rule.action(), FilterAction::Exclude);
        }
    }

    #[test]
    fn exclude_with_combined_modifiers_and_word_split() {
        let rules = parse_rules("-!pw *.o *.obj", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 2);
        for rule in &rules {
            assert!(rule.is_negated());
            assert!(rule.is_perishable());
        }
    }

    #[test]
    fn exclude_underscore_separator() {
        // Underscore can separate modifiers from pattern
        let rules = parse_rules("-!_ *.txt", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), "*.txt");
        assert!(rules[0].is_negated());
    }
}

// ============================================================================
// 3. Clear Rules (!)
// ============================================================================

mod clear_rules {
    use super::*;

    #[test]
    fn short_form_clear() {
        let rules = parse_rules("!", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Clear);
        assert!(rules[0].pattern().is_empty());
    }

    #[test]
    fn long_form_clear() {
        let rules = parse_rules("clear", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Clear);
    }

    #[test]
    fn long_form_clear_case_insensitive() {
        let rules = parse_rules("CLEAR", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Clear);
    }

    #[test]
    fn clear_removes_previous_excludes() {
        let rules = parse_rules("- *.tmp\n!\n+ *.txt", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Previous exclude cleared
        assert!(set.allows(Path::new("file.tmp"), false));

        // New include active
        assert!(set.allows(Path::new("file.txt"), false));
    }

    #[test]
    fn clear_removes_previous_includes() {
        let rules = parse_rules("+ important.txt\n- *\n!\n- *.log", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Previous rules cleared
        assert!(set.allows(Path::new("important.txt"), false));

        // New exclude active
        assert!(!set.allows(Path::new("debug.log"), false));
    }

    #[test]
    fn clear_removes_protect_rules() {
        let rules = parse_rules("P critical/\n!", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Protection cleared
        assert!(set.allows_deletion(Path::new("critical/data.dat"), false));
    }

    #[test]
    fn clear_removes_risk_rules() {
        let rules = parse_rules("P data/\nR data/temp/\n!", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // All rules cleared
        assert!(set.allows_deletion(Path::new("data/file.dat"), false));
        assert!(set.allows_deletion(Path::new("data/temp/scratch"), false));
    }

    #[test]
    fn clear_rule_properties() {
        let rule = FilterRule::clear();

        assert_eq!(rule.action(), FilterAction::Clear);
        assert!(rule.pattern().is_empty());
        assert!(rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn clear_with_sender_only() {
        let rule = FilterRule::clear().with_sides(true, false);

        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn clear_with_receiver_only() {
        let rule = FilterRule::clear().with_sides(false, true);

        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn multiple_clears() {
        let rules = parse_rules("- *.a\n!\n- *.b\n!\n- *.c", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Only last rule after last clear is active
        assert!(set.allows(Path::new("file.a"), false));
        assert!(set.allows(Path::new("file.b"), false));
        assert!(!set.allows(Path::new("file.c"), false));
    }

    #[test]
    fn clear_at_end_results_in_empty_set() {
        let rules =
            parse_rules("- *.tmp\n+ important/\nP critical/\n!", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(set.is_empty());
    }
}

// ============================================================================
// 4. Dir-Merge Rules (: filename)
// ============================================================================

mod dir_merge_rules {
    use super::*;

    #[test]
    fn short_form_dir_merge() {
        let rules = parse_rules(": .rsync-filter", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::DirMerge);
        assert_eq!(rules[0].pattern(), ".rsync-filter");
    }

    #[test]
    fn long_form_dir_merge() {
        let rules = parse_rules("dir-merge .rsync-filter", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::DirMerge);
        assert_eq!(rules[0].pattern(), ".rsync-filter");
    }

    #[test]
    fn long_form_dir_merge_case_insensitive() {
        let rules = parse_rules("DIR-MERGE .rsync-filter", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::DirMerge);
    }

    #[test]
    fn dir_merge_rule_properties() {
        let rule = FilterRule::dir_merge(".rsync-filter");

        assert_eq!(rule.action(), FilterAction::DirMerge);
        assert_eq!(rule.pattern(), ".rsync-filter");
        assert!(rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn dir_merge_with_custom_filename() {
        let rules = parse_rules(": .gitignore", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), ".gitignore");
    }

    #[test]
    fn dir_merge_skipped_in_filter_set() {
        // Dir-merge rules are processed per-directory during traversal,
        // not at compilation time
        let rules = parse_rules(": .rsync-filter\n- *.tmp", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Only the exclude rule is active
        assert!(!set.allows(Path::new("file.tmp"), false));
    }

    #[test]
    fn merge_rule_short_form() {
        let rules = parse_rules(". /etc/rsync/rules", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Merge);
        assert_eq!(rules[0].pattern(), "/etc/rsync/rules");
    }

    #[test]
    fn merge_rule_long_form() {
        let rules = parse_rules("merge /etc/rsync/rules", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Merge);
    }

    #[test]
    fn merge_rule_properties() {
        let rule = FilterRule::merge("/etc/rsync/global.rules");

        assert_eq!(rule.action(), FilterAction::Merge);
        assert_eq!(rule.pattern(), "/etc/rsync/global.rules");
        assert!(rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }
}

// ============================================================================
// 5. Hide/Show Rules (H/S)
// ============================================================================

mod hide_show_rules {
    use super::*;

    #[test]
    fn hide_short_form() {
        let rules = parse_rules("H *.secret", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
        assert_eq!(rules[0].pattern(), "*.secret");
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn hide_long_form() {
        let rules = parse_rules("hide *.secret", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn hide_long_form_case_insensitive() {
        let rules = parse_rules("HIDE *.secret", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
    }

    #[test]
    fn show_short_form() {
        let rules = parse_rules("S *.public", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert_eq!(rules[0].pattern(), "*.public");
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn show_long_form() {
        let rules = parse_rules("show *.public", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn show_long_form_case_insensitive() {
        let rules = parse_rules("SHOW *.public", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
    }

    #[test]
    fn hide_rule_properties() {
        let rule = FilterRule::hide("*.secret");

        assert_eq!(rule.action(), FilterAction::Exclude);
        assert_eq!(rule.pattern(), "*.secret");
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn show_rule_properties() {
        let rule = FilterRule::show("*.public");

        assert_eq!(rule.action(), FilterAction::Include);
        assert_eq!(rule.pattern(), "*.public");
        assert!(rule.applies_to_sender());
        assert!(!rule.applies_to_receiver());
    }

    #[test]
    fn hide_blocks_transfer_allows_deletion() {
        let rules = parse_rules("H *.hidden", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Hidden from transfer (sender)
        assert!(!set.allows(Path::new("file.hidden"), false));

        // But can be deleted (receiver - hide doesn't apply)
        assert!(set.allows_deletion(Path::new("file.hidden"), false));
    }

    #[test]
    fn show_allows_transfer_allows_deletion() {
        let rules = parse_rules("S visible/**", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Shown for transfer
        assert!(set.allows(Path::new("visible/file.txt"), false));

        // Can be deleted (show doesn't affect receiver)
        assert!(set.allows_deletion(Path::new("visible/file.txt"), false));
    }

    #[test]
    fn hide_with_negation() {
        let rules = parse_rules("H! *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_negated());
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn show_with_negation() {
        let rules = parse_rules("S! *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_negated());
        assert!(rules[0].applies_to_sender());
        assert!(!rules[0].applies_to_receiver());
    }

    #[test]
    fn hide_with_perishable() {
        let rules = parse_rules("Hp *.tmp", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_perishable());
    }

    #[test]
    fn show_with_perishable() {
        let rules = parse_rules("Sp important/*", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_perishable());
    }
}

// ============================================================================
// 6. Protect/Risk Rules (P/R)
// ============================================================================

mod protect_risk_rules {
    use super::*;

    #[test]
    fn protect_short_form() {
        let rules = parse_rules("P /important", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Protect);
        assert_eq!(rules[0].pattern(), "/important");
    }

    #[test]
    fn protect_long_form() {
        let rules = parse_rules("protect /important", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Protect);
    }

    #[test]
    fn protect_long_form_case_insensitive() {
        let rules = parse_rules("PROTECT /important", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Protect);
    }

    #[test]
    fn risk_short_form() {
        let rules = parse_rules("R /temp", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Risk);
        assert_eq!(rules[0].pattern(), "/temp");
    }

    #[test]
    fn risk_long_form() {
        let rules = parse_rules("risk /temp", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Risk);
    }

    #[test]
    fn risk_long_form_case_insensitive() {
        let rules = parse_rules("RISK /temp", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Risk);
    }

    #[test]
    fn protect_rule_properties() {
        let rule = FilterRule::protect("/critical");

        assert_eq!(rule.action(), FilterAction::Protect);
        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn risk_rule_properties() {
        let rule = FilterRule::risk("/temp");

        assert_eq!(rule.action(), FilterAction::Risk);
        assert!(!rule.applies_to_sender());
        assert!(rule.applies_to_receiver());
    }

    #[test]
    fn protect_blocks_deletion() {
        let rules = parse_rules("P *.conf", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Transfer allowed (protect doesn't affect transfer)
        assert!(set.allows(Path::new("app.conf"), false));

        // Deletion blocked
        assert!(!set.allows_deletion(Path::new("app.conf"), false));
    }

    #[test]
    fn protect_directory_includes_descendants() {
        let rules = parse_rules("P config/", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(!set.allows_deletion(Path::new("config"), true));
        assert!(!set.allows_deletion(Path::new("config/app.yaml"), false));
        assert!(!set.allows_deletion(Path::new("config/nested/db.yaml"), false));
    }

    #[test]
    fn risk_allows_deletion_before_protect() {
        // First-match-wins: risk must come before protect
        let rules = parse_rules("R data/temp/\nP data/", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // temp is not protected (risk matches first)
        assert!(set.allows_deletion(Path::new("data/temp/scratch"), false));

        // Other data is protected
        assert!(!set.allows_deletion(Path::new("data/important"), false));
    }

    #[test]
    fn protect_with_negation() {
        let rules = parse_rules("P! /excluded", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_negated());
    }

    #[test]
    fn risk_with_negation() {
        let rules = parse_rules("R! /included", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_negated());
    }

    #[test]
    fn protect_with_wildcard() {
        let rules = parse_rules("P **/credentials.*", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        assert!(!set.allows_deletion(Path::new("credentials.json"), false));
        assert!(!set.allows_deletion(Path::new("app/credentials.yaml"), false));
        assert!(!set.allows_deletion(Path::new("deep/nested/credentials.env"), false));
    }

    #[test]
    fn protect_and_exclude_same_file() {
        let rules = parse_rules("- *.bak\nP important.bak", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Excluded from transfer
        assert!(!set.allows(Path::new("important.bak"), false));

        // But protected from deletion
        assert!(!set.allows_deletion(Path::new("important.bak"), false));
    }
}

// ============================================================================
// 7. Combined Filter Types
// ============================================================================

mod combined_filter_types {
    use super::*;

    #[test]
    fn include_exclude_protect_combined() {
        let rules = parse_rules("+ *.rs\n- *.tmp\nP Cargo.lock", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Include works
        assert!(set.allows(Path::new("main.rs"), false));

        // Exclude works
        assert!(!set.allows(Path::new("file.tmp"), false));

        // Protect works
        assert!(!set.allows_deletion(Path::new("Cargo.lock"), false));
    }

    #[test]
    fn hide_show_with_protect_risk() {
        let rules = parse_rules(
            "H *.secret\nS *.public\nP *.lock\nR *.tmp",
            Path::new("test"),
        )
        .unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Hide blocks transfer, allows deletion
        assert!(!set.allows(Path::new("key.secret"), false));
        assert!(set.allows_deletion(Path::new("key.secret"), false));

        // Show allows transfer
        assert!(set.allows(Path::new("info.public"), false));

        // Protect blocks deletion
        assert!(!set.allows_deletion(Path::new("package.lock"), false));
    }

    #[test]
    fn all_rule_types_in_sequence() {
        let rules = parse_rules(
            "+ important/**\n\
             - temp/\n\
             H *.hidden\n\
             S visible/**\n\
             P critical/\n\
             R critical/temp/\n\
             : .rsync-filter",
            Path::new("test"),
        )
        .unwrap();

        assert_eq!(rules.len(), 7);
        assert_eq!(rules[0].action(), FilterAction::Include);
        assert_eq!(rules[1].action(), FilterAction::Exclude);
        assert_eq!(rules[2].action(), FilterAction::Exclude); // Hide
        assert_eq!(rules[3].action(), FilterAction::Include); // Show
        assert_eq!(rules[4].action(), FilterAction::Protect);
        assert_eq!(rules[5].action(), FilterAction::Risk);
        assert_eq!(rules[6].action(), FilterAction::DirMerge);
    }

    #[test]
    fn clear_in_middle_of_combined_rules() {
        let rules = parse_rules(
            "- *.tmp\nP important/\n!\n+ *.rs\nP Cargo.*",
            Path::new("test"),
        )
        .unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Rules before clear are gone
        assert!(set.allows(Path::new("file.tmp"), false));
        assert!(set.allows_deletion(Path::new("important/data"), false));

        // Rules after clear are active
        assert!(set.allows(Path::new("main.rs"), false));
        assert!(!set.allows_deletion(Path::new("Cargo.toml"), false));
    }

    #[test]
    fn sender_receiver_rules_combined() {
        let rules = parse_rules(
            "-s sender_only.txt\n\
             -r receiver_only.txt\n\
             - both.txt",
            Path::new("test"),
        )
        .unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Sender-only: blocks transfer, allows deletion
        assert!(!set.allows(Path::new("sender_only.txt"), false));
        assert!(set.allows_deletion(Path::new("sender_only.txt"), false));

        // Receiver-only: allows transfer, blocks deletion
        assert!(set.allows(Path::new("receiver_only.txt"), false));
        assert!(!set.allows_deletion(Path::new("receiver_only.txt"), false));

        // Both: blocks both
        assert!(!set.allows(Path::new("both.txt"), false));
        assert!(!set.allows_deletion(Path::new("both.txt"), false));
    }

    #[test]
    fn perishable_and_non_perishable_combined() {
        let rules = parse_rules(
            "+p keep/**\n\
             -p *.tmp\n\
             - *.bak",
            Path::new("test"),
        )
        .unwrap();

        assert!(rules[0].is_perishable());
        assert!(rules[1].is_perishable());
        assert!(!rules[2].is_perishable());
    }

    #[test]
    fn negated_and_regular_rules_combined() {
        // To properly use negated exclude to allow only certain files:
        // 1. First include specific exceptions
        // 2. Then use negated exclude to catch non-matching files
        let rules = parse_rules(
            "+ important.log\n\
             -! *.txt",
            Path::new("test"),
        )
        .unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // .txt files pass through negated exclude (pattern matches, so negated doesn't match)
        // and are allowed by default since no subsequent rules exclude them
        assert!(set.allows(Path::new("readme.txt"), false));

        // important.log explicitly included before negated exclude
        assert!(set.allows(Path::new("important.log"), false));

        // Non-.txt files: negated exclude matches (pattern doesn't match *.txt,
        // negated inverts to match), action=Exclude -> excluded
        assert!(!set.allows(Path::new("image.png"), false));
    }

    #[test]
    fn modifiers_on_different_rule_types() {
        let rules = parse_rules(
            "+!p special.txt\n\
             -!s *.hidden\n\
             P!r /protected\n\
             H!p *.secret",
            Path::new("test"),
        )
        .unwrap();

        // Include with negate and perishable
        assert!(rules[0].is_negated());
        assert!(rules[0].is_perishable());
        assert_eq!(rules[0].action(), FilterAction::Include);

        // Exclude with negate and sender-only
        assert!(rules[1].is_negated());
        assert!(rules[1].applies_to_sender());
        assert!(!rules[1].applies_to_receiver());

        // Protect with negate and receiver-only (default for protect)
        assert!(rules[2].is_negated());

        // Hide with negate and perishable
        assert!(rules[3].is_negated());
        assert!(rules[3].is_perishable());
    }
}

// ============================================================================
// 8. Rule Ordering
// ============================================================================

mod rule_ordering {
    use super::*;

    #[test]
    fn first_match_wins_include_before_exclude() {
        let rules = parse_rules("+ important.txt\n- *.txt", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Important.txt included by first rule
        assert!(set.allows(Path::new("important.txt"), false));

        // Other .txt files excluded by second rule
        assert!(!set.allows(Path::new("other.txt"), false));
    }

    #[test]
    fn first_match_wins_exclude_before_include() {
        let rules = parse_rules("- secret.txt\n+ *.txt", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // secret.txt excluded by first rule
        assert!(!set.allows(Path::new("secret.txt"), false));

        // Other .txt files included by second rule
        assert!(set.allows(Path::new("readme.txt"), false));
    }

    #[test]
    fn specific_before_general() {
        let rules = parse_rules("- test_*.rs\n+ *.rs\n- *", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // test_*.rs excluded
        assert!(!set.allows(Path::new("test_main.rs"), false));

        // Regular .rs included
        assert!(set.allows(Path::new("main.rs"), false));

        // Others excluded
        assert!(!set.allows(Path::new("Cargo.toml"), false));
    }

    #[test]
    fn anchored_before_unanchored() {
        let rules = parse_rules("+ /build\n- build", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Root build included
        assert!(set.allows(Path::new("build"), false));

        // Nested build excluded
        assert!(!set.allows(Path::new("src/build"), false));
    }

    #[test]
    fn directory_before_file_pattern() {
        let rules = parse_rules("+ build/\n- build", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Directory build included
        assert!(set.allows(Path::new("build"), true));

        // File build excluded
        assert!(!set.allows(Path::new("build"), false));
    }

    #[test]
    fn protect_risk_ordering() {
        // Risk first, protect second
        let rules1 = parse_rules("R temp/\nP temp/", Path::new("test")).unwrap();
        let set1 = FilterSet::from_rules(rules1).unwrap();
        assert!(set1.allows_deletion(Path::new("temp/file"), false)); // Risk wins

        // Protect first, risk second
        let rules2 = parse_rules("P temp/\nR temp/", Path::new("test")).unwrap();
        let set2 = FilterSet::from_rules(rules2).unwrap();
        assert!(!set2.allows_deletion(Path::new("temp/file"), false)); // Protect wins
    }

    #[test]
    fn complex_ordering_scenario() {
        let rules = parse_rules(
            "+ src/**/test/fixtures/**\n\
             - src/**/test/**\n\
             + src/**\n\
             - *",
            Path::new("test"),
        )
        .unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Fixtures included
        assert!(set.allows(Path::new("src/lib/test/fixtures/data.json"), false));

        // Other test files excluded
        assert!(!set.allows(Path::new("src/lib/test/unit.rs"), false));

        // Regular src files included
        assert!(set.allows(Path::new("src/main.rs"), false));

        // Root files excluded
        assert!(!set.allows(Path::new("Cargo.toml"), false));
    }

    #[test]
    fn ordering_with_clear() {
        let rules = parse_rules("- *.a\n- *.b\n!\n+ *.a\n- *.a", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Rules before clear are gone, new ordering applies
        // + *.a comes before - *.a, so .a files are included
        assert!(set.allows(Path::new("file.a"), false));

        // .b is allowed (original exclude cleared, no new rule)
        assert!(set.allows(Path::new("file.b"), false));
    }

    #[test]
    fn ordering_preserves_side_specificity() {
        let rules = parse_rules(
            "-s sender_first.txt\n\
             -r receiver_first.txt\n\
             + sender_first.txt\n\
             + receiver_first.txt",
            Path::new("test"),
        )
        .unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Sender context: sender_first excluded (sender rule matches first)
        assert!(!set.allows(Path::new("sender_first.txt"), false));

        // Receiver context: receiver_first excluded
        assert!(!set.allows_deletion(Path::new("receiver_first.txt"), false));

        // But the opposite sides work
        assert!(set.allows_deletion(Path::new("sender_first.txt"), false)); // No receiver exclude
    }

    #[test]
    fn no_match_defaults_to_include() {
        let rules = parse_rules("- *.tmp", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Non-matching paths are included by default
        assert!(set.allows(Path::new("file.txt"), false));
        assert!(set.allows_deletion(Path::new("file.txt"), false));
    }

    #[test]
    fn multiple_matching_patterns_first_wins() {
        let rules = parse_rules("+ *.txt\n- readme.txt\n+ readme.*", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // readme.txt matches *.txt first, so it's included
        // (even though - readme.txt would exclude it if checked first)
        assert!(set.allows(Path::new("readme.txt"), false));
    }

    #[test]
    fn perishable_rules_skipped_for_deletion() {
        let rules = parse_rules("-p *.tmp\n+ keep/**", Path::new("test")).unwrap();
        let set = FilterSet::from_rules(rules).unwrap();

        // Transfer: perishable exclude applies
        assert!(!set.allows(Path::new("file.tmp"), false));

        // Deletion: perishable exclude skipped, defaults to allow
        assert!(set.allows_deletion(Path::new("file.tmp"), false));
    }
}

// ============================================================================
// Comments and Whitespace
// ============================================================================

mod comments_and_whitespace {
    use super::*;

    #[test]
    fn hash_comment_ignored() {
        let rules = parse_rules("# This is a comment\n+ *.txt", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Include);
    }

    #[test]
    fn semicolon_comment_ignored() {
        let rules = parse_rules("; This is a comment\n- *.bak", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action(), FilterAction::Exclude);
    }

    #[test]
    fn empty_lines_ignored() {
        let rules = parse_rules("\n\n+ *.txt\n\n- *.bak\n\n", Path::new("test")).unwrap();
        assert_eq!(rules.len(), 2);
    }

    #[test]
    fn whitespace_trimmed() {
        let rules = parse_rules("  + *.txt  ", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), "*.txt");
    }

    #[test]
    fn mixed_comments_and_rules() {
        let rules = parse_rules(
            "# Include text files\n\
             + *.txt\n\
             \n\
             ; Exclude backups\n\
             - *.bak\n\
             # End of rules",
            Path::new("test"),
        )
        .unwrap();
        assert_eq!(rules.len(), 2);
    }
}

// ============================================================================
// Error Handling
// ============================================================================

mod error_handling {
    use super::*;

    #[test]
    fn unrecognized_rule_error() {
        let result = parse_rules("invalid rule", Path::new("test.rules"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("unrecognized"));
        assert_eq!(err.line, Some(1));
    }

    #[test]
    fn error_includes_line_number() {
        let result = parse_rules("+ *.txt\n- *.bak\nbad rule", Path::new("test.rules"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.line, Some(3));
    }

    #[test]
    fn error_includes_path() {
        let result = parse_rules("bad rule", Path::new("/path/to/rules.txt"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.path.contains("rules.txt"));
    }

    #[test]
    fn empty_pattern_after_action() {
        // Action with no pattern should fail
        let result = parse_rules("+ ", Path::new("test"));
        assert!(result.is_err());
    }

    #[test]
    fn invalid_glob_pattern() {
        let rules = parse_rules("- [", Path::new("test")).unwrap();
        let result = FilterSet::from_rules(rules);
        assert!(result.is_err());
    }
}

// ============================================================================
// Pattern Preservation
// ============================================================================

mod pattern_preservation {
    use super::*;

    #[test]
    fn pattern_case_preserved() {
        let rules = parse_rules("include README.TXT", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), "README.TXT");
    }

    #[test]
    fn pattern_with_spaces_preserved() {
        let rules = parse_rules("+ file with spaces.txt", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), "file with spaces.txt");
    }

    #[test]
    fn pattern_special_chars_preserved() {
        let rules = parse_rules("+ foo\\?bar", Path::new("test")).unwrap();
        assert_eq!(rules[0].pattern(), "foo\\?bar");
    }
}
