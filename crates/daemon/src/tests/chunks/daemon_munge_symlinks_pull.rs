/// End-to-end test for daemon `munge symlinks = yes` on a client pull.
///
/// Verifies that when a client pulls from a daemon module whose on-disk
/// content was previously munged, the daemon-side sender strips the
/// `/rsyncd-munged/` prefix from every symlink target before writing the
/// file-list entry onto the wire. The client receives the original target so
/// the link is usable again on the client side, matching upstream's symmetric
/// round-trip semantics.
///
/// # Scenario
///
/// Source (daemon module with `munge symlinks = yes`):
///   real_file.txt    (regular file, "real")
///   abs_link         -> /rsyncd-munged//etc/passwd
///   rel_link         -> /rsyncd-munged/real_file.txt
///   parent_link      -> /rsyncd-munged/../escape
///   bare_link        -> already_unmunged  (no prefix - passthrough)
///
/// Destination (client side, initially empty):
///   real_file.txt    (regular file copy)
///   abs_link         -> /etc/passwd
///   rel_link         -> real_file.txt
///   parent_link      -> ../escape
///   bare_link        -> already_unmunged
///
/// # Upstream Reference
///
/// - `clientserver.c:992-1004` - daemon resolves `munge_symlinks` from
///   `lp_munge_symlinks()` for sender and receiver alike.
/// - `flist.c:222-226` - sender strips `SYMLINK_PREFIX` after `readlink()`
///   so the wire bytes match what a non-munge daemon would send.
/// - `rsync.h:36` - `SYMLINK_PREFIX "/rsyncd-munged/"` (trailing slash kept).
#[cfg(unix)]
#[test]
fn daemon_munge_symlinks_pull_strips_prefix() {
    use std::os::unix::fs as unix_fs;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");

    fs::write(source_dir.join("real_file.txt"), b"real\n").expect("write real_file.txt");

    // Pre-munged on-disk targets, exactly as the receiver would have laid them
    // down on a prior push. The kernel cannot follow these because the
    // `/rsyncd-munged/` directory does not exist; that is the security property
    // upstream relies on.
    unix_fs::symlink("/rsyncd-munged//etc/passwd", source_dir.join("abs_link"))
        .expect("create abs_link");
    unix_fs::symlink("/rsyncd-munged/real_file.txt", source_dir.join("rel_link"))
        .expect("create rel_link");
    unix_fs::symlink("/rsyncd-munged/../escape", source_dir.join("parent_link"))
        .expect("create parent_link");

    // upstream: flist.c:222 - targets that lack the prefix pass through
    // unchanged via the `llen > SYMLINK_PREFIX_LEN && strncmp(...) == 0`
    // guard. Cover that branch alongside the strip path.
    unix_fs::symlink("already_unmunged", source_dir.join("bare_link"))
        .expect("create bare_link");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mungemod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n\
         munge symlinks = yes\n",
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
    drop(probe_stream);

    let rsync_url = format!("rsync://127.0.0.1:{port}/mungemod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&rsync_url), OsString::from(dest_dir.as_os_str())])
        .links(true)
        .build();

    let result = core::client::run_client(client_config);
    if let Err(e) = &result {
        let _ = daemon_handle.join();
        panic!("munge symlinks daemon pull failed: {e}");
    }

    let abs_link = fs::read_link(dest_dir.join("abs_link")).expect("read abs_link target");
    assert_eq!(
        abs_link,
        std::path::Path::new("/etc/passwd"),
        "daemon sender must strip the `/rsyncd-munged/` prefix before writing \
         the wire entry so the client receives the original absolute target \
         (upstream flist.c:222-226)",
    );

    let rel_link = fs::read_link(dest_dir.join("rel_link")).expect("read rel_link target");
    assert_eq!(
        rel_link,
        std::path::Path::new("real_file.txt"),
        "relative targets must round-trip back to their pre-munge form so the \
         client side link points at the actual file",
    );

    let parent_link =
        fs::read_link(dest_dir.join("parent_link")).expect("read parent_link target");
    assert_eq!(
        parent_link,
        std::path::Path::new("../escape"),
        "parent-escape targets must also strip cleanly; the security property \
         lives on the daemon disk, not in the wire format",
    );

    let bare_link = fs::read_link(dest_dir.join("bare_link")).expect("read bare_link target");
    assert_eq!(
        bare_link,
        std::path::Path::new("already_unmunged"),
        "targets without the prefix must pass through unchanged - the strip \
         is a `starts_with` operation, not an unconditional shorten",
    );

    let real_content =
        fs::read_to_string(dest_dir.join("real_file.txt")).expect("read real_file.txt");
    assert_eq!(
        real_content, "real\n",
        "regular files must transfer unchanged regardless of munge symlinks",
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
}
