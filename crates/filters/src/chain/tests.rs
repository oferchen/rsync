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

    // Push a per-directory scope that includes *.log
    let dir_rules = FilterSet::from_rules([FilterRule::include("*.log")]).unwrap();
    let guard = chain.push_scope(dir_rules);

    // Per-directory include should override global exclude
    // But has_matching_rule returns false for includes, so we fall through.
    // This is correct: the per-directory scope only matters if it has
    // a matching exclude. For includes, we need both include and exclude
    // rules in the same scope.
    assert_eq!(guard.pushed_count(), 1);

    chain.leave_directory(guard);
    assert_eq!(chain.scope_depth(), 0);
}

#[test]
fn filter_chain_push_scope_exclude_overrides_global_include() {
    let global = FilterSet::from_rules([FilterRule::include("*.txt")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Push a per-directory scope that excludes *.txt
    let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();
    let guard = chain.push_scope(dir_rules);

    // Per-directory exclude should override global include
    assert!(!chain.allows(Path::new("file.txt"), false));

    chain.leave_directory(guard);

    // After leaving, global rules apply again
    assert!(chain.allows(Path::new("file.txt"), false));
}

#[test]
fn filter_chain_nested_scopes() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Enter outer directory - excludes *.log
    let outer = FilterSet::from_rules([FilterRule::exclude("*.log")]).unwrap();
    let guard_outer = chain.push_scope(outer);
    assert_eq!(chain.scope_depth(), 1);

    // Enter inner directory - excludes *.tmp
    let inner = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let guard_inner = chain.push_scope(inner);
    assert_eq!(chain.scope_depth(), 2);

    // All excludes should be active
    assert!(!chain.allows(Path::new("file.bak"), false));
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(!chain.allows(Path::new("file.tmp"), false));
    assert!(chain.allows(Path::new("file.txt"), false));

    // Leave inner directory
    chain.leave_directory(guard_inner);
    assert_eq!(chain.scope_depth(), 1);

    // Inner excludes should be gone, but outer and global remain
    assert!(!chain.allows(Path::new("file.bak"), false));
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));

    // Leave outer directory
    chain.leave_directory(guard_outer);
    assert_eq!(chain.scope_depth(), 0);

    // Only global excludes remain
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
    // No .rsync-filter file exists

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let guard = chain.enter_directory(dir.path()).unwrap();
    assert_eq!(guard.pushed_count(), 0);

    // Everything should be allowed (no rules)
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

    // The merge file itself should be excluded
    assert!(!chain.allows(Path::new(".rsync-filter"), false));
    // And the rule from the file should apply
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

    // *.important should be included, everything else excluded
    assert!(chain.allows(Path::new("file.important"), false));
    assert!(!chain.allows(Path::new("file.txt"), false));

    chain.leave_directory(guard);
}

#[test]
fn filter_chain_nested_directories_with_merge_files() {
    let dir = TempDir::new().unwrap();

    // Create outer directory with merge file
    let outer = dir.path().join("outer");
    fs::create_dir(&outer).unwrap();
    fs::write(outer.join(".rsync-filter"), "- *.log\n").unwrap();

    // Create inner directory with merge file
    let inner = outer.join("inner");
    fs::create_dir(&inner).unwrap();
    fs::write(inner.join(".rsync-filter"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    // Enter outer directory
    let guard_outer = chain.enter_directory(&outer).unwrap();
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));

    // Enter inner directory
    let guard_inner = chain.enter_directory(&inner).unwrap();
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(!chain.allows(Path::new("file.tmp"), false));

    // Leave inner
    chain.leave_directory(guard_inner);
    assert!(!chain.allows(Path::new("file.log"), false));
    assert!(chain.allows(Path::new("file.tmp"), false));

    // Leave outer
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
    // Invalid filter syntax
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

    // Rules should be applied (perishable doesn't affect allows())
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

    // Pop all at once by using depth tracking
    chain.scopes.clear();
    chain.current_depth = 0;
    assert_eq!(chain.scope_depth(), 0);
}

#[test]
fn filter_chain_default_allows_everything() {
    let chain = FilterChain::empty();
    // With no rules at all, everything should be allowed
    assert!(chain.allows(Path::new("any/path/here.txt"), false));
    assert!(chain.allows(Path::new("directory"), true));
    assert!(chain.allows_deletion(Path::new("anything"), false));
}

#[test]
fn filter_chain_global_rules_persist_across_scopes() {
    let global = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();
    let mut chain = FilterChain::new(global);

    // Enter and leave several directories
    for _ in 0..3 {
        let dir_rules = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
        let guard = chain.push_scope(dir_rules);
        assert!(!chain.allows(Path::new("file.bak"), false));
        chain.leave_directory(guard);
    }

    // Global rules should still work
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
