/// End-to-end test for `--dry-run` push over daemon protocol.
///
/// Verifies that a push transfer with `--dry-run` reports what would happen
/// without actually writing any files to the destination module.
///
/// # Scenario
///
/// Source (client side):
///   file_a.txt  (small file, "alpha content")
///   file_b.txt  (small file, "beta content")
///   subdir/file_c.txt  (nested file, "gamma content")
///
/// Destination (daemon module):
///   (empty directory)
///
/// With `--dry-run`, the client should complete the transfer successfully and
/// report files that would be copied, but the destination must remain empty.
///
/// # Upstream Reference
///
/// - `clientserver.c` - daemon closes socket early during dry-run push
/// - `options.c` - `dry_run` sets `!do_xfers`
#[cfg(unix)]
#[test]
fn daemon_dry_run_push() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("file_a.txt"), b"alpha content\n").expect("write file_a");
    fs::write(source_dir.join("file_b.txt"), b"beta content\n").expect("write file_b");
    fs::write(source_subdir.join("file_c.txt"), b"gamma content\n").expect("write file_c");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config ---
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
            OsString::from("2"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);

    // Drop the probe connection so the daemon worker finishes quickly
    drop(probe_stream);

    // --- Run client push with --dry-run ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .dry_run(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            // Dry-run should report files that would be transferred
            assert!(
                summary.files_copied() >= 3,
                "expected at least 3 files reported, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("dry-run client push failed: {e}");
        }
    }

    // Verify destination remains empty - no files were actually written
    assert!(
        !dest_dir.join("file_a.txt").exists(),
        "file_a.txt must not exist after dry-run push"
    );
    assert!(
        !dest_dir.join("file_b.txt").exists(),
        "file_b.txt must not exist after dry-run push"
    );
    assert!(
        !dest_dir.join("subdir").exists(),
        "subdir must not exist after dry-run push"
    );

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
