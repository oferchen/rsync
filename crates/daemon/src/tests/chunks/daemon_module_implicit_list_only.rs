/// PR #6115 regression: `rsync://host:port/module/` with NO destination must
/// enter implicit list-only mode and list the module contents instead of
/// erroring "need at least one source and one destination".
///
/// upstream: options.c:2194 - a single source with `list_only` set lists the
/// module. Fix sites: `client/remote/daemon_transfer/mod.rs` (the
/// `args.len() < 2 && !list_only` guard + dummy-dest synthesis) and the CLI
/// frontend `.../workflow/run.rs` single-operand auto-promotion. `run_client`
/// does not auto-promote `list_only` the way the CLI frontend does, so the test
/// sets it explicitly to exercise the guarded daemon path.
#[cfg(unix)]
#[test]
fn daemon_module_implicit_list_only() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");
    fs::write(module_dir.join("readme.txt"), b"hello\n").expect("write readme");
    fs::write(module_dir.join("data.bin"), b"\x00\x01\x02").expect("write data");

    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[listing]\npath = {}\nread only = true\nuse chroot = false\n",
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
            OsString::from("5"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, port, held_listener);
    drop(probe_stream);

    // Single source operand, NO destination: the fix routes this to
    // run_daemon_transfer, which now accepts args.len() < 2 when list_only is
    // set (pre-fix this returned Err(... need at least one source ..., code 1)).
    let rsync_url = format!("rsync://127.0.0.1:{port}/listing/");
    let client_config = core::client::ClientConfig::builder()
        .transfer_args([OsString::from(&rsync_url)])
        .list_only(true)
        .build();

    let result = core::client::run_client(client_config);

    assert!(
        result.is_ok(),
        "implicit list-only of host::module must exit 0, got: {:?}",
        result.err()
    );

    drop(daemon_handle);
}
