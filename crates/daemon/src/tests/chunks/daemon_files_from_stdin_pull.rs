/// End-to-end test for `--files-from=-` (stdin) pull over daemon protocol.
///
/// Verifies that a pull transfer with `--files-from` backed by a local file
/// (simulating stdin) only transfers the files listed in the forwarded file
/// list. On the wire, both `--files-from=-` (stdin) and `--files-from=<file>`
/// follow the same protocol path: the client reads the names locally, sends
/// `--files-from=-` plus `--from0` to the daemon, then forwards the
/// NUL-separated file list over the protocol stream. The daemon reads from
/// its protocol input - equivalent to reading from stdin.
///
/// # Scenario
///
/// Source (daemon module):
///   dir/one.txt    (regular file, "first content")
///   dir/two.txt    (regular file, "second content")
///   dir/three.txt  (regular file, "third content")
///
/// Files-from list (forwarded via protocol as stdin):
///   dir/one.txt
///   dir/three.txt
///
/// Destination (client side):
///   (empty directory)
///
/// After pull with `--files-from`, the destination should contain dir/one.txt
/// and dir/three.txt but NOT dir/two.txt - the unlisted file is excluded from
/// the transfer.
///
/// # Upstream Reference
///
/// - `io.c:forward_filesfrom_data()` - client reads stdin/file, writes to socket
/// - `main.c:1354-1356` - `start_filesfrom_forwarding(filesfrom_fd)`
/// - `options.c:2944-2956` - server_options() forwarding of files-from to remote
#[cfg(unix)]
#[test]
fn daemon_files_from_stdin_pull_limits_transferred_files() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("dir");
    fs::create_dir_all(&source_subdir).expect("create source/dir");

    fs::write(source_subdir.join("one.txt"), b"first content\n").expect("write one.txt");
    fs::write(source_subdir.join("two.txt"), b"second content\n").expect("write two.txt");
    fs::write(source_subdir.join("three.txt"), b"third content\n").expect("write three.txt");

    // In a real `--files-from=-` invocation, the user pipes file names to stdin.
    // The client reads them and forwards over the protocol as NUL-separated data.
    // Using LocalFile exercises the identical daemon-side code path because both
    // Stdin and LocalFile produce the same `--files-from=-` + forwarded data on
    // the wire.
    let files_from_path = temp.path().join("filelist.txt");
    fs::write(&files_from_path, "dir/one.txt\ndir/three.txt\n").expect("write files-from list");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[pullmod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n",
        source_dir.display()
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

    // Pull: source is daemon URL, destination is local path.
    // The client sends --files-from=- to the daemon and forwards the file list
    // data over the protocol stream - identical to the stdin code path.
    let rsync_url = format!("rsync://127.0.0.1:{port}/pullmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .files_from(core::client::FilesFromSource::LocalFile(
            files_from_path.clone(),
        ))
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 2,
                "expected at least 2 files transferred (one.txt and three.txt), got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("files-from stdin pull failed: {e}");
        }
    }

    // Verify listed files were transferred
    let dest_one = dest_dir.join("dir/one.txt");
    assert!(dest_one.exists(), "dir/one.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_one).expect("read one.txt"),
        b"first content\n",
        "dir/one.txt content mismatch"
    );

    let dest_three = dest_dir.join("dir/three.txt");
    assert!(dest_three.exists(), "dir/three.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_three).expect("read three.txt"),
        b"third content\n",
        "dir/three.txt content mismatch"
    );

    // Verify unlisted file was NOT transferred
    assert!(
        !dest_dir.join("dir/two.txt").exists(),
        "dir/two.txt must not exist at destination (not in files-from list)"
    );

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
