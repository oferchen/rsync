//! Edge case tests for `FilterChain` covering empty chains, special characters,
//! path traversal patterns, deep nesting, case sensitivity, anchored vs
//! unanchored rule interactions, and directory-only vs file-only semantics.
//!
//! Reference: rsync 3.4.1 exclude.c for filter evaluation semantics.

use std::path::Path;

use filters::{FilterChain, FilterRule, FilterSet};

// ---------------------------------------------------------------------------
// 1. Empty filter chain behavior
// ---------------------------------------------------------------------------

/// An empty chain (no global rules, no scopes) allows every path.
#[test]
fn empty_chain_allows_all_files_and_dirs() {
    let chain = FilterChain::empty();

    assert!(chain.allows(Path::new("anything"), false));
    assert!(chain.allows(Path::new("a/b/c/d.txt"), false));
    assert!(chain.allows(Path::new("dir"), true));
    assert!(chain.allows(Path::new(""), false));
}

/// An empty chain permits deletion of every path.
#[test]
fn empty_chain_allows_all_deletions() {
    let chain = FilterChain::empty();

    assert!(chain.allows_deletion(Path::new("file.txt"), false));
    assert!(chain.allows_deletion(Path::new("nested/dir"), true));
}

/// Pushing an empty scope onto an empty chain changes nothing.
#[test]
fn empty_chain_with_empty_scope_still_allows_all() {
    let mut chain = FilterChain::empty();
    let guard = chain.push_scope(FilterSet::default());

    assert!(chain.allows(Path::new("anything.rs"), false));
    assert!(chain.allows_deletion(Path::new("anything.rs"), false));
    assert_eq!(guard.pushed_count(), 0);

    chain.leave_directory(guard);
    assert!(chain.is_empty());
}

/// Pushing multiple empty scopes does not alter behavior.
#[test]
fn multiple_empty_scopes_allow_all() {
    let mut chain = FilterChain::empty();
    let g1 = chain.push_scope(FilterSet::default());
    let g2 = chain.push_scope(FilterSet::default());
    let g3 = chain.push_scope(FilterSet::default());

    assert!(chain.allows(Path::new("deep/path/file"), false));

    chain.leave_directory(g3);
    chain.leave_directory(g2);
    chain.leave_directory(g1);
    assert!(chain.is_empty());
}

// ---------------------------------------------------------------------------
// 2. Special characters in patterns
// ---------------------------------------------------------------------------

/// Patterns with spaces match filenames containing spaces.
#[test]
fn pattern_with_spaces() {
    let global = FilterSet::from_rules([FilterRule::exclude("my file.txt")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("my file.txt"), false));
    assert!(chain.allows(Path::new("myfile.txt"), false));
}

/// Patterns with Unicode characters match correctly.
#[test]
fn pattern_with_unicode() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.dat")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("\u{00e9}l\u{00e8}ve.dat"), false));
    assert!(chain.allows(Path::new("\u{00e9}l\u{00e8}ve.txt"), false));
}

/// Escaped glob metacharacters are treated as literals.
#[test]
fn pattern_with_escaped_metacharacters() {
    let global = FilterSet::from_rules([FilterRule::exclude("report\\[2024\\].txt")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("report[2024].txt"), false));
    assert!(chain.allows(Path::new("report2024.txt"), false));
}

/// Patterns containing literal hash and semicolon (not comments in patterns).
#[test]
fn pattern_with_hash_and_semicolon() {
    let global = FilterSet::from_rules([
        FilterRule::exclude("C#_project"),
        FilterRule::exclude("config;backup"),
    ])
    .unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("C#_project"), false));
    assert!(!chain.allows(Path::new("config;backup"), false));
}

/// Patterns with consecutive dots in names.
#[test]
fn pattern_with_consecutive_dots() {
    let global = FilterSet::from_rules([FilterRule::exclude("file..bak")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("file..bak"), false));
    assert!(chain.allows(Path::new("file.bak"), false));
}

// ---------------------------------------------------------------------------
// 3. CVE-related path traversal patterns
// ---------------------------------------------------------------------------

/// A filter excluding `../` patterns does not affect normal paths.
#[test]
fn path_traversal_dot_dot_in_exclude_pattern() {
    let global = FilterSet::from_rules([FilterRule::exclude("../*")]).unwrap();
    let chain = FilterChain::new(global);

    // The pattern `../*` is anchored because it contains `/`.
    // It should match paths starting with `../`.
    assert!(!chain.allows(Path::new("../secret"), false));
    assert!(chain.allows(Path::new("normal/file"), false));
}

/// Deeply nested `../../..` traversal components in paths.
#[test]
fn deeply_nested_path_traversal_components() {
    let global = FilterSet::from_rules([FilterRule::exclude("**/../**")]).unwrap();
    let chain = FilterChain::new(global);

    // Paths with `..` components should be matched by the pattern.
    assert!(!chain.allows(Path::new("a/../b"), false));
    assert!(!chain.allows(Path::new("x/y/../../z"), false));
}

/// Excluding literal `..` as a filename component.
#[test]
fn exclude_literal_dot_dot_component() {
    let global = FilterSet::from_rules([FilterRule::exclude("..")]).unwrap();
    let chain = FilterChain::new(global);

    // Unanchored pattern matches `..` at any depth.
    assert!(!chain.allows(Path::new(".."), false));
    assert!(!chain.allows(Path::new("a/b/.."), false));
}

/// Single dot in pattern matches literal `.` filenames.
#[test]
fn single_dot_pattern() {
    let global = FilterSet::from_rules([FilterRule::exclude(".")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("."), false));
    assert!(chain.allows(Path::new("a"), false));
}

// ---------------------------------------------------------------------------
// 4. Deeply nested path matching
// ---------------------------------------------------------------------------

/// Double-star patterns match paths at arbitrary depth.
#[test]
fn double_star_matches_deeply_nested_path() {
    let global = FilterSet::from_rules([FilterRule::exclude("**/*.log")]).unwrap();
    let chain = FilterChain::new(global);

    let deep = Path::new("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t.log");
    assert!(!chain.allows(deep, false));
    assert!(chain.allows(Path::new("a/b/c/d.txt"), false));
}

/// Anchored patterns do not match at arbitrary depth.
#[test]
fn anchored_pattern_does_not_match_deeply_nested() {
    let global = FilterSet::from_rules([FilterRule::exclude("/top.txt")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("top.txt"), false));
    assert!(chain.allows(Path::new("a/top.txt"), false));
    assert!(chain.allows(Path::new("a/b/c/d/e/top.txt"), false));
}

/// Scoped rules at depth N are correctly removed when leaving that level.
#[test]
fn deeply_nested_scope_push_pop_cycle() {
    let mut chain = FilterChain::empty();

    let mut guards = Vec::new();
    for i in 0..20 {
        let rules = FilterSet::from_rules([FilterRule::exclude(format!("level{i}.tmp"))]).unwrap();
        guards.push(chain.push_scope(rules));
    }

    assert_eq!(chain.scope_depth(), 20);
    assert!(!chain.allows(Path::new("level0.tmp"), false));
    assert!(!chain.allows(Path::new("level19.tmp"), false));
    assert!(chain.allows(Path::new("level20.tmp"), false));

    // Pop all in reverse order.
    while let Some(g) = guards.pop() {
        chain.leave_directory(g);
    }

    assert_eq!(chain.scope_depth(), 0);
    assert!(chain.allows(Path::new("level0.tmp"), false));
}

/// Path with many intermediate components matched by double-star exclude.
#[test]
fn intermediate_double_star_pattern() {
    let global = FilterSet::from_rules([FilterRule::exclude("src/**/test/**/*.snap")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("src/foo/test/bar/output.snap"), false));
    assert!(!chain.allows(Path::new("src/a/b/c/test/d/e/f.snap"), false));
    assert!(chain.allows(Path::new("src/foo/bar.snap"), false));
}

// ---------------------------------------------------------------------------
// 5. Case sensitivity edge cases
// ---------------------------------------------------------------------------

/// Pattern matching is case-sensitive by default (upstream rsync behavior).
#[test]
fn case_sensitive_exclude() {
    let global = FilterSet::from_rules([FilterRule::exclude("Makefile")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("Makefile"), false));
    assert!(chain.allows(Path::new("makefile"), false));
    assert!(chain.allows(Path::new("MAKEFILE"), false));
}

/// Wildcard patterns are also case-sensitive.
#[test]
fn case_sensitive_wildcard() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.TXT")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("README.TXT"), false));
    assert!(chain.allows(Path::new("readme.txt"), false));
    assert!(chain.allows(Path::new("README.Txt"), false));
}

/// Character class ranges are case-sensitive.
#[test]
fn case_sensitive_character_class() {
    let global = FilterSet::from_rules([FilterRule::exclude("[A-Z]_config")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("A_config"), false));
    assert!(!chain.allows(Path::new("Z_config"), false));
    assert!(chain.allows(Path::new("a_config"), false));
    assert!(chain.allows(Path::new("z_config"), false));
}

/// Mixed-case extension pattern.
#[test]
fn mixed_case_extension_pattern() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.Jpg")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("photo.Jpg"), false));
    assert!(chain.allows(Path::new("photo.jpg"), false));
    assert!(chain.allows(Path::new("photo.JPG"), false));
}

// ---------------------------------------------------------------------------
// 6. Anchored vs unanchored rule interactions
// ---------------------------------------------------------------------------

/// An anchored exclude followed by an unanchored include - first-match-wins.
#[test]
fn anchored_exclude_then_unanchored_include() {
    let global =
        FilterSet::from_rules([FilterRule::exclude("/build"), FilterRule::include("build")])
            .unwrap();
    let chain = FilterChain::new(global);

    // At root, anchored exclude matches first.
    assert!(!chain.allows(Path::new("build"), false));
    // Nested: anchored rule doesn't match, unanchored include matches.
    assert!(chain.allows(Path::new("sub/build"), false));
}

/// Unanchored exclude matched before anchored include at root.
#[test]
fn unanchored_exclude_before_anchored_include() {
    let global =
        FilterSet::from_rules([FilterRule::exclude("temp"), FilterRule::include("/temp")]).unwrap();
    let chain = FilterChain::new(global);

    // Unanchored exclude comes first - matches everywhere including root.
    assert!(!chain.allows(Path::new("temp"), false));
    assert!(!chain.allows(Path::new("sub/temp"), false));
}

/// An anchored include protects a root path while unanchored exclude removes
/// the same name deeper in the tree (first-match-wins, so include wins).
#[test]
fn anchored_include_before_unanchored_exclude() {
    let global =
        FilterSet::from_rules([FilterRule::include("/keep"), FilterRule::exclude("keep")]).unwrap();
    let chain = FilterChain::new(global);

    // At root, anchored include matches first.
    assert!(chain.allows(Path::new("keep"), false));
    // Nested: anchored include does not match; unanchored exclude matches.
    assert!(!chain.allows(Path::new("sub/keep"), false));
}

/// Patterns containing internal slashes are implicitly anchored.
#[test]
fn internal_slash_implicit_anchor() {
    let global = FilterSet::from_rules([FilterRule::exclude("src/gen")]).unwrap();
    let chain = FilterChain::new(global);

    // Implicitly anchored - matches at root.
    assert!(!chain.allows(Path::new("src/gen"), false));
    // Does not match at arbitrary depth.
    assert!(chain.allows(Path::new("lib/src/gen"), false));
}

/// Scoped exclude overrides global include for the same anchored path.
#[test]
fn scoped_exclude_overrides_global_anchored_include() {
    let global = FilterSet::from_rules([FilterRule::include("/data")]).unwrap();
    let mut chain = FilterChain::new(global);

    let scope = FilterSet::from_rules([FilterRule::exclude("data")]).unwrap();
    let guard = chain.push_scope(scope);

    // Scoped exclude wins because per-directory scopes are checked first.
    assert!(!chain.allows(Path::new("data"), false));

    chain.leave_directory(guard);

    // After leaving scope, global include applies (default is include anyway).
    assert!(chain.allows(Path::new("data"), false));
}

// ---------------------------------------------------------------------------
// 7. Directory-only vs file-only filter rules
// ---------------------------------------------------------------------------

/// A directory-only exclude (trailing `/`) does not match a file.
#[test]
fn directory_only_exclude_does_not_match_file() {
    let global = FilterSet::from_rules([FilterRule::exclude("cache/")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("cache"), true));
    assert!(chain.allows(Path::new("cache"), false));
}

/// A directory-only exclude also excludes contents of matching directories.
#[test]
fn directory_only_exclude_covers_contents() {
    let global = FilterSet::from_rules([FilterRule::exclude("output/")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("output"), true));
    assert!(!chain.allows(Path::new("output/result.bin"), false));
    assert!(!chain.allows(Path::new("output/sub/deep.txt"), false));
}

/// A non-directory-only exclude matches both files and directories.
#[test]
fn non_directory_only_matches_both() {
    let global = FilterSet::from_rules([FilterRule::exclude("temp")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("temp"), false));
    assert!(!chain.allows(Path::new("temp"), true));
}

/// Anchored directory-only pattern only matches the root directory.
#[test]
fn anchored_directory_only_at_root() {
    let global = FilterSet::from_rules([FilterRule::exclude("/logs/")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("logs"), true));
    assert!(chain.allows(Path::new("logs"), false)); // file, not dir
    assert!(chain.allows(Path::new("sub/logs"), true)); // nested dir
}

/// Directory-only include followed by catch-all exclude.
#[test]
fn directory_only_include_then_catch_all_exclude() {
    let global =
        FilterSet::from_rules([FilterRule::include("assets/"), FilterRule::exclude("*")]).unwrap();
    let chain = FilterChain::new(global);

    // Directory named `assets` is included.
    assert!(chain.allows(Path::new("assets"), true));
    // File named `assets` is not covered by the directory-only include,
    // so the catch-all exclude matches.
    assert!(!chain.allows(Path::new("assets"), false));
    // Other files excluded.
    assert!(!chain.allows(Path::new("readme.md"), false));
}

/// Scoped directory-only protect prevents directory deletion.
#[test]
fn scoped_directory_only_protect() {
    let mut chain = FilterChain::empty();
    let scope = FilterSet::from_rules([FilterRule::protect("important/")]).unwrap();
    let guard = chain.push_scope(scope);

    // Directory is protected from deletion.
    assert!(!chain.allows_deletion(Path::new("important"), true));
    // File with the same name is not protected.
    assert!(chain.allows_deletion(Path::new("important"), false));

    chain.leave_directory(guard);

    // After leaving scope, everything is deletable again.
    assert!(chain.allows_deletion(Path::new("important"), true));
}

/// Wildcard directory-only exclude pattern.
#[test]
fn wildcard_directory_only_exclude() {
    let global = FilterSet::from_rules([FilterRule::exclude("__pycache__/")]).unwrap();
    let chain = FilterChain::new(global);

    assert!(!chain.allows(Path::new("__pycache__"), true));
    assert!(!chain.allows(Path::new("pkg/__pycache__"), true));
    assert!(chain.allows(Path::new("__pycache__"), false));
}

// ---------------------------------------------------------------------------
// Interaction: scoped rules with directory-only and anchored patterns combined
// ---------------------------------------------------------------------------

/// Scoped anchored directory-only exclude is removed when scope is popped.
#[test]
fn scoped_anchored_directory_only_lifecycle() {
    let mut chain = FilterChain::empty();

    let scope = FilterSet::from_rules([FilterRule::exclude("/vendor/")]).unwrap();
    let guard = chain.push_scope(scope);

    assert!(!chain.allows(Path::new("vendor"), true));
    assert!(chain.allows(Path::new("vendor"), false));
    assert!(chain.allows(Path::new("sub/vendor"), true));

    chain.leave_directory(guard);

    // All paths allowed after scope removal.
    assert!(chain.allows(Path::new("vendor"), true));
}

/// Multiple scopes with mixed anchored and unanchored, directory-only and
/// generic rules evaluate correctly in innermost-first order.
#[test]
fn multi_scope_mixed_anchor_and_dir_rules() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Outer scope: directory-only exclude for `tmp/`.
    let outer = FilterSet::from_rules([FilterRule::exclude("tmp/")]).unwrap();
    let g_outer = chain.push_scope(outer);

    // Inner scope: anchored exclude for `/config`.
    let inner = FilterSet::from_rules([FilterRule::exclude("/config")]).unwrap();
    let g_inner = chain.push_scope(inner);

    // Inner scope: anchored exclude matches at root.
    assert!(!chain.allows(Path::new("config"), false));
    // Outer scope: directory-only matches dirs named `tmp`.
    assert!(!chain.allows(Path::new("tmp"), true));
    assert!(chain.allows(Path::new("tmp"), false)); // file ok
    // Global: *.bak excluded.
    assert!(!chain.allows(Path::new("data.bak"), false));
    // Unaffected paths.
    assert!(chain.allows(Path::new("README.md"), false));

    chain.leave_directory(g_inner);

    // Inner rule gone, `config` allowed again.
    assert!(chain.allows(Path::new("config"), false));
    // Outer and global still active.
    assert!(!chain.allows(Path::new("tmp"), true));
    assert!(!chain.allows(Path::new("x.bak"), false));

    chain.leave_directory(g_outer);

    // Only global remains.
    assert!(chain.allows(Path::new("tmp"), true));
    assert!(!chain.allows(Path::new("x.bak"), false));
}
