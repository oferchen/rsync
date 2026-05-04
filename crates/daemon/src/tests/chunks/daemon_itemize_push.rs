/// End-to-end test for itemize (`-i`) output in daemon push mode.
///
/// Verifies that when verbosity is enabled during a push transfer, the client
/// collects events with correct kinds and relative paths for both newly created
/// and updated files.
///
/// # Scenario
///
/// Source (client side):
///   new_file.txt     (not present at destination)
///   existing.txt     (present at destination with different content)
///   subdir/nested.txt (nested new file)
///
/// Destination (daemon module):
///   existing.txt     (stale content, backdated mtime)
///
/// After push with verbosity=1, the client should report DataCopied events for
/// all three files and a DirectoryCreated event for subdir.
///
/// # Upstream Reference
///
/// - `log.c:log_item()` - formats itemize output
/// - `receiver.c` - emits MSG_INFO frames carrying itemize strings
#[cfg(unix)]
#[test]
fn daemon_itemize_push_reports_events() {
    use core::client::ClientEventKind;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("new_file.txt"), b"brand new content\n").expect("write new_file");
    fs::write(source_dir.join("existing.txt"), b"updated content\n").expect("write existing");
    fs::write(source_subdir.join("nested.txt"), b"nested content\n").expect("write nested");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // Pre-populate existing.txt with stale content and backdated mtime so
    // quick-check detects it as needing an update.
    fs::write(dest_dir.join("existing.txt"), b"old content\n").expect("write dest existing");
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(dest_dir.join("existing.txt"), old_time)
        .expect("backdate dest existing");

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
    drop(probe_stream);

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .verbosity(1)
        .force_event_collection(true)
        .build();

    let result = core::client::run_client(client_config);

    let summary = match result {
        Ok(summary) => summary,
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("itemize push failed: {e}");
        }
    };

    assert_eq!(
        fs::read(dest_dir.join("new_file.txt")).expect("read new_file"),
        b"brand new content\n",
        "new_file.txt content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("existing.txt")).expect("read existing"),
        b"updated content\n",
        "existing.txt content mismatch"
    );
    assert_eq!(
        fs::read(dest_dir.join("subdir/nested.txt")).expect("read nested"),
        b"nested content\n",
        "nested.txt content mismatch"
    );

    let events = summary.events();

    // Collect DataCopied events by relative path
    let data_copied_paths: Vec<&std::path::Path> = events
        .iter()
        .filter(|e| matches!(e.kind(), ClientEventKind::DataCopied))
        .map(|e| e.relative_path())
        .collect();

    assert!(
        data_copied_paths
            .iter()
            .any(|p| p == &std::path::Path::new("new_file.txt")),
        "expected DataCopied event for new_file.txt, got: {data_copied_paths:?}"
    );
    assert!(
        data_copied_paths
            .iter()
            .any(|p| p == &std::path::Path::new("existing.txt")),
        "expected DataCopied event for existing.txt, got: {data_copied_paths:?}"
    );
    assert!(
        data_copied_paths
            .iter()
            .any(|p| p == &std::path::Path::new("subdir/nested.txt")),
        "expected DataCopied event for subdir/nested.txt, got: {data_copied_paths:?}"
    );

    // At least 3 data-copied events (the three regular files)
    assert!(
        data_copied_paths.len() >= 3,
        "expected at least 3 DataCopied events, got {}",
        data_copied_paths.len()
    );

    // Verify at least one DirectoryCreated event for the subdir
    let dir_created_paths: Vec<&std::path::Path> = events
        .iter()
        .filter(|e| matches!(e.kind(), ClientEventKind::DirectoryCreated))
        .map(|e| e.relative_path())
        .collect();

    assert!(
        dir_created_paths
            .iter()
            .any(|p| p == &std::path::Path::new("subdir")),
        "expected DirectoryCreated event for subdir, got: {dir_created_paths:?}"
    );

    // Verify the newly created file is marked as created
    let new_file_event = events
        .iter()
        .find(|e| {
            matches!(e.kind(), ClientEventKind::DataCopied)
                && e.relative_path() == std::path::Path::new("new_file.txt")
        })
        .expect("new_file.txt event must exist");
    assert!(
        new_file_event.was_created(),
        "new_file.txt should be marked as newly created"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
