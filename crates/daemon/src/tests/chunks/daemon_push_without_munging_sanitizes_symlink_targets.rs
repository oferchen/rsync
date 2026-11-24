/// WHY: with `munge symlinks = no`, a daemon module outside a chroot still
/// sanitizes pushed symlink targets, because its path survives the
/// (absent) chroot and so sets `sanitize_paths` (clientserver.c:1071-1072,
/// flist.c:1557-1558). The decision is made by the daemon and carried to the
/// receiver in its server config; this cell fails if that hand-off is lost and
/// the receiver stores the targets verbatim. Measured against rsync 3.5.2 in
/// the same configuration: `/etc/passwd` -> `etc/passwd`, `../escape` ->
/// `escape`, `sub/up2 -> ../../x` -> `../x`.
#[cfg(unix)]
#[test]
fn daemon_push_without_munging_sanitizes_symlink_targets() {
    use std::os::unix::fs as unix_fs;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(source_dir.join("sub")).expect("create source");
    unix_fs::symlink("/etc/passwd", source_dir.join("abs")).expect("create abs");
    unix_fs::symlink("../escape", source_dir.join("up")).expect("create up");
    unix_fs::symlink("../../x", source_dir.join("sub/up2")).expect("create sub/up2");
    unix_fs::symlink("plain", source_dir.join("rel")).expect("create rel");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[mod]\n\
             path = {}\n\
             read only = false\n\
             use chroot = false\n\
             munge symlinks = no\n",
            dest_dir.display()
        ),
    )
    .expect("write daemon config");

    let (port, held_listener) = allocate_test_port();
    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
        ])
        .build();
    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([
            source_arg,
            OsString::from(format!("rsync://127.0.0.1:{port}/mod/")),
        ])
        .recursive(true)
        .links(true)
        .build();
    let result = core::client::run_client(client_config);
    daemon_handle.terminate();
    if let Err(e) = result {
        panic!("daemon push failed: {e}");
    }

    let stored = |name: &str| fs::read_link(dest_dir.join(name)).expect(name);
    assert_eq!(stored("abs"), std::path::Path::new("etc/passwd"));
    assert_eq!(stored("up"), std::path::Path::new("escape"));
    assert_eq!(stored("sub/up2"), std::path::Path::new("../x"));
    assert_eq!(stored("rel"), std::path::Path::new("plain"));
}
