//! Integration tests for CVS exclusion patterns.
//!
//! These tests verify the behavior of rsync's `--cvs-exclude` (`-C`) option
//! which automatically excludes common version control, build artifact, and
//! editor backup files. The patterns match upstream rsync's default CVS
//! exclusion list.
//!
//! Reference: rsync 3.4.1 options.c and exclude.c for CVS exclusion handling.

use filters::{FilterRule, FilterSet, cvs_default_patterns, cvs_exclusion_rules};
use std::path::Path;

// ============================================================================
// Default CVS Pattern Tests
// ============================================================================

/// Verifies all expected VCS directories are in the default patterns.
#[test]
fn default_patterns_include_vcs_directories() {
    let patterns: Vec<&str> = cvs_default_patterns().collect();

    // Git
    assert!(patterns.contains(&".git/"));

    // Subversion
    assert!(patterns.contains(&".svn/"));

    // Mercurial
    assert!(patterns.contains(&".hg/"));

    // Bazaar
    assert!(patterns.contains(&".bzr/"));

    // CVS
    assert!(patterns.contains(&"CVS"));

    // RCS
    assert!(patterns.contains(&"RCS"));

    // SCCS
    assert!(patterns.contains(&"SCCS"));
}

/// Verifies build artifacts are in the default patterns.
#[test]
fn default_patterns_include_build_artifacts() {
    let patterns: Vec<&str> = cvs_default_patterns().collect();

    // Object files
    assert!(patterns.contains(&"*.o"));
    assert!(patterns.contains(&"*.obj"));

    // Libraries
    assert!(patterns.contains(&"*.a"));
    assert!(patterns.contains(&"*.so"));

    // Executables
    assert!(patterns.contains(&"*.exe"));

    // Other library formats
    assert!(patterns.contains(&"*.olb"));
}

/// Verifies editor backup files are in the default patterns.
#[test]
fn default_patterns_include_editor_backups() {
    let patterns: Vec<&str> = cvs_default_patterns().collect();

    // Vim/Emacs backup files
    assert!(patterns.contains(&"*~"));

    // Emacs auto-save
    assert!(patterns.contains(&"#*"));
    assert!(patterns.contains(&".#*"));

    // Backup extensions
    assert!(patterns.contains(&"*.bak"));
    assert!(patterns.contains(&"*.BAK"));
    assert!(patterns.contains(&"*.old"));
    assert!(patterns.contains(&"*.orig"));
    assert!(patterns.contains(&"*.rej"));
}

/// Verifies miscellaneous CVS patterns.
#[test]
fn default_patterns_include_miscellaneous() {
    let patterns: Vec<&str> = cvs_default_patterns().collect();

    // Core dumps
    assert!(patterns.contains(&"core"));

    // Compressed files
    assert!(patterns.contains(&"*.Z"));

    // Emacs compiled Lisp
    assert!(patterns.contains(&"*.elc"));

    // Lint output
    assert!(patterns.contains(&"*.ln"));

    // CVS-specific files
    assert!(patterns.contains(&"CVS.adm"));
    assert!(patterns.contains(&"RCSLOG"));
    assert!(patterns.contains(&"cvslog.*"));

    // Tags files
    assert!(patterns.contains(&"tags"));
    assert!(patterns.contains(&"TAGS"));
}

/// Verifies reasonable number of patterns.
#[test]
fn default_patterns_count() {
    let count = cvs_default_patterns().count();

    // Should have a reasonable number of patterns (30-40 based on upstream)
    assert!(count >= 30, "Expected at least 30 patterns, got {count}");
    assert!(count <= 50, "Expected at most 50 patterns, got {count}");
}

// ============================================================================
// CVS Exclusion Rule Generation Tests
// ============================================================================

/// Verifies cvs_exclusion_rules generates exclude rules.
#[test]
fn cvs_exclusion_rules_generates_excludes() {
    let rules: Vec<FilterRule> = cvs_exclusion_rules(false).collect();

    assert!(!rules.is_empty());

    // All rules should be excludes
    for rule in &rules {
        assert_eq!(rule.action(), filters::FilterAction::Exclude);
    }
}

/// Verifies cvs_exclusion_rules respects perishable flag.
#[test]
fn cvs_exclusion_rules_perishable_flag() {
    // Non-perishable
    let non_perishable: Vec<FilterRule> = cvs_exclusion_rules(false).collect();
    for rule in &non_perishable {
        assert!(!rule.is_perishable());
    }

    // Perishable
    let perishable: Vec<FilterRule> = cvs_exclusion_rules(true).collect();
    for rule in &perishable {
        assert!(rule.is_perishable());
    }
}

/// Verifies cvs_exclusion_rules count matches patterns.
#[test]
fn cvs_exclusion_rules_count_matches_patterns() {
    let pattern_count = cvs_default_patterns().count();
    let rule_count = cvs_exclusion_rules(false).count();

    assert_eq!(pattern_count, rule_count);
}

// ============================================================================
// FilterSet with CVS Exclusion Tests
// ============================================================================

/// Verifies from_rules_with_cvs excludes VCS directories.
#[test]
fn filter_set_cvs_excludes_vcs_directories() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Git directory excluded
    assert!(!set.allows(Path::new(".git"), true));
    assert!(!set.allows(Path::new(".git/config"), false));
    assert!(!set.allows(Path::new(".git/objects/pack"), false));

    // Subversion directory excluded
    assert!(!set.allows(Path::new(".svn"), true));
    assert!(!set.allows(Path::new(".svn/entries"), false));

    // Mercurial directory excluded
    assert!(!set.allows(Path::new(".hg"), true));
    assert!(!set.allows(Path::new(".hg/store"), true));

    // Bazaar directory excluded
    assert!(!set.allows(Path::new(".bzr"), true));

    // CVS excluded
    assert!(!set.allows(Path::new("CVS"), true));
    assert!(!set.allows(Path::new("module/CVS"), true));
}

/// Verifies from_rules_with_cvs excludes object files.
#[test]
fn filter_set_cvs_excludes_object_files() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Object files
    assert!(!set.allows(Path::new("main.o"), false));
    assert!(!set.allows(Path::new("build/lib.o"), false));
    assert!(!set.allows(Path::new("main.obj"), false));

    // Libraries
    assert!(!set.allows(Path::new("libfoo.a"), false));
    assert!(!set.allows(Path::new("libfoo.so"), false));

    // Executables
    assert!(!set.allows(Path::new("program.exe"), false));
}

/// Verifies from_rules_with_cvs excludes backup files.
#[test]
fn filter_set_cvs_excludes_backup_files() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Vim/Emacs backups
    assert!(!set.allows(Path::new("file.txt~"), false));
    assert!(!set.allows(Path::new("#autosave#"), false));
    assert!(!set.allows(Path::new(".#lockfile"), false));

    // Backup extensions
    assert!(!set.allows(Path::new("config.bak"), false));
    assert!(!set.allows(Path::new("CONFIG.BAK"), false));
    assert!(!set.allows(Path::new("old.old"), false));
    assert!(!set.allows(Path::new("patch.orig"), false));
    assert!(!set.allows(Path::new("failed.rej"), false));
}

/// Verifies from_rules_with_cvs allows normal files.
#[test]
fn filter_set_cvs_allows_normal_files() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Source files
    assert!(set.allows(Path::new("main.c"), false));
    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("main.py"), false));
    assert!(set.allows(Path::new("main.js"), false));

    // Config files
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("package.json"), false));
    assert!(set.allows(Path::new("Makefile"), false));

    // Documentation
    assert!(set.allows(Path::new("README.md"), false));
    assert!(set.allows(Path::new("CHANGELOG.txt"), false));

    // Data files
    assert!(set.allows(Path::new("data.json"), false));
    assert!(set.allows(Path::new("config.yaml"), false));
}

// ============================================================================
// Explicit Rules Override CVS Patterns
// ============================================================================

/// Verifies explicit include rules override CVS exclusions.
#[test]
fn explicit_include_overrides_cvs() {
    // With first-match-wins, explicit include comes before CVS excludes
    let rules = vec![FilterRule::include("*.o")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Explicit include takes precedence
    assert!(set.allows(Path::new("main.o"), false));
    assert!(set.allows(Path::new("lib.o"), false));
}

/// Verifies explicit include for specific file overrides CVS.
#[test]
fn explicit_include_specific_overrides_cvs() {
    let rules = vec![FilterRule::include("important.bak")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Specific file included
    assert!(set.allows(Path::new("important.bak"), false));

    // Other bak files still excluded
    assert!(!set.allows(Path::new("scratch.bak"), false));
}

/// Verifies explicit exclude still works with CVS patterns.
#[test]
fn explicit_exclude_works_with_cvs() {
    let rules = vec![FilterRule::exclude("*.txt")];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Explicit exclude works
    assert!(!set.allows(Path::new("notes.txt"), false));

    // CVS patterns still work
    assert!(!set.allows(Path::new("main.o"), false));
    assert!(!set.allows(Path::new(".git"), true));
}

/// Verifies explicit rules interact correctly with CVS patterns.
#[test]
fn complex_explicit_rules_with_cvs() {
    let rules = vec![
        FilterRule::include("src/"),
        FilterRule::include("*.rs"),
        FilterRule::exclude("test_*.rs"),
    ];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Explicit rules work
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("main.rs"), false));
    // test_main.rs: include("*.rs") (rule 2) matches first (first-match-wins)
    // For exclusion to work, exclude must come before include
    assert!(set.allows(Path::new("test_main.rs"), false));

    // CVS patterns still work
    assert!(!set.allows(Path::new("main.o"), false));
    assert!(!set.allows(Path::new(".git/config"), false));
}

// ============================================================================
// Perishable CVS Rules Tests
// ============================================================================

/// Verifies perishable CVS rules affect transfer but not deletion.
#[test]
fn perishable_cvs_rules_transfer_vs_deletion() {
    let set = FilterSet::from_rules_with_cvs(vec![], true).unwrap();

    // Transfer: CVS patterns apply (perishable applies to sender)
    assert!(!set.allows(Path::new("main.o"), false));
    assert!(!set.allows(Path::new("file.bak"), false));

    // Deletion: perishable rules ignored, defaults to allow
    assert!(set.allows_deletion(Path::new("main.o"), false));
    assert!(set.allows_deletion(Path::new("file.bak"), false));
}

/// Verifies non-perishable CVS rules affect both transfer and deletion.
#[test]
fn non_perishable_cvs_rules_transfer_and_deletion() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Transfer: CVS patterns apply
    assert!(!set.allows(Path::new("main.o"), false));

    // Deletion: non-perishable rules also apply
    assert!(!set.allows_deletion(Path::new("main.o"), false));
}

/// Verifies perishable CVS with protect interaction.
#[test]
fn perishable_cvs_with_protect() {
    let rules = vec![FilterRule::protect("important.o")];
    let set = FilterSet::from_rules_with_cvs(rules, true).unwrap();

    // Transfer: excluded by perishable CVS rule
    assert!(!set.allows(Path::new("important.o"), false));

    // Deletion: perishable ignored, but protected
    assert!(!set.allows_deletion(Path::new("important.o"), false));
}

// ============================================================================
// CVS Patterns in Subdirectories
// ============================================================================

/// Verifies CVS patterns match at any depth.
#[test]
fn cvs_patterns_match_at_any_depth() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Object files at various depths
    assert!(!set.allows(Path::new("main.o"), false));
    assert!(!set.allows(Path::new("build/main.o"), false));
    assert!(!set.allows(Path::new("a/b/c/d/main.o"), false));

    // Git directories at various depths
    assert!(!set.allows(Path::new(".git"), true));
    assert!(!set.allows(Path::new("submodule/.git"), true));
    assert!(!set.allows(Path::new("deep/nested/submodule/.git"), true));

    // Backup files at various depths
    assert!(!set.allows(Path::new("file~"), false));
    assert!(!set.allows(Path::new("dir/file~"), false));
    assert!(!set.allows(Path::new("a/b/c/file~"), false));
}

/// Verifies VCS directory contents are excluded.
#[test]
fn vcs_directory_contents_excluded() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Git directory contents
    assert!(!set.allows(Path::new(".git/HEAD"), false));
    assert!(!set.allows(Path::new(".git/objects/pack/pack-abc.idx"), false));
    assert!(!set.allows(Path::new(".git/refs/heads/main"), false));

    // Subversion directory contents
    assert!(!set.allows(Path::new(".svn/wc.db"), false));
    assert!(!set.allows(Path::new(".svn/pristine/ab/abcd.svn-base"), false));
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies case sensitivity of CVS patterns.
#[test]
fn cvs_patterns_case_sensitivity() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // Lowercase patterns
    assert!(!set.allows(Path::new("file.bak"), false));
    assert!(!set.allows(Path::new("file.old"), false));

    // Uppercase patterns
    assert!(!set.allows(Path::new("FILE.BAK"), false));

    // Mixed case - depends on pattern specificity
    // .bak and .BAK are separate patterns
    assert!(!set.allows(Path::new("file.BAK"), false));
}

/// Verifies CVS with empty explicit rules.
#[test]
fn cvs_with_empty_explicit_rules() {
    let set1 = FilterSet::from_rules_with_cvs(vec![], false).unwrap();
    let set2 = FilterSet::from_rules_with_cvs(Vec::new(), false).unwrap();

    // Both should behave identically
    assert!(!set1.allows(Path::new("main.o"), false));
    assert!(!set2.allows(Path::new("main.o"), false));
    assert!(set1.allows(Path::new("main.rs"), false));
    assert!(set2.allows(Path::new("main.rs"), false));
}

/// Verifies CVS patterns don't exclude similarly named files.
#[test]
fn cvs_patterns_precise_matching() {
    let set = FilterSet::from_rules_with_cvs(vec![], false).unwrap();

    // *.o should not exclude .org files
    assert!(set.allows(Path::new("notes.org"), false));

    // core should not exclude Corefile
    assert!(set.allows(Path::new("Corefile"), false));

    // .git/ should not exclude .github
    assert!(set.allows(Path::new(".github"), true));
    assert!(set.allows(Path::new(".gitignore"), false));
    assert!(set.allows(Path::new(".gitattributes"), false));
}

/// Verifies combining CVS with clear rule.
#[test]
fn cvs_with_clear_rule() {
    let rules = vec![
        FilterRule::exclude("*.custom"),
        FilterRule::clear(),
        FilterRule::include("*.txt"),
    ];
    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Clear removes custom rule, CVS rules still apply after clear processing
    assert!(set.allows(Path::new("file.custom"), false)); // Custom rule cleared
    assert!(set.allows(Path::new("notes.txt"), false)); // Include works
    assert!(!set.allows(Path::new("main.o"), false)); // CVS still applies
}
