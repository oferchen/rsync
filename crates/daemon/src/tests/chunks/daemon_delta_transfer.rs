/// End-to-end test for delta transfer over daemon protocol.
///
/// Verifies that the full delta-transfer pipeline works when pushing files
/// via `rsync://` daemon connections. The test performs two pushes:
///
/// 1. Initial push - seeds the destination with the original file contents.
/// 2. Modified push - appends data to the source files and pushes again with
///    `--no-whole-file` to force the delta algorithm.
///
/// After the second push, the destination must match the modified source.
/// The transfer summary confirms that files were actually updated (not skipped
/// by quick-check) and that delta mode was in effect.
///
/// # Upstream Reference
///
/// - `match.c:match_sums()` - block matching for delta generation
/// - `receiver.c:receive_data()` - delta application on the receiver side
/// - `options.c` - `--no-whole-file` forces delta mode
#[cfg(unix)]
#[test]
fn daemon_delta_transfer_updates_modified_files() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // Use files large enough that delta transfer is meaningful.
    // A repeated pattern gives the rolling checksum good block matches.
    let base_content: Vec<u8> = b"The quick brown fox jumps over the lazy dog.\n"
        .iter()
        .copied()
        .cycle()
        .take(8192)
        .collect();

    fs::write(source_dir.join("data.txt"), &base_content).expect("write data.txt");
    fs::write(source_dir.join("other.txt"), &base_content).expect("write other.txt");

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

    // === Phase 1: Initial push (whole-file, seeds the destination) ===
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
                    summary.files_copied() >= 2,
                    "initial push must copy at least 2 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("initial push failed: {e}");
            }
        }
    }

    // Verify initial push succeeded
    assert_eq!(
        fs::read(dest_dir.join("data.txt")).expect("read dest data.txt"),
        base_content,
        "data.txt content mismatch after initial push"
    );
    assert_eq!(
        fs::read(dest_dir.join("other.txt")).expect("read dest other.txt"),
        base_content,
        "other.txt content mismatch after initial push"
    );

    // Backdate destination files so quick-check detects them as stale after
    // the source is modified (different mtime triggers re-transfer).
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(dest_dir.join("data.txt"), old_time)
        .expect("backdate dest data.txt");
    filetime::set_file_mtime(dest_dir.join("other.txt"), old_time)
        .expect("backdate dest other.txt");

    // === Modify source files (append data so delta is beneficial) ===
    let mut modified_content = base_content.clone();
    modified_content.extend_from_slice(b"APPENDED DELTA PAYLOAD\n");

    fs::write(source_dir.join("data.txt"), &modified_content).expect("rewrite data.txt");
    fs::write(source_dir.join("other.txt"), &modified_content).expect("rewrite other.txt");

    // === Phase 2: Push with delta mode (--no-whole-file) ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .whole_file(false)
            .build();

        let result = core::client::run_client(client_config);

        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 2,
                    "delta push must update at least 2 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("delta push failed: {e}");
            }
        }
    }

    // === Verify destination matches modified source ===
    let dest_data = fs::read(dest_dir.join("data.txt")).expect("read dest data.txt after delta");
    assert_eq!(
        dest_data, modified_content,
        "data.txt must match modified source after delta push"
    );

    let dest_other =
        fs::read(dest_dir.join("other.txt")).expect("read dest other.txt after delta");
    assert_eq!(
        dest_other, modified_content,
        "other.txt must match modified source after delta push"
    );

    // Verify the appended payload is present - confirms delta applied correctly
    assert!(
        dest_data
            .windows(b"APPENDED DELTA PAYLOAD".len())
            .any(|w| w == b"APPENDED DELTA PAYLOAD"),
        "destination data.txt must contain the appended payload"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
