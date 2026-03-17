//! Property-based tests for filter rule evaluation correctness.
//!
//! These tests verify semantic invariants of the filter matching engine:
//! first-match-wins ordering, anchored pattern behavior, directory-only
//! patterns, wildcard semantics, and empty filter chain defaults.
//! They complement the fuzz tests in `proptest_fuzz.rs` which focus on
//! panic-freedom rather than correctness properties.

use std::path::Path;

use filters::{FilterRule, FilterSet};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// Generates a single path segment without slashes - lowercase letters and digits.
fn path_segment() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop::sample::select(vec![
            'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', '0',
            '1', '2', '3',
        ]),
        1..8,
    )
    .prop_map(|v| v.into_iter().collect::<String>())
}

/// Generates a file extension.
fn file_extension() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "txt".to_owned(),
        "rs".to_owned(),
        "log".to_owned(),
        "tmp".to_owned(),
        "bak".to_owned(),
        "o".to_owned(),
        "c".to_owned(),
        "h".to_owned(),
    ])
}

/// Generates a filename with extension like `foo.txt`.
fn filename_with_ext() -> impl Strategy<Value = String> {
    (path_segment(), file_extension()).prop_map(|(name, ext)| format!("{name}.{ext}"))
}

/// Generates a multi-component relative path like `dir/subdir/file.txt`.
fn relative_path_with_ext() -> impl Strategy<Value = (String, usize)> {
    (
        proptest::collection::vec(path_segment(), 0..4),
        filename_with_ext(),
    )
        .prop_map(|(dirs, file)| {
            let depth = dirs.len();
            if dirs.is_empty() {
                (file, depth)
            } else {
                (format!("{}/{file}", dirs.join("/")), depth)
            }
        })
}

/// Generates a simple relative path (segments joined by `/`).
fn relative_path() -> impl Strategy<Value = String> {
    proptest::collection::vec(path_segment(), 1..5).prop_map(|segs| segs.join("/"))
}

// ---------------------------------------------------------------------------
// Property: Include rules match what they should, exclude rules exclude
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// An exclude rule for `*.EXT` must reject files with that extension and
    /// allow files with a different extension.
    #[test]
    fn exclude_by_extension_rejects_matching(
        ext in file_extension(),
        name in path_segment(),
        dirs in proptest::collection::vec(path_segment(), 0..3),
    ) {
        let set = FilterSet::from_rules([FilterRule::exclude(format!("*.{ext}"))]).unwrap();
        let matching_file = if dirs.is_empty() {
            format!("{name}.{ext}")
        } else {
            format!("{}/{name}.{ext}", dirs.join("/"))
        };
        prop_assert!(
            !set.allows(Path::new(&matching_file), false),
            "exclude *.{} should reject {}", ext, matching_file
        );
    }

    /// An include rule placed before a catch-all exclude must allow matching
    /// files through while the catch-all blocks the rest.
    #[test]
    fn include_before_catch_all_exclude(
        ext in file_extension(),
        name in path_segment(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::include(format!("*.{ext}")),
            FilterRule::exclude("*"),
        ]).unwrap();

        let matching = format!("{name}.{ext}");
        prop_assert!(
            set.allows(Path::new(&matching), false),
            "include *.{} before exclude * should allow {}", ext, matching
        );

        // A file with a guaranteed-different extension must be excluded.
        let other = format!("{name}.{ext}NOPE");
        prop_assert!(
            !set.allows(Path::new(&other), false),
            "exclude * should block {}", other
        );
    }
}

// ---------------------------------------------------------------------------
// Property: First-match-wins semantics
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// When an exclude rule for a specific extension appears before an include
    /// rule for the same extension, the exclude wins (first-match-wins).
    #[test]
    fn first_match_wins_exclude_before_include(
        ext in file_extension(),
        name in path_segment(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("*.{ext}")),
            FilterRule::include(format!("*.{ext}")),
        ]).unwrap();

        let file = format!("{name}.{ext}");
        prop_assert!(
            !set.allows(Path::new(&file), false),
            "exclude before include for *.{} should reject {}", ext, file
        );
    }

    /// When an include rule for a specific extension appears before an exclude
    /// rule for the same extension, the include wins (first-match-wins).
    #[test]
    fn first_match_wins_include_before_exclude(
        ext in file_extension(),
        name in path_segment(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::include(format!("*.{ext}")),
            FilterRule::exclude(format!("*.{ext}")),
        ]).unwrap();

        let file = format!("{name}.{ext}");
        prop_assert!(
            set.allows(Path::new(&file), false),
            "include before exclude for *.{} should allow {}", ext, file
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Anchored patterns only match at root
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// An anchored exclude (`/NAME`) must match at the root but not deeper.
    #[test]
    fn anchored_pattern_matches_root_only(
        name in path_segment(),
        parent in path_segment(),
    ) {
        // Ensure parent and name differ to avoid ambiguity.
        prop_assume!(parent != name);

        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("/{name}")),
        ]).unwrap();

        // Root-level match should be excluded.
        prop_assert!(
            !set.allows(Path::new(&name), false),
            "anchored /{} should exclude root-level {}", name, name
        );

        // Nested match should be allowed (not anchored to root).
        let nested = format!("{parent}/{name}");
        prop_assert!(
            set.allows(Path::new(&nested), false),
            "anchored /{} should NOT exclude nested {}", name, nested
        );
    }

    /// An unanchored exclude (`NAME`) matches at any depth.
    #[test]
    fn unanchored_pattern_matches_any_depth(
        name in path_segment(),
        parent in path_segment(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::exclude(name.clone()),
        ]).unwrap();

        // Root-level match.
        prop_assert!(
            !set.allows(Path::new(&name), false),
            "unanchored {} should exclude root-level", name
        );

        // Nested match.
        let nested = format!("{parent}/{name}");
        prop_assert!(
            !set.allows(Path::new(&nested), false),
            "unanchored {} should also exclude nested {}", name, nested
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Directory-only patterns (trailing `/`)
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// A directory-only pattern (`NAME/`) must match directories but not files
    /// with the same name.
    #[test]
    fn directory_only_pattern_matches_dirs_not_files(name in path_segment()) {
        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("{name}/")),
        ]).unwrap();

        // Directory with that name should be excluded.
        prop_assert!(
            !set.allows(Path::new(&name), true),
            "{}/ should exclude directory {}", name, name
        );

        // File with the same name should be allowed.
        prop_assert!(
            set.allows(Path::new(&name), false),
            "{}/ should NOT exclude file {}", name, name
        );
    }

    /// Descendants of a directory-only exclude are also excluded.
    #[test]
    fn directory_only_excludes_descendants(
        dir_name in path_segment(),
        child in path_segment(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("{dir_name}/")),
        ]).unwrap();

        let descendant = format!("{dir_name}/{child}");
        prop_assert!(
            !set.allows(Path::new(&descendant), false),
            "{}/ should also exclude descendant {}", dir_name, descendant
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Single-star `*` does not cross path separators
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// An anchored pattern `/PREFIX*SUFFIX` with a single `*` should NOT match
    /// a path that has a `/` between prefix and suffix.
    #[test]
    fn single_star_does_not_cross_separator(
        prefix in path_segment(),
        middle in path_segment(),
        suffix in path_segment(),
    ) {
        // Pattern: /PREFIX*SUFFIX matches PREFIXanythingSUFFIX at root only,
        // but the * cannot cross a `/`.
        let pattern = format!("/{prefix}*{suffix}");
        let set = FilterSet::from_rules([FilterRule::exclude(&pattern)]).unwrap();

        // A path with no separator between prefix and suffix should match.
        let flat = format!("{prefix}X{suffix}");
        prop_assert!(
            !set.allows(Path::new(&flat), false),
            "/{}*{} should match flat {}", prefix, suffix, flat
        );

        // A path with a separator between prefix and suffix should NOT match.
        let nested = format!("{prefix}/{middle}/{suffix}");
        prop_assert!(
            set.allows(Path::new(&nested), false),
            "/{}*{} should NOT match across separators: {}", prefix, suffix, nested
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Double-star `**` matches across path separators
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// `**/*.EXT` should match files with that extension at any depth.
    #[test]
    fn double_star_matches_across_separators(
        ext in file_extension(),
        (path, _depth) in relative_path_with_ext(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("**/*.{ext}")),
        ]).unwrap();

        // If the path ends with `.EXT` it should be excluded.
        if path.ends_with(&format!(".{ext}")) {
            prop_assert!(
                !set.allows(Path::new(&path), false),
                "**/*.{} should exclude {}", ext, path
            );
        }
    }

    /// `PREFIX/**` should match all descendants of PREFIX.
    #[test]
    fn double_star_suffix_matches_descendants(
        prefix in path_segment(),
        child_path in relative_path(),
    ) {
        let pattern = format!("{prefix}/**");
        let set = FilterSet::from_rules([FilterRule::exclude(&pattern)]).unwrap();

        let full = format!("{prefix}/{child_path}");
        prop_assert!(
            !set.allows(Path::new(&full), false),
            "{} should exclude descendant {}", pattern, full
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Empty filter chain includes everything
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// An empty FilterSet (no rules) should allow every path.
    #[test]
    fn empty_filter_set_includes_all(
        path in relative_path(),
        is_dir in any::<bool>(),
    ) {
        let set = FilterSet::default();
        prop_assert!(set.is_empty());
        prop_assert!(
            set.allows(Path::new(&path), is_dir),
            "empty filter set should allow {} (is_dir={})", path, is_dir
        );
    }

    /// An empty FilterSet also allows all deletions.
    #[test]
    fn empty_filter_set_allows_all_deletions(
        path in relative_path(),
        is_dir in any::<bool>(),
    ) {
        let set = FilterSet::default();
        prop_assert!(
            set.allows_deletion(Path::new(&path), is_dir),
            "empty filter set should allow deletion of {}", path
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Order sensitivity - swapping include/exclude changes results
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// For a file that matches both an include and exclude pattern, swapping
    /// the rule order must flip the outcome (first-match-wins).
    #[test]
    fn swapping_rule_order_changes_result(
        ext in file_extension(),
        name in path_segment(),
    ) {
        let inc = FilterRule::include(format!("*.{ext}"));
        let exc = FilterRule::exclude(format!("*.{ext}"));
        let file = format!("{name}.{ext}");

        let set_inc_first = FilterSet::from_rules([inc.clone(), exc.clone()]).unwrap();
        let set_exc_first = FilterSet::from_rules([exc, inc]).unwrap();

        let result_inc_first = set_inc_first.allows(Path::new(&file), false);
        let result_exc_first = set_exc_first.allows(Path::new(&file), false);

        prop_assert_ne!(
            result_inc_first, result_exc_first,
            "swapping include/exclude order for *.{} should flip result for {}", ext, file
        );
    }

    /// Adding an earlier matching rule overrides a later one.
    #[test]
    fn prepended_rule_overrides_later(
        ext in file_extension(),
        name in path_segment(),
    ) {
        let file = format!("{name}.{ext}");

        // Base: exclude *.EXT
        let base = FilterSet::from_rules([
            FilterRule::exclude(format!("*.{ext}")),
        ]).unwrap();
        prop_assert!(!base.allows(Path::new(&file), false));

        // Prepend include *.EXT - should override
        let overridden = FilterSet::from_rules([
            FilterRule::include(format!("*.{ext}")),
            FilterRule::exclude(format!("*.{ext}")),
        ]).unwrap();
        prop_assert!(
            overridden.allows(Path::new(&file), false),
            "prepended include should override later exclude for {}", file
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Catch-all exclude blocks everything, catch-all include is no-op
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// A single `exclude("*")` rule should reject every file.
    #[test]
    fn catch_all_exclude_blocks_all_files(
        path in relative_path(),
    ) {
        let set = FilterSet::from_rules([FilterRule::exclude("*")]).unwrap();
        prop_assert!(
            !set.allows(Path::new(&path), false),
            "exclude * should block {}", path
        );
    }

    /// A single `include("*")` rule (without any exclude) still allows
    /// everything because the default is already include.
    #[test]
    fn include_star_alone_is_permissive(
        path in relative_path(),
        is_dir in any::<bool>(),
    ) {
        let set = FilterSet::from_rules([FilterRule::include("*")]).unwrap();
        prop_assert!(
            set.allows(Path::new(&path), is_dir),
            "include * alone should still allow {}", path
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Anchored + directory-only combined
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// `/NAME/` (anchored + directory-only) should exclude only the root-level
    /// directory with that name, not a nested directory or a file.
    #[test]
    fn anchored_directory_only(
        name in path_segment(),
        parent in path_segment(),
    ) {
        prop_assume!(parent != name);

        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("/{name}/")),
        ]).unwrap();

        // Root dir with that name: excluded
        prop_assert!(
            !set.allows(Path::new(&name), true),
            "/{}/ should exclude root dir {}", name, name
        );

        // Root file with that name: allowed (directory-only)
        prop_assert!(
            set.allows(Path::new(&name), false),
            "/{}/ should NOT exclude root file {}", name, name
        );

        // Nested dir with that name: allowed (anchored)
        let nested = format!("{parent}/{name}");
        prop_assert!(
            set.allows(Path::new(&nested), true),
            "/{}/ should NOT exclude nested dir {}", name, nested
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Clear rule resets the chain
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// A clear rule followed by no further rules should include everything,
    /// regardless of what rules preceded the clear.
    #[test]
    fn clear_rule_resets_to_default(
        ext in file_extension(),
        name in path_segment(),
    ) {
        let file = format!("{name}.{ext}");
        let set = FilterSet::from_rules([
            FilterRule::exclude(format!("*.{ext}")),
            FilterRule::clear(),
        ]).unwrap();

        prop_assert!(
            set.allows(Path::new(&file), false),
            "after clear, {} should be allowed", file
        );
    }

    /// Rules after a clear take effect as if the chain started fresh.
    #[test]
    fn rules_after_clear_take_effect(
        ext in file_extension(),
        name in path_segment(),
    ) {
        let file = format!("{name}.{ext}");

        // Include *.EXT, then clear, then exclude *.EXT.
        // The include is cleared; only the exclude remains.
        let set = FilterSet::from_rules([
            FilterRule::include(format!("*.{ext}")),
            FilterRule::clear(),
            FilterRule::exclude(format!("*.{ext}")),
        ]).unwrap();

        prop_assert!(
            !set.allows(Path::new(&file), false),
            "exclude after clear should block {}", file
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Protect rules block deletion
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// A protect rule for a path prevents deletion even when the path is
    /// included by transfer rules.
    #[test]
    fn protect_prevents_deletion(
        name in path_segment(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::protect(format!("/{name}")),
        ]).unwrap();

        // Transfer is allowed (no include/exclude rules block it).
        prop_assert!(set.allows(Path::new(&name), false));

        // Deletion is blocked by protect.
        prop_assert!(
            !set.allows_deletion(Path::new(&name), false),
            "protect /{} should block deletion of {}", name, name
        );
    }

    /// A risk rule after a protect re-allows deletion (first-match-wins in
    /// the protect/risk list).
    #[test]
    fn risk_overrides_protect(
        name in path_segment(),
    ) {
        let set = FilterSet::from_rules([
            FilterRule::risk(format!("/{name}")),
            FilterRule::protect(format!("/{name}")),
        ]).unwrap();

        // Risk matches first, so deletion is allowed.
        prop_assert!(
            set.allows_deletion(Path::new(&name), false),
            "risk before protect should allow deletion of {}", name
        );
    }
}

// ---------------------------------------------------------------------------
// Property: Internal slash implies anchoring
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// A pattern containing an internal `/` (e.g., `dir/file`) is implicitly
    /// anchored - it matches from the transfer root, not at any depth.
    #[test]
    fn internal_slash_implies_anchored(
        dir in path_segment(),
        file in path_segment(),
        outer in path_segment(),
    ) {
        prop_assume!(outer != dir);

        let pattern = format!("{dir}/{file}");
        let set = FilterSet::from_rules([FilterRule::exclude(&pattern)]).unwrap();

        // Direct path should be excluded (matches from root).
        prop_assert!(
            !set.allows(Path::new(&pattern), false),
            "{} should exclude {} at root", pattern, pattern
        );

        // Nested under a different parent should be allowed (anchored).
        let nested = format!("{outer}/{dir}/{file}");
        prop_assert!(
            set.allows(Path::new(&nested), false),
            "{} (internal slash) should NOT exclude {}", pattern, nested
        );
    }
}
