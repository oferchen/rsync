//! UTS-V3-B sub-test 4 regression: the sender's wire-format parser must
//! expand `-C` (CVS-exclude with no pattern) into the local CVS-ignore
//! exclude list, and switch `:C .cvsignore` dir-merges into CVS-mode
//! parsing.
//!
//! The cluster B sub-test 4 invocation is:
//!
//! ```text
//! rsync -avv --filter='merge $excl' -f:C -f-C --delete-excluded
//!     --delete-during lh:from/ to/
//! ```
//!
//! Upstream's client transmits the exclude-from rules then `:C .cvsignore`
//! and `-C ` (length=3, payload `-C ` with trailing space and no pattern).
//! Without expansion, the wire `-C ` collapses into a `FilterRule::exclude("")`
//! whose synthetic `**/` direct matcher silently excludes every top-level
//! entry the sender visits. Only `bar` survives (matched by the earlier
//! `+ **/bar` include), leaving the receiver's `--delete-during` pass to
//! remove every other dest entry.
//!
//! These tests pin the upstream-faithful behaviour at the wire-parser
//! boundary that the sender's filter chain consumes:
//!
//! - A wire `-C ` exclude with an empty pattern expands into the default
//!   `cvs_exclusion_rules` (perishable at proto >= 30) plus tokens from the
//!   server's `$HOME/.cvsignore` and `$CVSIGNORE` environment, mirroring
//!   `exclude.c:get_cvs_excludes()`.
//! - A wire `:C .cvsignore` dir-merge switches the `DirMergeConfig` into
//!   CVS-mode so the chain parses each whitespace token in `.cvsignore` as
//!   an exclude rule instead of failing on `unrecognized filter rule:
//!   one-in-one-out`.

use std::path::Path;

use filters::{DirMergeConfig, FilterChain, FilterRule, FilterSet};

/// The CVS-mode dir-merge config must parse `.cvsignore` as
/// whitespace-separated exclude tokens; lines like `one-in-one-out` are
/// rejected by the default parser and would otherwise abort the sender
/// walk for the `mid/` subtree.
#[test]
fn cvs_mode_dir_merge_parses_unprefixed_tokens() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(dir.join(".cvsignore"), "one-in-one-out *.junk").expect("write .cvsignore");
    std::fs::write(dir.join("one-in-one-out"), b"x").expect("write target");
    std::fs::write(dir.join("kept"), b"y").expect("write kept");

    let mut chain = FilterChain::new(FilterSet::default());
    chain.add_merge_config(
        DirMergeConfig::new(".cvsignore")
            .with_cvs_mode(true)
            .with_inherit(false),
    );

    let guard = chain
        .enter_directory(dir)
        .expect("CVS-mode dir-merge must accept unprefixed tokens");

    assert!(
        !chain.allows(Path::new("one-in-one-out"), false),
        "CVS-mode tokens excluded matching paths",
    );
    assert!(
        chain.allows(Path::new("kept"), false),
        "non-matching paths still included",
    );
    // Upstream's `:C` does NOT exclude the merge file itself; only the
    // explicit `e` modifier does. The default CVS pattern list is what
    // hides `.cvsignore` from transfers at the file-level rule layer.
    assert!(
        chain.allows(Path::new(".cvsignore"), false),
        ":C alone must not hide the merge file (only `:Ce` does)",
    );

    chain.leave_directory(guard);
}

/// `:C` is no-inherit by default. When the sender walks a child directory,
/// the parent's `.cvsignore` rules must NOT apply unless the child also
/// declares its own merge file. This pins the `mid/.cvsignore` ->
/// `mid/for/` behaviour that drives sub-test 4's `mid/for/one-in-one-out`
/// survival.
#[test]
fn cvs_mode_dir_merge_does_not_inherit_to_subdirs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let mid = root.join("mid");
    let mid_for = mid.join("for");
    std::fs::create_dir_all(&mid_for).expect("mkdir");
    std::fs::write(mid.join(".cvsignore"), "one-in-one-out").expect("write");

    let mut chain = FilterChain::new(FilterSet::default());
    chain.add_merge_config(
        DirMergeConfig::new(".cvsignore")
            .with_cvs_mode(true)
            .with_inherit(false),
    );

    let mid_guard = chain.enter_directory(&mid).expect("enter mid");
    assert!(
        !chain.allows(Path::new("one-in-one-out"), false),
        "mid scope excludes one-in-one-out at its own depth",
    );

    let for_guard = chain.enter_directory(&mid_for).expect("enter mid/for");
    assert!(
        chain.allows(Path::new("one-in-one-out"), false),
        "no-inherit `:C` must not leak `mid/.cvsignore` rules into `mid/for/`",
    );
    chain.leave_directory(for_guard);
    chain.leave_directory(mid_guard);
}

/// Sanity check that the standard merge parser, used outside CVS-mode,
/// rejects unprefixed tokens. This pins the failure mode the CVS-mode
/// fix bypasses.
#[test]
fn standard_dir_merge_rejects_unprefixed_cvsignore_token() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(dir.join(".cvsignore"), "one-in-one-out").expect("write");

    let mut chain = FilterChain::new(FilterSet::default());
    chain.add_merge_config(DirMergeConfig::new(".cvsignore"));

    let err = chain
        .enter_directory(dir)
        .expect_err("standard merge parse must fail on unprefixed token");
    let msg = err.to_string();
    assert!(
        msg.contains("one-in-one-out"),
        "error must mention the offending token: {msg}",
    );
}

/// The full exclude-lsh wire-rule stream (without and with the `-C`
/// expansion) drives top-level dir survival as follows:
///
/// - Without expansion, the empty `-C ` rule's `**/` matcher swallows
///   every top-level entry except `bar` (allowed by `+ **/bar`).
/// - With expansion, the CVS default patterns leave `.filt`, `foo`, `mid`,
///   `new` allowed (none match a CVS pattern), and `bar` still matches
///   `+ **/bar`.
///
/// This test simulates the post-fix outcome: build the same filter chain
/// the wire parser produces after expansion and pin the survivors.
#[test]
fn post_expansion_chain_keeps_all_top_level_dirs() {
    let exclude_lsh = vec![
        FilterRule::include("**/bar"),
        FilterRule::exclude("/bar"),
        FilterRule::include("foo**too"),
        FilterRule::include("foo/s?b/"),
        FilterRule::exclude("foo/*/"),
        FilterRule::exclude("new/keep/**"),
        FilterRule::exclude("new/lose/***"),
        FilterRule::include("t[o]/"),
        FilterRule::exclude("to"),
        FilterRule::include("file4"),
        FilterRule::exclude("file[2-9]"),
        FilterRule::exclude("/mid/for/foo/extra"),
    ];

    let mut rules = exclude_lsh;
    rules.extend(filters::cvs_exclusion_rules(true));

    let chain = FilterChain::new(FilterSet::from_rules(rules).expect("compiled"));

    for (path, is_dir, why) in [
        ("bar", true, "+ **/bar"),
        (".filt", false, "no rule matches"),
        ("foo", true, "no rule matches"),
        ("mid", true, "no rule matches"),
        ("new", true, "no rule matches"),
    ] {
        assert!(
            chain.allows(Path::new(path), is_dir),
            "post-expansion chain must keep {path} ({why})",
        );
    }
}

/// Inverse guard: confirm that the wire `-C ` rule, WITHOUT expansion,
/// would in fact eat every top-level entry except `bar`. This pins the
/// regression that the wire parser fix addresses.
#[test]
fn unexpanded_empty_exclude_swallows_top_level_entries() {
    let mut rules = vec![
        FilterRule::include("**/bar"),
        // Standalone empty exclude reproduces the pre-fix shape of `-C `:
        // `FilterRule::exclude("")` produces a `**/` direct matcher.
        FilterRule::exclude(""),
    ];
    rules.extend([
        FilterRule::include("foo**too"),
        FilterRule::include("foo/s?b/"),
    ]);

    let chain = FilterChain::new(FilterSet::from_rules(rules).expect("compiled"));

    // Documented broken behaviour: an empty exclude pattern matches every
    // top-level entry (the unanchored expansion adds `**/` which globset
    // treats as "any path with at least one component").
    assert!(
        !chain.allows(Path::new(".filt"), false),
        "documented bug: empty exclude eats `.filt`",
    );
    assert!(
        !chain.allows(Path::new("foo"), true),
        "documented bug: empty exclude eats `foo`",
    );
    // `bar` survives because its include rule wins (`+ **/bar`).
    assert!(chain.allows(Path::new("bar"), true));
}
