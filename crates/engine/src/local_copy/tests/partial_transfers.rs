// Integration tests for partial file transfer functionality.

use std::env;

#[test]
fn partial_mode_delete_does_not_preserve_temp_files() {
    let mode = PartialMode::from_options(false, None);
    assert_eq!(mode, PartialMode::Delete);
    assert!(!mode.preserves_on_failure());
}

#[test]
fn partial_mode_keep_preserves_temp_files() {
    let mode = PartialMode::from_options(true, None);
    assert_eq!(mode, PartialMode::Keep);
    assert!(mode.preserves_on_failure());
}

#[test]
fn partial_mode_partial_dir_preserves_in_separate_directory() {
    let mode = PartialMode::from_options(true, Some(".partial".into()));
    assert!(matches!(mode, PartialMode::PartialDir(_)));
    assert!(mode.preserves_on_failure());
    assert!(mode.uses_partial_dir());
}

#[test]
fn partial_mode_respects_rsync_partial_dir_env() {
    // Safety: This test is single-threaded and we restore the environment after
    unsafe {
        env::set_var("RSYNC_PARTIAL_DIR", "/tmp/test-partial");
    }
    let mode = PartialMode::from_options(true, None);
    unsafe {
        env::remove_var("RSYNC_PARTIAL_DIR");
    }

    assert!(matches!(mode, PartialMode::PartialDir(_)));
    assert_eq!(
        mode.partial_dir_path(),
        Some(Path::new("/tmp/test-partial"))
    );
}

#[test]
fn partial_mode_explicit_dir_overrides_env_var() {
    // Safety: This test is single-threaded and we restore the environment after
    unsafe {
        env::set_var("RSYNC_PARTIAL_DIR", "/tmp/env-partial");
    }
    let mode = PartialMode::from_options(true, Some("/tmp/explicit-partial".into()));
    unsafe {
        env::remove_var("RSYNC_PARTIAL_DIR");
    }

    assert_eq!(
        mode.partial_dir_path(),
        Some(Path::new("/tmp/explicit-partial"))
    );
}

#[test]
fn partial_file_manager_finds_keep_mode_partial() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("file.txt");
    let partial = dir.path().join(".rsync-partial-file.txt");

    // Create a partial file
    fs::write(&partial, b"partial content").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::Keep);
    let found = manager.find_basis(&dest).expect("find_basis");

    assert_eq!(found, Some(partial));
}

#[test]
fn partial_file_manager_finds_partial_dir_mode_partial() {
    let dir = tempdir().expect("tempdir");
    let partial_dir = dir.path().join(".partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    let dest = dir.path().join("file.txt");
    let partial = partial_dir.join("file.txt");

    // Create a partial file in the partial directory
    fs::write(&partial, b"partial content").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir));
    let found = manager.find_basis(&dest).expect("find_basis");

    assert_eq!(found, Some(partial));
}

#[test]
fn partial_file_manager_returns_none_when_no_partial_exists() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("file.txt");

    let manager = PartialFileManager::new(PartialMode::Keep);
    let found = manager.find_basis(&dest).expect("find_basis");

    assert_eq!(found, None);
}

#[test]
fn partial_file_manager_cleanup_removes_keep_mode_partial() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("file.txt");
    let partial = dir.path().join(".rsync-partial-file.txt");

    // Create a partial file
    fs::write(&partial, b"partial content").expect("write partial");
    assert!(partial.exists());

    let manager = PartialFileManager::new(PartialMode::Keep);
    manager.cleanup_partial(&dest).expect("cleanup");

    assert!(!partial.exists());
}

#[test]
fn partial_file_manager_cleanup_removes_partial_dir_mode_partial() {
    let dir = tempdir().expect("tempdir");
    let partial_dir = dir.path().join(".partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    let dest = dir.path().join("file.txt");
    let partial = partial_dir.join("file.txt");

    // Create a partial file
    fs::write(&partial, b"partial content").expect("write partial");
    assert!(partial.exists());

    let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir));
    manager.cleanup_partial(&dest).expect("cleanup");

    assert!(!partial.exists());
}

#[test]
fn partial_file_manager_cleanup_succeeds_when_no_partial() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("file.txt");

    let manager = PartialFileManager::new(PartialMode::Keep);
    let result = manager.cleanup_partial(&dest);

    assert!(result.is_ok());
}

#[test]
fn partial_dir_relative_path_resolved_per_destination() {
    let base = tempdir().expect("tempdir");

    // Create two different destination directories
    let dest1_dir = base.path().join("dest1");
    let dest2_dir = base.path().join("dest2");
    fs::create_dir(&dest1_dir).expect("create dest1");
    fs::create_dir(&dest2_dir).expect("create dest2");

    // Create partial directories relative to each destination
    let partial1_dir = dest1_dir.join(".partial");
    let partial2_dir = dest2_dir.join(".partial");
    fs::create_dir(&partial1_dir).expect("create partial1");
    fs::create_dir(&partial2_dir).expect("create partial2");

    // Create partial files in each
    let dest1 = dest1_dir.join("file.txt");
    let dest2 = dest2_dir.join("file.txt");
    let partial1 = partial1_dir.join("file.txt");
    let partial2 = partial2_dir.join("file.txt");

    fs::write(&partial1, b"partial1").expect("write partial1");
    fs::write(&partial2, b"partial2").expect("write partial2");

    // Use relative partial dir
    let manager = PartialFileManager::new(PartialMode::PartialDir(".partial".into()));

    // Should find correct partial for each destination
    let found1 = manager.find_basis(&dest1).expect("find_basis 1");
    let found2 = manager.find_basis(&dest2).expect("find_basis 2");

    assert_eq!(found1, Some(partial1));
    assert_eq!(found2, Some(partial2));
}

#[test]
fn partial_dir_absolute_path_is_global() {
    let base = tempdir().expect("tempdir");
    let global_partial = base.path().join("global-partial");
    fs::create_dir(&global_partial).expect("create global partial");

    // Create destination directories
    let dest1_dir = base.path().join("dest1");
    let dest2_dir = base.path().join("dest2");
    fs::create_dir(&dest1_dir).expect("create dest1");
    fs::create_dir(&dest2_dir).expect("create dest2");

    let dest1 = dest1_dir.join("file.txt");
    let dest2 = dest2_dir.join("file.txt");

    // Both should use the same global partial file
    let partial = global_partial.join("file.txt");
    fs::write(&partial, b"global partial").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::PartialDir(global_partial.clone()));

    let found1 = manager.find_basis(&dest1).expect("find_basis 1");
    let found2 = manager.find_basis(&dest2).expect("find_basis 2");

    // Both should find the same global partial file
    assert_eq!(found1, Some(partial.clone()));
    assert_eq!(found2, Some(partial));
}

#[test]
fn partial_dir_handles_nested_directory_structures() {
    let base = tempdir().expect("tempdir");
    let partial_dir = base.path().join(".partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    // Nested destination
    let nested_dest = base.path().join("a").join("b").join("c").join("file.txt");
    fs::create_dir_all(nested_dest.parent().unwrap()).expect("create nested dirs");

    // Partial should be in the relative partial dir
    let nested_partial_dir = base.path().join("a").join("b").join("c").join(".partial");
    fs::create_dir(&nested_partial_dir).expect("create nested partial dir");
    let partial = nested_partial_dir.join("file.txt");
    fs::write(&partial, b"nested partial").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::PartialDir(".partial".into()));
    let found = manager.find_basis(&nested_dest).expect("find_basis");

    assert_eq!(found, Some(partial));
}

#[test]
fn local_copy_options_partial_enabled_returns_true_when_partial_set() {
    let opts = LocalCopyOptions::new().partial(true);
    assert!(opts.partial_enabled());
}

#[test]
fn local_copy_options_partial_enabled_returns_true_when_partial_dir_set() {
    let opts = LocalCopyOptions::new().with_partial_directory(Some("/tmp/partial"));
    assert!(opts.partial_enabled());
}

#[test]
fn local_copy_options_partial_directory_path_returns_configured_path() {
    let opts = LocalCopyOptions::new().with_partial_directory(Some("/tmp/partial"));
    assert_eq!(
        opts.partial_directory_path(),
        Some(Path::new("/tmp/partial"))
    );
}

#[test]
fn local_copy_options_partial_directory_path_returns_none_by_default() {
    let opts = LocalCopyOptions::new();
    assert_eq!(opts.partial_directory_path(), None);
}

#[test]
fn partial_file_transfer_workflow() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("file.txt");
    let partial = dir.path().join(".rsync-partial-file.txt");

    // Simulate a partial transfer
    fs::write(&partial, b"partial content from interrupted transfer").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::Keep);

    // Step 1: Find basis file for resume
    let basis = manager.find_basis(&dest).expect("find_basis");
    assert_eq!(basis, Some(partial.clone()));

    // Step 2: Transfer would complete here...
    // Simulate by creating the final file
    fs::write(&dest, b"complete content").expect("write dest");

    // Step 3: Clean up partial file after success
    manager.cleanup_partial(&dest).expect("cleanup");
    assert!(!partial.exists());
    assert!(dest.exists());
}

#[test]
fn partial_dir_transfer_workflow() {
    let dir = tempdir().expect("tempdir");
    let partial_dir = dir.path().join(".rsync-partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    let dest = dir.path().join("file.txt");
    let partial = partial_dir.join("file.txt");

    // Simulate a partial transfer in partial dir
    fs::write(&partial, b"partial content").expect("write partial");

    let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir.clone()));

    // Step 1: Find basis file
    let basis = manager.find_basis(&dest).expect("find_basis");
    assert_eq!(basis, Some(partial.clone()));

    // Step 2: Transfer completes
    fs::write(&dest, b"complete content").expect("write dest");

    // Step 3: Clean up partial dir file
    manager.cleanup_partial(&dest).expect("cleanup");
    assert!(!partial.exists());
    assert!(dest.exists());

    // Partial directory itself should still exist
    assert!(partial_dir.exists());
}

#[test]
fn integration_test_partial_mode_with_local_copy() {
    let dir = tempdir().expect("tempdir");
    let source = dir.path().join("source.txt");
    let dest = dir.path().join("dest.txt");

    // Create source file
    fs::write(&source, b"source content").expect("write source");

    // Execute copy with partial mode enabled
    let opts = LocalCopyOptions::new().partial(true);
    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("create plan");
    let result = plan.execute_with_options(LocalCopyExecution::Apply, opts);

    assert!(result.is_ok());
    assert!(dest.exists());

    // Verify content
    let content = fs::read_to_string(&dest).expect("read dest");
    assert_eq!(content, "source content");
}

#[test]
fn integration_test_partial_dir_mode_with_local_copy() {
    let dir = tempdir().expect("tempdir");
    let partial_dir = dir.path().join(".partial");
    fs::create_dir(&partial_dir).expect("create partial dir");

    let source = dir.path().join("source.txt");
    let dest = dir.path().join("dest.txt");

    // Create source file
    fs::write(&source, b"source content").expect("write source");

    // Execute copy with partial-dir mode
    let opts = LocalCopyOptions::new().with_partial_directory(Some(partial_dir));
    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("create plan");
    let result = plan.execute_with_options(LocalCopyExecution::Apply, opts);

    assert!(result.is_ok());
    assert!(dest.exists());

    // Verify content
    let content = fs::read_to_string(&dest).expect("read dest");
    assert_eq!(content, "source content");
}

#[test]
fn partial_mode_manager_is_cloneable() {
    let mode = PartialMode::Keep;
    let manager = PartialFileManager::new(mode.clone());
    let cloned = manager.clone();

    assert_eq!(manager.mode(), cloned.mode());
}

#[test]
fn partial_mode_is_cloneable() {
    let mode1 = PartialMode::PartialDir("/tmp/partial".into());
    let mode2 = mode1.clone();

    assert_eq!(mode1, mode2);
}
