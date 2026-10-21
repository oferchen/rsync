/// A `fake super = yes` module caps `-XX` at `-X`, so a pushing client cannot
/// plant its own `rsync.%stat` on the module's files.
///
/// Under `-XX` the internal `rsync.%FOO` names travel as plain xattrs, and a
/// receiver at level 2 stores them. A fake-super module records its own
/// `%stat`, and a forged one would claim any owner or setuid mode.
///
/// # Upstream Reference
///
/// - `clientserver.c:1197-1200` - `if (lp_fake_super(module_id)) { if
///   (preserve_xattrs > 1) preserve_xattrs = 1; am_root = -1; }`.
/// - `xattrs.c:902-906` - receive_xattr() drops `rsync.%FOO` below level 2.
#[cfg(all(target_os = "linux", feature = "xattr"))]
#[test]
fn daemon_fake_super_module_drops_a_client_supplied_stat_under_double_x() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");
    let payload = source_dir.join("f");
    fs::write(&payload, b"data\n").expect("write payload");
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&payload, fs::Permissions::from_mode(0o644)).expect("chmod payload");
    }
    if xattr::set(&payload, metadata::FAKE_SUPER_XATTR, b"104755 0,0 0:0").is_err() {
        eprintln!("skipping: the filesystem rejects user xattrs");
        return;
    }
    xattr::set(&payload, "user.keep", b"1").expect("set an ordinary xattr");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");
    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[fakesuper]\n\
             path = {}\n\
             read only = false\n\
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
    let mut source_arg = source_dir.into_os_string();
    source_arg.push("/");
    let result = core::client::run_client(
        core::client::ClientConfig::builder()
            .transfer_args([
                source_arg,
                OsString::from(format!("rsync://127.0.0.1:{port}/fakesuper/")),
            ])
            .recursive(true)
            .owner(true)
            .group(true)
            .permissions(true)
            .xattrs(2)
            .build(),
    );
    let _ = finish_daemon(daemon_handle);
    if let Err(e) = result {
        panic!("push into the fake-super module failed: {e}");
    }
    let dest = module_dir.join("f");
    assert_eq!(
        xattr::get(&dest, "user.keep").expect("read user.keep"),
        Some(b"1".to_vec()),
        "ordinary xattrs still transfer"
    );
    assert_eq!(
        xattr::get(&dest, metadata::FAKE_SUPER_XATTR).expect("read %stat"),
        None,
        "the client's %stat must not land; a same-owner 0644 file needs none"
    );
}
