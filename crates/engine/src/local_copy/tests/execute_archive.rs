// Comprehensive tests for --archive (-a) flag behavior matching upstream rsync.
//
// In upstream rsync, -a is equivalent to -rlptgoD:
//   -r  recursive
//   -l  links (symlink preservation)
//   -p  perms (permission preservation)
//   -t  times (timestamp preservation)
//   -g  group preservation
//   -o  owner preservation
//   -D  devices + specials
//
// Key behaviors verified:
//   1. archive() preset enables all expected options
//   2. archive() does NOT enable hardlinks, ACLs, xattrs, sparse, checksum, etc.
//   3. Individual options can be disabled after archive() (--no-OPTION pattern)
//   4. Functional file transfer with archive mode preserves metadata
//   5. Builder .archive() and fluent API .archive_options() produce equivalent results

// =============================================================================
// Section 1: Builder archive() preset enables correct options
// =============================================================================

#[test]
fn archive_builder_enables_recursive() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.recursive_enabled(),
        "archive must enable recursive (-r)"
    );
}

#[test]
fn archive_builder_enables_symlinks() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.links_enabled(),
        "archive must enable symlink preservation (-l)"
    );
}

#[test]
fn archive_builder_enables_permissions() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.preserve_permissions(),
        "archive must enable permission preservation (-p)"
    );
}

#[test]
fn archive_builder_enables_times() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.preserve_times(),
        "archive must enable time preservation (-t)"
    );
}

#[test]
fn archive_builder_enables_group() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.preserve_group(),
        "archive must enable group preservation (-g)"
    );
}

#[test]
fn archive_builder_enables_owner() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.preserve_owner(),
        "archive must enable owner preservation (-o)"
    );
}

#[test]
fn archive_builder_enables_devices() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.devices_enabled(),
        "archive must enable device preservation (part of -D)"
    );
}

#[test]
fn archive_builder_enables_specials() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        options.specials_enabled(),
        "archive must enable special file preservation (part of -D)"
    );
}

#[test]
fn archive_builder_enables_all_components_together() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");

    assert!(options.recursive_enabled(), "recursive (-r)");
    assert!(options.links_enabled(), "links (-l)");
    assert!(options.preserve_permissions(), "perms (-p)");
    assert!(options.preserve_times(), "times (-t)");
    assert!(options.preserve_group(), "group (-g)");
    assert!(options.preserve_owner(), "owner (-o)");
    assert!(options.devices_enabled(), "devices (part of -D)");
    assert!(options.specials_enabled(), "specials (part of -D)");
}

// =============================================================================
// Section 2: archive() does NOT enable options outside -rlptgoD
// =============================================================================

#[test]
fn archive_does_not_enable_hard_links() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.hard_links_enabled(),
        "archive must NOT enable hardlinks (-H is separate)"
    );
}

#[test]
fn archive_does_not_enable_sparse() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.sparse_enabled(),
        "archive must NOT enable sparse (-S is separate)"
    );
}

#[test]
fn archive_does_not_enable_checksum() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.checksum_enabled(),
        "archive must NOT enable checksum (-c is separate)"
    );
}

#[test]
fn archive_does_not_enable_compress() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.compress_enabled(),
        "archive must NOT enable compression (-z is separate)"
    );
}

#[test]
fn archive_does_not_enable_delete() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.delete_extraneous(),
        "archive must NOT enable delete (--delete is separate)"
    );
}

#[test]
fn archive_does_not_enable_numeric_ids() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.numeric_ids_enabled(),
        "archive must NOT enable numeric-ids (--numeric-ids is separate)"
    );
}

#[test]
fn archive_does_not_enable_omit_dir_times() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.omit_dir_times_enabled(),
        "archive must NOT enable omit-dir-times (-O is separate)"
    );
}

#[test]
fn archive_does_not_enable_inplace() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.inplace_enabled(),
        "archive must NOT enable inplace (--inplace is separate)"
    );
}

#[test]
fn archive_does_not_enable_partial() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.partial_enabled(),
        "archive must NOT enable partial (--partial is separate)"
    );
}

#[test]
fn archive_does_not_enable_copy_links() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.copy_links_enabled(),
        "archive must NOT enable copy-links (-L is separate)"
    );
}

#[test]
fn archive_does_not_enable_relative_paths() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.relative_paths_enabled(),
        "archive must NOT enable relative paths (-R is separate)"
    );
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn archive_does_not_enable_xattrs() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.preserve_xattrs(),
        "archive must NOT enable xattrs (-X is separate)"
    );
}

#[cfg(all(unix, feature = "acl"))]
#[test]
fn archive_does_not_enable_acls() {
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid archive options");
    assert!(
        !options.acls_enabled(),
        "archive must NOT enable ACLs (-A is separate)"
    );
}

// =============================================================================
// Section 3: --no-OPTION overrides after archive()
// =============================================================================

#[test]
fn archive_then_no_recursive_disables_recursion() {
    let options = LocalCopyOptions::builder()
        .archive()
        .recursive(false)
        .build()
        .expect("valid options");
    assert!(
        !options.recursive_enabled(),
        "-a --no-recursive must disable recursion"
    );
    // Other archive flags remain enabled
    assert!(options.links_enabled());
    assert!(options.preserve_permissions());
    assert!(options.preserve_times());
}

#[test]
fn archive_then_no_links_disables_symlink_preservation() {
    let options = LocalCopyOptions::builder()
        .archive()
        .preserve_symlinks(false)
        .build()
        .expect("valid options");
    assert!(
        !options.links_enabled(),
        "-a --no-links must disable symlink preservation"
    );
    // Other archive flags remain enabled
    assert!(options.recursive_enabled());
    assert!(options.preserve_permissions());
}

#[test]
fn archive_then_no_perms_disables_permission_preservation() {
    let options = LocalCopyOptions::builder()
        .archive()
        .preserve_permissions(false)
        .build()
        .expect("valid options");
    assert!(
        !options.preserve_permissions(),
        "-a --no-perms must disable permission preservation"
    );
    // Other archive flags remain enabled
    assert!(options.recursive_enabled());
    assert!(options.preserve_times());
    assert!(options.preserve_owner());
}

#[test]
fn archive_then_no_times_disables_time_preservation() {
    let options = LocalCopyOptions::builder()
        .archive()
        .preserve_times(false)
        .build()
        .expect("valid options");
    assert!(
        !options.preserve_times(),
        "-a --no-times must disable time preservation"
    );
    // Other archive flags remain enabled
    assert!(options.recursive_enabled());
    assert!(options.preserve_permissions());
}

#[test]
fn archive_then_no_group_disables_group_preservation() {
    let options = LocalCopyOptions::builder()
        .archive()
        .preserve_group(false)
        .build()
        .expect("valid options");
    assert!(
        !options.preserve_group(),
        "-a --no-group must disable group preservation"
    );
    // Other archive flags remain enabled
    assert!(options.preserve_owner());
    assert!(options.preserve_times());
}

#[test]
fn archive_then_no_owner_disables_owner_preservation() {
    let options = LocalCopyOptions::builder()
        .archive()
        .preserve_owner(false)
        .build()
        .expect("valid options");
    assert!(
        !options.preserve_owner(),
        "-a --no-owner must disable owner preservation"
    );
    // Other archive flags remain enabled
    assert!(options.preserve_group());
    assert!(options.preserve_times());
}

#[test]
fn archive_then_no_devices_disables_device_preservation() {
    let options = LocalCopyOptions::builder()
        .archive()
        .devices(false)
        .build()
        .expect("valid options");
    assert!(
        !options.devices_enabled(),
        "-a --no-devices must disable device preservation"
    );
    // Specials remains enabled (--no-devices only disables devices, not specials)
    assert!(options.specials_enabled());
}

#[test]
fn archive_then_no_specials_disables_special_preservation() {
    let options = LocalCopyOptions::builder()
        .archive()
        .specials(false)
        .build()
        .expect("valid options");
    assert!(
        !options.specials_enabled(),
        "-a --no-specials must disable special file preservation"
    );
    // Devices remains enabled
    assert!(options.devices_enabled());
}

#[test]
fn archive_then_no_devices_no_specials_disables_both() {
    let options = LocalCopyOptions::builder()
        .archive()
        .devices(false)
        .specials(false)
        .build()
        .expect("valid options");
    assert!(
        !options.devices_enabled(),
        "both devices and specials should be disabled"
    );
    assert!(!options.specials_enabled());
}

#[test]
fn archive_with_multiple_overrides() {
    // Equivalent to: rsync -a --no-perms --no-times --no-owner --no-group
    let options = LocalCopyOptions::builder()
        .archive()
        .preserve_permissions(false)
        .preserve_times(false)
        .preserve_owner(false)
        .preserve_group(false)
        .build()
        .expect("valid options");

    // These should all be disabled
    assert!(!options.preserve_permissions());
    assert!(!options.preserve_times());
    assert!(!options.preserve_owner());
    assert!(!options.preserve_group());

    // These should remain from archive
    assert!(options.recursive_enabled());
    assert!(options.links_enabled());
    assert!(options.devices_enabled());
    assert!(options.specials_enabled());
}

// =============================================================================
// Section 4: archive() combined with additional options
// =============================================================================

#[test]
fn archive_with_delete_is_sync_mode() {
    let options = LocalCopyOptions::builder()
        .archive()
        .delete(true)
        .build()
        .expect("valid options");
    assert!(options.recursive_enabled());
    assert!(options.delete_extraneous());
    assert!(options.links_enabled());
}

#[test]
fn archive_with_compress() {
    let options = LocalCopyOptions::builder()
        .archive()
        .compress(true)
        .build()
        .expect("valid options");
    assert!(options.compress_enabled());
    assert!(options.recursive_enabled());
}

#[test]
fn archive_with_hard_links() {
    let options = LocalCopyOptions::builder()
        .archive()
        .hard_links(true)
        .build()
        .expect("valid options");
    assert!(options.hard_links_enabled());
    assert!(options.recursive_enabled());
}

#[test]
fn archive_with_checksum() {
    let options = LocalCopyOptions::builder()
        .archive()
        .checksum(true)
        .build()
        .expect("valid options");
    assert!(options.checksum_enabled());
    assert!(options.recursive_enabled());
}

#[test]
fn archive_with_numeric_ids() {
    let options = LocalCopyOptions::builder()
        .archive()
        .numeric_ids(true)
        .build()
        .expect("valid options");
    assert!(options.numeric_ids_enabled());
    assert!(options.preserve_owner());
    assert!(options.preserve_group());
}

#[test]
fn archive_with_partial() {
    let options = LocalCopyOptions::builder()
        .archive()
        .partial(true)
        .build()
        .expect("valid options");
    assert!(options.partial_enabled());
    assert!(options.recursive_enabled());
}

// =============================================================================
// Section 5: Fluent API archive preset equivalence
// =============================================================================

#[test]
fn fluent_archive_options_match_builder_archive() {
    let from_builder = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid options");
    let from_fluent = test_helpers::presets::archive_options();

    assert_eq!(
        from_builder.recursive_enabled(),
        from_fluent.recursive_enabled()
    );
    assert_eq!(
        from_builder.links_enabled(),
        from_fluent.links_enabled()
    );
    assert_eq!(
        from_builder.preserve_permissions(),
        from_fluent.preserve_permissions()
    );
    assert_eq!(
        from_builder.preserve_times(),
        from_fluent.preserve_times()
    );
    assert_eq!(
        from_builder.preserve_group(),
        from_fluent.preserve_group()
    );
    assert_eq!(
        from_builder.preserve_owner(),
        from_fluent.preserve_owner()
    );
    assert_eq!(
        from_builder.devices_enabled(),
        from_fluent.devices_enabled()
    );
    assert_eq!(
        from_builder.specials_enabled(),
        from_fluent.specials_enabled()
    );
}

#[test]
fn fluent_archive_does_not_enable_extras() {
    let options = test_helpers::presets::archive_options();
    assert!(!options.hard_links_enabled());
    assert!(!options.sparse_enabled());
    assert!(!options.checksum_enabled());
    assert!(!options.compress_enabled());
    assert!(!options.delete_extraneous());
}

// =============================================================================
// Section 6: Functional tests -- archive mode file transfers
// =============================================================================

#[test]
fn archive_copies_single_file_content() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("hello.txt", b"archive content");

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_file_content(
        &ctx.dest.join("hello.txt"),
        b"archive content",
    );
}

#[test]
fn archive_copies_directory_recursively() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("dir1/file1.txt", b"file1");
    ctx.write_source("dir1/dir2/file2.txt", b"file2");
    ctx.write_source("top.txt", b"top");

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_file_content(&ctx.dest.join("top.txt"), b"top");
    test_helpers::assert_file_content(&ctx.dest.join("dir1/file1.txt"), b"file1");
    test_helpers::assert_file_content(
        &ctx.dest.join("dir1/dir2/file2.txt"),
        b"file2",
    );
}

#[test]
fn archive_preserves_modification_times() {
    let ctx = test_helpers::setup_copy_test();
    let mtime = test_helpers::test_time();
    test_helpers::create_file_with_mtime(
        &ctx.source.join("timed.txt"),
        b"time test",
        test_helpers::TEST_TIMESTAMP,
    );

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_mtime(&ctx.dest.join("timed.txt"), mtime);
}

#[cfg(unix)]
#[test]
fn archive_preserves_file_permissions() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("perms.txt", b"perms test");
    test_helpers::set_permissions(&ctx.source.join("perms.txt"), 0o754);
    // Set mtime to avoid skipping due to recent-write heuristics
    test_helpers::create_file_with_mtime(
        &ctx.source.join("perms.txt"),
        b"perms test",
        test_helpers::TEST_TIMESTAMP,
    );
    test_helpers::set_permissions(&ctx.source.join("perms.txt"), 0o754);

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_permissions(&ctx.dest.join("perms.txt"), 0o754);
}

#[cfg(unix)]
#[test]
fn archive_preserves_symlinks_as_links() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("target.txt", b"symlink target");
    test_helpers::create_relative_symlink("target.txt", &ctx.source.join("link.txt"));

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_is_symlink(&ctx.dest.join("link.txt"));
    test_helpers::assert_symlink_target(
        &ctx.dest.join("link.txt"),
        Path::new("target.txt"),
    );
}

#[cfg(unix)]
#[test]
fn archive_preserves_directory_permissions() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("subdir/file.txt", b"content");
    test_helpers::set_permissions(&ctx.source.join("subdir"), 0o750);

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_permissions(&ctx.dest.join("subdir"), 0o750);
}

// =============================================================================
// Section 7: Functional tests -- archive mode with overrides
// =============================================================================

#[test]
fn archive_no_times_skips_timestamp_preservation() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_file_with_mtime(
        &ctx.source.join("old.txt"),
        b"old file",
        1_400_000_000, // 2014
    );

    // archive but with times disabled
    let options = test_helpers::presets::archive_options().times(false);
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    // Destination mtime should be recent, not the 2014 timestamp
    let old_mtime = FileTime::from_unix_time(1_400_000_000, 0);
    let dest_mtime = test_helpers::get_mtime(&ctx.dest.join("old.txt"));
    assert_ne!(
        dest_mtime, old_mtime,
        "with times disabled, mtime should not be preserved"
    );
}

#[cfg(unix)]
#[test]
fn archive_no_perms_skips_permission_preservation() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_file_with_mtime(
        &ctx.source.join("restricted.txt"),
        b"restricted",
        test_helpers::TEST_TIMESTAMP,
    );
    test_helpers::set_permissions(&ctx.source.join("restricted.txt"), 0o600);

    // archive but with perms disabled
    let options = test_helpers::presets::archive_options().permissions(false);
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    let dest_perms = test_helpers::get_permissions(&ctx.dest.join("restricted.txt"));
    // With perms disabled, the destination will get umask-based permissions,
    // not the source's 0o600
    assert_ne!(
        dest_perms & 0o777,
        0o600,
        "with perms disabled, mode should not be exactly 0o600"
    );
}

#[cfg(unix)]
#[test]
fn archive_no_links_copies_symlink_targets_instead() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("real.txt", b"real content");
    test_helpers::create_relative_symlink("real.txt", &ctx.source.join("sym.txt"));

    // archive but with links disabled and copy_links enabled
    // (upstream: -a --no-links --copy-links copies symlink targets)
    let options = test_helpers::presets::archive_options()
        .links(false)
        .copy_links(true);
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    // sym.txt should be a regular file, not a symlink
    let dest_sym = ctx.dest.join("sym.txt");
    assert!(dest_sym.exists(), "sym.txt should exist");
    let meta = fs::symlink_metadata(&dest_sym).expect("metadata");
    assert!(
        !meta.file_type().is_symlink(),
        "with links disabled and copy_links enabled, sym.txt should be a regular file"
    );
    assert_eq!(
        fs::read(&dest_sym).expect("read"),
        b"real content",
        "sym.txt should contain the target content"
    );
}

#[test]
fn archive_no_recursive_disables_recursion_in_options() {
    // Verify that -a --no-recursive produces options with recursion disabled.
    // The engine with recursive(false) skips subdirectory traversal entirely,
    // which is tested separately in execute_directories tests.
    let options = test_helpers::presets::archive_options().recursive(false);
    assert!(
        !options.recursive_enabled(),
        "recursion should be disabled"
    );
    // All other archive components should remain
    assert!(options.links_enabled());
    assert!(options.preserve_permissions());
    assert!(options.preserve_times());
    #[cfg(unix)]
    {
        assert!(options.preserve_group());
        assert!(options.preserve_owner());
    }
    assert!(options.devices_enabled());
    assert!(options.specials_enabled());
}

// =============================================================================
// Section 8: Archive mode summary verification
// =============================================================================

#[test]
fn archive_copies_multiple_files_and_reports_summary() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("a.txt", b"aaa");
    ctx.write_source("b.txt", b"bbb");
    ctx.write_source("c.txt", b"ccc");

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::SummaryAssertions::new(&summary)
        .files_copied(3)
        .assert();
}

#[test]
fn archive_idempotent_second_run_copies_nothing() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("data.txt", b"data");
    test_helpers::create_file_with_mtime(
        &ctx.source.join("data.txt"),
        b"data",
        test_helpers::TEST_TIMESTAMP,
    );

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();

    // First run: copies the file
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options.clone())
        .expect("first copy");

    // Second run: should copy nothing (files match)
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("second copy");

    test_helpers::SummaryAssertions::new(&summary)
        .files_copied(0)
        .assert();
}

// =============================================================================
// Section 9: Builder sync() and backup_preset() verify archive as base
// =============================================================================

#[test]
fn sync_preset_includes_all_archive_components() {
    let options = LocalCopyOptions::builder()
        .sync()
        .build()
        .expect("valid sync options");

    // All archive components must be present
    assert!(options.recursive_enabled());
    assert!(options.links_enabled());
    assert!(options.preserve_permissions());
    assert!(options.preserve_times());
    assert!(options.preserve_group());
    assert!(options.preserve_owner());
    assert!(options.devices_enabled());
    assert!(options.specials_enabled());
    // Plus delete
    assert!(options.delete_extraneous());
}

#[test]
fn backup_preset_includes_all_archive_components() {
    let options = LocalCopyOptions::builder()
        .backup_preset()
        .build()
        .expect("valid backup options");

    // All archive components must be present
    assert!(options.recursive_enabled());
    assert!(options.links_enabled());
    assert!(options.preserve_permissions());
    assert!(options.preserve_times());
    assert!(options.preserve_group());
    assert!(options.preserve_owner());
    assert!(options.devices_enabled());
    assert!(options.specials_enabled());
    // Plus hard links and partial
    assert!(options.hard_links_enabled());
    assert!(options.partial_enabled());
}

// =============================================================================
// Section 10: Edge cases
// =============================================================================

#[test]
fn archive_followed_by_archive_is_idempotent() {
    // Calling archive() twice should produce same result as once
    let single = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid");
    let double = LocalCopyOptions::builder()
        .archive()
        .archive()
        .build()
        .expect("valid");

    assert_eq!(single.recursive_enabled(), double.recursive_enabled());
    assert_eq!(single.links_enabled(), double.links_enabled());
    assert_eq!(single.preserve_permissions(), double.preserve_permissions());
    assert_eq!(single.preserve_times(), double.preserve_times());
    assert_eq!(single.preserve_group(), double.preserve_group());
    assert_eq!(single.preserve_owner(), double.preserve_owner());
    assert_eq!(single.devices_enabled(), double.devices_enabled());
    assert_eq!(single.specials_enabled(), double.specials_enabled());
}

#[test]
fn archive_does_not_override_previously_set_delete() {
    // If delete was set before archive(), it should remain
    let options = LocalCopyOptions::builder()
        .delete(true)
        .archive()
        .build()
        .expect("valid");
    assert!(
        options.delete_extraneous(),
        "archive should not reset delete flag"
    );
}

#[test]
fn archive_does_not_override_previously_set_compress() {
    let options = LocalCopyOptions::builder()
        .compress(true)
        .archive()
        .build()
        .expect("valid");
    assert!(
        options.compress_enabled(),
        "archive should not reset compress flag"
    );
}

#[test]
fn disabling_all_archive_components_produces_minimal_options() {
    let options = LocalCopyOptions::builder()
        .archive()
        .recursive(false)
        .preserve_symlinks(false)
        .preserve_permissions(false)
        .preserve_times(false)
        .preserve_group(false)
        .preserve_owner(false)
        .devices(false)
        .specials(false)
        .build()
        .expect("valid");

    assert!(!options.recursive_enabled());
    assert!(!options.links_enabled());
    assert!(!options.preserve_permissions());
    assert!(!options.preserve_times());
    assert!(!options.preserve_group());
    assert!(!options.preserve_owner());
    assert!(!options.devices_enabled());
    assert!(!options.specials_enabled());
}

#[test]
fn archive_with_empty_source_directory() {
    let ctx = test_helpers::setup_copy_test();
    // Source directory exists but is empty

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::SummaryAssertions::new(&summary)
        .files_copied(0)
        .assert();
}

#[test]
fn archive_preserves_deeply_nested_structure() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("a/b/c/d/e/deep.txt", b"deep");

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_file_content(&ctx.dest.join("a/b/c/d/e/deep.txt"), b"deep");
}

#[cfg(unix)]
#[test]
fn archive_preserves_multiple_symlinks_and_targets() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("target1.txt", b"t1");
    ctx.write_source("target2.txt", b"t2");
    test_helpers::create_relative_symlink("target1.txt", &ctx.source.join("link1.txt"));
    test_helpers::create_relative_symlink("target2.txt", &ctx.source.join("link2.txt"));

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_is_symlink(&ctx.dest.join("link1.txt"));
    test_helpers::assert_is_symlink(&ctx.dest.join("link2.txt"));
    test_helpers::assert_symlink_target(
        &ctx.dest.join("link1.txt"),
        Path::new("target1.txt"),
    );
    test_helpers::assert_symlink_target(
        &ctx.dest.join("link2.txt"),
        Path::new("target2.txt"),
    );
}

#[cfg(unix)]
#[test]
fn archive_preserves_mixed_permissions_across_files() {
    let ctx = test_helpers::setup_copy_test();
    test_helpers::create_file_with_mtime(
        &ctx.source.join("exec.sh"),
        b"#!/bin/sh\necho hi",
        test_helpers::TEST_TIMESTAMP,
    );
    test_helpers::set_permissions(&ctx.source.join("exec.sh"), 0o755);

    test_helpers::create_file_with_mtime(
        &ctx.source.join("readonly.txt"),
        b"readonly",
        test_helpers::TEST_TIMESTAMP,
    );
    test_helpers::set_permissions(&ctx.source.join("readonly.txt"), 0o444);

    test_helpers::create_file_with_mtime(
        &ctx.source.join("private.key"),
        b"secret",
        test_helpers::TEST_TIMESTAMP,
    );
    test_helpers::set_permissions(&ctx.source.join("private.key"), 0o600);

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_permissions(&ctx.dest.join("exec.sh"), 0o755);
    test_helpers::assert_permissions(&ctx.dest.join("readonly.txt"), 0o444);
    test_helpers::assert_permissions(&ctx.dest.join("private.key"), 0o600);
}

#[test]
fn archive_preserves_timestamps_across_multiple_files() {
    let ctx = test_helpers::setup_copy_test();

    let t1 = 1_500_000_000;
    let t2 = 1_600_000_000;
    let t3 = 1_700_000_000;

    test_helpers::create_file_with_mtime(&ctx.source.join("old.txt"), b"old", t1);
    test_helpers::create_file_with_mtime(&ctx.source.join("mid.txt"), b"mid", t2);
    test_helpers::create_file_with_mtime(&ctx.source.join("new.txt"), b"new", t3);

    let options = test_helpers::presets::archive_options();
    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy");

    test_helpers::assert_mtime(&ctx.dest.join("old.txt"), FileTime::from_unix_time(t1, 0));
    test_helpers::assert_mtime(&ctx.dest.join("mid.txt"), FileTime::from_unix_time(t2, 0));
    test_helpers::assert_mtime(&ctx.dest.join("new.txt"), FileTime::from_unix_time(t3, 0));
}
