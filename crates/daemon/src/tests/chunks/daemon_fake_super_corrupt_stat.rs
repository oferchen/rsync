/// A pull from a `fake super = yes` module whose file carries an unparsable
/// `%stat` must end with `RERR_FILEIO` (11), as upstream's sender does.
///
/// # Upstream Reference
///
/// - `xattrs.c:1253-1258` - get_stat_xattr() prints `Corrupt %s xattr
///   attached to %s` and calls `exit_cleanup(RERR_FILEIO)`.
#[cfg(target_os = "linux")]
#[test]
fn daemon_fake_super_pull_of_a_corrupt_stat_exits_file_io() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");
    let bad = module_dir.join("bad");
    fs::write(&bad, b"x").expect("write module file");
    if xattr::set(&bad, metadata::FAKE_SUPER_XATTR, b"garbage").is_err() {
        eprintln!("skipping: the filesystem rejects user xattrs");
        return;
    }
    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");
    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[fakesuper]\n\
             path = {}\n\
             use chroot = false\n\
             fake super = yes\n",
            module_dir.display()
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
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();
    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");
    let result = core::client::run_client(
        core::client::ClientConfig::builder()
            .transfer_args([
                OsString::from(format!("rsync://127.0.0.1:{port}/fakesuper/")),
                dest_arg,
            ])
            .recursive(true)
            .build(),
    );
    let _ = finish_daemon(daemon_handle);
    let error = result.expect_err("a corrupt %stat must end the pull");
    assert_eq!(error.exit_code(), 11, "got {error}");
    assert!(
        !dest_dir.join("bad").exists(),
        "the file with the corrupt %stat must not be copied"
    );
}
