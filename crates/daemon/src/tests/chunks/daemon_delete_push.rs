/// End-to-end test for `--delete` during a daemon push.
///
/// Verifies that extraneous destination files are removed when pushing with
/// deletion enabled. The test performs two pushes:
///
/// 1. Initial push - seeds the destination with files A, B, and C.
/// 2. Modified push - removes file B from the source, then pushes again with
///    `delete(true)` to trigger deletion of extraneous destination entries.
///
/// After the second push, the destination must contain A and C but not B.
///
/// # Upstream Reference
///
/// - `receiver.c` - `delete_in_dir()` removes extraneous entries
/// - `options.c` - `--delete` handling and mode selection
#[cfg(unix)]
#[test]
fn daemon_delete_push_removes_extraneous_destination_files() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    fs::write(source_dir.join("file_a.txt"), "contents of file A\n").expect("write file_a");
    fs::write(source_dir.join("file_b.txt"), "contents of file B\n").expect("write file_b");
    fs::write(source_dir.join("file_c.txt"), "contents of file C\n").expect("write file_c");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[pushmod]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        dest_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("4"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // === Phase 1: Initial push - seed destination with A, B, C ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .build();

        let result = core::client::run_client(client_config);

        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 3,
                    "initial push must copy at least 3 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("initial push failed: {e}");
            }
        }
    }

    // Verify initial push succeeded - all three files present
    assert!(
        dest_dir.join("file_a.txt").exists(),
        "file_a.txt must exist after initial push"
    );
    assert!(
        dest_dir.join("file_b.txt").exists(),
        "file_b.txt must exist after initial push"
    );
    assert!(
        dest_dir.join("file_c.txt").exists(),
        "file_c.txt must exist after initial push"
    );

    // Backdate destination files so quick-check does not skip the transfer
    // on the second push (different mtime forces re-evaluation).
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    for name in &["file_a.txt", "file_b.txt", "file_c.txt"] {
        filetime::set_file_mtime(dest_dir.join(name), old_time)
            .unwrap_or_else(|e| panic!("backdate dest {name}: {e}"));
    }

    // === Remove file B from source ===
    fs::remove_file(source_dir.join("file_b.txt")).expect("remove file_b from source");

    // === Phase 2: Push with --delete - extraneous file B should be removed ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .delete(true)
            .build();

        let result = core::client::run_client(client_config);

        match &result {
            Ok(_summary) => {
                // Transfer succeeded - deletion should have been performed
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("delete push failed: {e}");
            }
        }
    }

    // === Verify destination state after delete push ===

    // Files A and C must still be present
    assert!(
        dest_dir.join("file_a.txt").exists(),
        "file_a.txt must survive delete push"
    );
    assert!(
        dest_dir.join("file_c.txt").exists(),
        "file_c.txt must survive delete push"
    );

    // File B must have been removed by --delete
    assert!(
        !dest_dir.join("file_b.txt").exists(),
        "file_b.txt must be deleted from destination (extraneous after source removal)"
    );

    // Verify file contents are intact
    assert_eq!(
        fs::read_to_string(dest_dir.join("file_a.txt")).expect("read dest file_a"),
        "contents of file A\n",
        "file_a.txt content mismatch after delete push"
    );
    assert_eq!(
        fs::read_to_string(dest_dir.join("file_c.txt")).expect("read dest file_c"),
        "contents of file C\n",
        "file_c.txt content mismatch after delete push"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
