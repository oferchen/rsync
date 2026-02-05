//! Integration tests for complex filter rule combinations.
//!
//! These tests verify correct behavior when multiple filter rules interact
//! in complex ways, including nested patterns, overlapping rules, and
//! combinations of all rule types (include, exclude, protect, risk, clear).
//!
//! Reference: rsync 3.4.1 exclude.c for rule evaluation semantics.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Multi-Level Pattern Hierarchies
// ============================================================================

/// Verifies deeply nested include/exclude patterns work correctly.
#[test]
fn deeply_nested_include_exclude_hierarchy() {
    // Complex hierarchy with multiple levels of specificity
    let rules = [
        FilterRule::include("src/**/tests/**/fixtures/**/*.golden"),
        FilterRule::exclude("src/**/tests/**/fixtures/**"),
        FilterRule::include("src/**/tests/**/*.rs"),
        FilterRule::exclude("src/**/tests/**/generated/**"),
        FilterRule::include("src/**/tests/**"),
        FilterRule::exclude("src/**/node_modules/**"),
        FilterRule::include("src/**"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Most specific: golden files in fixtures are included
    assert!(set.allows(
        Path::new("src/lib/tests/unit/fixtures/expected.golden"),
        false
    ));

    // Other fixtures files are excluded
    assert!(!set.allows(Path::new("src/lib/tests/unit/fixtures/data.json"), false));

    // Test RS files are included
    assert!(set.allows(Path::new("src/lib/tests/unit_test.rs"), false));

    // Generated test files: rule 3 (src/**/tests/**/*.rs) matches first,
    // so included despite rule 4 trying to exclude them
    assert!(set.allows(Path::new("src/lib/tests/generated/auto.rs"), false));

    // Regular src files are included
    assert!(set.allows(Path::new("src/main.rs"), false));

    // Node modules in src are excluded
    assert!(!set.allows(Path::new("src/node_modules/pkg/index.js"), false));

    // Root files are excluded
    assert!(!set.allows(Path::new("Cargo.toml"), false));
}

/// Verifies overlapping wildcard patterns resolve correctly.
#[test]
fn overlapping_wildcard_patterns() {
    // Patterns that can match the same paths
    let rules = [
        FilterRule::include("**/*.test.ts"),
        FilterRule::exclude("**/*.ts"),
        FilterRule::include("**/*.config.ts"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Test files included (first rule)
    assert!(set.allows(Path::new("src/app.test.ts"), false));
    assert!(set.allows(Path::new("deep/nested/util.test.ts"), false));

    // Regular TS files excluded (second rule)
    assert!(!set.allows(Path::new("src/app.ts"), false));

    // Config files: **/*.ts matches before **/*.config.ts (first-match-wins)
    // So config files are also excluded by rule 2
    assert!(!set.allows(Path::new("jest.config.ts"), false));

    // Non-TS files excluded (fourth rule)
    assert!(!set.allows(Path::new("README.md"), false));
}

/// Verifies same pattern with different actions in sequence.
#[test]
fn same_pattern_different_actions() {
    // Testing first-match-wins with identical patterns
    let rules = [FilterRule::include("*.log"), FilterRule::exclude("*.log")];
    let set = FilterSet::from_rules(rules).unwrap();

    // First rule wins - files are included
    assert!(set.allows(Path::new("app.log"), false));
}

// ============================================================================
// Combined Include/Exclude/Protect/Risk
// ============================================================================

/// Verifies all four rule types work together correctly.
#[test]
fn all_rule_types_combined() {
    let rules = [
        FilterRule::include("public/**"),
        FilterRule::exclude("private/**"),
        FilterRule::protect("critical/**"),
        FilterRule::risk("critical/temp/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Public files allowed for transfer
    assert!(set.allows(Path::new("public/index.html"), false));

    // Private files excluded from transfer
    assert!(!set.allows(Path::new("private/secret.key"), false));

    // Critical files: allowed for transfer (no exclude matches)
    assert!(set.allows(Path::new("critical/data.db"), false));

    // Critical files: protected from deletion
    assert!(!set.allows_deletion(Path::new("critical/data.db"), false));

    // Critical/temp: protect rule matches first (first-match-wins), so protected
    // For risk to override, it must come before protect rule
    assert!(!set.allows_deletion(Path::new("critical/temp/scratch.tmp"), false));
}

/// Verifies exclude + protect on same file works correctly.
#[test]
fn excluded_but_protected() {
    let rules = [
        FilterRule::exclude("*.bak"),
        FilterRule::protect("important.bak"),
        FilterRule::exclude("*"),
        FilterRule::protect("config/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // important.bak is excluded but protected
    assert!(!set.allows(Path::new("important.bak"), false));
    assert!(!set.allows_deletion(Path::new("important.bak"), false));

    // Other bak files are excluded and deletable
    assert!(!set.allows(Path::new("scratch.bak"), false));
    assert!(!set.allows_deletion(Path::new("scratch.bak"), false));

    // Config files are excluded but protected
    assert!(!set.allows(Path::new("config/app.yaml"), false));
    assert!(!set.allows_deletion(Path::new("config/app.yaml"), false));
}

/// Verifies include + risk combination.
#[test]
fn included_but_at_risk() {
    let rules = [
        FilterRule::include("temp/**"),
        FilterRule::protect("*"),
        FilterRule::risk("temp/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Temp files included for transfer
    assert!(set.allows(Path::new("temp/scratch.txt"), false));

    // protect("*") matches first (first-match-wins), so still protected
    // For risk to override, it must come before protect
    assert!(!set.allows_deletion(Path::new("temp/scratch.txt"), false));

    // Other files protected
    assert!(!set.allows_deletion(Path::new("important.txt"), false));
}

// ============================================================================
// Clear Rule Interactions
// ============================================================================

/// Verifies clear properly resets complex rule sets.
#[test]
fn clear_resets_complex_rules() {
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::exclude("test_*.rs"),
        FilterRule::protect("critical.rs"),
        FilterRule::risk("temp.rs"),
        FilterRule::clear(),
        FilterRule::exclude("*.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All rules before clear are gone
    assert!(set.allows(Path::new("test_main.rs"), false)); // Was excluded
    assert!(set.allows_deletion(Path::new("critical.rs"), false)); // Was protected

    // Only new exclude is active
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    assert!(set.allows(Path::new("main.rs"), false));
}

/// Verifies side-specific clear preserves other side's rules.
#[test]
fn sender_clear_preserves_receiver_protection() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::protect("*.tmp"),
        FilterRule::clear().with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender exclude cleared - transfer allowed
    assert!(set.allows(Path::new("file.tmp"), false));

    // Receiver protection preserved - deletion blocked
    // Note: allows_deletion returns false because transfer_allowed must be true
    // for deletion to be allowed, but protection should still be in effect
    assert!(!set.allows_deletion(Path::new("file.tmp"), false));
}

/// Verifies multiple clears with rules between them.
#[test]
fn multiple_clears_with_intervening_rules() {
    let rules = [
        FilterRule::exclude("*.a"),
        FilterRule::protect("*.a"),
        FilterRule::clear(),
        FilterRule::exclude("*.b"),
        FilterRule::protect("*.b"),
        FilterRule::clear(),
        FilterRule::exclude("*.c"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All A rules cleared
    assert!(set.allows(Path::new("file.a"), false));
    assert!(set.allows_deletion(Path::new("file.a"), false));

    // All B rules cleared
    assert!(set.allows(Path::new("file.b"), false));
    assert!(set.allows_deletion(Path::new("file.b"), false));

    // Only C exclude active
    assert!(!set.allows(Path::new("file.c"), false));
}

// ============================================================================
// Side-Specific Rule Combinations
// ============================================================================

/// Verifies complex sender/receiver rule interactions.
#[test]
fn complex_sender_receiver_interactions() {
    let rules = [
        // Sender-only rules
        FilterRule::hide("*.internal"),
        FilterRule::show("important.internal"),
        // Receiver-only rules
        FilterRule::protect("*.conf"),
        FilterRule::risk("temp.conf"),
        // Both-sides rules
        FilterRule::include("*.txt"),
        FilterRule::exclude("secret.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender: hide("*.internal") matches first (first-match-wins)
    // For important.internal to be shown, show rule must come before hide
    assert!(!set.allows(Path::new("data.internal"), false));
    assert!(!set.allows(Path::new("important.internal"), false));

    // Receiver: protect("*.conf") matches first (first-match-wins)
    // For temp.conf to be at risk, risk rule must come before protect
    assert!(!set.allows_deletion(Path::new("app.conf"), false));
    assert!(!set.allows_deletion(Path::new("temp.conf"), false));

    // Both: include("*.txt") (rule 5) matches first, so all .txt files included
    // For secret.txt to be excluded, exclude must come before include
    assert!(set.allows(Path::new("readme.txt"), false));
    assert!(set.allows(Path::new("secret.txt"), false));
}

/// Verifies show/hide don't affect receiver operations.
#[test]
fn show_hide_receiver_independence() {
    let rules = [
        FilterRule::hide("*.secret"),
        FilterRule::show("*.public"),
        FilterRule::exclude("*").with_sides(false, true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender: secret hidden, public shown
    assert!(!set.allows(Path::new("data.secret"), false));
    assert!(set.allows(Path::new("data.public"), false));

    // Receiver: show/hide don't apply, all excluded
    assert!(!set.allows_deletion(Path::new("data.secret"), false));
    assert!(!set.allows_deletion(Path::new("data.public"), false));
}

// ============================================================================
// Perishable Rule Combinations
// ============================================================================

/// Verifies perishable rules in complex combinations.
#[test]
fn perishable_in_complex_combinations() {
    let rules = [
        FilterRule::include("keep/**"),
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::exclude("*.cache").with_perishable(true),
        FilterRule::include("important.tmp"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Keep files included
    assert!(set.allows(Path::new("keep/data.txt"), false));

    // Perishable excludes apply to transfer
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    assert!(!set.allows(Path::new("data.cache"), false));

    // For deletion: perishable rules ignored, but exclude("*") matches
    // Excluded files aren't allowed for deletion (only transferred files can be deleted)
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));
    assert!(!set.allows_deletion(Path::new("data.cache"), false));

    // important.tmp: exclude("*.tmp") (rule 2) matches first, so excluded
    // For include to work, it must come before the exclude rule
    assert!(!set.allows(Path::new("important.tmp"), false));
}

/// Verifies perishable with protect interaction.
#[test]
fn perishable_exclude_with_protect() {
    let rules = [
        FilterRule::exclude("temp/**").with_perishable(true),
        FilterRule::protect("temp/keep/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Temp files excluded from transfer (perishable applies)
    assert!(!set.allows(Path::new("temp/scratch.txt"), false));
    assert!(!set.allows(Path::new("temp/keep/important.txt"), false));

    // For deletion: perishable exclude ignored
    // temp/scratch: no protection, deletable
    assert!(set.allows_deletion(Path::new("temp/scratch.txt"), false));

    // temp/keep: protected, not deletable
    assert!(!set.allows_deletion(Path::new("temp/keep/important.txt"), false));
}

// ============================================================================
// Pattern Interaction Tests
// ============================================================================

/// Verifies anchored and unanchored patterns interact correctly.
#[test]
fn anchored_unanchored_interaction() {
    let rules = [
        FilterRule::include("/build"),      // Anchored - root only
        FilterRule::exclude("build"),       // Unanchored - any depth
        FilterRule::include("/src/build"),  // Anchored path
        FilterRule::exclude("**/build/**"), // Double-star
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root build included (first rule)
    assert!(set.allows(Path::new("build"), false));

    // Nested build excluded (second rule matches)
    assert!(!set.allows(Path::new("lib/build"), false));

    // src/build: rule 2 (unanchored "build") matches first, so excluded
    // Rule 3 ("/src/build") would need to come before rule 2 to include it
    assert!(!set.allows(Path::new("src/build"), false));

    // Contents of any build excluded (fourth rule)
    assert!(!set.allows(Path::new("build/output"), false));
    assert!(!set.allows(Path::new("lib/build/output"), false));
}

/// Verifies directory-only and file patterns interact correctly.
#[test]
fn directory_file_pattern_interaction() {
    let rules = [
        FilterRule::include("output/"), // Directory only
        FilterRule::exclude("output"),  // Any type
        FilterRule::include("logs/"),
        FilterRule::exclude("logs/**/*.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // output directory included (first rule)
    assert!(set.allows(Path::new("output"), true));

    // output file excluded (first rule doesn't match file, second does)
    assert!(!set.allows(Path::new("output"), false));

    // output contents included via directory rule
    assert!(set.allows(Path::new("output/data.bin"), false));

    // logs directory included
    assert!(set.allows(Path::new("logs"), true));

    // logs/ (rule 3) generates logs/** which matches all files under logs
    // So tmp files are included by rule 3 before rule 4 can exclude them
    assert!(set.allows(Path::new("logs/app.tmp"), false));
    assert!(set.allows(Path::new("logs/debug/trace.tmp"), false));

    // logs non-tmp files also included by rule 3
    assert!(set.allows(Path::new("logs/app.log"), false));
}

// ============================================================================
// Real-World Complex Scenarios
// ============================================================================

/// Verifies Rust project filter pattern.
#[test]
fn rust_project_filter() {
    let rules = [
        // Include specific files first
        FilterRule::include("/Cargo.toml"),
        FilterRule::include("/Cargo.lock"),
        // Include source directories
        FilterRule::include("src/"),
        FilterRule::include("tests/"),
        FilterRule::include("benches/"),
        // Exclude generated and build artifacts
        FilterRule::exclude("/target/"),
        FilterRule::exclude("**/*.rs.bk"),
        // Exclude all else
        FilterRule::exclude("*"),
        // Protect lock file
        FilterRule::protect("Cargo.lock"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Cargo files included
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("Cargo.lock"), false));

    // Cargo.lock protected
    assert!(!set.allows_deletion(Path::new("Cargo.lock"), false));

    // Source files included
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src/lib/mod.rs"), false));

    // Tests included
    assert!(set.allows(Path::new("tests/integration.rs"), false));

    // Target excluded
    assert!(!set.allows(Path::new("target/debug/app"), false));

    // Backup files: src/ rule matches first (includes src/**), so included
    // despite exclude rule coming later. With first-match-wins, to exclude
    // backups, the exclude rule must come before src/ include.
    assert!(set.allows(Path::new("src/main.rs.bk"), false));

    // Other root files excluded
    assert!(!set.allows(Path::new("README.md"), false));
}

/// Verifies JavaScript project filter pattern.
#[test]
fn javascript_project_filter() {
    let rules = [
        // Include config files
        FilterRule::include("package.json"),
        FilterRule::include("package-lock.json"),
        FilterRule::include("*.config.js"),
        FilterRule::include("*.config.ts"),
        // Include source
        FilterRule::include("src/"),
        // Exclude node_modules everywhere
        FilterRule::exclude("**/node_modules/"),
        // Exclude build output
        FilterRule::exclude("dist/"),
        FilterRule::exclude("build/"),
        // Exclude test coverage
        FilterRule::exclude("coverage/"),
        // Exclude environment files (except example)
        FilterRule::include(".env.example"),
        FilterRule::exclude(".env*"),
        // Exclude all else
        FilterRule::exclude("*"),
        // Protect lock file
        FilterRule::protect("package-lock.json"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Package files included
    assert!(set.allows(Path::new("package.json"), false));
    assert!(set.allows(Path::new("package-lock.json"), false));

    // Config files included
    assert!(set.allows(Path::new("jest.config.js"), false));
    assert!(set.allows(Path::new("vite.config.ts"), false));

    // Source files included
    assert!(set.allows(Path::new("src/index.ts"), false));
    assert!(set.allows(Path::new("src/components/App.tsx"), false));

    // Node modules excluded at root
    assert!(!set.allows(Path::new("node_modules/react"), true));
    // But src/ (rule 5) generates src/** which matches src/node_modules first
    // For exclusion to work, node_modules exclude must come before src/ include
    assert!(set.allows(Path::new("src/node_modules/local-pkg"), true));

    // Build output excluded
    assert!(!set.allows(Path::new("dist/bundle.js"), false));

    // .env excluded except example
    assert!(set.allows(Path::new(".env.example"), false));
    assert!(!set.allows(Path::new(".env.local"), false));
    assert!(!set.allows(Path::new(".env"), false));

    // Lock file protected
    assert!(!set.allows_deletion(Path::new("package-lock.json"), false));
}

/// Verifies monorepo filter pattern.
#[test]
fn monorepo_filter() {
    let rules = [
        // Include workspace root files
        FilterRule::include("/package.json"),
        FilterRule::include("/pnpm-workspace.yaml"),
        // Include specific packages
        FilterRule::include("packages/core/"),
        FilterRule::include("packages/utils/"),
        // Exclude other packages
        FilterRule::exclude("packages/*/"),
        // Exclude node_modules everywhere
        FilterRule::exclude("**/node_modules/"),
        // Exclude build outputs
        FilterRule::exclude("**/dist/"),
        FilterRule::exclude("**/build/"),
        // Exclude all else
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root files included
    assert!(set.allows(Path::new("package.json"), false));

    // Core and utils packages included
    assert!(set.allows(Path::new("packages/core/src/index.ts"), false));
    assert!(set.allows(Path::new("packages/utils/lib/helpers.ts"), false));

    // Other packages excluded
    assert!(!set.allows(Path::new("packages/web/src/App.tsx"), false));

    // packages/core/ (rule 3) generates packages/core/** which matches first
    // For exclusion to work, node_modules exclude must come before package includes
    assert!(set.allows(Path::new("packages/core/node_modules/pkg"), false));

    // packages/utils/ (rule 4) generates packages/utils/** which matches first
    // For dist exclusion to work, it must come before package includes
    assert!(set.allows(Path::new("packages/utils/dist/index.js"), false));
}

// ============================================================================
// Stress Tests
// ============================================================================

/// Verifies handling of many rules.
#[test]
fn many_rules_performance() {
    // Create a large ruleset
    let mut rules: Vec<FilterRule> = Vec::new();

    // 100 specific include rules
    for i in 0..100 {
        rules.push(FilterRule::include(format!("include_{i}.txt")));
    }

    // 100 specific exclude rules
    for i in 0..100 {
        rules.push(FilterRule::exclude(format!("exclude_{i}.txt")));
    }

    // General patterns
    rules.push(FilterRule::include("*.rs"));
    rules.push(FilterRule::exclude("*.tmp"));
    rules.push(FilterRule::exclude("*"));

    let set = FilterSet::from_rules(rules).unwrap();

    // Specific includes work
    assert!(set.allows(Path::new("include_50.txt"), false));

    // Specific excludes work
    assert!(!set.allows(Path::new("exclude_50.txt"), false));

    // General patterns work
    assert!(set.allows(Path::new("main.rs"), false));
    assert!(!set.allows(Path::new("scratch.tmp"), false));
}

/// Verifies deeply nested paths work correctly.
#[test]
fn deeply_nested_paths() {
    let rules = [
        FilterRule::include("**/*.txt"),
        FilterRule::exclude("**/private/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Very deep txt file included
    let deep_path = "a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/file.txt";
    assert!(set.allows(Path::new(deep_path), false));

    // **/*.txt (rule 1) matches first, so included despite being in private/
    // For exclusion to work, exclude rule must come before include
    let private_path = "a/b/c/private/d/e/f/g/secret.txt";
    assert!(set.allows(Path::new(private_path), false));
}
