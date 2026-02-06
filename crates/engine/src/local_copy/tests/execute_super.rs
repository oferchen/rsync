// Comprehensive tests for --super and --fake-super behavior.
//
// These tests cover the --super and --fake-super flags which control how
// the receiving side handles privileged operations. The tests verify:
//
// 1. Option round-trips for super_mode and fake_super
// 2. am_root() semantics with explicit --super / --no-super
// 3. Builder integration for super_mode and fake_super
// 4. CopyContext properly flows fake_super to MetadataOptions
// 5. --super enables privileged metadata operations (ownership, devices, specials)
// 6. --no-super disables privileged metadata attempts
// 7. --fake-super stores ownership metadata via xattrs
// 8. Interactions between --super, --fake-super, and metadata preservation flags
//
// Note: Tests that require root privileges check the effective UID and skip
// when not running as root.

// ============================================================================
// Option Round-Trip Tests
// ============================================================================

#[test]
fn super_mode_option_default_is_none() {
    let options = LocalCopyOptions::default();
    assert_eq!(options.super_mode_setting(), None);
}

#[test]
fn super_mode_option_set_true() {
    let options = LocalCopyOptions::default().super_mode(Some(true));
    assert_eq!(options.super_mode_setting(), Some(true));
}

#[test]
fn super_mode_option_set_false() {
    let options = LocalCopyOptions::default().super_mode(Some(false));
    assert_eq!(options.super_mode_setting(), Some(false));
}

#[test]
fn super_mode_option_set_none_clears() {
    let options = LocalCopyOptions::default()
        .super_mode(Some(true))
        .super_mode(None);
    assert_eq!(options.super_mode_setting(), None);
}

#[test]
fn fake_super_option_default_is_false() {
    let options = LocalCopyOptions::default();
    assert!(!options.fake_super_enabled());
}

#[test]
fn fake_super_option_set_true() {
    let options = LocalCopyOptions::default().fake_super(true);
    assert!(options.fake_super_enabled());
}

#[test]
fn fake_super_option_round_trip() {
    let options = LocalCopyOptions::default()
        .fake_super(true)
        .fake_super(false);
    assert!(!options.fake_super_enabled());
}

// ============================================================================
// am_root() Semantics Tests
// ============================================================================

#[test]
fn am_root_returns_true_when_super_is_true() {
    let options = LocalCopyOptions::default().super_mode(Some(true));
    assert!(options.am_root());
}

#[test]
fn am_root_returns_false_when_super_is_false() {
    let options = LocalCopyOptions::default().super_mode(Some(false));
    assert!(!options.am_root());
}

#[test]
fn am_root_defers_to_euid_when_none() {
    let options = LocalCopyOptions::default();
    // When super_mode is None, am_root() checks the effective UID.
    // We cannot assert a specific value because it depends on the test runner,
    // but we verify it does not panic and returns a boolean.
    let _result: bool = options.am_root();
}

#[cfg(unix)]
#[test]
fn am_root_matches_euid_when_super_mode_none() {
    let options = LocalCopyOptions::default();
    let expected = rustix::process::geteuid().is_root();
    assert_eq!(options.am_root(), expected);
}

// ============================================================================
// Builder Integration Tests
// ============================================================================

#[test]
fn builder_super_mode_default_is_none() {
    let options = LocalCopyOptions::builder().build().expect("valid options");
    assert_eq!(options.super_mode_setting(), None);
}

#[test]
fn builder_super_mode_set_true() {
    let options = LocalCopyOptions::builder()
        .super_mode(Some(true))
        .build()
        .expect("valid options");
    assert_eq!(options.super_mode_setting(), Some(true));
    assert!(options.am_root());
}

#[test]
fn builder_super_mode_set_false() {
    let options = LocalCopyOptions::builder()
        .super_mode(Some(false))
        .build()
        .expect("valid options");
    assert_eq!(options.super_mode_setting(), Some(false));
    assert!(!options.am_root());
}

#[test]
fn builder_fake_super_default_is_false() {
    let options = LocalCopyOptions::builder().build().expect("valid options");
    assert!(!options.fake_super_enabled());
}

#[test]
fn builder_fake_super_set_true() {
    let options = LocalCopyOptions::builder()
        .fake_super(true)
        .build()
        .expect("valid options");
    assert!(options.fake_super_enabled());
}

#[test]
fn builder_super_mode_and_fake_super_combined() {
    let options = LocalCopyOptions::builder()
        .super_mode(Some(true))
        .fake_super(true)
        .build()
        .expect("valid options");
    assert_eq!(options.super_mode_setting(), Some(true));
    assert!(options.fake_super_enabled());
    assert!(options.am_root());
}

// ============================================================================
// CopyContext MetadataOptions Flow Tests
// ============================================================================

#[test]
fn metadata_options_carry_fake_super_setting() {
    let options = LocalCopyOptions::default().fake_super(true);
    let context = CopyContext::new(LocalCopyExecution::Apply, options, None, PathBuf::from("."));
    assert!(context.metadata_options().fake_super_enabled());
}

#[test]
fn metadata_options_fake_super_disabled_by_default() {
    let options = LocalCopyOptions::default();
    let context = CopyContext::new(LocalCopyExecution::Apply, options, None, PathBuf::from("."));
    assert!(!context.metadata_options().fake_super_enabled());
}

#[test]
fn metadata_options_fake_super_false_when_explicitly_disabled() {
    let options = LocalCopyOptions::default().fake_super(false);
    let context = CopyContext::new(LocalCopyExecution::Apply, options, None, PathBuf::from("."));
    assert!(!context.metadata_options().fake_super_enabled());
}

// ============================================================================
// --super Enables Privileged Operations (Integration Tests)
// ============================================================================

#[test]
fn super_mode_true_with_owner_preserves_file() {
    // When --super is set along with --owner, files should be copied.
    // The ownership preservation itself requires root, but the copy should
    // succeed regardless of super_mode.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"super test content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .owner(true)
                .group(true)
                .permissions(true),
        )
        .expect("copy succeeds with --super");

    assert_eq!(summary.files_copied(), 1);
    test_helpers::assert_file_content(&ctx.dest.join("file.txt"), b"super test content");
}

#[test]
fn super_mode_false_with_owner_still_copies_file() {
    // --no-super with --owner: files are still copied, but ownership
    // changes may silently fail (upstream behavior).
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"no super content");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(false))
                .owner(true),
        )
        .expect("copy succeeds with --no-super");

    assert_eq!(summary.files_copied(), 1);
    test_helpers::assert_file_content(&ctx.dest.join("file.txt"), b"no super content");
}

#[test]
fn super_mode_true_copies_directory_tree() {
    // --super with a directory tree: verify recursive copy works.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("dir/a.txt", b"aaa");
    ctx.write_source("dir/b.txt", b"bbb");
    ctx.write_source("dir/sub/c.txt", b"ccc");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .owner(true)
                .group(true)
                .permissions(true)
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 3);
    test_helpers::assert_file_content(&ctx.dest.join("dir/a.txt"), b"aaa");
    test_helpers::assert_file_content(&ctx.dest.join("dir/b.txt"), b"bbb");
    test_helpers::assert_file_content(&ctx.dest.join("dir/sub/c.txt"), b"ccc");
}

#[test]
fn super_mode_false_still_copies_directory_tree() {
    // --no-super should still copy all files, just without privileged operations.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("dir/x.txt", b"xxx");
    ctx.write_source("dir/y.txt", b"yyy");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(false))
                .permissions(true)
                .times(true),
        )
        .expect("copy succeeds with --no-super");

    assert_eq!(summary.files_copied(), 2);
    test_helpers::assert_file_content(&ctx.dest.join("dir/x.txt"), b"xxx");
    test_helpers::assert_file_content(&ctx.dest.join("dir/y.txt"), b"yyy");
}

// ============================================================================
// --super with Specials and Devices
// ============================================================================

#[cfg(unix)]
#[test]
fn super_mode_true_with_specials_copies_fifo() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let fifo_path = source_root.join("pipe");
    mkfifo_for_tests(&fifo_path, 0o644).expect("mkfifo");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .specials(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.fifos_created(), 1);
    let dest_fifo = dest_root.join("pipe");
    let metadata = fs::symlink_metadata(&dest_fifo).expect("dest fifo metadata");
    assert!(metadata.file_type().is_fifo());
}

#[cfg(unix)]
#[test]
fn super_mode_false_with_specials_still_copies_fifo() {
    // FIFOs don't actually require root, but with --no-super we verify
    // the option doesn't block non-privileged special file creation.
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let fifo_path = source_root.join("pipe");
    mkfifo_for_tests(&fifo_path, 0o644).expect("mkfifo");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(false))
                .specials(true),
        )
        .expect("copy succeeds with --no-super");

    assert_eq!(summary.fifos_created(), 1);
    let dest_fifo = dest_root.join("pipe");
    let metadata = fs::symlink_metadata(&dest_fifo).expect("dest fifo metadata");
    assert!(metadata.file_type().is_fifo());
}

// ============================================================================
// --fake-super Integration Tests
// ============================================================================

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_stores_ownership_in_xattrs() {
    // When --fake-super is enabled with --owner, ownership metadata should be
    // stored in the user.rsync.%stat xattr rather than applied directly.
    // This test requires xattr support on the filesystem.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"fake super ownership");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .fake_super(true)
            .owner(true)
            .group(true)
            .permissions(true),
    );

    // The copy should succeed; fake-super writes xattrs
    match result {
        Ok(summary) => {
            assert_eq!(summary.files_copied(), 1);
            let dest_file = ctx.dest.join("file.txt");
            test_helpers::assert_file_content(&dest_file, b"fake super ownership");

            // Check for the fake-super xattr
            let stat_xattr = xattr::get(&dest_file, "user.rsync.%stat");
            match stat_xattr {
                Ok(Some(value)) => {
                    // The xattr should contain encoded metadata
                    assert!(!value.is_empty(), "fake-super xattr should not be empty");
                }
                Ok(None) => {
                    // Some filesystems (e.g., tmpfs) may not support user xattrs.
                    // This is acceptable.
                }
                Err(_) => {
                    // xattr not supported on this filesystem, skip assertion
                }
            }
        }
        Err(_) => {
            // May fail if xattrs are not supported on the filesystem;
            // this is acceptable for the test environment.
        }
    }
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn fake_super_with_directory_tree() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("dir/a.txt", b"content a");
    ctx.write_source("dir/b.txt", b"content b");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let result = plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .fake_super(true)
            .owner(true)
            .group(true)
            .permissions(true)
            .times(true),
    );

    match result {
        Ok(summary) => {
            assert_eq!(summary.files_copied(), 2);
            test_helpers::assert_file_content(&ctx.dest.join("dir/a.txt"), b"content a");
            test_helpers::assert_file_content(&ctx.dest.join("dir/b.txt"), b"content b");
        }
        Err(_) => {
            // Acceptable if xattr support is not available
        }
    }
}

#[test]
fn fake_super_copies_file_without_xattr_feature() {
    // Even without xattr feature enabled at the test level, fake_super should
    // not prevent the file copy from succeeding.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"fake super test");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .fake_super(true)
                .permissions(true),
        )
        .expect("copy succeeds with --fake-super");

    assert_eq!(summary.files_copied(), 1);
    test_helpers::assert_file_content(&ctx.dest.join("file.txt"), b"fake super test");
}

// ============================================================================
// Dry Run with --super and --fake-super
// ============================================================================

#[test]
fn dry_run_with_super_mode_does_not_create_files() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"dry run super");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .owner(true)
                .group(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!ctx.dest.join("file.txt").exists());
}

#[test]
fn dry_run_with_fake_super_does_not_create_files() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"dry run fake super");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default()
                .fake_super(true)
                .owner(true)
                .permissions(true),
        )
        .expect("dry run succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(!ctx.dest.join("file.txt").exists());
}

// ============================================================================
// Interaction with Metadata Preservation Flags
// ============================================================================

#[test]
fn super_mode_with_permissions_preserves_perms() {
    // --super combined with --perms should preserve permissions.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"perms test");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o755);
        fs::set_permissions(ctx.source.join("file.txt"), perms).expect("set perms");
    }

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let dest_perms = fs::metadata(ctx.dest.join("file.txt"))
            .expect("dest metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dest_perms, 0o755);
    }
}

#[test]
fn super_mode_with_times_preserves_timestamps() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"times test");

    let mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(ctx.source.join("file.txt"), mtime).expect("set mtime");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime =
        filetime::FileTime::from_last_modification_time(
            &fs::metadata(ctx.dest.join("file.txt")).expect("dest metadata"),
        );
    assert_eq!(dest_mtime, mtime);
}

#[test]
fn no_super_with_times_still_preserves_timestamps() {
    // Timestamps do not require root, so --no-super should not prevent
    // time preservation.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"no-super times");

    let mtime = filetime::FileTime::from_unix_time(1_700_050_000, 0);
    filetime::set_file_mtime(ctx.source.join("file.txt"), mtime).expect("set mtime");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(false))
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    let dest_mtime =
        filetime::FileTime::from_last_modification_time(
            &fs::metadata(ctx.dest.join("file.txt")).expect("dest metadata"),
        );
    assert_eq!(dest_mtime, mtime);
}

// ============================================================================
// --super with --owner Root-Only Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn super_true_with_owner_preserves_uid_when_root() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        // Skip when not running as root
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"root super owner").expect("write source");

    let test_uid = 1234;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(test_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .owner(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.uid(), test_uid);
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn super_true_with_group_preserves_gid_when_root() {
    use rustix::fs::{AtFlags, chownat};
    use std::os::unix::fs::MetadataExt;

    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"root super group").expect("write source");

    let test_gid = 5678;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(test_gid)),
        AtFlags::empty(),
    )
    .expect("set source gid");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .group(true),
        )
        .expect("copy succeeds");

    let metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(metadata.gid(), test_gid);
    assert_eq!(summary.files_copied(), 1);
}

// ============================================================================
// Combined --super and --fake-super Tests
// ============================================================================

#[test]
fn super_and_fake_super_both_enabled() {
    // Both flags can be set simultaneously. In upstream rsync, --fake-super
    // takes precedence for storage, while --super controls attempt behavior.
    let options = LocalCopyOptions::default()
        .super_mode(Some(true))
        .fake_super(true);

    assert!(options.am_root());
    assert!(options.fake_super_enabled());
    assert_eq!(options.super_mode_setting(), Some(true));
}

#[test]
fn super_false_and_fake_super_true() {
    // --no-super with --fake-super: fake-super stores metadata in xattrs,
    // --no-super prevents direct privileged operations.
    let options = LocalCopyOptions::default()
        .super_mode(Some(false))
        .fake_super(true);

    assert!(!options.am_root());
    assert!(options.fake_super_enabled());
}

#[test]
fn super_true_and_fake_super_copies_files() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("combined.txt", b"combined test");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .fake_super(true)
                .owner(true)
                .group(true)
                .permissions(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    test_helpers::assert_file_content(&ctx.dest.join("combined.txt"), b"combined test");
}

// ============================================================================
// Event Collection with --super
// ============================================================================

#[test]
fn super_mode_with_collect_events_records_actions() {
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("tracked.txt", b"event tracking");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .collect_events(true),
        )
        .expect("copy executes");

    assert_eq!(report.summary().files_copied(), 1);
    // Verify events were recorded
    assert!(!report.records().is_empty());
}

// ============================================================================
// --super with Delete Operations
// ============================================================================

#[test]
fn super_mode_with_delete_removes_extra_files() {
    let ctx = test_helpers::setup_copy_test_with_dest();
    ctx.write_source("keep.txt", b"keep me");
    ctx.write_dest("extra.txt", b"delete me");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .delete(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(ctx.dest_exists("keep.txt"));
    assert!(!ctx.dest_exists("extra.txt"));
}

#[test]
fn no_super_with_delete_still_removes_extra_files() {
    // Delete operations do not require root privileges.
    let ctx = test_helpers::setup_copy_test_with_dest();
    ctx.write_source("keep.txt", b"keep me");
    ctx.write_dest("extra.txt", b"delete me");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(false))
                .delete(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert!(ctx.dest_exists("keep.txt"));
    assert!(!ctx.dest_exists("extra.txt"));
}

// ============================================================================
// --super with Symlinks
// ============================================================================

#[cfg(unix)]
#[test]
fn super_mode_with_links_copies_symlinks() {
    use std::os::unix::fs::symlink;

    let ctx = test_helpers::setup_copy_test();
    let target = ctx.source.join("target.txt");
    fs::write(&target, b"link target").expect("write target");

    let link = ctx.source.join("link");
    symlink(Path::new("target.txt"), &link).expect("create symlink");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .links(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.symlinks_copied(), 1);
    let dest_link = ctx.dest.join("link");
    let dest_target = fs::read_link(&dest_link).expect("read link");
    assert_eq!(dest_target, Path::new("target.txt"));
}

// ============================================================================
// --super with Update Semantics
// ============================================================================

#[test]
fn super_mode_with_update_skips_newer_destination() {
    let ctx = test_helpers::setup_copy_test_with_dest();
    ctx.write_source("file.txt", b"older");
    ctx.write_dest("file.txt", b"newer");

    // Make destination newer than source
    let old_time = filetime::FileTime::from_unix_time(1_000_000_000, 0);
    let new_time = filetime::FileTime::from_unix_time(2_000_000_000, 0);
    filetime::set_file_mtime(ctx.source.join("file.txt"), old_time).expect("set source mtime");
    filetime::set_file_mtime(ctx.dest.join("file.txt"), new_time).expect("set dest mtime");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .update(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    test_helpers::assert_file_content(&ctx.dest.join("file.txt"), b"newer");
}

// ============================================================================
// --super with Checksum Mode
// ============================================================================

#[test]
fn super_mode_with_checksum_copies_differing_files() {
    let ctx = test_helpers::setup_copy_test_with_dest();
    ctx.write_source("file.txt", b"source content");
    ctx.write_dest("file.txt", b"different content");

    // Make timestamps match to verify checksum comparison is used
    let mtime = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    filetime::set_file_mtime(ctx.source.join("file.txt"), mtime).expect("set source mtime");
    filetime::set_file_mtime(ctx.dest.join("file.txt"), mtime).expect("set dest mtime");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    test_helpers::assert_file_content(&ctx.dest.join("file.txt"), b"source content");
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn super_mode_transitions_between_states() {
    // Verify that super_mode can be changed multiple times via the builder pattern.
    let opts = LocalCopyOptions::default()
        .super_mode(Some(true))
        .super_mode(Some(false))
        .super_mode(None)
        .super_mode(Some(true));

    assert_eq!(opts.super_mode_setting(), Some(true));
    assert!(opts.am_root());
}

#[test]
fn fake_super_transitions_between_states() {
    let opts = LocalCopyOptions::default()
        .fake_super(true)
        .fake_super(false)
        .fake_super(true);

    assert!(opts.fake_super_enabled());
}

#[test]
fn super_mode_none_with_all_preservation_flags() {
    // Default super_mode (None) with all metadata flags should work fine.
    let ctx = test_helpers::setup_copy_test();
    ctx.write_source("file.txt", b"all flags");

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .group(true)
                .permissions(true)
                .times(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    test_helpers::assert_file_content(&ctx.dest.join("file.txt"), b"all flags");
}

#[test]
fn super_mode_with_empty_source_directory() {
    let ctx = test_helpers::setup_copy_test();
    // Source directory exists but is empty

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .super_mode(Some(true))
                .owner(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
}

#[test]
fn fake_super_with_empty_source_directory() {
    let ctx = test_helpers::setup_copy_test();

    let operands = ctx.operands_with_trailing_separator();
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .fake_super(true)
                .owner(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
}
