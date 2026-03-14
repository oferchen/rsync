/// End-to-end test for `--checksum` (`-c`) mode during a daemon push transfer.
///
/// Verifies that `always_checksum` forces full file checksum comparison instead
/// of the default quick-check (mtime + size). The test performs three phases:
///
/// 1. Initial push - seeds the destination with the original file contents.
/// 2. Rewrite source files with DIFFERENT content but the SAME size, then
///    backdate destination file mtimes to match the source - so quick-check
///    would see identical mtime + size and skip the files.
/// 3. Push again with `checksum(true)` - forces whole-file checksum comparison
///    which detects the content difference and re-transfers the files.
///
/// After the third phase the destination must contain the NEW content, proving
/// that checksum mode detected the change that quick-check would have missed.
///
/// # Upstream Reference
///
/// - `generator.c:recv_generator()` - quick-check vs checksum comparison logic
/// - `options.c` - `-c` / `--checksum` sets `always_checksum`
/// - `flist.c:send_file_entry()` - includes file checksums when `always_checksum`
#[cfg(unix)]
#[test]
fn daemon_checksum_push_detects_content_change_despite_matching_mtime() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    // Use content that is exactly the same length so the replacement in phase 2
    // produces files with identical size - the key condition for tricking
    // quick-check into skipping the transfer.
    let original_content = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";
    let modified_content = b"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB\n";
    assert_eq!(
        original_content.len(),
        modified_content.len(),
        "content lengths must match to defeat quick-check"
    );

    fs::write(source_dir.join("alpha.txt"), original_content).expect("write alpha.txt");
    fs::write(source_dir.join("beta.txt"), original_content).expect("write beta.txt");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config with max-sessions=4 (probe + initial push + checksum push + margin) ---
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

    // === Phase 1: Initial push (seeds destination with original content) ===
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

    // Verify initial push landed correctly
    assert_eq!(
        fs::read(dest_dir.join("alpha.txt")).expect("read dest alpha.txt"),
        original_content,
        "alpha.txt content mismatch after initial push"
    );
    assert_eq!(
        fs::read(dest_dir.join("beta.txt")).expect("read dest beta.txt"),
        original_content,
        "beta.txt content mismatch after initial push"
    );

    // === Phase 2: Rewrite source with different content of the same size ===
    fs::write(source_dir.join("alpha.txt"), modified_content).expect("rewrite alpha.txt");
    fs::write(source_dir.join("beta.txt"), modified_content).expect("rewrite beta.txt");

    // Read back the source mtime so we can stamp the destination with it -
    // this makes quick-check see identical mtime + size and skip the files.
    let source_alpha_mtime =
        filetime::FileTime::from_last_modification_time(&fs::metadata(source_dir.join("alpha.txt")).expect("stat source alpha"));
    let source_beta_mtime =
        filetime::FileTime::from_last_modification_time(&fs::metadata(source_dir.join("beta.txt")).expect("stat source beta"));

    filetime::set_file_mtime(dest_dir.join("alpha.txt"), source_alpha_mtime)
        .expect("set dest alpha.txt mtime");
    filetime::set_file_mtime(dest_dir.join("beta.txt"), source_beta_mtime)
        .expect("set dest beta.txt mtime");

    // Sanity: destination still has the OLD content but now has matching mtime
    assert_eq!(
        fs::read(dest_dir.join("alpha.txt")).expect("read dest alpha.txt pre-checksum"),
        original_content,
        "destination must still have original content before checksum push"
    );

    // === Phase 3: Push with checksum mode - must detect content difference ===
    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .checksum(true)
            .build();

        let result = core::client::run_client(client_config);

        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 2,
                    "checksum push must transfer at least 2 files (content differs), got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("checksum push failed: {e}");
            }
        }
    }

    // === Verify destination now has the modified content ===
    let dest_alpha = fs::read(dest_dir.join("alpha.txt")).expect("read dest alpha.txt after checksum push");
    assert_eq!(
        dest_alpha, modified_content,
        "alpha.txt must match modified source after checksum push"
    );

    let dest_beta = fs::read(dest_dir.join("beta.txt")).expect("read dest beta.txt after checksum push");
    assert_eq!(
        dest_beta, modified_content,
        "beta.txt must match modified source after checksum push"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
