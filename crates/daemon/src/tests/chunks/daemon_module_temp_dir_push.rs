/// Pushes one file into a module whose `temp dir` is `temp_dir`, returning the
/// client result and whether the file landed in the module.
#[cfg(unix)]
fn push_into_module_with_temp_dir(temp_dir: &str, create: bool) -> (Result<(), String>, bool) {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");
    fs::write(source_dir.join("f.txt"), b"temp dir push\n").expect("write source");

    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module");
    if create {
        fs::create_dir(module_dir.join("tt")).expect("create module temp dir");
    }

    let config_file = temp.path().join("rsyncd.conf");
    fs::write(
        &config_file,
        format!(
            "[m]\npath = {}\nread only = false\nuse chroot = false\ntemp dir = {temp_dir}\n",
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

    let client_config = core::client::ClientConfig::builder()
        .transfer_args([
            source_dir.join("f.txt").into_os_string(),
            OsString::from(format!("rsync://127.0.0.1:{port}/m/")),
        ])
        .build();
    let result = core::client::run_client(client_config)
        .map(|_| ())
        .map_err(|error| format!("{} [exit {}]", error, error.exit_code()));
    let _ = finish_daemon(daemon_handle);
    (result, module_dir.join("f.txt").exists())
}

/// A module `temp dir` names a directory inside the module, whether it is
/// written absolute or relative.
///
/// upstream: options.c:2415-2416 re-roots an absolute `tmpdir` at the module
/// (`sanitize_path(NULL, tmpdir, NULL, 0, SP_DEFAULT)`), and main.c:1059
/// do_recv() resolves a relative one from the destination it has chdir'd
/// into. rsync 3.5.1 serves both configs below with exit 0.
#[cfg(unix)]
#[test]
fn daemon_module_temp_dir_resolves_inside_the_module() {
    for temp_dir in ["/tt", "tt"] {
        let (result, landed) = push_into_module_with_temp_dir(temp_dir, true);
        assert!(result.is_ok(), "temp dir = {temp_dir}: {result:?}");
        assert!(landed, "temp dir = {temp_dir}: file did not land");
    }
}

/// A module `temp dir` that does not exist ends the push with upstream's
/// message and `RERR_SYNTAX`.
///
/// upstream: main.c:1066-1069 - `The temp-dir does not exist: %s` and
/// `exit_cleanup(RERR_SYNTAX)`.
#[cfg(unix)]
#[test]
fn daemon_module_temp_dir_missing_is_a_syntax_error() {
    let (result, landed) = push_into_module_with_temp_dir("nonexist", false);
    let error = result.expect_err("a missing temp dir must fail the push");
    assert!(error.contains("[exit 1]"), "{error}");
    assert!(!landed);
}
