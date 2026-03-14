/// End-to-end test for `--relative` push over daemon protocol.
///
/// Verifies that a push transfer with `--relative` reconstructs the full
/// source-relative path hierarchy at the destination, including implied
/// parent directories.
///
/// # Scenario
///
/// Source (client side):
///   a/b/c/deep.txt     (regular file, "deep content")
///   a/b/shallow.txt     (regular file, "shallow content")
///   a/top.txt           (regular file, "top content")
///
/// Destination (daemon module, initially empty):
///   After push with `--relative`, the destination should contain the full
///   nested directory structure:
///     a/b/c/deep.txt
///     a/b/shallow.txt
///     a/top.txt
///
/// # Upstream Reference
///
/// - `options.c` - `--relative` sets `relative_paths`
/// - `sender.c` - relative path construction in file list
/// - `receiver.c` - implied directory creation from relative paths
#[cfg(unix)]
#[test]
fn daemon_relative_receive_preserves_nested_paths() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) with nested directories ---
    let source_dir = temp.path().join("source");
    let nested_abc = source_dir.join("a").join("b").join("c");
    fs::create_dir_all(&nested_abc).expect("create source/a/b/c");

    fs::write(nested_abc.join("deep.txt"), b"deep content\n").expect("write deep.txt");
    fs::write(
        source_dir.join("a").join("b").join("shallow.txt"),
        b"shallow content\n",
    )
    .expect("write shallow.txt");
    fs::write(source_dir.join("a").join("top.txt"), b"top content\n").expect("write top.txt");

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

    // --- Run client push with --relative ---
    // Use trailing slash on source so contents are transferred with relative paths
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .relative_paths(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 3,
                "expected at least 3 files transferred, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("relative client push failed: {e}");
        }
    }

    // Verify full nested directory structure was reconstructed
    let dest_deep = dest_dir.join("a").join("b").join("c").join("deep.txt");
    assert!(dest_deep.exists(), "a/b/c/deep.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_deep).expect("read deep.txt"),
        b"deep content\n",
        "deep.txt content mismatch"
    );

    let dest_shallow = dest_dir.join("a").join("b").join("shallow.txt");
    assert!(
        dest_shallow.exists(),
        "a/b/shallow.txt must exist at destination"
    );
    assert_eq!(
        fs::read(&dest_shallow).expect("read shallow.txt"),
        b"shallow content\n",
        "shallow.txt content mismatch"
    );

    let dest_top = dest_dir.join("a").join("top.txt");
    assert!(dest_top.exists(), "a/top.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_top).expect("read top.txt"),
        b"top content\n",
        "top.txt content mismatch"
    );

    // Verify implied parent directories were created
    assert!(
        dest_dir.join("a").is_dir(),
        "implied directory 'a' must exist"
    );
    assert!(
        dest_dir.join("a").join("b").is_dir(),
        "implied directory 'a/b' must exist"
    );
    assert!(
        dest_dir.join("a").join("b").join("c").is_dir(),
        "implied directory 'a/b/c' must exist"
    );

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
