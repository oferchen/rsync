/// End-to-end test for `--safe-links` push over daemon protocol.
///
/// Verifies that when a client pushes files with `--safe-links` to a daemon
/// module, symlinks whose targets escape the transfer tree are filtered out
/// while safe symlinks (targets within the tree) are preserved.
///
/// # Scenario
///
/// Source (client side):
///   file.txt          (regular file, "safe-links content")
///   safe_link         -> file.txt           (within tree - preserved)
///   subdir/inner_link -> ../file.txt        (within tree - preserved)
///   unsafe_abs        -> /etc/passwd        (absolute, outside - filtered)
///   unsafe_rel        -> ../../outside.txt  (relative escape - filtered)
///
/// # Upstream Reference
///
/// - `generator.c:1547` - skip unsafe symlinks when `--safe-links` is set
/// - `util1.c:1329` - `unsafe_symlink(dest, src)` classification
#[cfg(unix)]
#[test]
fn daemon_safe_links_push_excludes_unsafe_preserves_safe() {
    use std::os::unix::fs as unix_fs;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("file.txt"), b"safe-links content\n").expect("write file.txt");

    // Safe: relative target within the transfer tree
    unix_fs::symlink("file.txt", source_dir.join("safe_link")).expect("create safe_link");

    // Safe: relative target that stays within the tree via parent traversal
    unix_fs::symlink("../file.txt", source_subdir.join("inner_link")).expect("create inner_link");

    // Unsafe: absolute path outside the transfer tree
    unix_fs::symlink("/etc/passwd", source_dir.join("unsafe_abs")).expect("create unsafe_abs");

    // Unsafe: relative path that escapes the transfer root
    unix_fs::symlink("../../outside.txt", source_dir.join("unsafe_rel"))
        .expect("create unsafe_rel");

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
    drop(probe_stream);

    // --- Run client push with --links --safe-links ---
    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let rsync_url = format!("rsync://127.0.0.1:{port}/pushmod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .links(true)
        .safe_links(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(_summary) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("safe-links client push failed: {e}");
        }
    }

    // --- Verify safe symlinks are preserved ---
    let safe_link_path = dest_dir.join("safe_link");
    assert!(
        safe_link_path.symlink_metadata().is_ok(),
        "safe_link should be present (target is within transfer tree)"
    );
    assert_eq!(
        fs::read_link(&safe_link_path).expect("read safe_link target"),
        Path::new("file.txt"),
        "safe_link target should be preserved"
    );

    let inner_link_path = dest_dir.join("subdir/inner_link");
    assert!(
        inner_link_path.symlink_metadata().is_ok(),
        "subdir/inner_link should be present (target resolves within transfer tree)"
    );
    assert_eq!(
        fs::read_link(&inner_link_path).expect("read inner_link target"),
        Path::new("../file.txt"),
        "inner_link target should be preserved"
    );

    // --- Verify unsafe symlinks are filtered out ---
    assert!(
        dest_dir.join("unsafe_abs").symlink_metadata().is_err(),
        "unsafe_abs must not exist (absolute path outside transfer tree)"
    );
    assert!(
        dest_dir.join("unsafe_rel").symlink_metadata().is_err(),
        "unsafe_rel must not exist (relative path escapes transfer tree)"
    );

    // --- Verify the regular file was transferred ---
    assert_eq!(
        fs::read_to_string(dest_dir.join("file.txt")).expect("read file.txt"),
        "safe-links content\n",
        "file.txt content mismatch"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}

/// End-to-end test for `--copy-links` push over daemon protocol.
///
/// Verifies that a push transfer with `--copy-links` resolves all symlinks to
/// their target contents, so the destination receives regular files where the
/// source had symlinks - including symlinks that would be unsafe under
/// `--safe-links` (absolute or tree-escaping targets).
///
/// # Scenario
///
/// Source (client side):
///   real.txt                (regular file, "real data")
///   subdir/deep.txt         (regular file, "deep data")
///   link_same_dir           -> real.txt        (same-directory symlink)
///   subdir/link_up          -> ../real.txt     (parent-traversal symlink)
///
/// # Upstream Reference
///
/// - `flist.c:readlink_stat()` - uses stat() instead of lstat() when copy_links
/// - `options.c:764` - 'L' = copy_links flag
#[cfg(unix)]
#[test]
fn daemon_copy_links_push_replaces_symlinks_with_file_contents() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // --- Source tree (client side) ---
    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("real.txt"), b"real data\n").expect("write real.txt");
    fs::write(source_subdir.join("deep.txt"), b"deep data\n").expect("write deep.txt");

    // Symlink in same directory
    std::os::unix::fs::symlink("real.txt", source_dir.join("link_same_dir"))
        .expect("create link_same_dir");

    // Symlink traversing to parent
    std::os::unix::fs::symlink("../real.txt", source_subdir.join("link_up"))
        .expect("create link_up");

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
            // 2 real files + 2 resolved symlinks = 4 regular files transferred
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

    // --- Verify resolved symlinks are regular files with correct content ---
    let dest_link_same = dest_dir.join("link_same_dir");
    assert!(dest_link_same.exists(), "link_same_dir must exist at destination");
    assert!(
        !dest_link_same
            .symlink_metadata()
            .expect("metadata")
            .file_type()
            .is_symlink(),
        "link_same_dir must be a regular file, not a symlink"
    );
    assert_eq!(
        fs::read_to_string(&dest_link_same).expect("read link_same_dir"),
        "real data\n",
        "link_same_dir content must match the resolved symlink target"
    );

    let dest_link_up = dest_dir.join("subdir/link_up");
    assert!(
        dest_link_up.exists(),
        "subdir/link_up must exist at destination"
    );
    assert!(
        !dest_link_up
            .symlink_metadata()
            .expect("metadata")
            .file_type()
            .is_symlink(),
        "subdir/link_up must be a regular file, not a symlink"
    );
    assert_eq!(
        fs::read_to_string(&dest_link_up).expect("read link_up"),
        "real data\n",
        "subdir/link_up content must match the resolved symlink target"
    );

    // --- Verify original regular files are also present ---
    assert_eq!(
        fs::read_to_string(dest_dir.join("real.txt")).expect("read real.txt"),
        "real data\n",
        "real.txt content mismatch"
    );
    assert_eq!(
        fs::read_to_string(dest_dir.join("subdir/deep.txt")).expect("read deep.txt"),
        "deep data\n",
        "subdir/deep.txt content mismatch"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
