/// End-to-end test for INC_RECURSE push over daemon protocol at protocol 32.
///
/// Verifies that a recursive push transfer with incremental recursion enabled
/// works correctly for deeply nested directory structures. At protocol 30+,
/// the client advertises INC_RECURSE ('i') in its capability string for daemon
/// transfers, and the server decides whether to use it.
///
/// # Scenario
///
/// Source (client side) - a nested directory tree:
///   src/
///     top.txt
///     a/
///       mid_a.txt
///       b/
///         mid_b.txt
///         c/
///           deep.txt
///     d/
///       side.txt
///
/// Destination (daemon module, initially empty):
///   After push, all directories and files must be present with correct content.
///
/// The test uses protocol 32 (which supports INC_RECURSE) and explicit
/// recursive mode to exercise the incremental file-list pipeline where
/// directories are discovered and transmitted on-the-fly rather than
/// pre-scanned into a single monolithic file list.
///
/// # Upstream Reference
///
/// - compat.c:720 set_allow_inc_recurse() - enables incremental recursion
/// - flist.c:send_directory() - sends sub-list entries as directories are opened
/// - options.c:2707-2713 - capability string with 'i' for INC_RECURSE
#[cfg(unix)]
#[test]
fn daemon_inc_recurse_push_nested_directories() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) with nested structure ---
    let source_dir = temp.path().join("source");
    let dir_a = source_dir.join("a");
    let dir_ab = dir_a.join("b");
    let dir_abc = dir_ab.join("c");
    let dir_d = source_dir.join("d");

    fs::create_dir_all(&dir_abc).expect("create source/a/b/c");
    fs::create_dir_all(&dir_d).expect("create source/d");

    fs::write(source_dir.join("top.txt"), b"top level content\n").expect("write top.txt");
    fs::write(dir_a.join("mid_a.txt"), b"mid level a content\n").expect("write mid_a.txt");
    fs::write(dir_ab.join("mid_b.txt"), b"mid level b content\n").expect("write mid_b.txt");
    fs::write(dir_abc.join("deep.txt"), b"deep nested content\n").expect("write deep.txt");
    fs::write(dir_d.join("side.txt"), b"side branch content\n").expect("write side.txt");

    // --- Destination (served by daemon, writable, initially empty) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // --- Daemon config ---
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[incmod]\n\
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
    drop(probe_stream);

    // --- Run client push with recursive + protocol 32 ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/incmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .recursive(true)
        .protocol_version(Some(ProtocolVersion::V32))
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 5,
                "expected at least 5 files transferred, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("inc_recurse push at protocol 32 failed: {e}");
        }
    }

    // --- Verify all directories were created ---
    assert!(
        dest_dir.join("a").is_dir(),
        "directory 'a' must exist after push"
    );
    assert!(
        dest_dir.join("a/b").is_dir(),
        "directory 'a/b' must exist after push"
    );
    assert!(
        dest_dir.join("a/b/c").is_dir(),
        "directory 'a/b/c' must exist after push"
    );
    assert!(
        dest_dir.join("d").is_dir(),
        "directory 'd' must exist after push"
    );

    // --- Verify all files arrived with correct content ---
    assert_eq!(
        fs::read(dest_dir.join("top.txt")).expect("read dest top.txt"),
        b"top level content\n",
        "top.txt content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("a/mid_a.txt")).expect("read dest mid_a.txt"),
        b"mid level a content\n",
        "mid_a.txt content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("a/b/mid_b.txt")).expect("read dest mid_b.txt"),
        b"mid level b content\n",
        "mid_b.txt content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("a/b/c/deep.txt")).expect("read dest deep.txt"),
        b"deep nested content\n",
        "deep.txt content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("d/side.txt")).expect("read dest side.txt"),
        b"side branch content\n",
        "side.txt content mismatch"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
