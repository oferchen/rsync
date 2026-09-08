/// A peer-supplied `OPTION key=value` handshake line must never change the
/// module's effective configuration.
///
/// upstream: `--dparam`/`-M` is a DAEMON-side, process-local option with no wire
/// representation. `options.c:867` maps a client-mode `--dparam` to `OPT_DAEMON`
/// ("you meant --daemon"); `options.c:1532` then re-parses argv with
/// `long_daemon_options[]`, where `options.c:875` collects it into `dparam_list`
/// (`options.c:1552-1562`). `loadparm.c:667 set_dparams()` applies that list from
/// exactly two callers, both reading the daemon's OWN argv - `loadparm.c:618-621`
/// during `lp_load()` and `clientserver.c:1745` in `daemon_main()`. A client that
/// passes `--dparam` is refused by `options.c:1584-1589` ("Daemon option(s) used
/// without --daemon.", RERR_SYNTAX), and client-mode `-M` is `--remote-option`
/// (`options.c:859`), a different option entirely.
///
/// This is the sibling of `run_daemon_rejects_push_to_read_only_module` with one
/// `OPTION read only=no` line inserted before the module name. The rejection must
/// be byte-identical: the override is not honoured, so the push still fails with
/// the framed `ERROR: module is read only` + `RERR_SYNTAX`. Honouring it let an
/// UNAUTHENTICATED peer relax `read only`, `use chroot`, `max connections` and
/// the chmod directives, since the module request is processed before the
/// module's own authentication runs.
#[test]
fn run_daemon_ignores_a_client_supplied_daemon_param_override() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[readonly]\npath = {}\nread only = true\nuse chroot = false\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "expected greeting, got: {line}"
    );

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    // The bait line: a peer asking the daemon to drop `read only` on the module
    // it is about to select.
    stream
        .write_all(b"@RSYNCD: OPTION read only=no\n")
        .expect("send daemon param override");
    stream.flush().expect("flush daemon param override");

    stream
        .write_all(b"readonly\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // The override changes nothing, so the module is still selected normally:
    // no `@ERROR: invalid daemon param: ...` and no relaxed configuration.
    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(
        line, "@RSYNCD: OK\n",
        "a client-supplied daemon param must neither be applied nor diagnosed"
    );

    stream
        .write_all(b"--server\0-logDtpr\0.\0readonly/\0\0")
        .expect("send client args");
    stream.flush().expect("flush client args");

    assert_read_only_multiplexed_rejection(&mut reader);

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
