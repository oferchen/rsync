/// A failing `early exec` command aborts the module session before any
/// transfer starts, but the module's `post-xfer exec` hook must still run -
/// upstream's post-xfer parent waits for the module child and runs the hook
/// regardless of outcome, so an early-exec abort (child returns `-1`, which
/// `_exit()` truncates to 255) fires the hook with `RSYNC_EXIT_STATUS=255`.
///
/// upstream: clientserver.c:908-933 - post-xfer exec runs after the child
/// exits for any reason; clientserver.c:945-949 - early exec runs after the
/// post-xfer-exec fork point, and its failure returns `-1` from the module
/// child. Regression guard for the daemon skipping the hook on this
/// early-return abort path.
#[cfg(unix)]
#[test]
fn run_daemon_runs_post_xfer_exec_on_early_exec_failure() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    // post-xfer exec hook records $RSYNC_EXIT_STATUS to a marker file so the
    // test can assert the hook ran and saw the aborted-session exit code.
    let marker = dir.path().join("post.out");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[earlytest]\npath = {}\nread only = false\nuse chroot = false\nearly exec = exit 1\npost-xfer exec = echo \"$RSYNC_EXIT_STATUS\" > {}\n",
            module_dir.display(),
            marker.display()
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
    assert!(line.starts_with("@RSYNCD:"), "expected greeting, got: {line}");

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream
        .write_all(b"earlytest\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // Daemon sends OK for unauthenticated modules before running early exec.
    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(line, "@RSYNCD: OK\n");

    // Early exec fails immediately - the daemon never waits for client args.
    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert!(
        line.starts_with("@ERROR:"),
        "expected @ERROR, got: {line}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());

    // The post-xfer hook must have run despite the early-exec abort,
    // recording the truncated `-1` exit status the child conveyed to the
    // post-xfer parent.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut contents = String::new();
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(&marker) {
            if !text.trim().is_empty() {
                contents = text;
                break;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        contents.trim(),
        MODULE_ABORT_EXIT_CODE.to_string(),
        "post-xfer exec must run on an early-exec abort with RSYNC_EXIT_STATUS=255",
    );
}
