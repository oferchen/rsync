/// A push to a `read only = yes` module is refused, but the module's
/// `post-xfer exec` hook must still run - upstream's post-xfer parent waits for
/// the module child and runs the hook regardless of outcome, so a refused push
/// (child exits `RERR_SYNTAX`) fires the hook with `RSYNC_EXIT_STATUS=1`.
///
/// upstream: clientserver.c:906-931 - post-xfer exec runs after the child exits
/// for any reason; main.c:1166-1169 - the read-only push child exits
/// `RERR_SYNTAX` (1). Regression guard for the daemon skipping the hook on the
/// refuse early-return path.
#[cfg(unix)]
#[test]
fn run_daemon_runs_post_xfer_exec_on_read_only_refuse() {
    use std::os::unix::fs::PermissionsExt;

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    // post-xfer exec hook records $RSYNC_EXIT_STATUS to a marker file so the
    // test can assert the hook ran and saw the refused-transfer exit code.
    let marker = dir.path().join("post.out");
    let hook = dir.path().join("post.sh");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\necho \"$RSYNC_EXIT_STATUS\" > {}\nexit 0\n",
            marker.display()
        ),
    )
    .expect("write hook script");
    fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod hook");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[readonly]\npath = {}\nread only = true\nuse chroot = false\npost-xfer exec = {}\n",
            module_dir.display(),
            hook.display()
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
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream
        .write_all(b"readonly\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(line, "@RSYNCD: OK\n");

    // A push (no --sender): the read-only module must refuse it.
    stream
        .write_all(b"--server\0-logDtpr\0.\0readonly/\0\0")
        .expect("send client args");
    stream.flush().expect("flush client args");

    // The refusal still arrives framed, exactly as the plain refuse test.
    assert_read_only_multiplexed_rejection(&mut reader);

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());

    // The post-xfer hook must have run despite the refusal, recording the
    // RERR_SYNTAX exit status the child conveyed to the post-xfer parent.
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
        RERR_SYNTAX_EXIT_CODE.to_string(),
        "post-xfer exec must run on a refused read-only push with RSYNC_EXIT_STATUS=1",
    );
}
