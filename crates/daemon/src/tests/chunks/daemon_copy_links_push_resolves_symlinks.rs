/// End-to-end test for `--copy-links` push over daemon protocol.
///
/// Verifies that a push transfer with `--copy-links` resolves symlinks to their
/// target files, so the destination receives regular files where the source had
/// symlinks.
///
/// # Scenario
///
/// Source (client side):
///   real_file.txt    (regular file, "real content")
///   link_to_file.txt (symlink -> real_file.txt)
///   subdir/nested.txt       (regular file, "nested content")
///   subdir/link_nested.txt  (symlink -> nested.txt)
///
/// Destination (daemon module):
///   (empty directory)
///
/// With `--copy-links`, the client resolves all symlinks before transmission.
/// The destination should contain only regular files - no symlinks.
///
/// # Upstream Reference
///
/// - `flist.c:readlink_stat()` - uses stat() instead of lstat() when copy_links
/// - `options.c:764` - 'L' = copy_links flag
#[cfg(unix)]
#[test]
fn daemon_copy_links_push_resolves_symlinks() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("real_file.txt"), b"real content\n").expect("write real_file");

    // Create symlink to real_file.txt
    std::os::unix::fs::symlink("real_file.txt", source_dir.join("link_to_file.txt"))
        .expect("create symlink link_to_file.txt");

    fs::write(source_subdir.join("nested.txt"), b"nested content\n").expect("write nested");

    // Create symlink to nested.txt (relative, within same directory)
    std::os::unix::fs::symlink("nested.txt", source_subdir.join("link_nested.txt"))
        .expect("create symlink link_nested.txt");

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

    // --- Run client push with --copy-links ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .copy_links(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            // Both real files and resolved symlinks should be transferred
            assert!(
                summary.files_copied() >= 4,
                "expected at least 4 files transferred (2 real + 2 resolved symlinks), got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("copy-links client push failed: {e}");
        }
    }

    // Verify destination has regular files where source had symlinks
    let dest_link = dest_dir.join("link_to_file.txt");
    assert!(dest_link.exists(), "link_to_file.txt must exist at destination");
    assert!(
        !dest_link.symlink_metadata().expect("metadata").file_type().is_symlink(),
        "link_to_file.txt must be a regular file, not a symlink"
    );
    assert_eq!(
        fs::read(&dest_link).expect("read link_to_file.txt"),
        b"real content\n",
        "link_to_file.txt content must match the symlink target"
    );

    let dest_real = dest_dir.join("real_file.txt");
    assert!(dest_real.exists(), "real_file.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_real).expect("read real_file.txt"),
        b"real content\n",
        "real_file.txt content mismatch"
    );

    let dest_nested_link = dest_dir.join("subdir/link_nested.txt");
    assert!(
        dest_nested_link.exists(),
        "subdir/link_nested.txt must exist at destination"
    );
    assert!(
        !dest_nested_link
            .symlink_metadata()
            .expect("metadata")
            .file_type()
            .is_symlink(),
        "subdir/link_nested.txt must be a regular file, not a symlink"
    );
    assert_eq!(
        fs::read(&dest_nested_link).expect("read link_nested.txt"),
        b"nested content\n",
        "subdir/link_nested.txt content must match the symlink target"
    );

    let dest_nested = dest_dir.join("subdir/nested.txt");
    assert!(dest_nested.exists(), "subdir/nested.txt must exist at destination");
    assert_eq!(
        fs::read(&dest_nested).expect("read nested.txt"),
        b"nested content\n",
        "subdir/nested.txt content mismatch"
    );

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
