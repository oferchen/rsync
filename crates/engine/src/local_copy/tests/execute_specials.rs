
// Tests for upstream rsync --specials behavior.
//
// In upstream rsync, IS_SPECIAL(mode) covers both S_ISSOCK and S_ISFIFO.
// These tests verify that Unix-domain sockets are handled correctly alongside
// FIFOs under the --specials flag.

/// Helper: creates a Unix-domain socket file at `path`.
///
/// Uses `std::os::unix::net::UnixListener::bind` which creates the socket
/// node on the filesystem. The listener is immediately dropped, but the
/// socket node remains.
#[cfg(unix)]
fn mksocket_for_tests(path: &Path) -> io::Result<()> {
    use std::os::unix::net::UnixListener;
    let _listener = UnixListener::bind(path)?;
    // Dropping the listener closes it, but the socket node on disk persists.
    Ok(())
}

// ==================== Socket Single-Source Tests ====================

#[cfg(unix)]
#[test]
fn execute_copies_socket_with_specials_enabled() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_socket = temp.path().join("dest.sock");
    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().specials(true),
        )
        .expect("socket copy succeeds");

    let dest_metadata = fs::symlink_metadata(&dest_socket).expect("dest metadata");
    assert!(
        dest_metadata.file_type().is_socket(),
        "destination should be a socket"
    );
    assert_eq!(summary.fifos_created(), 1, "sockets count as fifos_created");
}

#[cfg(unix)]
#[test]
fn execute_without_specials_skips_socket() {
    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_socket = temp.path().join("dest.sock");
    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("copy succeeds without specials");

    assert_eq!(summary.fifos_created(), 0);
    assert!(
        fs::symlink_metadata(&dest_socket).is_err(),
        "destination socket should not exist"
    );
}

#[cfg(unix)]
#[test]
fn execute_without_specials_records_socket_skip_event() {
    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("skip.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest = temp.path().join("dest.sock");
    let operands = vec![
        source_socket.into_os_string(),
        dest.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().collect_events(true),
        )
        .expect("copy executes");

    assert!(fs::symlink_metadata(&dest).is_err());
    assert!(
        report.records().iter().any(|record| {
            record.action() == &LocalCopyAction::SkippedNonRegular
                && record.relative_path() == Path::new("skip.sock")
        }),
        "should record a SkippedNonRegular event for the socket"
    );
}

// ==================== Socket Metadata Preservation ====================

#[cfg(unix)]
#[test]
fn execute_copies_socket_preserving_permissions() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    fs::set_permissions(&source_socket, PermissionsExt::from_mode(0o750))
        .expect("set socket permissions");

    let dest_socket = temp.path().join("dest.sock");
    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .specials(true)
            .permissions(true),
    )
    .expect("socket copy succeeds");

    let dest_metadata = fs::symlink_metadata(&dest_socket).expect("dest metadata");
    assert!(dest_metadata.file_type().is_socket());
    assert_eq!(
        dest_metadata.permissions().mode() & 0o777,
        0o750,
        "socket permissions should be preserved"
    );
}

#[cfg(unix)]
#[test]
fn execute_copies_socket_preserving_timestamps() {
    use filetime::{FileTime, set_file_times};
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let atime = FileTime::from_unix_time(1_700_050_000, 0);
    let mtime = FileTime::from_unix_time(1_700_060_000, 0);
    set_file_times(&source_socket, atime, mtime).expect("set socket timestamps");

    let dest_socket = temp.path().join("dest.sock");
    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .specials(true)
            .times(true),
    )
    .expect("socket copy succeeds");

    let dest_metadata = fs::symlink_metadata(&dest_socket).expect("dest metadata");
    assert!(dest_metadata.file_type().is_socket());
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_mtime, mtime, "socket mtime should be preserved");
}

// ==================== Socket Within Directory ====================

#[cfg(unix)]
#[test]
fn execute_copies_socket_within_directory() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let nested = source_root.join("dir");
    fs::create_dir_all(&nested).expect("create nested");

    let source_socket = nested.join("my.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_root = temp.path().join("dest");
    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().specials(true),
        )
        .expect("socket copy within directory succeeds");

    let dest_socket = dest_root.join("dir").join("my.sock");
    let metadata = fs::symlink_metadata(&dest_socket).expect("dest socket metadata");
    assert!(
        metadata.file_type().is_socket(),
        "destination should be a socket"
    );
    assert_eq!(summary.fifos_created(), 1);
}

// ==================== Mixed FIFOs and Sockets ====================

#[cfg(unix)]
#[test]
fn execute_copies_mixed_fifos_and_sockets() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a FIFO
    let fifo = source_root.join("pipe");
    mkfifo_for_tests(&fifo, 0o600).expect("mkfifo");

    // Create a socket
    let socket = source_root.join("sock");
    mksocket_for_tests(&socket).expect("mksocket");

    // Create a regular file
    fs::write(source_root.join("regular.txt"), b"content").expect("write regular");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().specials(true),
        )
        .expect("copy succeeds");

    // Both FIFO and socket should be created
    let dest_fifo = dest_root.join("pipe");
    let dest_socket = dest_root.join("sock");
    let dest_regular = dest_root.join("regular.txt");

    assert!(
        fs::symlink_metadata(&dest_fifo)
            .expect("fifo meta")
            .file_type()
            .is_fifo(),
        "FIFO should be recreated"
    );
    assert!(
        fs::symlink_metadata(&dest_socket)
            .expect("socket meta")
            .file_type()
            .is_socket(),
        "socket should be recreated"
    );
    assert!(dest_regular.is_file(), "regular file should be copied");
    assert_eq!(
        summary.fifos_created(),
        2,
        "both FIFO and socket count as fifos_created"
    );
    assert_eq!(summary.files_copied(), 1);
}

#[cfg(unix)]
#[test]
fn execute_without_specials_skips_both_fifos_and_sockets() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let fifo = source_root.join("pipe");
    mkfifo_for_tests(&fifo, 0o600).expect("mkfifo");

    let socket = source_root.join("sock");
    mksocket_for_tests(&socket).expect("mksocket");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().collect_events(true),
        )
        .expect("copy executes");

    let summary = report.summary();
    assert_eq!(summary.fifos_created(), 0);
    assert!(!dest_root.join("pipe").exists());
    assert!(!dest_root.join("sock").exists());

    let skip_count = report
        .records()
        .iter()
        .filter(|record| record.action() == &LocalCopyAction::SkippedNonRegular)
        .count();
    assert_eq!(
        skip_count, 2,
        "both FIFO and socket should produce SkippedNonRegular events"
    );
}

// ==================== Archive Mode (--specials implied) ====================

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
#[test]
fn execute_archive_mode_copies_socket() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let socket = source_root.join("my.sock");
    mksocket_for_tests(&socket).expect("mksocket");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // archive_options() enables specials (among other things)
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            test_helpers::presets::archive_options(),
        )
        .expect("archive copy succeeds");

    let dest_socket = dest_root.join("my.sock");
    let metadata = fs::symlink_metadata(&dest_socket).expect("dest socket metadata");
    assert!(metadata.file_type().is_socket());
    assert_eq!(summary.fifos_created(), 1);
}

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
#[test]
fn execute_archive_mode_copies_fifo_and_socket_together() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    let fifo = source_root.join("pipe");
    mkfifo_for_tests(&fifo, 0o600).expect("mkfifo");
    let socket = source_root.join("sock");
    mksocket_for_tests(&socket).expect("mksocket");
    fs::write(source_root.join("file.txt"), b"content").expect("write");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            test_helpers::presets::archive_options(),
        )
        .expect("archive copy succeeds");

    assert!(
        fs::symlink_metadata(dest_root.join("pipe"))
            .expect("meta")
            .file_type()
            .is_fifo()
    );
    assert!(
        fs::symlink_metadata(dest_root.join("sock"))
            .expect("meta")
            .file_type()
            .is_socket()
    );
    assert!(dest_root.join("file.txt").is_file());
    assert_eq!(summary.fifos_created(), 2);
    assert_eq!(summary.files_copied(), 1);
}

// ==================== Dry Run ====================

#[cfg(unix)]
#[test]
fn execute_dry_run_does_not_create_socket() {
    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_socket = temp.path().join("dest.sock");
    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::DryRun,
            LocalCopyOptions::default().specials(true),
        )
        .expect("dry run succeeds");

    assert_eq!(
        summary.fifos_created(),
        1,
        "dry run should still count the socket"
    );
    assert!(!dest_socket.exists(), "dry run should not create the socket");
}

// ==================== Force Replacement ====================

#[cfg(unix)]
#[test]
fn execute_socket_replaces_directory_when_force_enabled() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_socket = temp.path().join("dest.sock");
    fs::create_dir_all(&dest_socket).expect("create conflicting directory");

    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .specials(true)
            .force_replacements(true),
    )
    .expect("forced replacement succeeds");

    let metadata = fs::symlink_metadata(&dest_socket).expect("dest metadata");
    assert!(
        metadata.file_type().is_socket(),
        "socket should replace directory"
    );
}

#[cfg(unix)]
#[test]
fn execute_socket_replaces_regular_file() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_socket = temp.path().join("dest.sock");
    fs::write(&dest_socket, b"regular file").expect("write conflicting file");

    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().specials(true),
    )
    .expect("replacement succeeds");

    let metadata = fs::symlink_metadata(&dest_socket).expect("dest metadata");
    assert!(
        metadata.file_type().is_socket(),
        "socket should replace regular file"
    );
}

// ==================== Socket Hard Link Preservation ====================

#[cfg(unix)]
#[test]
fn execute_preserves_socket_hard_links() {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let sock_a = source_root.join("sock-a");
    mksocket_for_tests(&sock_a).expect("mksocket a");
    let sock_b = source_root.join("sock-b");
    fs::hard_link(&sock_a, &sock_b).expect("link socket");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .hard_links(true)
                .specials(true),
        )
        .expect("copy succeeds");

    let dest_a = dest_root.join("sock-a");
    let dest_b = dest_root.join("sock-b");
    let meta_a = fs::symlink_metadata(&dest_a).expect("dest a metadata");
    let meta_b = fs::symlink_metadata(&dest_b).expect("dest b metadata");

    assert!(meta_a.file_type().is_socket());
    assert!(meta_b.file_type().is_socket());
    assert_eq!(meta_a.ino(), meta_b.ino(), "sockets should share inode");
    assert_eq!(meta_a.nlink(), 2);
    assert!(summary.hard_links_created() >= 1);
    assert_eq!(
        summary.fifos_created(),
        1,
        "only one socket should be created; the other is a hard link"
    );
}

// ==================== Specials with Delete ====================

#[cfg(unix)]
#[test]
fn execute_delete_removes_extraneous_socket() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");
    fs::write(source_root.join("keep.txt"), b"keep").expect("write keep");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("keep.txt"), b"keep").expect("write keep");

    // Create an extraneous socket in the destination
    let extraneous_socket = dest_root.join("extraneous.sock");
    mksocket_for_tests(&extraneous_socket).expect("mksocket");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .specials(true)
                .delete(true),
        )
        .expect("delete copy succeeds");

    assert!(
        !extraneous_socket.exists(),
        "extraneous socket should be deleted"
    );
    assert!(dest_root.join("keep.txt").is_file(), "keep.txt should remain");
    assert!(summary.items_deleted() >= 1);
}

// ==================== Specials Disabled Does Not Delete Specials ====================

#[cfg(unix)]
#[test]
fn execute_without_specials_with_delete_does_not_delete_socket_from_keep_list() {
    // When --specials is disabled, sockets in the source are skipped and
    // not added to the keep list. With --delete, this means they would be
    // removed from the destination. This test verifies the skip behavior.
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Source has a socket + regular file
    let source_socket = source_root.join("my.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");
    fs::write(source_root.join("file.txt"), b"data").expect("write");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest");
    fs::write(dest_root.join("file.txt"), b"old").expect("write old");

    let mut source_operand = source_root.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().delete(true),
        )
        .expect("copy succeeds");

    // Socket in source was skipped (specials disabled), so it won't appear
    // in the destination either.
    assert!(!dest_root.join("my.sock").exists());
    assert!(dest_root.join("file.txt").is_file());
    assert_eq!(summary.fifos_created(), 0);
}

// ==================== Specials with Collect Events ====================

#[cfg(unix)]
#[test]
fn execute_socket_produces_fifo_copied_event() {
    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("events.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_socket = temp.path().join("dest.sock");
    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let report = plan
        .execute_with_report(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .specials(true)
                .collect_events(true),
        )
        .expect("copy executes");

    assert_eq!(report.summary().fifos_created(), 1);
    assert!(
        report.records().iter().any(|record| {
            record.action() == &LocalCopyAction::FifoCopied
        }),
        "should record a FifoCopied event for the socket"
    );
}

// ==================== Socket Idempotent Re-copy ====================

#[cfg(unix)]
#[test]
fn execute_recopy_socket_replaces_existing_socket() {
    use std::os::unix::fs::FileTypeExt;

    let temp = tempdir().expect("tempdir");
    let source_socket = temp.path().join("source.sock");
    mksocket_for_tests(&source_socket).expect("mksocket");

    let dest_socket = temp.path().join("dest.sock");
    // Pre-create a socket at the destination
    mksocket_for_tests(&dest_socket).expect("mksocket dest");

    let operands = vec![
        source_socket.into_os_string(),
        dest_socket.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().specials(true),
        )
        .expect("re-copy succeeds");

    let metadata = fs::symlink_metadata(&dest_socket).expect("dest metadata");
    assert!(metadata.file_type().is_socket());
    assert_eq!(summary.fifos_created(), 1);
}

// ==================== Specials with Symlinks ====================

#[cfg(unix)]
#[test]
fn execute_copies_socket_and_symlink_together() {
    use std::os::unix::fs::{FileTypeExt, symlink};

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source");

    // Create a socket
    let socket = source_root.join("my.sock");
    mksocket_for_tests(&socket).expect("mksocket");

    // Create a regular file and a symlink
    let target = source_root.join("target.txt");
    fs::write(&target, b"target").expect("write target");
    symlink(Path::new("target.txt"), source_root.join("link")).expect("create symlink");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .specials(true)
                .links(true),
        )
        .expect("copy succeeds");

    assert!(
        fs::symlink_metadata(dest_root.join("my.sock"))
            .expect("meta")
            .file_type()
            .is_socket()
    );
    assert!(
        fs::symlink_metadata(dest_root.join("link"))
            .expect("meta")
            .file_type()
            .is_symlink()
    );
    assert_eq!(summary.fifos_created(), 1);
    assert_eq!(summary.symlinks_copied(), 1);
    assert_eq!(summary.files_copied(), 1);
}

// ==================== Options Builder Tests ====================

#[test]
fn options_specials_enabled_returns_false_by_default() {
    let options = LocalCopyOptions::default();
    assert!(
        !options.specials_enabled(),
        "specials should be disabled by default"
    );
}

#[test]
fn options_specials_enabled_after_setting() {
    let options = LocalCopyOptions::default().specials(true);
    assert!(options.specials_enabled());
}

#[test]
fn options_specials_disabled_after_explicit_disable() {
    let options = LocalCopyOptions::default().specials(true).specials(false);
    assert!(
        !options.specials_enabled(),
        "specials(false) should disable specials"
    );
}

#[test]
fn archive_options_enable_specials() {
    let options = test_helpers::presets::archive_options();
    assert!(
        options.specials_enabled(),
        "archive mode should enable specials"
    );
}

#[test]
fn archive_options_enable_devices() {
    let options = test_helpers::presets::archive_options();
    assert!(
        options.devices_enabled(),
        "archive mode should enable devices (as -D implies --devices --specials)"
    );
}
