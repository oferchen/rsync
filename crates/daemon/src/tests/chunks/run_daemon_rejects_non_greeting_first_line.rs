/// The client's first line must be its `@RSYNCD:` version banner.
///
/// upstream: clientserver.c:209-213 - `if (sscanf(buf, "@RSYNCD: %d.%d", ...) < 1)`
/// the daemon writes `@ERROR: protocol startup error` and drops the connection.
/// oc previously fell through to module selection and echoed the raw line back as
/// `@ERROR: Unknown module '<line>'`, which both diverged from upstream and
/// reflected unvalidated input (an HTTP request or port scan) into the reply.
/// Verified against rsync 3.4.4: a first line of `#list` or `GET / HTTP/1.0`
/// draws the startup error, while a greeting followed by `#list` lists modules.
#[test]
fn run_daemon_rejects_non_greeting_first_line() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("temp dir");
    let module_dir = dir.path().join("archive");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("inline.conf");
    fs::write(
        &config_path,
        format!("[archive]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let inline_config = format!("--config={}", config_path.display());
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--no-detach"),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from(inline_config),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, legacy_daemon_greeting());

    // A module name where the version banner belongs must not be taken for a
    // module request.
    stream
        .write_all(b"GET / HTTP/1.0\n")
        .expect("send non-greeting line");
    stream.flush().expect("flush");

    line.clear();
    reader.read_line(&mut line).expect("startup error");
    assert_eq!(
        line, "@ERROR: protocol startup error\n",
        "a non-banner first line must draw upstream's startup error"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
