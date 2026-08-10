use super::scope::DirScope;
use super::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn dir_merge_config_defaults() {
    let config = DirMergeConfig::new(".rsync-filter");
    assert_eq!(config.filename(), ".rsync-filter");
    assert!(config.inherits());
    assert!(!config.excludes_self());
}

#[test]
fn dir_merge_config_no_inherit() {
    let config = DirMergeConfig::new(".rsync-filter").with_inherit(false);
    assert!(!config.inherits());
}

#[test]
fn dir_merge_config_exclude_self() {
    let config = DirMergeConfig::new(".rsync-filter").with_exclude_self(true);
    assert!(config.excludes_self());
}

#[test]
fn dir_merge_config_sender_only() {
    let config = DirMergeConfig::new(".rsync-filter").with_sender_only(true);
    let rule = config.apply_modifiers(FilterRule::exclude("*.tmp"));
    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn dir_merge_config_receiver_only() {
    let config = DirMergeConfig::new(".rsync-filter").with_receiver_only(true);
    let rule = config.apply_modifiers(FilterRule::exclude("*.tmp"));
    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn dir_merge_config_anchor_root() {
    let config = DirMergeConfig::new(".rsync-filter").with_anchor_root(true);
    let rule = config.apply_modifiers(FilterRule::exclude("test"));
    assert_eq!(rule.pattern(), "/test");
}

#[test]
fn dir_merge_config_perishable() {
    let config = DirMergeConfig::new(".rsync-filter").with_perishable(true);
    let rule = config.apply_modifiers(FilterRule::exclude("*.tmp"));
    assert!(rule.is_perishable());
}

#[test]
fn dir_merge_config_clone() {
    let config = DirMergeConfig::new(".rsync-filter")
        .with_inherit(false)
        .with_exclude_self(true);
    let cloned = config.clone();
    assert_eq!(cloned.filename(), ".rsync-filter");
    assert!(!cloned.inherits());
    assert!(cloned.excludes_self());
}

#[test]
fn filter_chain_empty() {
    let chain = FilterChain::empty();
    assert!(chain.is_empty());
    assert_eq!(chain.scope_depth(), 0);
    assert_eq!(chain.current_depth(), 0);
}

#[test]
fn filter_chain_with_global_rules() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
    let chain = FilterChain::new(global);
    assert!(!chain.is_empty());
    assert!(!chain.allows(Path::new("file.bak"), false));
    assert!(chain.allows(Path::new("file.txt"), false));
}

#[test]
fn filter_chain_global_deletion() {
    let global = FilterSet::from_rules([FilterRule::protect("/important")]).unwrap();
    let chain = FilterChain::new(global);
    assert!(!chain.allows_deletion(Path::new("important"), false));
    assert!(chain.allows_deletion(Path::new("other"), false));
}

#[test]
fn filter_chain_push_scope_override() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();
    let mut chain = FilterChain::new(global);

    let dir_rules = FilterSet::from_rules([FilterRule::include("*.log")]).unwrap();
    let guard = chain.push_scope(dir_rules);

    // has_matching_rule returns false for includes, so we fall through to the
    // global rules. Per-directory scopes only stop the lookup when they contain
    // a matching exclude; pure-include scopes need a paired exclude rule.
    assert_eq!(guard.pushed_count(), 1);

    chain.leave_directory(guard);
    assert_eq!(chain.scope_depth(), 0);
}

#[test]
fn filter_chain_push_scope_exclude_overrides_global_include() {
    let global = FilterSet::from_rules([FilterRule::include("*.txt")]).unwrap();
    let mut chain = FilterChain::new(global);

    let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();
    let guard = chain.push_scope(dir_rules);

    assert!(!chain.allows(Path::new("file.txt"), false));

    chain.leave_directory(guard);

    assert!(chain.allows(Path::new("file.txt"), false));
}

#[test]
fn filter_chain_nested_scopes() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
    let mut chain = FilterChain::new(global);

    let outer = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();
    let guard_outer = chain.push_scope(outer);
    assert_eq!(chain.scope_depth(), 1);

    let inner = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let guard_inner = chain.push_scope(inner);
    assert_eq!(chain.scope_depth(), 2);

    assert!(!chain.allows(Path::new("file.bak"), false));
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(!chain.allows(Path::new("file.tmp"), false));
    assert!(chain.allows(Path::new("file.txt"), false));

    chain.leave_directory(guard_inner);
    assert_eq!(chain.scope_depth(), 1);

    assert!(!chain.allows(Path::new("file.bak"), false));
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));

    chain.leave_directory(guard_outer);
    assert_eq!(chain.scope_depth(), 0);

    assert!(!chain.allows(Path::new("file.bak"), false));
    assert!(chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));
}

#[test]
fn filter_chain_enter_directory_reads_merge_file() {
    let dir = TempDir::new().unwrap();
    let filter_content = "- *.tmp\n- *.log\n";
    fs::write(dir.path().join(".rsync-filter"), filter_content).unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    assert!(!chain.allows(Path::new("file.tmp"), false));
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.txt"), false));

    chain.leave_directory(guard);
    assert!(chain.allows(Path::new("file.tmp"), false));
}

#[test]
fn filter_chain_enter_directory_no_merge_file() {
    let dir = TempDir::new().unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 0);

    assert!(chain.allows(Path::new("file.tmp"), false));

    chain.leave_directory(guard);
}

#[test]
fn filter_chain_enter_directory_empty_merge_file() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".rsync-filter"), "").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 0);

    chain.leave_directory(guard);
}

#[test]
fn filter_chain_enter_directory_comments_only() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join(".rsync-filter"),
        "# This is a comment\n; Another comment\n\n",
    )
    .unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 0);

    chain.leave_directory(guard);
}

#[test]
fn filter_chain_enter_directory_exclude_self() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".rsync-filter"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter").with_exclude_self(true));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    assert!(!chain.allows(Path::new(".rsync-filter"), false));
    assert!(!chain.allows(Path::new("file.tmp"), false));

    chain.leave_directory(guard);
}

#[test]
fn filter_chain_enter_directory_with_include_rules() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".rsync-filter"), "+ *.important\n- *\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard = chain.enter_directory(dir.path()).unwrap();

    assert!(chain.allows(Path::new("file.important"), false));
    assert!(!chain.allows(Path::new("file.txt"), false));

    chain.leave_directory(guard);
}

#[test]
fn filter_chain_nested_directories_with_merge_files() {
    let dir = TempDir::new().unwrap();

    let outer = dir.path().join("outer");
    fs::create_dir(&outer).unwrap();
    fs::write(outer.join(".rsync-filter"), "- *.log\n").unwrap();

    let inner = outer.join("inner");
    fs::create_dir(&inner).unwrap();
    fs::write(inner.join(".rsync-filter"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard_outer = chain.enter_directory(&outer).unwrap();
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));

    let guard_inner = chain.enter_directory(&inner).unwrap();
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(!chain.allows(Path::new("file.tmp"), false));

    chain.leave_directory(guard_inner);
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));

    chain.leave_directory(guard_outer);
    assert!(chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));
}

#[test]
fn filter_chain_multiple_merge_configs() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".rsync-filter"), "- *.log\n").unwrap();
    fs::write(dir.path().join(".exclude"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));
    chain.add_merge_config(DirMergeConfig::new(".exclude"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 2);

    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(!chain.allows(Path::new("file.tmp"), false));
    assert!(chain.allows(Path::new("file.txt"), false));

    chain.leave_directory(guard);
}

#[test]
fn filter_chain_parse_error_in_merge_file() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".rsync-filter"), "invalid_directive\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let result = chain.enter_directory(dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("parse"));
}

#[test]
fn filter_chain_modifier_application() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".rsync-filter"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter").with_perishable(true));

    let guard = chain.enter_directory(dir.path()).unwrap();

    // perishable does not affect allows(); only delete-excluded processing.
    assert!(!chain.allows(Path::new("file.tmp"), false));

    chain.leave_directory(guard);
}

#[test]
fn dir_filter_guard_depth() {
    let global = FilterSet::default();
    let mut chain = FilterChain::new(global);
    let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let guard = chain.push_scope(dir_rules);
    assert_eq!(guard.depth(), 1);
    chain.leave_directory(guard);
}

#[test]
fn dir_filter_guard_pushed_count_empty() {
    let mut chain = FilterChain::empty();
    let guard = chain.push_scope(FilterSet::default());
    assert_eq!(guard.pushed_count(), 0);
    chain.leave_directory(guard);
}

#[test]
fn filter_chain_error_display_io() {
    let err = FilterChainError::Io {
        path: PathBuf::from("/test/.rsync-filter"),
        source: io::Error::other("disk error"),
    };
    let display = err.to_string();
    assert!(display.contains("/test/.rsync-filter"));
    assert!(display.contains("disk error"));
}

#[test]
fn filter_chain_error_display_parse() {
    let err = FilterChainError::Parse {
        path: PathBuf::from("/test/.rsync-filter"),
        message: "bad syntax".to_owned(),
    };
    let display = err.to_string();
    assert!(display.contains("/test/.rsync-filter"));
    assert!(display.contains("bad syntax"));
}

#[test]
fn filter_chain_error_source() {
    let err = FilterChainError::Io {
        path: PathBuf::from("/test"),
        source: io::Error::new(io::ErrorKind::NotFound, "not found"),
    };
    assert!(std::error::Error::source(&err).is_some());

    let err2 = FilterChainError::Parse {
        path: PathBuf::from("/test"),
        message: "bad".to_owned(),
    };
    assert!(std::error::Error::source(&err2).is_none());
}

#[test]
fn filter_chain_scope_push_pop_symmetry() {
    let mut chain = FilterChain::empty();

    for i in 0..5 {
        let rules = FilterSet::from_rules([FilterRule::exclude(format!("*.ext{i}"))]).unwrap();
        let _guard = chain.push_scope(rules);
    }

    assert_eq!(chain.scope_depth(), 5);

    chain.scopes.clear();
    chain.current_depth = 0;
    assert_eq!(chain.scope_depth(), 0);
}

#[test]
fn filter_chain_default_allows_everything() {
    let chain = FilterChain::empty();
    assert!(chain.allows(Path::new("any/path/here.txt"), false));
    assert!(chain.allows(Path::new("directory"), true));
    assert!(chain.allows_deletion(Path::new("anything"), false));
}

#[test]
fn filter_chain_global_rules_persist_across_scopes() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
    let mut chain = FilterChain::new(global);

    for _ in 0..3 {
        let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let guard = chain.push_scope(dir_rules);
        assert!(!chain.allows(Path::new("file.bak"), false));
        chain.leave_directory(guard);
    }

    assert!(!chain.allows(Path::new("file.bak"), false));
}

#[test]
fn filter_chain_protect_in_scope() {
    let mut chain = FilterChain::empty();
    let dir_rules = FilterSet::from_rules([FilterRule::protect("/important")]).unwrap();
    let guard = chain.push_scope(dir_rules);

    assert!(!chain.allows_deletion(Path::new("important"), false));
    assert!(chain.allows_deletion(Path::new("other"), false));

    chain.leave_directory(guard);
    assert!(chain.allows_deletion(Path::new("important"), false));
}

/// `:C` inside a per-directory merge file must register a CVS-style
/// `.cvsignore` for the current directory only, loading its whitespace
/// tokens as exclude rules. Without this, the `:C` rule is silently dropped
/// by [`FilterSet`] compilation and the named file is never consulted -
/// the failing path that drives the upstream `exclude` / `exclude-lsh`
/// testsuite checks.
///
/// upstream: exclude.c:parse_rule_tok() (rsync-3.4.3) - the `:C` token
/// expands to `:s n,e+ .cvsignore` (no-prefix, word-split, no-inherit,
/// FILTRULE_CVS_IGNORE), and exclude.c:1419-1428 registers the resulting
/// per-dir merge so the named file is read at the current scope.
#[test]
fn dir_merge_inline_colon_c_loads_cvsignore_no_inherit() {
    let parent = TempDir::new().unwrap();
    fs::create_dir(parent.path().join("child")).unwrap();
    fs::write(parent.path().join(".filt"), ":C\n").unwrap();
    fs::write(parent.path().join(".cvsignore"), "one-in-one-out\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".filt"));

    let parent_guard = chain.enter_directory(parent.path()).unwrap();
    assert!(parent_guard.pushed_count() >= 1, "expected `:C` scope push");

    // The `.cvsignore` token excludes `one-in-one-out` inside the parent dir.
    assert!(!chain.allows(Path::new("one-in-one-out"), false));
    assert!(chain.allows(Path::new("other-file"), false));

    // upstream `:C` is FILTRULE_NO_INHERIT: descending into a child
    // directory must not carry the parent's `.cvsignore` rules along.
    let child_guard = chain.enter_directory(&parent.path().join("child")).unwrap();
    assert!(
        chain.allows(Path::new("one-in-one-out"), false),
        "no-inherit `:C` must not propagate parent rules into descendants"
    );
    chain.leave_directory(child_guard);

    chain.leave_directory(parent_guard);
    assert!(chain.allows(Path::new("one-in-one-out"), false));
}

/// `:C` inside a per-directory merge file must be a no-op when the
/// referenced `.cvsignore` is absent, matching upstream's silent skip
/// for missing per-dir merge files.
///
/// upstream: exclude.c:push_local_filters() - missing files are skipped
/// without error.
#[test]
fn dir_merge_inline_colon_c_missing_cvsignore_is_noop() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".filt"), ":C\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".filt"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 0, "missing `.cvsignore` is silent");
    assert!(chain.allows(Path::new("anything"), false));

    chain.leave_directory(guard);
}

/// A leading-`/` rule read from a per-directory merge file is anchored at
/// the merge file's own directory, not the transfer root. With the
/// transfer root recorded, `- /file1` inside `<root>/foo/.filt` must match
/// `foo/file1` and leave a top-level `file1` untouched.
///
/// upstream: exclude.c:200-228 add_rule under XFLG_ANCHORED2ABS prepends
/// the merge directory (relative to the module root) to a leading-`/`
/// pattern. oc-rsync expresses root anchoring as a leading `/`, so the
/// rewrite is `/file1` -> `/foo/file1`.
#[test]
fn dir_merge_leading_slash_rule_reanchors_to_merge_dir() {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("foo")).unwrap();
    fs::write(root.path().join("foo/.filt"), "- /file1\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".filt"));
    chain.set_transfer_root(root.path());

    let root_guard = chain.enter_directory(root.path()).unwrap();
    let foo_guard = chain.enter_directory(&root.path().join("foo")).unwrap();
    assert_eq!(
        foo_guard.pushed_count(),
        1,
        "expected `foo/.filt` scope push"
    );

    // `- /file1` from `foo/.filt` re-anchors to `/foo/file1`.
    assert!(
        !chain.allows(Path::new("foo/file1"), false),
        "re-anchored rule must exclude foo/file1"
    );
    // A top-level `file1` is at the module root, not below `foo`, so the
    // re-anchored rule must not touch it.
    assert!(
        chain.allows(Path::new("file1"), false),
        "re-anchored rule must not exclude top-level file1"
    );

    chain.leave_directory(foo_guard);
    chain.leave_directory(root_guard);
}

/// A `dir-merge` directive parsed from inside a per-directory merge file
/// becomes a persistent per-directory config that is re-read in every
/// descendant directory, not a one-shot read of the declaring directory.
/// Sibling subtrees each load their own copy of the named file.
///
/// upstream: exclude.c:294 appends a `dir-merge` parsed from a merge file
/// to the global `mergelist_parents`, so `push_local_filters()` re-reads
/// it at every directory below the declaration.
#[test]
fn dir_merge_nested_directive_inherits_into_every_descendant() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("bar/d1")).unwrap();
    fs::create_dir_all(root.path().join("bar/d2")).unwrap();
    fs::write(root.path().join("bar/.filt"), "dir-merge .filt2\n").unwrap();
    fs::write(root.path().join("bar/d1/.filt2"), "- a.deep\n").unwrap();
    fs::write(root.path().join("bar/d2/.filt2"), "- b.deep\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".filt"));
    chain.set_transfer_root(root.path());

    let root_guard = chain.enter_directory(root.path()).unwrap();
    // Reading `bar/.filt` registers the nested `dir-merge .filt2`.
    let bar_guard = chain.enter_directory(&root.path().join("bar")).unwrap();

    // First descendant re-reads its own `.filt2`.
    let d1_guard = chain.enter_directory(&root.path().join("bar/d1")).unwrap();
    assert!(
        !chain.allows(Path::new("bar/d1/a.deep"), false),
        "bar/d1/.filt2 must exclude a.deep"
    );
    assert!(
        chain.allows(Path::new("bar/d1/b.deep"), false),
        "b.deep is only named in the sibling d2/.filt2"
    );
    chain.leave_directory(d1_guard);

    // Sibling descendant re-reads its own `.filt2`; without persistent
    // registration the nested dir-merge would have been one-shot at `bar`
    // and this exclusion would never fire.
    let d2_guard = chain.enter_directory(&root.path().join("bar/d2")).unwrap();
    assert!(
        !chain.allows(Path::new("bar/d2/b.deep"), false),
        "bar/d2/.filt2 must be re-read and exclude b.deep"
    );
    chain.leave_directory(d2_guard);

    chain.leave_directory(bar_guard);
    chain.leave_directory(root_guard);
}

/// An inner scope's anchored exclude (`- /foo`) synthesizes a
/// `foo/**` descendant matcher in the compiled rule. When the outer
/// scope's `- down` rule is the only rule that *actually* applies to
/// `foo/down`, the inner scope must NOT short-circuit fall-through
/// merely because its descendant matcher pretends to match.
///
/// Upstream `exclude.c:rule_matches()` has no descendant matching at
/// all; descendant exclusion is a side effect of the sender walk not
/// descending into excluded directories. The per-directory chain must
/// reflect that and fall through whenever a scope contains no real
/// user-written rule for the path.
///
/// Pre-fix behaviour: the inner scope's `foo/**` descendant matcher
/// would advertise a deletion-side match in `has_matching_rule`,
/// causing the chain to consult the inner scope's traversal-mode
/// `allows_during_traversal` (which skips descendants and so says
/// include) and short-circuit before the outer `- down` rule fired,
/// leaking `foo/down` into the destination.
#[test]
fn filter_chain_inner_scope_descendant_does_not_block_outer_fall_through() {
    // Outer (global) rule that excludes any path with basename `down`.
    let global = FilterSet::from_rules([FilterRule::exclude("down")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Inner scope with `- /foo`: direct matcher is `foo`, descendant
    // matcher is the synthetic `foo/**`. Only the descendant matcher
    // touches `foo/down`, and it must not stop fall-through.
    let inner = FilterSet::from_rules([FilterRule::exclude("/foo")]).unwrap();
    let _guard = chain.push_scope(inner);

    assert!(!chain.allows(Path::new("foo/down"), true));
    assert!(!chain.allows(Path::new("foo/down"), false));
}

/// Root-level filter chain with `exclude down` must exclude every
/// directory whose basename is `down`, regardless of nesting depth and
/// regardless of whether an intervening per-directory scope contributes
/// a rule. Mirrors the upstream `exclude` testsuite expectation that a
/// root-anchored exclude propagates through `.filt` scopes that are
/// silent on the same path.
#[test]
fn filter_chain_root_exclude_down_propagates_to_nested_dirs() {
    let global = FilterSet::from_rules([FilterRule::exclude("down")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Simulate descent into `foo/` with a per-dir scope that says nothing
    // about `down`.
    let foo_scope =
        FilterSet::from_rules([FilterRule::include(".filt"), FilterRule::exclude("/file1")])
            .unwrap();
    let foo_guard = chain.push_scope(foo_scope);
    assert!(!chain.allows(Path::new("foo/down"), true));
    chain.leave_directory(foo_guard);

    // Simulate descent into `bar/down/to/foo/` with several intervening
    // per-dir scopes that are silent on `down`.
    let bar_scope = FilterSet::from_rules([
        FilterRule::exclude("home-cvs-exclude"),
        FilterRule::include("to"),
    ])
    .unwrap();
    let bar_guard = chain.push_scope(bar_scope);

    let bar_down_to_scope = FilterSet::from_rules([FilterRule::exclude(".filt2")]).unwrap();
    let bar_down_to_guard = chain.push_scope(bar_down_to_scope);

    let bar_down_to_foo_scope = FilterSet::from_rules([FilterRule::include("*.junk")]).unwrap();
    let bar_down_to_foo_guard = chain.push_scope(bar_down_to_foo_scope);

    // Even though none of the per-dir scopes match `down`, the outer
    // `exclude down` rule must still apply.
    assert!(!chain.allows(Path::new("bar/down"), true));
    assert!(!chain.allows(Path::new("foo/down"), true));

    chain.leave_directory(bar_down_to_foo_guard);
    chain.leave_directory(bar_down_to_guard);
    chain.leave_directory(bar_guard);
}

/// A non-inheriting (`:C`-style) scope at depth N must not block outer
/// inherited rules from applying at depth N. When the non-inheriting
/// scope has no rule matching the path, evaluation must fall through to
/// outer inherited scopes. Mirrors upstream's recursive
/// `check_filter()` which returns 0 when no rule matches in the
/// mergelist, letting the caller continue iterating the outer list.
#[test]
fn filter_chain_non_inheriting_scope_falls_through_to_outer_inherited() {
    let global = FilterSet::from_rules([FilterRule::exclude("down")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Outer inheriting scope (depth 1) with an unrelated rule.
    let outer = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
    let outer_guard = chain.push_scope(outer);

    // Inner non-inheriting scope (depth 2) silent on `down`.
    let inner_set = FilterSet::from_rules([FilterRule::exclude("*.junk")]).unwrap();
    chain.current_depth += 1;
    let inner_depth = chain.current_depth;
    chain.scopes.push(DirScope {
        depth: inner_depth,
        filter_set: inner_set,
        inherits: false,
    });

    // At depth 2 with a no-inherit inner scope that does not match
    // `down`, the outer scope (depth 1) is also silent, and global
    // `exclude down` must apply.
    assert!(!chain.allows(Path::new("down"), true));
    // Sanity: rules from each layer still fire on their own patterns.
    assert!(!chain.allows(Path::new("file.junk"), false));
    assert!(!chain.allows(Path::new("file.bak"), false));

    // Pop the inner non-inheriting scope manually.
    chain.scopes.retain(|s| s.depth != inner_depth);
    chain.current_depth -= 1;

    chain.leave_directory(outer_guard);
}

/// The Deletion-side scope fall-through uses the same descendant-free
/// match predicate as the Transfer side. Without this, a scope that
/// looks "silent" on a path via direct matchers would still claim a
/// match via a synthetic `pattern/**` descendant matcher in the
/// deletion code path, breaking outer-scope evaluation.
#[test]
fn filter_chain_deletion_falls_through_when_only_descendant_matches() {
    // Outer (global) rule protects `foo/x` from deletion via a real
    // protect rule that fires on the path's basename.
    let global = FilterSet::from_rules([FilterRule::protect("x")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Inner scope's `- /foo` synthesizes a `foo/**` descendant matcher.
    // Direct matchers are silent on `foo/x`. With the fix the scope is
    // detected as silent (no real rule match) and evaluation falls
    // through to the outer protect rule.
    let inner = FilterSet::from_rules([FilterRule::exclude("/foo")]).unwrap();
    let _guard = chain.push_scope(inner);

    assert!(!chain.allows_deletion(Path::new("foo/x"), false));
}

/// A per-directory scope whose predicate match is decided by an include
/// rule must not be overridden in the deletion commit by an earlier
/// exclude rule's synthetic descendant matcher. The bug was that the
/// chain's commit step in [`FilterChain::allows_deletion`] called
/// `FilterSet::allows_deletion`, which re-enables synthetic
/// `pattern/**` descendant matchers. An earlier `- /bar` rule then
/// fired its `bar/**` synthetic against `bar/x` and beat the later
/// include rule that the descendant-free predicate had selected.
///
/// Mirrors upstream `exclude.c:rule_matches()`, which has NO descendant
/// matching: descendant exclusion is a side effect of the sender walk
/// not descending into excluded directories, never a rule match in its
/// own right. The receiver-side deletion commit through a per-dir
/// scope must therefore honour the same descendant-free semantics.
#[test]
fn filter_chain_per_dir_deletion_does_not_block_via_synthetic_descendant() {
    let global = FilterSet::default();
    let mut chain = FilterChain::new(global);

    // Scope rule order matters: the anchored `- /bar` is first, so
    // its synthetic `bar/**` descendant would beat the later include
    // if descendants were active during the scope commit.
    let scope =
        FilterSet::from_rules([FilterRule::exclude("/bar"), FilterRule::include("x")]).unwrap();
    let _guard = chain.push_scope(scope);

    // Descendant-free predicate sees `+ x` match (via `**/x`), so the
    // chain routes the deletion decision through this scope. The
    // commit must agree: with descendants suppressed, `+ x` is still
    // the first matching rule and `bar/x` is deletable.
    assert!(
        chain.allows_deletion(Path::new("bar/x"), false),
        "scope commit must not let synthetic `bar/**` override the include rule"
    );
}

// upstream: exclude.c:1116-1133 parse_rule_tok - when FILTRULE_NO_PREFIXES is
// set on the template, every per-dir merge line is consumed as a literal
// pattern. `+ foo`, `- bar`, `include baz` become LITERAL excludes - not
// include/exclude rules - so paths matching those literal strings are blocked.
#[test]
fn dir_merge_no_prefixes_minus_literal_excludes() {
    let dir = TempDir::new().unwrap();
    let filter_content = "+ foo\n- bar\ninclude baz\n";
    fs::write(dir.path().join(".filt"), filter_content).unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".filt").with_no_prefixes(true, false));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    // Each literal line excludes the EXACT verbatim pattern, so a file
    // named "foo" (without `+ ` prefix) is NOT excluded by `+ foo`.
    assert!(chain.allows(Path::new("foo"), false));
    assert!(chain.allows(Path::new("bar"), false));
    assert!(chain.allows(Path::new("baz"), false));

    // But a file whose pattern matches the literal IS excluded. Patterns
    // containing spaces match path components by name; rsync's matcher
    // treats them as literal globs.
    assert!(!chain.allows(Path::new("+ foo"), false));
    assert!(!chain.allows(Path::new("- bar"), false));
    assert!(!chain.allows(Path::new("include baz"), false));

    chain.leave_directory(guard);
}

// upstream: exclude.c:1116-1133 with FILTRULE_INCLUDE - the `+` variant of
// the no-prefixes modifier emits literal include rules instead of excludes.
#[test]
fn dir_merge_no_prefixes_plus_literal_includes() {
    let dir = TempDir::new().unwrap();
    let filter_content = "+ foo\n- bar\ninclude baz\n";
    fs::write(dir.path().join(".filt"), filter_content).unwrap();

    let global = FilterSet::from_rules([FilterRule::exclude("*")]).unwrap();
    let mut chain = FilterChain::new(global);
    chain.add_merge_config(DirMergeConfig::new(".filt").with_no_prefixes(true, true));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    // Literal include lines beat the global `- *` for files matching the
    // literal verbatim pattern.
    assert!(chain.allows(Path::new("+ foo"), false));
    assert!(chain.allows(Path::new("- bar"), false));
    assert!(chain.allows(Path::new("include baz"), false));

    // Files NOT matching the literals are still excluded by the global rule.
    assert!(!chain.allows(Path::new("other"), false));

    chain.leave_directory(guard);
}

// upstream: exclude.c:1123-1124 - without FILTRULE_CVS_IGNORE the bare `!`
// line is just another literal pattern (no FILTRULE_CLEAR_LIST escape).
#[test]
fn dir_merge_no_prefixes_bang_is_literal_without_cvs() {
    let dir = TempDir::new().unwrap();
    let filter_content = "- foo\n!\n- bar\n";
    fs::write(dir.path().join(".filt"), filter_content).unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".filt").with_no_prefixes(true, false));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    // List was NOT cleared - both literal excludes still bind, AND `!`
    // itself binds as a literal exclude of the filename "!".
    assert!(!chain.allows(Path::new("- foo"), false));
    assert!(!chain.allows(Path::new("- bar"), false));
    assert!(!chain.allows(Path::new("!"), false));

    chain.leave_directory(guard);
}

// upstream: exclude.c:1123-1124 - with FILTRULE_CVS_IGNORE inherited from
// the template (`:-C` modifier combination), a bare `!` line tentatively
// triggers FILTRULE_CLEAR_LIST and clears any previously parsed rules.
#[test]
fn dir_merge_no_prefixes_bang_clears_list_with_cvs() {
    let dir = TempDir::new().unwrap();
    let filter_content = "- foo\n!\n- bar\n";
    fs::write(dir.path().join(".filt"), filter_content).unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(
        DirMergeConfig::new(".filt")
            .with_no_prefixes(true, false)
            .with_cvs_mode(true),
    );

    let guard = chain.enter_directory(dir.path()).unwrap();

    // After the `!` clear, the prior `- foo` literal exclude no longer
    // applies; only the literal exclude of `- bar` (parsed after the
    // clear) remains.
    assert!(chain.allows(Path::new("- foo"), false));
    assert!(!chain.allows(Path::new("- bar"), false));

    chain.leave_directory(guard);
}

/// upstream: exclude.c:1324-1332 parse_rule_tok - under --delete-excluded,
/// tokens expanded from a `:C .cvsignore` per-directory merge acquire the
/// implicit FILTRULE_SENDER_SIDE flag. The receiver's delete-pass then
/// observes `applies_to_receiver=false`, so the receiver-side rule
/// no longer fires and the delete-pass proceeds (file is deleted).
#[test]
fn cvs_dir_merge_expands_to_sender_side_under_delete_excluded() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".cvsignore"), "*.junk *.bak").unwrap();

    let config = DirMergeConfig::new(".cvsignore").with_cvs_mode(true);
    let mut chain = FilterChain::empty().with_delete_excluded(true);
    chain.add_merge_config(config);

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);
    assert!(chain.delete_excluded());

    // Sender-side traversal still excludes these patterns from transfer,
    // matching upstream's `parse_rule_tok` OR'ing on FILTRULE_SENDER_SIDE.
    assert!(!chain.allows(Path::new("file.junk"), false));
    assert!(!chain.allows(Path::new("file.bak"), false));

    // Receiver-side deletion is no longer blocked because the rule lost
    // its applies_to_receiver bit; without the implicit flip the rule
    // would have matched on the receiver side and skipped deletion.
    assert!(
        chain.allows_deletion(Path::new("file.junk"), false),
        "merge-expanded rule must not block receiver deletion under delete-excluded"
    );
    assert!(
        chain.allows_deletion(Path::new("file.bak"), false),
        "merge-expanded rule must not block receiver deletion under delete-excluded"
    );

    chain.leave_directory(guard);
}

/// Without --delete-excluded the implicit flip must NOT fire, so the
/// expanded exclude rules continue to apply to both sides exactly as
/// upstream's `add_rule()` leaves them in the default case. The receiver
/// then matches the rule and skips deletion (default behaviour).
#[test]
fn cvs_dir_merge_preserves_both_sides_without_delete_excluded() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".cvsignore"), "*.junk").unwrap();

    let config = DirMergeConfig::new(".cvsignore").with_cvs_mode(true);
    let mut chain = FilterChain::empty();
    chain.add_merge_config(config);
    assert!(!chain.delete_excluded());

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    // Both sides still see the exclude when --delete-excluded is off.
    assert!(!chain.allows(Path::new("file.junk"), false));
    assert!(!chain.allows_deletion(Path::new("file.junk"), false));

    chain.leave_directory(guard);
}

// upstream: exclude.c:1279-1283, 1499 - a `:w` dir-merge (FILTRULE_WORD_SPLIT)
// tokenises its merge file on any whitespace and parses each token as its own
// rule. With the `_` separator standing in for the space between a rule's
// prefix and its pattern, `-_*.log -_*.tmp -_*.bak` on one line becomes three
// separate excludes. Without word-split the whole line is one malformed rule
// and none of the three patterns take effect (the remote-sender bug this
// guards against).
#[test]
fn dir_merge_word_split_parses_whitespace_separated_rules() {
    let dir = TempDir::new().unwrap();
    // Tab and space mixed, and split across two lines, to prove any whitespace
    // acts as a token boundary (upstream isspace()).
    fs::write(dir.path().join(".filt"), "-_*.log\t-_*.tmp -_*.bak\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".filt").with_word_split(true));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    assert!(!chain.allows(Path::new("a.log"), false));
    assert!(!chain.allows(Path::new("b.tmp"), false));
    assert!(!chain.allows(Path::new("c.bak"), false));
    assert!(chain.allows(Path::new("keep.dat"), false));

    chain.leave_directory(guard);
}

// upstream: exclude.c:1122-1133, 1499 - `:w-` combines FILTRULE_WORD_SPLIT with
// FILTRULE_NO_PREFIXES: the file is tokenised on whitespace and each token is a
// literal exclude pattern (no `-`/`+` prefix). `*.log *.tmp *.bak` becomes three
// literal excludes.
#[test]
fn dir_merge_word_split_no_prefixes_literal_excludes() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join(".filt"), "*.log\t*.tmp *.bak\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(
        DirMergeConfig::new(".filt")
            .with_word_split(true)
            .with_no_prefixes(true, false),
    );

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 1);

    assert!(!chain.allows(Path::new("a.log"), false));
    assert!(!chain.allows(Path::new("b.tmp"), false));
    assert!(!chain.allows(Path::new("c.bak"), false));
    assert!(chain.allows(Path::new("keep.dat"), false));

    chain.leave_directory(guard);
}

// upstream: exclude.c:903-960 rule_matches with ABS_ANCHOR - a leading-`/`
// exclude anchors to the transfer root and must NOT match the same basename
// nested in a subdirectory. This mirrors the remote-sender path where the rule
// arrives over the wire and is compiled into the chain's global set.
#[test]
fn anchored_root_exclude_does_not_match_nested_basename() {
    let global = FilterSet::from_rules([FilterRule::exclude("/drop.txt")]).unwrap();
    let chain = FilterChain::new(global);

    // Top-level `drop.txt` is excluded.
    assert!(!chain.allows(Path::new("drop.txt"), false));
    // Nested `sub/drop.txt` is NOT excluded by the anchored rule.
    assert!(chain.allows(Path::new("sub/drop.txt"), false));
    assert!(chain.allows(Path::new("keep.txt"), false));
}
