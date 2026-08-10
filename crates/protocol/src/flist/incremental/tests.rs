use std::collections::HashSet;

use super::*;
use crate::flist::FileEntry;

fn make_file(name: &str) -> FileEntry {
    FileEntry::new_file(name.into(), 0, 0o644)
}

fn make_dir(name: &str) -> FileEntry {
    FileEntry::new_directory(name.into(), 0o755)
}

#[test]
fn test_root_entries_immediately_ready() {
    let mut incremental = IncrementalFileList::new();

    assert!(incremental.push(make_file("file.txt")));
    assert_eq!(incremental.ready_count(), 1);

    assert!(incremental.push(make_dir("subdir")));
    assert_eq!(incremental.ready_count(), 2);
}

#[test]
fn test_nested_file_waits_for_parent() {
    let mut incremental = IncrementalFileList::new();

    assert!(!incremental.push(make_file("subdir/file.txt")));
    assert_eq!(incremental.ready_count(), 0);
    assert_eq!(incremental.pending_count(), 1);

    assert!(incremental.push(make_dir("subdir")));
    assert_eq!(incremental.ready_count(), 2);
    assert_eq!(incremental.pending_count(), 0);
}

#[test]
fn test_deeply_nested_structure() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("a/b/c/file.txt"));
    incremental.push(make_dir("a/b/c"));
    incremental.push(make_dir("a/b"));
    incremental.push(make_dir("a"));

    assert_eq!(incremental.ready_count(), 4);
    assert_eq!(incremental.pending_count(), 0);
}

#[test]
fn test_pop_returns_entries_in_order() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_dir("a"));
    incremental.push(make_file("a/file1.txt"));
    incremental.push(make_file("a/file2.txt"));

    let entry1 = incremental.pop().unwrap();
    assert_eq!(entry1.name(), "a");

    let entry2 = incremental.pop().unwrap();
    assert_eq!(entry2.name(), "a/file1.txt");

    let entry3 = incremental.pop().unwrap();
    assert_eq!(entry3.name(), "a/file2.txt");

    assert!(incremental.pop().is_none());
}

#[test]
fn test_mark_directory_created() {
    let mut incremental = IncrementalFileList::new();

    incremental.mark_directory_created("existing");

    assert!(incremental.push(make_file("existing/file.txt")));
    assert_eq!(incremental.ready_count(), 1);
}

#[test]
fn test_builder() {
    let incremental = IncrementalFileListBuilder::new()
        .incremental_recursion(true)
        .pre_created_dir("existing1")
        .pre_created_dir("existing2")
        .build();

    assert!(incremental.is_incremental_recursion());
}

#[test]
fn test_drain_ready() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("a.txt"));
    incremental.push(make_file("b.txt"));
    incremental.push(make_file("c.txt"));

    let ready = incremental.drain_ready();
    assert_eq!(ready.len(), 3);
    assert!(incremental.is_empty());
    assert_eq!(incremental.entries_yielded(), 3);
}

#[test]
fn test_finish_returns_orphans() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("missing/file1.txt"));
    incremental.push(make_file("missing/file2.txt"));

    let orphans = incremental.finish();
    assert_eq!(orphans.len(), 2);
}

#[test]
fn test_parent_path() {
    assert_eq!(IncrementalFileList::parent_path("."), "");
    assert_eq!(IncrementalFileList::parent_path(""), "");
    assert_eq!(IncrementalFileList::parent_path("file.txt"), ".");
    assert_eq!(IncrementalFileList::parent_path("dir/file.txt"), "dir");
    assert_eq!(IncrementalFileList::parent_path("a/b/c.txt"), "a/b");
}

#[test]
fn test_dot_directory() {
    let mut incremental = IncrementalFileList::new();

    assert!(incremental.push(make_dir(".")));
    assert_eq!(incremental.ready_count(), 1);
}

#[test]
fn test_entries_yielded_counter() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("a.txt"));
    incremental.push(make_file("b.txt"));

    assert_eq!(incremental.entries_yielded(), 0);

    incremental.pop();
    assert_eq!(incremental.entries_yielded(), 1);

    incremental.pop();
    assert_eq!(incremental.entries_yielded(), 2);
}

#[test]
fn test_peek() {
    let mut incremental = IncrementalFileList::new();

    assert!(incremental.peek().is_none());

    incremental.push(make_file("test.txt"));

    let peeked = incremental.peek().unwrap();
    assert_eq!(peeked.name(), "test.txt");

    assert_eq!(incremental.ready_count(), 1);
}

#[test]
fn test_has_pending() {
    let mut incremental = IncrementalFileList::new();

    assert!(!incremental.has_pending());

    incremental.push(make_file("nonexistent/file.txt"));
    assert!(incremental.has_pending());

    incremental.push(make_dir("nonexistent"));
    assert!(!incremental.has_pending());
}

#[test]
fn test_into_iter() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("a.txt"));
    incremental.push(make_file("b.txt"));

    let entries: Vec<_> = incremental.into_iter().collect();
    assert_eq!(entries.len(), 2);
}

#[test]
fn test_multiple_pending_directories() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("a/file1.txt"));
    incremental.push(make_file("b/file2.txt"));
    incremental.push(make_file("c/file3.txt"));

    assert_eq!(incremental.pending_count(), 3);
    assert_eq!(incremental.ready_count(), 0);

    incremental.push(make_dir("a"));
    assert_eq!(incremental.ready_count(), 2);
    assert_eq!(incremental.pending_count(), 2);

    incremental.push(make_dir("b"));
    assert_eq!(incremental.ready_count(), 4);
    assert_eq!(incremental.pending_count(), 1);
}

#[test]
fn test_finalize_no_orphans() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_dir("a"));
    incremental.push(make_file("a/file.txt"));
    incremental.push(make_file("root.txt"));

    let _ = incremental.drain_ready();

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.orphan_count(), 0);
    assert!(result.stats.no_orphans());
    assert!(result.stats.all_resolved());
    assert_eq!(result.stats.orphans_detected, 0);
    assert_eq!(result.stats.placeholder_dirs_created, 0);
}

#[test]
fn test_finalize_resolves_single_orphan() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("missing_dir/file.txt"));

    assert_eq!(incremental.pending_count(), 1);
    assert_eq!(incremental.ready_count(), 0);

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.orphan_count(), 0);
    assert_eq!(result.resolved_count(), 1);
    assert_eq!(result.stats.orphans_detected, 1);
    assert_eq!(result.stats.orphans_resolved, 1);
    assert_eq!(result.stats.orphans_unresolved, 0);
    assert_eq!(result.stats.placeholder_dirs_created, 1);

    assert_eq!(result.resolved_entries[0].name(), "missing_dir/file.txt");
}

#[test]
fn test_finalize_resolves_deeply_nested_orphan() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("a/b/c/d/deep_file.txt"));

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.orphan_count(), 0);
    assert_eq!(result.resolved_count(), 1);

    // Should have created placeholders for a, a/b, a/b/c, a/b/c/d
    assert_eq!(result.stats.placeholder_dirs_created, 4);
    assert_eq!(result.stats.orphans_resolved, 1);
}

#[test]
fn test_finalize_multiple_orphans_same_missing_parent() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("missing/file1.txt"));
    incremental.push(make_file("missing/file2.txt"));
    incremental.push(make_file("missing/file3.txt"));

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.orphan_count(), 0);
    assert_eq!(result.resolved_count(), 3);
    assert_eq!(result.stats.orphans_detected, 3);
    assert_eq!(result.stats.orphans_resolved, 3);
    assert_eq!(result.stats.placeholder_dirs_created, 1);

    let names: Vec<&str> = result.resolved_entries.iter().map(|e| e.name()).collect();
    assert!(names.contains(&"missing/file1.txt"));
    assert!(names.contains(&"missing/file2.txt"));
    assert!(names.contains(&"missing/file3.txt"));
}

#[test]
fn test_finalize_multiple_missing_parents() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("alpha/file1.txt"));
    incremental.push(make_file("beta/file2.txt"));
    incremental.push(make_file("gamma/file3.txt"));

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 3);
    assert_eq!(result.stats.placeholder_dirs_created, 3);
}

#[test]
fn test_finalize_orphan_that_eventually_gets_parent() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("later_dir/file.txt"));
    assert_eq!(incremental.pending_count(), 1);

    incremental.push(make_dir("later_dir"));
    assert_eq!(incremental.pending_count(), 0);
    assert_eq!(incremental.ready_count(), 2);

    let _ = incremental.drain_ready();
    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.stats.orphans_detected, 0);
    assert_eq!(result.stats.placeholder_dirs_created, 0);
}

#[test]
fn test_finalize_cascading_orphan_resolution() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("missing/sub/file.txt"));
    incremental.push(make_dir("missing/sub"));

    assert_eq!(incremental.pending_count(), 2);

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 2);
    // Only "missing" needs a placeholder; "missing/sub" was a real directory entry
    assert_eq!(result.stats.placeholder_dirs_created, 1);
    assert_eq!(result.stats.orphans_resolved, 2);
}

#[test]
fn test_finalize_preserves_unprocessed_ready_entries() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("ready1.txt"));
    incremental.push(make_file("ready2.txt"));
    incremental.push(make_file("orphan_dir/orphan.txt"));

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.stats.unprocessed_ready, 2);
    assert_eq!(result.resolved_count(), 3);

    let names: Vec<&str> = result.resolved_entries.iter().map(|e| e.name()).collect();
    assert!(names.contains(&"ready1.txt"));
    assert!(names.contains(&"ready2.txt"));
    assert!(names.contains(&"orphan_dir/orphan.txt"));
}

#[test]
fn test_finalize_mixed_ready_and_orphans() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_dir("existing"));
    incremental.push(make_file("existing/normal.txt"));

    incremental.push(make_file("missing_a/orphan1.txt"));
    incremental.push(make_file("missing_b/orphan2.txt"));

    assert_eq!(incremental.ready_count(), 2);
    assert_eq!(incremental.pending_count(), 2);

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.stats.unprocessed_ready, 2);
    assert_eq!(result.stats.orphans_detected, 2);
    assert_eq!(result.stats.orphans_resolved, 2);
    assert_eq!(result.stats.placeholder_dirs_created, 2);
}

#[test]
fn test_finalize_deeply_nested_multiple_branches() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("x/y/z/file1.txt"));
    incremental.push(make_file("a/b/c/file2.txt"));

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 2);
    // x, x/y, x/y/z + a, a/b, a/b/c = 6 placeholders
    assert_eq!(result.stats.placeholder_dirs_created, 6);
}

#[test]
fn test_finalize_shared_ancestor_placeholders() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("shared/branch_a/file1.txt"));
    incremental.push(make_file("shared/branch_b/file2.txt"));

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 2);
    // "shared" placeholder is shared, then "shared/branch_a" and "shared/branch_b"
    assert_eq!(result.stats.placeholder_dirs_created, 3);
}

#[test]
fn test_finalize_empty_list() {
    let incremental = IncrementalFileList::new();
    let result = incremental.finalize();

    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 0);
    assert_eq!(result.orphan_count(), 0);
    assert!(result.stats.no_orphans());
    assert!(result.stats.all_resolved());
}

#[test]
fn test_finalize_with_incremental_recursion_mode() {
    let mut incremental = IncrementalFileList::with_incremental_recursion();
    incremental.push(make_file("late_dir/file.txt"));

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.stats.orphans_resolved, 1);
}

#[test]
fn test_finalize_orphan_directory_releases_children() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("missing/sub/file.txt"));
    incremental.push(make_dir("missing/sub"));
    incremental.push(make_file("missing/direct.txt"));

    assert_eq!(incremental.pending_count(), 3);

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 3);
    assert_eq!(result.stats.placeholder_dirs_created, 1); // Only "missing"
}

#[test]
fn test_collect_missing_ancestors() {
    let created = {
        let mut s = HashSet::new();
        s.insert(String::new());
        s.insert(".".to_string());
        s
    };

    let ancestors = IncrementalFileList::collect_missing_ancestors("a/b/c", &created);
    assert_eq!(ancestors, vec!["a", "a/b"]);

    let mut created_with_a = created.clone();
    created_with_a.insert("a".to_string());
    let ancestors = IncrementalFileList::collect_missing_ancestors("a/b/c", &created_with_a);
    assert_eq!(ancestors, vec!["a/b"]);

    let ancestors = IncrementalFileList::collect_missing_ancestors("x", &created);
    assert!(ancestors.is_empty());

    let ancestors = IncrementalFileList::collect_missing_ancestors("a/b/c/d/e", &created);
    assert_eq!(ancestors, vec!["a", "a/b", "a/b/c", "a/b/c/d"]);
}

#[test]
fn test_orphan_entry_accessors() {
    let entry = make_file("test/file.txt");
    let orphan = OrphanEntry {
        entry: entry.clone(),
        missing_parent: "test".to_string(),
    };

    assert_eq!(orphan.entry().name(), "test/file.txt");
    assert_eq!(orphan.missing_parent(), "test");
}

#[test]
fn test_finalization_stats_predicates() {
    let stats_clean = FinalizationStats {
        orphans_detected: 0,
        orphans_resolved: 0,
        orphans_unresolved: 0,
        placeholder_dirs_created: 0,
        unprocessed_ready: 0,
    };
    assert!(stats_clean.no_orphans());
    assert!(stats_clean.all_resolved());

    let stats_resolved = FinalizationStats {
        orphans_detected: 5,
        orphans_resolved: 5,
        orphans_unresolved: 0,
        placeholder_dirs_created: 2,
        unprocessed_ready: 0,
    };
    assert!(!stats_resolved.no_orphans());
    assert!(stats_resolved.all_resolved());

    let stats_unresolved = FinalizationStats {
        orphans_detected: 3,
        orphans_resolved: 1,
        orphans_unresolved: 2,
        placeholder_dirs_created: 1,
        unprocessed_ready: 0,
    };
    assert!(!stats_unresolved.no_orphans());
    assert!(!stats_unresolved.all_resolved());
}

#[test]
fn test_finalize_symlink_orphan() {
    let mut incremental = IncrementalFileList::new();
    let symlink = FileEntry::new_symlink("missing_dir/link".into(), "target".into());
    incremental.push(symlink);

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 1);
    assert!(result.resolved_entries[0].is_symlink());
    assert_eq!(result.stats.placeholder_dirs_created, 1);
}

#[test]
fn test_finalize_after_partial_drain() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("ready1.txt"));
    incremental.push(make_file("ready2.txt"));
    incremental.push(make_file("missing/orphan.txt"));

    let _ = incremental.pop();
    assert_eq!(incremental.ready_count(), 1);

    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.resolved_count(), 2);
    assert_eq!(result.stats.unprocessed_ready, 1);
    assert_eq!(result.stats.orphans_resolved, 1);
}

#[test]
fn test_finalize_with_builder_pre_created_dirs() {
    let mut incremental = IncrementalFileListBuilder::new()
        .pre_created_dir("pre_existing")
        .build();

    incremental.push(make_file("pre_existing/file.txt"));
    assert_eq!(incremental.ready_count(), 1);

    incremental.push(make_file("other/file.txt"));

    let _ = incremental.drain_ready();
    let result = incremental.finalize();
    assert!(result.is_complete());
    assert_eq!(result.stats.placeholder_dirs_created, 1);
}

fn make_symlink(name: &str, target: &str) -> FileEntry {
    FileEntry::new_symlink(name.into(), target.into())
}

fn make_block_device(name: &str) -> FileEntry {
    FileEntry::new_block_device(name.into(), 0o660, 8, 1)
}

fn make_fifo(name: &str) -> FileEntry {
    FileEntry::new_fifo(name.into(), 0o644)
}

fn no_filter(_name: &str, _is_dir: bool) -> bool {
    false
}

fn no_failures(_name: &str) -> Option<String> {
    None
}

#[test]
fn test_process_ready_entry_regular_file() {
    let entry = make_file("src/main.rs");
    let action = process_ready_entry(entry, no_filter, no_failures);
    assert!(matches!(action, ReadyEntryAction::TransferFile(_)));
    assert!(action.is_actionable());
    assert!(!action.is_skipped());
    assert_eq!(action.entry().name(), "src/main.rs");
}

#[test]
fn test_process_ready_entry_directory() {
    let entry = make_dir("src");
    let action = process_ready_entry(entry, no_filter, no_failures);
    assert!(matches!(action, ReadyEntryAction::CreateDirectory(_)));
    assert!(action.is_actionable());
    assert_eq!(action.entry().name(), "src");
}

#[test]
fn test_process_ready_entry_symlink() {
    let entry = make_symlink("link", "/target");
    let action = process_ready_entry(entry, no_filter, no_failures);
    assert!(matches!(action, ReadyEntryAction::CreateSymlink(_)));
    assert!(action.is_actionable());
    assert_eq!(action.entry().name(), "link");
}

#[test]
fn test_process_ready_entry_block_device() {
    let entry = make_block_device("dev/sda1");
    let action = process_ready_entry(entry, no_filter, no_failures);
    assert!(matches!(action, ReadyEntryAction::CreateDevice(_)));
    assert!(action.is_actionable());
}

#[test]
fn test_process_ready_entry_fifo() {
    let entry = make_fifo("my_pipe");
    let action = process_ready_entry(entry, no_filter, no_failures);
    assert!(matches!(action, ReadyEntryAction::CreateSpecial(_)));
    assert!(action.is_actionable());
}

#[test]
fn test_process_ready_entry_filtered_out() {
    let entry = make_file("build/output.o");
    let action = process_ready_entry(entry, |_name, _is_dir| true, no_failures);
    assert!(matches!(action, ReadyEntryAction::SkipFiltered(_)));
    assert!(action.is_skipped());
    assert!(!action.is_actionable());
    assert_eq!(action.entry().name(), "build/output.o");
}

#[test]
fn test_process_ready_entry_filtered_directory() {
    let entry = make_dir(".git");
    let action = process_ready_entry(entry, |name, _is_dir| name == ".git", no_failures);
    assert!(matches!(action, ReadyEntryAction::SkipFiltered(_)));
    assert!(action.is_skipped());
}

#[test]
fn test_process_ready_entry_failed_parent() {
    let entry = make_file("broken_dir/file.txt");
    let action = process_ready_entry(entry, no_filter, |name| {
        if name.starts_with("broken_dir") {
            Some("broken_dir".to_string())
        } else {
            None
        }
    });
    match &action {
        ReadyEntryAction::SkipFailedParent {
            entry,
            failed_ancestor,
        } => {
            assert_eq!(entry.name(), "broken_dir/file.txt");
            assert_eq!(failed_ancestor, "broken_dir");
        }
        other => panic!("expected SkipFailedParent, got {other:?}"),
    }
    assert!(action.is_skipped());
    assert!(!action.is_actionable());
}

#[test]
fn test_process_ready_entry_failed_parent_takes_priority_over_filter() {
    let entry = make_file("bad/excluded.o");
    let action = process_ready_entry(
        entry,
        |_name, _is_dir| true,           // would be filtered
        |_name| Some("bad".to_string()), // also has failed parent
    );
    assert!(matches!(action, ReadyEntryAction::SkipFailedParent { .. }));
}

#[test]
fn test_process_ready_entry_filter_receives_correct_is_dir() {
    let dir_entry = make_dir("mydir");
    let mut received_is_dir = false;
    let _ = process_ready_entry(
        dir_entry,
        |_name, is_dir| {
            received_is_dir = is_dir;
            false
        },
        no_failures,
    );
    assert!(received_is_dir, "directory entry should pass is_dir=true");

    let file_entry = make_file("myfile.txt");
    let mut received_is_dir = true;
    let _ = process_ready_entry(
        file_entry,
        |_name, is_dir| {
            received_is_dir = is_dir;
            false
        },
        no_failures,
    );
    assert!(!received_is_dir, "file entry should pass is_dir=false");
}

#[test]
fn test_process_ready_entry_into_entry() {
    let entry = make_file("test.txt");
    let action = process_ready_entry(entry, no_filter, no_failures);
    let recovered = action.into_entry();
    assert_eq!(recovered.name(), "test.txt");
}

#[test]
fn test_process_ready_entry_into_entry_from_skip() {
    let entry = make_file("filtered.txt");
    let action = process_ready_entry(entry, |_, _| true, no_failures);
    let recovered = action.into_entry();
    assert_eq!(recovered.name(), "filtered.txt");
}

#[test]
fn test_process_ready_entry_into_entry_from_failed_parent() {
    let entry = make_file("bad/file.txt");
    let action = process_ready_entry(entry, no_filter, |_| Some("bad".to_string()));
    let recovered = action.into_entry();
    assert_eq!(recovered.name(), "bad/file.txt");
}

#[test]
fn test_process_ready_entries_multiple() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_dir("src"));
    incremental.push(make_file("src/main.rs"));
    incremental.push(make_file("README.md"));
    incremental.push(make_symlink("latest", "v1.0"));

    let actions = process_ready_entries(&mut incremental, no_filter, no_failures);
    assert_eq!(actions.len(), 4);
    assert!(matches!(actions[0], ReadyEntryAction::CreateDirectory(_)));
    assert!(matches!(actions[1], ReadyEntryAction::TransferFile(_)));
    assert!(matches!(actions[2], ReadyEntryAction::TransferFile(_)));
    assert!(matches!(actions[3], ReadyEntryAction::CreateSymlink(_)));
}

#[test]
fn test_process_ready_entries_with_filter() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("keep.txt"));
    incremental.push(make_file("skip.o"));
    incremental.push(make_file("also_keep.rs"));

    let actions = process_ready_entries(
        &mut incremental,
        |name, _is_dir| name.ends_with(".o"),
        no_failures,
    );
    assert_eq!(actions.len(), 3);
    assert!(matches!(actions[0], ReadyEntryAction::TransferFile(_)));
    assert!(matches!(actions[1], ReadyEntryAction::SkipFiltered(_)));
    assert!(matches!(actions[2], ReadyEntryAction::TransferFile(_)));
}

#[test]
fn test_process_ready_entries_with_failed_dirs() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_dir("good"));
    incremental.push(make_file("good/file.txt"));
    incremental.push(make_file("bad/orphan.txt")); // pending (parent missing)
    incremental.push(make_dir("bad")); // releases orphan
    incremental.push(make_file("ok.txt"));

    let mut failed_set: HashSet<String> = HashSet::new();
    failed_set.insert("bad".to_string());

    let actions = process_ready_entries(&mut incremental, no_filter, |name| {
        if failed_set.contains(name) {
            return Some(name.to_string());
        }
        let mut check = name;
        while let Some(pos) = check.rfind('/') {
            check = &check[..pos];
            if failed_set.contains(check) {
                return Some(check.to_string());
            }
        }
        None
    });

    assert_eq!(actions.len(), 5);

    let action_names: Vec<(&str, bool)> = actions
        .iter()
        .map(|a| (a.entry().name(), a.is_skipped()))
        .collect();

    let good_action = actions.iter().find(|a| a.entry().name() == "good").unwrap();
    assert!(matches!(good_action, ReadyEntryAction::CreateDirectory(_)));

    let good_file = actions
        .iter()
        .find(|a| a.entry().name() == "good/file.txt")
        .unwrap();
    assert!(matches!(good_file, ReadyEntryAction::TransferFile(_)));

    let bad_dir = actions.iter().find(|a| a.entry().name() == "bad").unwrap();
    match bad_dir {
        ReadyEntryAction::SkipFailedParent {
            failed_ancestor, ..
        } => {
            assert_eq!(failed_ancestor, "bad");
        }
        other => panic!("expected SkipFailedParent for 'bad', got {other:?}"),
    }

    let bad_orphan = actions
        .iter()
        .find(|a| a.entry().name() == "bad/orphan.txt")
        .unwrap();
    match bad_orphan {
        ReadyEntryAction::SkipFailedParent {
            failed_ancestor, ..
        } => {
            assert_eq!(failed_ancestor, "bad");
        }
        other => panic!("expected SkipFailedParent for 'bad/orphan.txt', got {other:?}"),
    }

    let ok_file = actions
        .iter()
        .find(|a| a.entry().name() == "ok.txt")
        .unwrap();
    assert!(matches!(ok_file, ReadyEntryAction::TransferFile(_)));

    let skipped = action_names.iter().filter(|(_, s)| *s).count();
    let actionable = action_names.iter().filter(|(_, s)| !*s).count();
    assert_eq!(skipped, 2, "bad dir and bad/orphan.txt should be skipped");
    assert_eq!(
        actionable, 3,
        "good, good/file.txt, ok.txt should be actionable"
    );
}

#[test]
fn test_process_ready_entries_empty() {
    let mut incremental = IncrementalFileList::new();
    let actions = process_ready_entries(&mut incremental, no_filter, no_failures);
    assert!(actions.is_empty());
}

#[test]
fn test_process_ready_entries_drains_queue() {
    let mut incremental = IncrementalFileList::new();
    incremental.push(make_file("a.txt"));
    incremental.push(make_file("b.txt"));

    let actions = process_ready_entries(&mut incremental, no_filter, no_failures);
    assert_eq!(actions.len(), 2);

    assert!(incremental.is_empty());
    assert_eq!(incremental.ready_count(), 0);
}

#[test]
fn test_process_ready_entry_char_device() {
    let entry = FileEntry::new_char_device("dev/tty0".into(), 0o666, 4, 0);
    let action = process_ready_entry(entry, no_filter, no_failures);
    assert!(matches!(action, ReadyEntryAction::CreateDevice(_)));
    assert!(action.is_actionable());
}

#[test]
fn test_process_ready_entry_socket() {
    let entry = FileEntry::new_socket("run/app.sock".into(), 0o755);
    let action = process_ready_entry(entry, no_filter, no_failures);
    assert!(matches!(action, ReadyEntryAction::CreateSpecial(_)));
    assert!(action.is_actionable());
}

#[test]
fn test_process_ready_entry_sequence_mixed_types() {
    let entries = vec![
        make_dir("."),
        make_dir("src"),
        make_file("src/lib.rs"),
        make_file("src/main.rs"),
        make_dir("tests"),
        make_file("tests/integration.rs"),
        make_symlink("latest", "v1.0"),
        make_fifo("events"),
        make_file("Cargo.toml"),
    ];

    let mut dir_count = 0;
    let mut file_count = 0;
    let mut symlink_count = 0;
    let mut special_count = 0;

    for entry in entries {
        let action = process_ready_entry(entry, no_filter, no_failures);
        match action {
            ReadyEntryAction::CreateDirectory(_) => dir_count += 1,
            ReadyEntryAction::TransferFile(_) => file_count += 1,
            ReadyEntryAction::CreateSymlink(_) => symlink_count += 1,
            ReadyEntryAction::CreateSpecial(_) => special_count += 1,
            _ => panic!("unexpected skip action"),
        }
    }

    assert_eq!(dir_count, 3);
    assert_eq!(file_count, 4);
    assert_eq!(symlink_count, 1);
    assert_eq!(special_count, 1);
}

#[test]
fn test_process_ready_entry_selective_filter() {
    let filter =
        |name: &str, _is_dir: bool| -> bool { name.ends_with(".o") || name.ends_with(".tmp") };

    let cases = vec![
        (make_file("main.o"), true),
        (make_file("temp.tmp"), true),
        (make_file("source.rs"), false),
        (make_dir("build"), false),
        (make_symlink("link", "target"), false),
    ];

    for (entry, should_filter) in cases {
        let name = entry.name().to_string();
        let action = process_ready_entry(entry, filter, no_failures);
        if should_filter {
            assert!(action.is_skipped(), "expected {name} to be filtered");
        } else {
            assert!(action.is_actionable(), "expected {name} to be actionable");
        }
    }
}

#[test]
fn test_ready_entry_action_entry_accessor() {
    let file_action = process_ready_entry(make_file("f.txt"), no_filter, no_failures);
    assert_eq!(file_action.entry().name(), "f.txt");

    let dir_action = process_ready_entry(make_dir("d"), no_filter, no_failures);
    assert_eq!(dir_action.entry().name(), "d");

    let sym_action = process_ready_entry(make_symlink("l", "t"), no_filter, no_failures);
    assert_eq!(sym_action.entry().name(), "l");

    let dev_action = process_ready_entry(make_block_device("dev"), no_filter, no_failures);
    assert_eq!(dev_action.entry().name(), "dev");

    let special_action = process_ready_entry(make_fifo("fifo"), no_filter, no_failures);
    assert_eq!(special_action.entry().name(), "fifo");

    let filtered_action = process_ready_entry(make_file("x"), |_, _| true, no_failures);
    assert_eq!(filtered_action.entry().name(), "x");

    let failed_action =
        process_ready_entry(make_file("bad/y"), no_filter, |_| Some("bad".to_string()));
    assert_eq!(failed_action.entry().name(), "bad/y");
}

#[test]
fn test_process_ready_entries_integrated_with_incremental() {
    let mut incremental = IncrementalFileList::new();

    incremental.push(make_file("alpha/deep/file.txt")); // pending
    incremental.push(make_dir("alpha")); // releases alpha/deep/file.txt? No, alpha/deep still missing
    incremental.push(make_file("root.txt")); // ready immediately
    incremental.push(make_dir("alpha/deep")); // releases alpha/deep/file.txt

    let actions = process_ready_entries(&mut incremental, no_filter, no_failures);

    assert_eq!(actions.len(), 4);

    let names: Vec<(&str, bool)> = actions
        .iter()
        .map(|a| (a.entry().name(), a.is_actionable()))
        .collect();
    assert!(names.contains(&("alpha", true)));
    assert!(names.contains(&("root.txt", true)));
    assert!(names.contains(&("alpha/deep", true)));
    assert!(names.contains(&("alpha/deep/file.txt", true)));

    assert!(incremental.is_empty());
    assert_eq!(incremental.pending_count(), 0);
}
