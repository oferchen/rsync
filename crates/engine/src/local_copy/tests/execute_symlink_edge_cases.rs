// Comprehensive tests for symlink handling edge cases
//
// This module tests various edge cases in symlink handling:
// - Absolute vs relative symlink targets
// - Broken symlinks
// - Symlink chains
// - Symlinks pointing outside the tree
// - Self-referencing symlinks
// - Permission handling for symlinks

// ==================== Absolute vs Relative Symlink Targets ====================

#[cfg(unix)]
#[test]
fn symlink_absolute_target_within_source_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create target file inside source tree
    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Create absolute symlink pointing to target (within tree)
    let link = source_root.join("abs_link");
    symlink(&target, &link).expect("create absolute symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    // Symlink should be copied (absolute path preserved)
    let dest_link = dest_root.join("abs_link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    // Absolute target path should be preserved exactly
    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(copied_target, target);
    assert!(summary.symlinks_copied() >= 1);
}

#[cfg(unix)]
#[test]
fn symlink_relative_target_stays_relative() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Create relative symlink
    let link = source_root.join("rel_link");
    symlink(Path::new("target.txt"), &link).expect("create relative symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("rel_link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    // Relative target should be preserved exactly (not converted to absolute)
    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(copied_target, Path::new("target.txt"));
}

#[cfg(unix)]
#[test]
fn symlink_complex_relative_path_with_dotdot() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    let subdir1 = source_root.join("dir1");
    let subdir2 = source_root.join("dir2");
    fs::create_dir_all(&subdir1).expect("create dir1");
    fs::create_dir_all(&subdir2).expect("create dir2");

    let target = subdir2.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Create symlink in dir1 pointing to ../dir2/target.txt
    let link = subdir1.join("link");
    symlink(Path::new("../dir2/target.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("dir1/link");
    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(copied_target, Path::new("../dir2/target.txt"));

    // The link should resolve correctly in destination
    assert!(dest_link.exists(), "symlink should resolve to target in dest");
}

// ==================== Broken Symlinks ====================

#[cfg(unix)]
#[test]
fn broken_symlink_with_relative_target_preserved() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create symlink to non-existent relative target
    let link = source_root.join("broken");
    symlink(Path::new("nonexistent_file.txt"), &link).expect("create broken symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    let dest_link = dest_root.join("broken");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    // Broken symlinks are still copied
    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(copied_target, Path::new("nonexistent_file.txt"));
    assert_eq!(summary.symlinks_copied(), 1);
}

#[cfg(unix)]
#[test]
fn broken_symlink_with_absolute_target_preserved() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create symlink to non-existent absolute path
    let link = source_root.join("broken_abs");
    symlink(Path::new("/nonexistent/absolute/path.txt"), &link).expect("create broken symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With safe_links disabled, broken absolute symlinks are preserved
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).safe_links(false),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("broken_abs");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    let copied_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(copied_target, Path::new("/nonexistent/absolute/path.txt"));
}

#[cfg(unix)]
#[test]
fn broken_symlink_becomes_valid_when_target_copied() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create target file
    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Create symlink to target
    let link = source_root.join("link");
    symlink(Path::new("target.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    // Both target and link should exist in destination
    let dest_target = dest_root.join("target.txt");
    let dest_link = dest_root.join("link");

    assert!(dest_target.is_file());
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    // Link should resolve to target in destination
    assert!(dest_link.exists(), "symlink should resolve correctly");
    assert_eq!(fs::read(&dest_link).expect("read via link"), b"content");
}

// ==================== Symlink Chains ====================

#[cfg(unix)]
#[test]
fn symlink_chain_two_levels() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create chain: link2 -> link1 -> target
    let target = source_root.join("target.txt");
    fs::write(&target, b"final content").expect("write target");

    let link1 = source_root.join("link1");
    symlink(Path::new("target.txt"), &link1).expect("create link1");

    let link2 = source_root.join("link2");
    symlink(Path::new("link1"), &link2).expect("create link2");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    // All links and target should be copied
    assert!(dest_root.join("target.txt").is_file());
    assert!(fs::symlink_metadata(dest_root.join("link1")).expect("meta").file_type().is_symlink());
    assert!(fs::symlink_metadata(dest_root.join("link2")).expect("meta").file_type().is_symlink());

    // Chain should resolve correctly
    assert_eq!(fs::read_link(dest_root.join("link1")).expect("read"), Path::new("target.txt"));
    assert_eq!(fs::read_link(dest_root.join("link2")).expect("read"), Path::new("link1"));

    // Following full chain should reach content
    assert_eq!(fs::read(dest_root.join("link2")).expect("read content"), b"final content");
    assert_eq!(summary.symlinks_copied(), 2);
}

#[cfg(unix)]
#[test]
fn symlink_chain_three_levels_deep() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create chain: link3 -> link2 -> link1 -> target
    let target = source_root.join("target.txt");
    fs::write(&target, b"deep content").expect("write target");

    let link1 = source_root.join("link1");
    symlink(Path::new("target.txt"), &link1).expect("create link1");

    let link2 = source_root.join("link2");
    symlink(Path::new("link1"), &link2).expect("create link2");

    let link3 = source_root.join("link3");
    symlink(Path::new("link2"), &link3).expect("create link3");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    // All levels should be preserved
    for link_name in ["link1", "link2", "link3"] {
        let dest_link = dest_root.join(link_name);
        assert!(
            fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink(),
            "{link_name} should be a symlink"
        );
    }

    // Full chain resolution should work
    assert_eq!(fs::read(dest_root.join("link3")).expect("read"), b"deep content");
}

#[cfg(unix)]
#[test]
fn symlink_chain_across_directories() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    let dir_a = source_root.join("a");
    let dir_b = source_root.join("b");
    fs::create_dir_all(&dir_a).expect("create a");
    fs::create_dir_all(&dir_b).expect("create b");

    // Target in dir_a
    let target = dir_a.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // link1 in dir_a pointing to target
    let link1 = dir_a.join("link1");
    symlink(Path::new("target.txt"), &link1).expect("create link1");

    // link2 in dir_b pointing to ../a/link1
    let link2 = dir_b.join("link2");
    symlink(Path::new("../a/link1"), &link2).expect("create link2");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    // Cross-directory chain should work
    let dest_link2 = dest_root.join("b/link2");
    assert!(fs::symlink_metadata(&dest_link2).expect("meta").file_type().is_symlink());
    assert_eq!(fs::read_link(&dest_link2).expect("read"), Path::new("../a/link1"));

    // Should resolve to content
    assert_eq!(fs::read(&dest_link2).expect("read content"), b"content");
}

// ==================== Symlinks Pointing Outside the Tree ====================

#[cfg(unix)]
#[test]
fn symlink_to_parent_directory_escapes_tree() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a file outside the source tree
    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"outside content").expect("write outside");

    // Create symlink that escapes via many parent dirs to be sure it escapes
    // regardless of how the relative path is computed
    let link = source_root.join("escape");
    symlink(Path::new("../../../../../outside.txt"), &link).expect("create escaping symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With safe_links, escaping symlinks should be skipped
    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let dest_link = dest_root.join("escape");
    assert!(!dest_link.exists(), "escaping symlink should be skipped");
    assert_eq!(report.summary().symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn symlink_multiple_dotdot_levels_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    let deep_dir = source_root.join("a/b/c");
    fs::create_dir_all(&deep_dir).expect("create deep dir");

    // Create symlink that uses many parent dirs to definitely escape.
    // The relative path from the destination root would be a/b/c/escape (depth 4)
    // plus the file name counts too, so we need MORE parent dirs to be sure.
    // Using 10 parent dirs ensures this will escape no matter what.
    let link = deep_dir.join("escape");
    symlink(Path::new("../../../../../../../../../../outside.txt"), &link)
        .expect("create escaping symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .safe_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy completes");

    let dest_link = dest_root.join("a/b/c/escape");
    assert!(!dest_link.exists(), "deeply escaping symlink should be skipped");
    assert_eq!(report.summary().symlinks_copied(), 0);
}

#[cfg(unix)]
#[test]
fn symlink_escapes_but_copy_unsafe_links_dereferences() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a file outside the source tree
    let outside_file = temp.path().join("outside.txt");
    fs::write(&outside_file, b"outside content").expect("write outside");

    // Create absolute symlink to outside file
    let link = source_root.join("escape");
    symlink(&outside_file, &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With copy_unsafe_links, the file should be dereferenced
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .links(true)
                .safe_links(true)
                .copy_unsafe_links(true),
        )
        .expect("copy succeeds");

    let dest_file = dest_root.join("escape");
    let meta = fs::symlink_metadata(&dest_file).expect("meta");

    // Should be dereferenced to a regular file
    assert!(meta.file_type().is_file());
    assert!(!meta.file_type().is_symlink());
    assert_eq!(fs::read(&dest_file).expect("read"), b"outside content");
    assert_eq!(summary.files_copied(), 1);
}

// ==================== Self-Referencing and Circular Symlinks ====================

#[cfg(unix)]
#[test]
fn self_referencing_symlink_handled() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a symlink that points to itself
    let link = source_root.join("self");
    symlink(Path::new("self"), &link).expect("create self-referencing symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With links mode (not copy_links), self-referencing symlinks are just copied
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    let dest_link = dest_root.join("self");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
    assert_eq!(fs::read_link(&dest_link).expect("read"), Path::new("self"));
    assert_eq!(summary.symlinks_copied(), 1);
}

#[cfg(unix)]
#[test]
fn mutual_symlink_cycle_preserved() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create link_a first (broken initially)
    let link_a = source_root.join("link_a");
    symlink(Path::new("link_b"), &link_a).expect("create link_a");

    // Create link_b pointing back to link_a
    let link_b = source_root.join("link_b");
    symlink(Path::new("link_a"), &link_b).expect("create link_b");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With links mode, the cycle is just copied as-is
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    let dest_link_a = dest_root.join("link_a");
    let dest_link_b = dest_root.join("link_b");

    assert!(fs::symlink_metadata(&dest_link_a).expect("meta").file_type().is_symlink());
    assert!(fs::symlink_metadata(&dest_link_b).expect("meta").file_type().is_symlink());
    assert_eq!(fs::read_link(&dest_link_a).expect("read"), Path::new("link_b"));
    assert_eq!(fs::read_link(&dest_link_b).expect("read"), Path::new("link_a"));
    assert_eq!(summary.symlinks_copied(), 2);
}

#[cfg(unix)]
#[test]
fn three_way_symlink_cycle() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a three-way cycle: a -> b -> c -> a
    let link_a = source_root.join("a");
    let link_b = source_root.join("b");
    let link_c = source_root.join("c");

    symlink(Path::new("b"), &link_a).expect("create a");
    symlink(Path::new("c"), &link_b).expect("create b");
    symlink(Path::new("a"), &link_c).expect("create c");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true),
        )
        .expect("copy succeeds");

    // All three links should be copied preserving the cycle
    for name in ["a", "b", "c"] {
        let dest_link = dest_root.join(name);
        assert!(
            fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink(),
            "link {name} should be preserved"
        );
    }
    assert_eq!(summary.symlinks_copied(), 3);
}

// ==================== Permission Handling for Symlinks ====================

#[cfg(unix)]
#[test]
fn symlink_preserves_target_timestamp_not_link() {
    use std::os::unix::fs::symlink;
    use filetime::{FileTime, set_file_mtime};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Set a specific mtime on target
    let old_time = FileTime::from_unix_time(1000000000, 0);
    set_file_mtime(&target, old_time).expect("set target mtime");

    let link = source_root.join("link");
    symlink(Path::new("target.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).times(true),
    )
    .expect("copy succeeds");

    // The symlink itself is preserved
    let dest_link = dest_root.join("link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    // Target should have its mtime preserved
    let dest_target = dest_root.join("target.txt");
    let target_meta = fs::metadata(&dest_target).expect("target meta");
    let target_mtime = FileTime::from_last_modification_time(&target_meta);
    assert_eq!(target_mtime, old_time);
}

#[cfg(unix)]
#[test]
fn symlink_to_directory_with_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a directory with specific permissions
    let target_dir = source_root.join("target_dir");
    fs::create_dir(&target_dir).expect("create target dir");
    fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o755)).expect("set perms");
    fs::write(target_dir.join("file.txt"), b"data").expect("write file");

    // Create symlink to directory
    let link = source_root.join("dir_link");
    symlink(Path::new("target_dir"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).permissions(true),
    )
    .expect("copy succeeds");

    // Symlink should be preserved
    let dest_link = dest_root.join("dir_link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    // Original directory should have permissions preserved
    let dest_dir = dest_root.join("target_dir");
    let dir_perms = fs::metadata(&dest_dir).expect("dir meta").permissions();
    assert_eq!(dir_perms.mode() & 0o777, 0o755);
}

#[cfg(unix)]
#[test]
fn symlink_omit_link_times_option() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    let link = source_root.join("link");
    symlink(Path::new("target.txt"), &link).expect("create symlink");

    // Add a small delay so times differ
    thread::sleep(Duration::from_millis(50));

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // With omit_link_times, symlink times should not be preserved
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .links(true)
            .times(true)
            .omit_link_times(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());

    // The symlink should exist and work
    assert!(dest_link.exists());
}

// ==================== Edge Cases with Special Characters ====================

#[cfg(unix)]
#[test]
fn symlink_with_spaces_in_name() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target file.txt");
    fs::write(&target, b"content").expect("write target");

    let link = source_root.join("link with spaces");
    symlink(Path::new("target file.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("link with spaces");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
    assert_eq!(fs::read_link(&dest_link).expect("read"), Path::new("target file.txt"));
}

#[cfg(unix)]
#[test]
fn symlink_with_unicode_characters() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target_\u{00e9}\u{00e0}\u{00fc}.txt");
    fs::write(&target, b"unicode content").expect("write target");

    let link = source_root.join("link_\u{00f1}");
    symlink(Path::new("target_\u{00e9}\u{00e0}\u{00fc}.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("link_\u{00f1}");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link).expect("read"),
        Path::new("target_\u{00e9}\u{00e0}\u{00fc}.txt")
    );
}

#[cfg(unix)]
#[test]
fn symlink_target_with_newline() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a target with newline in name (valid on Unix)
    let target = source_root.join("target\nwith\nnewlines.txt");
    fs::write(&target, b"content").expect("write target");

    let link = source_root.join("link");
    symlink(Path::new("target\nwith\nnewlines.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
    assert_eq!(
        fs::read_link(&dest_link).expect("read"),
        Path::new("target\nwith\nnewlines.txt")
    );
}

// ==================== Symlink to . and .. ====================

#[cfg(unix)]
#[test]
fn symlink_to_current_directory() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    // Create symlink pointing to current directory
    let link = source_root.join("dot_link");
    symlink(Path::new("."), &link).expect("create dot symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("dot_link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
    assert_eq!(fs::read_link(&dest_link).expect("read"), Path::new("."));
}

#[cfg(unix)]
#[test]
fn symlink_to_parent_within_safe_boundary() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    let subdir = source_root.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");

    // Create target in source root
    fs::write(source_root.join("target.txt"), b"content").expect("write target");

    // Create symlink in subdir pointing to parent
    let link = subdir.join("parent_link");
    symlink(Path::new(".."), &link).expect("create parent symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // This symlink points to parent but stays within the tree
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true).safe_links(true),
    )
    .expect("copy succeeds");

    let dest_link = dest_root.join("subdir/parent_link");
    assert!(fs::symlink_metadata(&dest_link).expect("meta").file_type().is_symlink());
    assert_eq!(fs::read_link(&dest_link).expect("read"), Path::new(".."));
}

// ==================== Hard Links to Symlinks ====================

#[cfg(unix)]
#[test]
fn hard_link_to_symlink_preserved() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Create symlink
    let link1 = source_root.join("link1");
    symlink(Path::new("target.txt"), &link1).expect("create symlink");

    // Create hard link to the symlink
    let link2 = source_root.join("link2");
    fs::hard_link(&link1, &link2).expect("create hard link to symlink");

    // Verify they share the same inode
    let meta1 = fs::symlink_metadata(&link1).expect("meta1");
    let meta2 = fs::symlink_metadata(&link2).expect("meta2");
    assert_eq!(meta1.ino(), meta2.ino());

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().links(true).hard_links(true),
        )
        .expect("copy succeeds");

    // Both should be symlinks in destination
    let dest_link1 = dest_root.join("link1");
    let dest_link2 = dest_root.join("link2");

    assert!(fs::symlink_metadata(&dest_link1).expect("meta").file_type().is_symlink());
    assert!(fs::symlink_metadata(&dest_link2).expect("meta").file_type().is_symlink());

    // They should share the same inode (hard link preserved)
    let dest_meta1 = fs::symlink_metadata(&dest_link1).expect("dest meta1");
    let dest_meta2 = fs::symlink_metadata(&dest_link2).expect("dest meta2");
    assert_eq!(dest_meta1.ino(), dest_meta2.ino());

    // Both should point to the same target
    assert_eq!(fs::read_link(&dest_link1).expect("read"), Path::new("target.txt"));
    assert_eq!(fs::read_link(&dest_link2).expect("read"), Path::new("target.txt"));

    assert!(summary.hard_links_created() >= 1);
    assert_eq!(summary.symlinks_copied(), 2);
}

// ==================== Symlink Size Handling ====================

#[cfg(unix)]
#[test]
fn symlink_size_is_target_path_length() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("src");
    fs::create_dir_all(&source_root).expect("create source");

    let target = source_root.join("target.txt");
    fs::write(&target, b"content").expect("write target");

    // Create symlink with specific target path length
    let link = source_root.join("link");
    symlink(Path::new("target.txt"), &link).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().links(true),
    )
    .expect("copy succeeds");

    // Symlink size should be the length of the target path
    let source_link_meta = fs::symlink_metadata(&link).expect("source meta");
    let dest_link = dest_root.join("link");
    let dest_link_meta = fs::symlink_metadata(&dest_link).expect("dest meta");

    // The size of a symlink is the length of its target path
    assert_eq!(source_link_meta.size(), dest_link_meta.size());
    assert_eq!(dest_link_meta.size(), "target.txt".len() as u64);
}
