/// End-to-end test for daemon `munge symlinks = yes` on a client push.
///
/// Verifies that when a client pushes symlinks into a daemon module configured
/// with `munge symlinks = yes`, the receiver-side write path prepends the
/// `/rsyncd-munged/` prefix to every symlink target before calling `symlinkat`.
/// The on-disk link is intentionally broken so that following it cannot escape
/// the module root, matching the anti-escape guard upstream documents in
/// `clientserver.c` and `flist.c`.
///
/// # Scenario
///
/// Source (client side):
///   real_file.txt    (regular file, "real")
///   abs_link         -> /etc/passwd        (absolute, outside module)
///   rel_link         -> real_file.txt      (relative, inside module)
///   parent_link      -> ../escape          (relative escape)
///
/// Destination (daemon module with `munge symlinks = yes`):
///   real_file.txt    (regular file copy)
///   abs_link         -> /rsyncd-munged//etc/passwd
///   rel_link         -> /rsyncd-munged/real_file.txt
///   parent_link      -> /rsyncd-munged/../escape
///
/// # Upstream Reference
///
/// - `clientserver.c:992-1004` - daemon resolves `munge_symlinks` from
///   `lp_munge_symlinks()` before fork-and-serve.
/// - `flist.c:1122-1126` - receiver prepends `SYMLINK_PREFIX` to the wire
///   target so the on-disk link cannot resolve outside the module root.
/// - `rsync.h:36` - `SYMLINK_PREFIX "/rsyncd-munged/"` (trailing slash kept).
#[cfg(unix)]
#[test]
fn daemon_munge_symlinks_push_prepends_prefix() {
    use std::os::unix::fs as unix_fs;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source");

    fs::write(source_dir.join("real_file.txt"), b"real\n").expect("write real_file.txt");
    unix_fs::symlink("/etc/passwd", source_dir.join("abs_link")).expect("create abs_link");
    unix_fs::symlink("real_file.txt", source_dir.join("rel_link")).expect("create rel_link");
    unix_fs::symlink("../escape", source_dir.join("parent_link")).expect("create parent_link");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[mungemod]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n\
         munge symlinks = yes\n",
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
    let rsync_url = format!("rsync://127.0.0.1:{port}/mungemod/");

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([source_arg, OsString::from(&rsync_url)])
        .links(true)
        .build();

    let result = core::client::run_client(client_config);
    if let Err(e) = &result {
        let _ = daemon_handle.join();
        panic!("munge symlinks daemon push failed: {e}");
    }

    let abs_link = fs::read_link(dest_dir.join("abs_link")).expect("read abs_link target");
    assert_eq!(
        abs_link,
        std::path::Path::new("/rsyncd-munged//etc/passwd"),
        "absolute targets must carry the `/rsyncd-munged/` prefix verbatim \
         so the kernel cannot follow them out of the module root \
         (upstream flist.c:1122-1126)",
    );

    let rel_link = fs::read_link(dest_dir.join("rel_link")).expect("read rel_link target");
    assert_eq!(
        rel_link,
        std::path::Path::new("/rsyncd-munged/real_file.txt"),
        "relative targets must also carry the prefix, turning the link into a \
         disabled placeholder rather than a usable in-tree shortcut",
    );

    let parent_link =
        fs::read_link(dest_dir.join("parent_link")).expect("read parent_link target");
    assert_eq!(
        parent_link,
        std::path::Path::new("/rsyncd-munged/../escape"),
        "parent-escape targets must carry the prefix unmodified, so the \
         munge guard composes with `--safe-links` rather than substituting it",
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
