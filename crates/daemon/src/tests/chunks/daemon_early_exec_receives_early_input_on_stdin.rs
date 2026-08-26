/// Drives one `@RSYNCD:` session against a module whose `early exec` hook
/// records `$RSYNC_REQUEST` and everything on its stdin, and returns what the
/// hook wrote.
///
/// `early_input` is sent as upstream's pre-module-name `#early_input=<len>`
/// line plus the raw bytes (clientserver.c:320-322, decoded at
/// clientserver.c:1541-1548).
#[cfg(unix)]
fn early_exec_hook_capture(early_input: Option<&[u8]>) -> String {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    // The hook writes RSYNC_REQUEST and then drains stdin, so one marker
    // carries both halves of the contract and an empty stdin is visible as a
    // bare separator rather than as a missing file.
    let marker = dir.path().join("early.out");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[earlytest]\npath = {}\nread only = false\nuse chroot = false\nearly exec = {{ printf \"%s|\" \"$RSYNC_REQUEST\"; cat; }} > {}\n",
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
    assert!(
        line.starts_with("@RSYNCD:"),
        "expected greeting, got: {line}"
    );

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    if let Some(data) = early_input {
        stream
            .write_all(format!("#early_input={}\n", data.len()).as_bytes())
            .expect("send early-input header");
        stream.write_all(data).expect("send early-input payload");
    }

    stream
        .write_all(b"earlytest\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(line, "@RSYNCD: OK\n");

    // Early exec runs after the OK. Close the connection rather than sending
    // client args: the hook has all of its input by then, and the daemon ends
    // the session on EOF.
    drop(reader);
    drop(stream);
    let _ = handle.join().expect("daemon thread");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(&marker) {
            if !text.is_empty() {
                return text;
            }
        }
        assert!(
            Instant::now() < deadline,
            "early exec hook never wrote its marker"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// The client's `--early-input` bytes must arrive on the early-exec hook's
/// stdin, and the hook must see `RSYNC_REQUEST=(NONE)`.
///
/// Early exec is the only hook that gets those bytes: upstream writes them
/// under `exec_type == 1` (clientserver.c:630-631) and frees `early_input`
/// (clientserver.c:1035-1038) before the pre-xfer and name-converter args are
/// written at all. It also passes a NULL request, which
/// `write_pre_exec_args` renders as `(NONE)` (clientserver.c:614-615, called
/// at clientserver.c:1004).
#[cfg(unix)]
#[test]
fn daemon_early_exec_receives_early_input_on_stdin() {
    let captured = early_exec_hook_capture(Some(b"EARLY-PAYLOAD"));
    assert_eq!(
        captured, "(NONE)|EARLY-PAYLOAD",
        "early exec must see RSYNC_REQUEST=(NONE) and the --early-input bytes on stdin"
    );
}

/// Non-vacuity companion: without `--early-input` the same hook still runs and
/// still terminates, writing the separator and nothing after it. Without this,
/// the assertion above would also pass if the harness simply never delivered a
/// payload, and a hook that blocked forever on stdin would look like a hang
/// rather than a wrong value.
#[cfg(unix)]
#[test]
fn daemon_early_exec_sees_empty_stdin_without_early_input() {
    let captured = early_exec_hook_capture(None);
    assert_eq!(
        captured, "(NONE)|",
        "with no --early-input the hook must reach EOF on an empty stdin"
    );
}
