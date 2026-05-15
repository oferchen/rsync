//! Stress test: nested `.rsync-filter` inheritance across deep trees.
//!
//! Exercises [`FilterChain::enter_directory`] / [`FilterChain::leave_directory`]
//! against a five-level directory tree populated with `.rsync-filter` files at
//! multiple depths. Each scope is pushed and popped as the walker descends,
//! mirroring upstream rsync's per-directory merge handling.
//!
//! Reference: upstream rsync 3.4.1 `exclude.c:push_local_filters` /
//! `pop_local_filters`. The reference behavior is documented inline next to
//! each assertion as the upstream source of truth for the expected outcome.

// upstream: exclude.c push_local_filters / pop_local_filters

use std::fs;
use std::path::{Path, PathBuf};

use filters::{DirMergeConfig, FilterChain};
use tempfile::TempDir;

/// Five-level tree layout (paths relative to the transfer root):
///
/// ```text
/// root/                          .rsync-filter:  + /allowed/**
///                                                - *
/// root/allowed/secret            (file)
/// root/allowed/public            (file)
/// root/allowed/nested/           .rsync-filter:  - /allowed/secret
///                                                - notes.txt
/// root/allowed/nested/notes.txt  (file)
/// root/allowed/nested/keep.md    (file)
/// root/allowed/nested/deep/inner/  .rsync-filter: + keep.md
///                                                 - *
/// root/allowed/nested/deep/inner/keep.md (file)
/// root/allowed/nested/deep/inner/drop.md (file)
/// root/elsewhere/                (sibling, excluded by root)
/// ```
fn build_tree() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    fs::write(root.join(".rsync-filter"), "+ /allowed/**\n- *\n").unwrap();

    let allowed = root.join("allowed");
    fs::create_dir(&allowed).unwrap();
    fs::write(allowed.join("secret"), b"shh").unwrap();
    fs::write(allowed.join("public"), b"ok").unwrap();

    let nested = allowed.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(
        nested.join(".rsync-filter"),
        "- /allowed/secret\n- notes.txt\n",
    )
    .unwrap();
    fs::write(nested.join("notes.txt"), b"drop me").unwrap();
    fs::write(nested.join("keep.md"), b"keep me").unwrap();

    let inner = nested.join("deep").join("inner");
    fs::create_dir_all(&inner).unwrap();
    fs::write(inner.join(".rsync-filter"), "+ keep.md\n- *\n").unwrap();
    fs::write(inner.join("keep.md"), b"keep").unwrap();
    fs::write(inner.join("drop.md"), b"drop").unwrap();

    fs::create_dir(root.join("elsewhere")).unwrap();
    fs::write(root.join("elsewhere").join("file"), b"x").unwrap();

    dir
}

/// Push scopes for `root`, then every ancestor between `root` and `subdir`.
/// Returns the guards in pop order so the caller can release them.
fn enter_to(chain: &mut FilterChain, root: &Path, subdir: &Path) -> Vec<filters::DirFilterGuard> {
    let rel = subdir.strip_prefix(root).unwrap();
    let mut current = root.to_path_buf();
    let mut guards = vec![chain.enter_directory(&current).unwrap()];
    for component in rel.components() {
        current.push(component);
        guards.push(chain.enter_directory(&current).unwrap());
    }
    guards
}

fn leave_all(chain: &mut FilterChain, mut guards: Vec<filters::DirFilterGuard>) {
    while let Some(g) = guards.pop() {
        chain.leave_directory(g);
    }
}

fn fresh_chain() -> FilterChain {
    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));
    chain
}

#[test]
fn top_level_include_exclude_propagates() {
    let tree = build_tree();
    let mut chain = fresh_chain();
    let _g = chain.enter_directory(tree.path()).unwrap();

    // `+ /allowed/**` keeps the whitelisted subtree, `- *` drops siblings.
    assert!(chain.allows(Path::new("allowed"), true));
    assert!(chain.allows(Path::new("allowed/public"), false));
    assert!(chain.allows(Path::new("allowed/secret"), false));
    assert!(!chain.allows(Path::new("elsewhere"), true));
    assert!(!chain.allows(Path::new("elsewhere/file"), false));
}

#[test]
fn mid_level_override_drops_parent_decision() {
    let tree = build_tree();
    let root = tree.path();
    let nested = root.join("allowed").join("nested");
    let mut chain = fresh_chain();
    let guards = enter_to(&mut chain, root, &nested);

    // Innermost scope's anchored `- /allowed/secret` beats the inherited
    // `+ /allowed/**` include.
    assert!(!chain.allows(Path::new("allowed/secret"), false));
    // Unanchored `- notes.txt` fires at this depth.
    assert!(!chain.allows(Path::new("allowed/nested/notes.txt"), false));
    // Files not mentioned fall through to the parent include.
    assert!(chain.allows(Path::new("allowed/nested/keep.md"), false));
    assert!(chain.allows(Path::new("allowed/public"), false));

    leave_all(&mut chain, guards);
}

#[test]
fn anchor_style_variants_match_consistently() {
    // Two `.rsync-filter` payloads differing only in anchor style produce
    // identical decisions for the same path.
    for content in ["+ /allowed/**\n- *\n", "+ allowed/**\n- *\n"] {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join(".rsync-filter"), content).unwrap();
        fs::create_dir(root.join("allowed")).unwrap();
        fs::create_dir(root.join("other")).unwrap();

        let mut chain = fresh_chain();
        let _g = chain.enter_directory(root).unwrap();

        assert!(
            chain.allows(Path::new("allowed/file"), false),
            "variant {content:?} should allow allowed/file",
        );
        assert!(
            !chain.allows(Path::new("other/file"), false),
            "variant {content:?} should drop other/file",
        );
    }
}

#[test]
fn deepest_scope_wins_first_match() {
    let tree = build_tree();
    let root = tree.path();
    let inner = root.join("allowed/nested/deep/inner");
    let mut chain = fresh_chain();
    let guards = enter_to(&mut chain, root, &inner);

    // `+ keep.md` matches first; `- *` never fires for that path.
    assert!(chain.allows(Path::new("allowed/nested/deep/inner/keep.md"), false));
    // `drop.md` falls through and is rejected by `- *`.
    assert!(!chain.allows(Path::new("allowed/nested/deep/inner/drop.md"), false));
    // Mid-level scope still suppresses `secret` and `notes.txt`.
    assert!(!chain.allows(Path::new("allowed/secret"), false));
    assert!(!chain.allows(Path::new("allowed/nested/notes.txt"), false));

    leave_all(&mut chain, guards);
}

#[test]
fn leaving_directory_restores_parent_decisions() {
    let tree = build_tree();
    let root = tree.path();
    let inner = root.join("allowed/nested/deep/inner");
    let mut chain = fresh_chain();
    let mut guards = enter_to(&mut chain, root, &inner);

    // Inside `inner`: deepest scope governs.
    assert!(chain.allows(Path::new("allowed/nested/deep/inner/keep.md"), false));
    assert!(!chain.allows(Path::new("allowed/nested/deep/inner/drop.md"), false));

    // Pop `inner` then `deep`: mid-level scope is now innermost.
    chain.leave_directory(guards.pop().unwrap());
    chain.leave_directory(guards.pop().unwrap());
    assert!(chain.allows(Path::new("allowed/nested/deep/inner/keep.md"), false));
    assert!(chain.allows(Path::new("allowed/nested/deep/inner/drop.md"), false));

    // Pop `nested`: only the top-level whitelist remains.
    chain.leave_directory(guards.pop().unwrap());
    assert!(chain.allows(Path::new("allowed/secret"), false));
    assert!(chain.allows(Path::new("allowed/nested/notes.txt"), false));

    leave_all(&mut chain, guards);
    assert_eq!(chain.scope_depth(), 0);
}

#[test]
fn sibling_subtrees_inherit_parent_independently() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(root.join(".rsync-filter"), "- *.log\n").unwrap();
    let left = root.join("left");
    let right = root.join("right");
    fs::create_dir(&left).unwrap();
    fs::create_dir(&right).unwrap();
    fs::write(left.join(".rsync-filter"), "- /left/private\n").unwrap();

    let mut chain = fresh_chain();
    let _root_guard = chain.enter_directory(root).unwrap();

    // `right`: inherits parent unchanged.
    {
        let _g = chain.enter_directory(&right).unwrap();
        assert!(!chain.allows(Path::new("right/error.log"), false));
        assert!(chain.allows(Path::new("right/data.bin"), false));
    }
    // `left`: parent rule plus its own anchored exclude.
    {
        let _g = chain.enter_directory(&left).unwrap();
        assert!(!chain.allows(Path::new("left/error.log"), false));
        assert!(!chain.allows(Path::new("left/private"), false));
        assert!(chain.allows(Path::new("left/public"), false));
    }
}

/// Golden snapshot: lock the full include/exclude decision matrix for the
/// nested tree against upstream rsync 3.4.1's reference behavior. Any change
/// to inheritance or precedence surfaces here before reaching the wire.
#[test]
fn snapshot_decision_matrix() {
    let tree = build_tree();
    let root = tree.path();
    let nested = root.join("allowed/nested");
    let inner = nested.join("deep/inner");

    // (directory walked into, path checked relative to root, is_dir, expected)
    let cases: &[(PathBuf, &str, bool, bool)] = &[
        (root.to_path_buf(), "allowed", true, true),
        (root.to_path_buf(), "allowed/public", false, true),
        (root.to_path_buf(), "elsewhere", true, false),
        (root.to_path_buf(), "elsewhere/file", false, false),
        (nested.clone(), "allowed/secret", false, false),
        (nested.clone(), "allowed/nested/notes.txt", false, false),
        (nested.clone(), "allowed/nested/keep.md", false, true),
        (
            inner.clone(),
            "allowed/nested/deep/inner/keep.md",
            false,
            true,
        ),
        (inner, "allowed/nested/deep/inner/drop.md", false, false),
    ];

    for (subdir, path, is_dir, expected) in cases {
        let mut chain = fresh_chain();
        let guards = enter_to(&mut chain, root, subdir);
        let got = chain.allows(Path::new(path), *is_dir);
        assert_eq!(
            got,
            *expected,
            "subdir={} path={} is_dir={} expected={} got={}",
            subdir.display(),
            path,
            is_dir,
            expected,
            got,
        );
        leave_all(&mut chain, guards);
    }
}
