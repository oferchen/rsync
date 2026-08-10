/// End-to-end test for `--files-from` push over daemon protocol.
///
/// Verifies that a push transfer with `--files-from` only transfers the files
/// listed in the files-from list, ignoring unlisted files in the source tree.
///
/// # Scenario
///
/// Source (client side):
///   a.txt  (regular file, "alpha content")
///   b.txt  (regular file, "beta content")
///   c.txt  (regular file, "gamma content")
///
/// Files-from list:
///   a.txt
///   c.txt
///
/// Destination (daemon module):
///   (empty directory)
///
/// After push with `--files-from`, the destination should contain a.txt and
/// c.txt but NOT b.txt - the unlisted file is excluded from the transfer.
///
/// # Upstream Reference
///
/// - `options.c:2944-2956` - server_options() forwarding of files-from to remote
/// - `clientserver.c` - daemon push file list construction from forwarded paths
#[cfg(unix)]
#[test]
fn daemon_files_from_push_limits_transferred_files() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    fs::write(source_dir.join("a.txt"), b"alpha content\n").expect("write a.txt");
    fs::write(source_dir.join("b.txt"), b"beta content\n").expect("write b.txt");
    fs::write(source_dir.join("c.txt"), b"gamma content\n").expect("write c.txt");

    let files_from_path = temp.path().join("filelist.txt");
    fs::write(&files_from_path, "a.txt\nc.txt\n").expect("write files-from list");

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
            OsString::from("2"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);

    // Drop the probe connection so the daemon worker finishes quickly
    drop(probe_stream);

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .files_from(core::client::FilesFromSource::LocalFile(
            files_from_path.clone(),
        ))
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 2,
                "expected at least 2 files transferred (a.txt and c.txt), got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("files-from client push failed: {e}");
        }
    }

    // Verify listed files were transferred
    let dest_a = dest_dir.join("a.txt");
    assert!(dest_a.exists(), "a.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_a).expect("read a.txt"),
        b"alpha content\n",
        "a.txt content mismatch"
    );

    let dest_c = dest_dir.join("c.txt");
    assert!(dest_c.exists(), "c.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_c).expect("read c.txt"),
        b"gamma content\n",
        "c.txt content mismatch"
    );

    // Verify unlisted file was NOT transferred
    assert!(
        !dest_dir.join("b.txt").exists(),
        "b.txt must not exist at destination (not in files-from list)"
    );

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
