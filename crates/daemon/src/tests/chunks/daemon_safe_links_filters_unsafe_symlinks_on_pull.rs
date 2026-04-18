/// End-to-end test for `--safe-links` filtering over daemon pull protocol.
///
/// Verifies that a daemon pull with `--safe-links` filters out symlinks whose
/// targets escape the transfer tree while preserving symlinks that point within
/// the module path.
///
/// # Scenario
///
/// Source (daemon module):
///   file.txt          (regular file, "hello")
///   safe_link         -> file.txt           (within module - preserved)
///   subdir/inner_link -> ../file.txt        (within module - preserved)
///   unsafe_link       -> /etc/passwd        (absolute, outside - filtered)
///   escape_link       -> ../../outside.txt  (relative escape - filtered)
///
/// After pull with `--safe-links`:
///   file.txt          - present
///   safe_link         - present (target within tree)
///   subdir/inner_link - present (target within tree)
///   unsafe_link       - absent  (target outside tree)
///   escape_link       - absent  (target outside tree)
///
/// # Upstream Reference
///
/// - `generator.c:1547` - skip unsafe symlinks when `--safe-links` is set
/// - `util1.c:1329` - `unsafe_symlink(dest, src)` classification
#[cfg(unix)]
#[test]
fn daemon_safe_links_filters_unsafe_symlinks_on_pull() {
    use std::os::unix::fs as unix_fs;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    let source_subdir = source_dir.join("subdir");
    fs::create_dir_all(&source_subdir).expect("create source/subdir");

    fs::write(source_dir.join("file.txt"), b"hello\n").expect("write file.txt");

    // Safe symlink: relative target within the module tree
    unix_fs::symlink("file.txt", source_dir.join("safe_link")).expect("create safe_link");

    // Safe symlink: relative target that stays within the module via parent traversal
    unix_fs::symlink("../file.txt", source_subdir.join("inner_link"))
        .expect("create inner_link");

    // Unsafe symlink: absolute path outside the transfer tree
    unix_fs::symlink("/etc/passwd", source_dir.join("unsafe_link"))
        .expect("create unsafe_link");

    // Unsafe symlink: relative path that escapes the module root
    unix_fs::symlink("../../outside.txt", source_dir.join("escape_link"))
        .expect("create escape_link");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[safemod]\n\
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

    let rsync_url = format!("rsync://127.0.0.1:{port}/safemod/");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([
            OsString::from(&rsync_url),
            OsString::from(dest_dir.as_os_str()),
        ])
        .links(true)
        .safe_links(true)
        .build();

    let result = core::client::run_client(client_config);

    match &result {
        Ok(_summary) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("safe-links client pull failed: {e}");
        }
    }

    let safe_link_path = dest_dir.join("safe_link");
    assert!(
        safe_link_path.symlink_metadata().is_ok(),
        "safe_link should be present (target is within module tree)"
    );
    let safe_target = fs::read_link(&safe_link_path).expect("read safe_link target");
    assert_eq!(
        safe_target.to_str().unwrap(),
        "file.txt",
        "safe_link target should be preserved"
    );

    let inner_link_path = dest_dir.join("subdir/inner_link");
    assert!(
        inner_link_path.symlink_metadata().is_ok(),
        "subdir/inner_link should be present (target resolves within module tree)"
    );
    let inner_target = fs::read_link(&inner_link_path).expect("read inner_link target");
    assert_eq!(
        inner_target.to_str().unwrap(),
        "../file.txt",
        "inner_link target should be preserved"
    );

    assert!(
        !dest_dir.join("unsafe_link").exists()
            && dest_dir.join("unsafe_link").symlink_metadata().is_err(),
        "unsafe_link must not exist (absolute path outside transfer tree)"
    );

    assert!(
        !dest_dir.join("escape_link").exists()
            && dest_dir.join("escape_link").symlink_metadata().is_err(),
        "escape_link must not exist (relative path escapes transfer tree)"
    );

    let file_content = fs::read_to_string(dest_dir.join("file.txt")).expect("read file.txt");
    assert_eq!(file_content, "hello\n", "file.txt content mismatch");

    // Daemon exits after serving max_sessions connections
    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
