/// End-to-end test for in-place writes during a daemon push transfer.
///
/// Verifies that `--inplace` writes directly to destination files rather than
/// using the default temp-file-then-rename strategy. The test pre-populates
/// destination files with old content, records their inode numbers, pushes
/// updated source files with `inplace(true)`, and then confirms that:
///
/// 1. Destination content matches the source (transfer succeeded).
/// 2. Destination inode numbers are unchanged (proving the files were
///    overwritten in place rather than replaced via temp + rename).
///
/// # Upstream Reference
///
/// - `receiver.c` - in-place write path (skips temp file when `inplace` is set)
/// - `options.c` - `--inplace` handling
#[cfg(unix)]
#[test]
fn daemon_inplace_push_preserves_destination_inodes() {
    use std::os::unix::fs::MetadataExt;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");

    let source_content_a = b"alpha new content for inplace test\n";
    let source_content_b = b"beta new content for inplace test - slightly longer\n";

    fs::write(source_dir.join("alpha.txt"), source_content_a).expect("write alpha.txt");
    fs::write(source_dir.join("beta.txt"), source_content_b).expect("write beta.txt");

    // --- Destination (served by daemon, writable, pre-populated with old content) ---
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    // Pre-populate destination with different content so quick-check triggers
    // an update. Use different sizes to guarantee quick-check detects staleness.
    fs::write(dest_dir.join("alpha.txt"), b"old alpha\n").expect("write dest alpha.txt");
    fs::write(dest_dir.join("beta.txt"), b"old beta\n").expect("write dest beta.txt");

    // Backdate destination files so mtime differs from source, ensuring
    // quick-check does not skip the transfer.
    let old_time = filetime::FileTime::from_unix_time(1_000_000, 0);
    filetime::set_file_mtime(dest_dir.join("alpha.txt"), old_time)
        .expect("backdate dest alpha.txt");
    filetime::set_file_mtime(dest_dir.join("beta.txt"), old_time)
        .expect("backdate dest beta.txt");

    // Record inode numbers before the push - these must be preserved by inplace.
    let inode_alpha_before = fs::metadata(dest_dir.join("alpha.txt"))
        .expect("metadata alpha before")
        .ino();
    let inode_beta_before = fs::metadata(dest_dir.join("beta.txt"))
        .expect("metadata beta before")
        .ino();

    // --- Daemon config with max-sessions=2 (probe + 1 push) ---
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

    // --- Push with inplace(true) ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .inplace(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(summary) => {
            assert!(
                summary.files_copied() >= 2,
                "inplace push must transfer at least 2 files, got {}",
                summary.files_copied()
            );
        }
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("inplace push failed: {e}");
        }
    }

    // --- Verify destination content matches source ---
    assert_eq!(
        fs::read(dest_dir.join("alpha.txt")).expect("read dest alpha.txt"),
        source_content_a,
        "alpha.txt content mismatch after inplace push"
    );
    assert_eq!(
        fs::read(dest_dir.join("beta.txt")).expect("read dest beta.txt"),
        source_content_b,
        "beta.txt content mismatch after inplace push"
    );

    // --- Verify inode numbers are preserved (proves inplace, not temp+rename) ---
    let inode_alpha_after = fs::metadata(dest_dir.join("alpha.txt"))
        .expect("metadata alpha after")
        .ino();
    let inode_beta_after = fs::metadata(dest_dir.join("beta.txt"))
        .expect("metadata beta after")
        .ino();

    assert_eq!(
        inode_alpha_before, inode_alpha_after,
        "alpha.txt inode changed - transfer used temp+rename instead of inplace"
    );
    assert_eq!(
        inode_beta_before, inode_beta_after,
        "beta.txt inode changed - transfer used temp+rename instead of inplace"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
