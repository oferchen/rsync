//! Advanced CVS pattern tests covering complex scenarios and edge cases.
//!
//! This test suite focuses on:
//! - CVS pattern interaction with other filter types
//! - Perishable behavior in different contexts
//! - Pattern specificity and override scenarios
//! - Directory traversal implications

use filters::{FilterAction, FilterRule, FilterSet, cvs_default_patterns, cvs_exclusion_rules};
use std::path::Path;

// =============================================================================
// CVS Pattern List Validation
// =============================================================================

#[test]
fn cvs_patterns_no_duplicates() {
    let patterns: Vec<&str> = cvs_default_patterns().collect();
    let mut seen = std::collections::HashSet::new();

    for pattern in &patterns {
        assert!(seen.insert(pattern), "Duplicate pattern found: {pattern}");
    }
}

#[test]
fn cvs_patterns_all_trimmed() {
    for pattern in cvs_default_patterns() {
        assert_eq!(pattern, pattern.trim(), "Pattern not trimmed: {pattern:?}");
        assert!(!pattern.is_empty(), "Empty pattern found");
    }
}

#[test]
fn cvs_patterns_directory_consistency() {
    let patterns: Vec<&str> = cvs_default_patterns().collect();

    // Patterns ending with / should be for directories
    for pattern in &patterns {
        if pattern.ends_with('/') {
            // These should be actual directory names
            assert!(
                !pattern.contains('*') || *pattern == "**/",
                "Directory pattern with wildcard: {pattern}"
            );
        }
    }
}

#[test]
fn cvs_patterns_specific_vcs_coverage() {
    let patterns: Vec<&str> = cvs_default_patterns().collect();

    // Verify we have coverage for major VCS systems
    let has_git = patterns.iter().any(|p| p.contains(".git"));
    let has_svn = patterns.iter().any(|p| p.contains(".svn"));
    let has_hg = patterns.iter().any(|p| p.contains(".hg"));
    let has_cvs = patterns.iter().any(|p| p.to_uppercase().contains("CVS"));

    assert!(has_git, "Missing Git patterns");
    assert!(has_svn, "Missing SVN patterns");
    assert!(has_hg, "Missing Mercurial patterns");
    assert!(has_cvs, "Missing CVS patterns");
}

// =============================================================================
// CVS Rules Generation Tests
// =============================================================================

#[test]
fn cvs_rules_all_apply_to_both_sides_by_default() {
    let rules: Vec<FilterRule> = cvs_exclusion_rules(false).collect();

    for rule in &rules {
        assert!(rule.applies_to_sender(), "Rule should apply to sender");
        assert!(rule.applies_to_receiver(), "Rule should apply to receiver");
    }
}

#[test]
fn cvs_rules_are_exclude_actions() {
    let rules: Vec<FilterRule> = cvs_exclusion_rules(false).collect();

    for rule in &rules {
        assert_eq!(rule.action(), FilterAction::Exclude);
    }
}

#[test]
fn cvs_rules_not_xattr_only() {
    let rules: Vec<FilterRule> = cvs_exclusion_rules(false).collect();

    for rule in &rules {
        assert!(!rule.is_xattr_only());
    }
}

#[test]
fn cvs_rules_not_negated() {
    let rules: Vec<FilterRule> = cvs_exclusion_rules(false).collect();

    for rule in &rules {
        assert!(!rule.is_negated());
    }
}

#[test]
fn cvs_rules_perishable_consistency() {
    let perishable: Vec<FilterRule> = cvs_exclusion_rules(true).collect();
    let non_perishable: Vec<FilterRule> = cvs_exclusion_rules(false).collect();

    assert_eq!(perishable.len(), non_perishable.len());

    for (p, np) in perishable.iter().zip(non_perishable.iter()) {
        assert_eq!(p.pattern(), np.pattern());
        assert!(p.is_perishable());
        assert!(!np.is_perishable());
    }
}

// =============================================================================
// CVS Pattern Matching Specifics
// =============================================================================

#[test]
fn cvs_wildcard_patterns_match_correctly() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // *.o should match at any depth
    assert!(!set.allows(Path::new("main.o"), false));
    assert!(!set.allows(Path::new("src/lib.o"), false));
    assert!(!set.allows(Path::new("a/b/c/test.o"), false));

    // But should not match .org files
    assert!(set.allows(Path::new("README.org"), false));
}

#[test]
fn cvs_directory_patterns_match_correctly() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // .git/ should match .git directory
    assert!(!set.allows(Path::new(".git"), true));

    // And its contents
    assert!(!set.allows(Path::new(".git/config"), false));
    assert!(!set.allows(Path::new(".git/objects/abc"), false));

    // But not files named .git
    // (depends on directory_only handling)
}

#[test]
fn cvs_exact_name_patterns() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // "core" should match file named core
    assert!(!set.allows(Path::new("core"), false));
    assert!(!set.allows(Path::new("dir/core"), false));

    // But not similar names
    assert!(set.allows(Path::new("core2"), false));
    assert!(set.allows(Path::new("mycore"), false));
    assert!(set.allows(Path::new("hardcore"), false));
}

#[test]
fn cvs_prefix_patterns() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // #* should match files starting with #
    assert!(!set.allows(Path::new("#autosave#"), false));
    assert!(!set.allows(Path::new("#test"), false));

    // But not files with # elsewhere
    assert!(set.allows(Path::new("test#"), false));
    assert!(set.allows(Path::new("te#st"), false));
}

#[test]
fn cvs_suffix_patterns() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // *~ should match files ending with ~
    assert!(!set.allows(Path::new("file~"), false));
    assert!(!set.allows(Path::new("backup~"), false));

    // But not files with ~ elsewhere
    assert!(set.allows(Path::new("~file"), false));
    assert!(set.allows(Path::new("fi~le"), false));
}

// =============================================================================
// CVS Interaction with Explicit Rules
// =============================================================================

#[test]
fn explicit_include_overrides_cvs_all_patterns() {
    // Test that explicit includes override various CVS patterns
    let rules = vec![
        FilterRule::include("*.o"),
        FilterRule::include("*.bak"),
        FilterRule::include(".git/"),
        FilterRule::include("core"),
    ];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // All should be included despite CVS patterns
    assert!(set.allows(Path::new("main.o"), false));
    assert!(set.allows(Path::new("file.bak"), false));
    assert!(set.allows(Path::new(".git"), true));
    assert!(set.allows(Path::new("core"), false));
}

#[test]
fn explicit_exclude_before_cvs() {
    // Explicit excludes come before CVS, so they match first
    let rules = vec![FilterRule::exclude("*.rs")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Rust files excluded by explicit rule
    assert!(!set.allows(Path::new("main.rs"), false));

    // CVS patterns still work
    assert!(!set.allows(Path::new("main.o"), false));

    // Non-matching files allowed
    assert!(set.allows(Path::new("README.md"), false));
}

#[test]
fn explicit_include_then_exclude_with_cvs() {
    // Complex layering: include, then exclude, with CVS underneath
    let rules = vec![FilterRule::include("src/"), FilterRule::exclude("*.tmp")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Include src directory
    assert!(set.allows(Path::new("src/main.rs"), false));

    // src/ (rule 1) generates src/** which matches first (first-match-wins)
    // For exclusion to work, exclude must come before include
    assert!(set.allows(Path::new("src/test.tmp"), false));

    // src/ includes everything under src/, even .o files
    assert!(set.allows(Path::new("src/lib.o"), false));
}

#[test]
fn cvs_with_wildcard_include() {
    // Include everything explicitly, then CVS patterns
    let rules = vec![FilterRule::include("**")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // ** include matches first, so everything is included
    assert!(set.allows(Path::new("main.o"), false));
    assert!(set.allows(Path::new("file.bak"), false));
    assert!(set.allows(Path::new(".git/config"), false));
}

#[test]
fn cvs_with_wildcard_exclude() {
    // Explicit exclude everything
    let rules = vec![FilterRule::exclude("**")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // ** exclude matches first, so everything is excluded
    assert!(!set.allows(Path::new("main.rs"), false));
    assert!(!set.allows(Path::new("README.md"), false));
}

// =============================================================================
// Perishable CVS Patterns Tests
// =============================================================================

#[test]
fn perishable_cvs_ignored_for_deletion_context() {
    let set = FilterSet::from_rules_with_cvs(vec![], true).unwrap();

    // For transfer (sender context): perishable patterns apply
    assert!(!set.allows(Path::new("main.o"), false));

    // For deletion (receiver context): perishable patterns ignored
    assert!(set.allows_deletion(Path::new("main.o"), false));
}

#[test]
fn non_perishable_cvs_applies_to_deletion() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // For transfer: patterns apply
    assert!(!set.allows(Path::new("main.o"), false));

    // For deletion: patterns also apply
    assert!(!set.allows_deletion(Path::new("main.o"), false));
}

#[test]
fn perishable_cvs_with_explicit_protect() {
    let rules = vec![FilterRule::protect("*.o")];
    let set = FilterSet::from_rules_with_cvs(rules, true).unwrap();

    // Transfer: excluded by perishable CVS
    assert!(!set.allows(Path::new("main.o"), false));

    // Deletion: perishable ignored, but protected
    assert!(!set.allows_deletion(Path::new("main.o"), false));
}

#[test]
fn perishable_cvs_with_explicit_risk() {
    let rules = vec![FilterRule::protect("*.o"), FilterRule::risk("main.o")];
    let set = FilterSet::from_rules_with_cvs(rules, true).unwrap();

    // Transfer: excluded by perishable CVS
    assert!(!set.allows(Path::new("main.o"), false));

    // Deletion: protect("*.o") (rule 1) matches first (first-match-wins)
    // For risk to override, it must come before protect
    assert!(!set.allows_deletion(Path::new("main.o"), false));

    // Other .o files also protected by rule 1
    assert!(!set.allows_deletion(Path::new("lib.o"), false));
}

#[test]
fn perishable_cvs_delete_excluded_behavior() {
    let set = FilterSet::from_rules_with_cvs(vec![], true).unwrap();

    // Transfer: .o excluded
    assert!(!set.allows(Path::new("main.o"), false));

    // Delete-excluded: should allow (perishable ignored)
    assert!(set.allows_deletion_when_excluded_removed(Path::new("main.o"), false));
}

// =============================================================================
// CVS with Other Filter Actions
// =============================================================================

#[test]
fn cvs_with_show_hide_rules() {
    let rules = vec![
        FilterRule::show("*.o"),   // Sender-only include
        FilterRule::hide("*.bak"), // Sender-only exclude
    ];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Show overrides CVS for .o files (sender side)
    assert!(set.allows(Path::new("main.o"), false));

    // Hide adds to CVS for .bak files (redundant)
    assert!(!set.allows(Path::new("file.bak"), false));
}

#[test]
fn cvs_with_clear_rule() {
    let rules = vec![
        FilterRule::exclude("*.custom"),
        FilterRule::clear(),
        FilterRule::include("*.txt"),
    ];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Clear removes *.custom rule
    assert!(set.allows(Path::new("file.custom"), false));

    // But CVS patterns still apply (added after clear)
    assert!(!set.allows(Path::new("main.o"), false));

    // Include rule works
    assert!(set.allows(Path::new("notes.txt"), false));
}

#[test]
fn cvs_after_clear_resets_to_cvs_only() {
    let rules = vec![FilterRule::clear()];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Only CVS patterns should be active
    assert!(!set.allows(Path::new("main.o"), false));
    assert!(set.allows(Path::new("main.rs"), false));
}

// =============================================================================
// Complex Nested Directory Tests
// =============================================================================

#[test]
fn cvs_nested_vcs_directories() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Top-level .git
    assert!(!set.allows(Path::new(".git"), true));
    assert!(!set.allows(Path::new(".git/config"), false));

    // Nested .git (submodules)
    assert!(!set.allows(Path::new("submodule/.git"), true));
    assert!(!set.allows(Path::new("submodule/.git/config"), false));

    // Deeply nested
    assert!(!set.allows(Path::new("a/b/c/.git"), true));
    assert!(!set.allows(Path::new("a/b/c/.git/refs/heads/main"), false));
}

#[test]
fn cvs_build_artifacts_in_various_locations() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Root level
    assert!(!set.allows(Path::new("main.o"), false));

    // Build directories
    assert!(!set.allows(Path::new("build/main.o"), false));
    assert!(!set.allows(Path::new("target/debug/lib.o"), false));

    // Source directories
    assert!(!set.allows(Path::new("src/main.o"), false));
    assert!(!set.allows(Path::new("lib/util.o"), false));

    // But source files are allowed
    assert!(set.allows(Path::new("src/main.c"), false));
    assert!(set.allows(Path::new("lib/util.c"), false));
}

#[test]
fn cvs_backup_files_various_extensions() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Vim backups
    assert!(!set.allows(Path::new("file~"), false));
    assert!(!set.allows(Path::new(".vimrc~"), false));

    // Emacs auto-save
    assert!(!set.allows(Path::new("#file#"), false));
    assert!(!set.allows(Path::new(".#file"), false));

    // Various backup extensions
    assert!(!set.allows(Path::new("config.bak"), false));
    assert!(!set.allows(Path::new("CONFIG.BAK"), false));
    assert!(!set.allows(Path::new("data.old"), false));
    assert!(!set.allows(Path::new("patch.orig"), false));
    assert!(!set.allows(Path::new("merge.rej"), false));
}

// =============================================================================
// CVS Pattern Edge Cases
// =============================================================================

#[test]
fn cvs_core_file_vs_core_prefix() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // "core" pattern should match exact name
    assert!(!set.allows(Path::new("core"), false));

    // But not prefixed names
    assert!(set.allows(Path::new("core.txt"), false));
    assert!(set.allows(Path::new("corefile"), false));
    assert!(set.allows(Path::new("multicore"), false));
}

#[test]
fn cvs_tags_files_case_sensitivity() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Both "tags" and "TAGS" patterns
    assert!(!set.allows(Path::new("tags"), false));
    assert!(!set.allows(Path::new("TAGS"), false));

    // But mixed case should be allowed (no pattern for Tags)
    assert!(set.allows(Path::new("Tags"), false));
}

#[test]
fn cvs_wildcard_not_too_greedy() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // _$* pattern should match _$ followed by anything
    assert!(!set.allows(Path::new("_$file"), false));
    assert!(!set.allows(Path::new("_$123"), false));

    // $ at start not matched by _$* or *$
    assert!(set.allows(Path::new("$file"), false));
    // file_$ matches *$ pattern (anything ending with $)
    assert!(!set.allows(Path::new("file_$"), false));
}

#[test]
fn cvs_compressed_files() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // *.Z pattern for compress format
    assert!(!set.allows(Path::new("archive.Z"), false));

    // But other compression formats not in CVS patterns
    assert!(set.allows(Path::new("archive.gz"), false));
    assert!(set.allows(Path::new("archive.bz2"), false));
    assert!(set.allows(Path::new("archive.xz"), false));
}

// =============================================================================
// CVS Integration with FilterSet Methods
// =============================================================================

#[test]
fn cvs_allows_deletion_respects_patterns() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Non-perishable CVS patterns affect deletion
    assert!(!set.allows_deletion(Path::new("main.o"), false));
    assert!(!set.allows_deletion(Path::new(".git"), true));
}

#[test]
fn cvs_with_protect_prevents_deletion() {
    let rules = vec![FilterRule::protect(".git/")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Transfer: excluded by CVS
    assert!(!set.allows(Path::new(".git"), true));

    // Deletion: CVS excludes AND protected
    assert!(!set.allows_deletion(Path::new(".git"), true));
}

#[test]
fn cvs_empty_set_behavior() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Set should not be empty (has CVS rules)
    assert!(!set.is_empty());
}

// =============================================================================
// Multiple CVS Pattern Application
// =============================================================================

#[test]
fn file_matching_multiple_cvs_patterns() {
    // Some files might match multiple patterns (e.g., .bak.old)
    // First match wins
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // File matching multiple patterns
    assert!(!set.allows(Path::new("file.bak.old"), false));

    // The fact that it matches multiple is irrelevant, first match is enough
}

#[test]
fn directory_and_file_pattern_interaction() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // .git/ directory pattern
    assert!(!set.allows(Path::new(".git"), true));

    // Files within should also be excluded
    assert!(!set.allows(Path::new(".git/config"), false));
    assert!(!set.allows(Path::new(".git/objects/abc"), false));
}
