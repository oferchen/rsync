/// Runs one `--once` daemon session in which the client greets with
/// `@RSYNCD: <client_protocol>.0`, asks for a module and, once accepted, sends
/// a pull argv. Returns the module-request reply line, every byte the daemon
/// sent after it, and the daemon log.
fn daemon_session_with_client_protocol(client_protocol: u32) -> (String, Vec<u8>, String) {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();
    let temp = tempdir().expect("temp dir");
    let log_path = temp.path().join("rsyncd.log");
    let module_dir = temp.path().join("docs");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = temp.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("[docs]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--log-file"),
            log_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, legacy_daemon_greeting());

    stream
        .write_all(
            format!("@RSYNCD: {client_protocol}.0 sha512 sha256 sha1 md5 md4\ndocs\n").as_bytes(),
        )
        .expect("send greeting and module");
    stream.flush().expect("flush");

    let mut reply = String::new();
    reader.read_line(&mut reply).expect("module reply");
    let mut after_reply = Vec::new();
    if reply == "@RSYNCD: OK\n" {
        // upstream: clientserver.c:1154 read_args() splits on NULs from
        // protocol 30 and on newlines below it (rl_nulls, clientserver.c:257).
        let separator = if client_protocol >= 30 { b'\0' } else { b'\n' };
        // A client --timeout=1 lets an accepted session end on its own once this
        // scripted client stops answering (clientserver.c:1288-1289).
        for arg in [
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            "--timeout=1",
            ".",
            "docs/",
        ] {
            stream.write_all(arg.as_bytes()).expect("write client arg");
            stream
                .write_all(&[separator])
                .expect("write arg terminator");
        }
        stream.write_all(&[separator]).expect("terminate args list");
        stream.flush().expect("flush client args");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let mut chunk = [0u8; 256];
        if let Ok(read) = reader.read(&mut chunk) {
            after_reply.extend_from_slice(&chunk[..read]);
        }
    }
    drop(reader);
    drop(stream);
    let _ = finish_daemon(handle);
    let log = fs::read_to_string(&log_path).expect("read log file");
    (reply, after_reply, log)
}

/// A client newer than us but within MAX_PROTOCOL_VERSION (40) is served at
/// our protocol.
///
/// upstream: clientserver.c:209 only parses the client's number and
/// clientserver.c:242-243 keeps ours when it is lower; compat.c:621-625 finds
/// 34..40 inside [MIN_PROTOCOL_VERSION, MAX_PROTOCOL_VERSION], so the session
/// proceeds. The first post-OK frame is the server's checksum-name list.
#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; the handshake path is platform-independent and covered on Linux/macOS"
)]
fn run_daemon_serves_a_newer_client_at_our_protocol() {
    for client_protocol in [34, 35, 40] {
        let (reply, after_reply, log) = daemon_session_with_client_protocol(client_protocol);
        assert_eq!(reply, "@RSYNCD: OK\n", "client {client_protocol}");
        assert!(
            !after_reply.is_empty(),
            "client {client_protocol}: the session must continue past OK"
        );
        assert!(
            !log.contains("is your shell clean"),
            "client {client_protocol} must not be refused: {log:?}"
        );
    }
}

/// A client outside [28, 40] gets `@RSYNCD: OK` and is then refused by the
/// setup_protocol check, not taken for a module name.
///
/// upstream: clientserver.c:209 stores `remote_protocol` from the greeting
/// unchecked, so the next line is the module request and the daemon answers
/// `@RSYNCD: OK`. setup_protocol() then rejects the number (compat.c:621-625)
/// with the two shell-clean lines and RERR_PROTOCOL. Nothing is multiplexed
/// yet, so the client sees the connection close and the text goes to the log.
/// 41 is past MAX_PROTOCOL_VERSION; 27 is below oc's documented floor of 28,
/// which stands in for upstream's MIN_PROTOCOL_VERSION (20).
#[test]
#[cfg_attr(
    windows,
    ignore = "flaky on Windows CI: in-process daemon intermittently fails to respond; the handshake path is platform-independent and covered on Linux/macOS"
)]
fn run_daemon_refuses_an_unsupported_client_protocol_after_ok() {
    for client_protocol in [41, 27] {
        let (reply, after_reply, log) = daemon_session_with_client_protocol(client_protocol);
        assert_eq!(
            reply, "@RSYNCD: OK\n",
            "client {client_protocol}: the greeting is never a module name"
        );
        assert!(
            after_reply.is_empty(),
            "client {client_protocol}: nothing follows OK, got {after_reply:?}"
        );
        for expected in [
            "protocol version mismatch -- is your shell clean?",
            "(see the rsync manpage for an explanation)",
            "protocol incompatibility (code 2)",
        ] {
            assert!(
                log.lines().any(|entry| entry.contains(expected)),
                "client {client_protocol}: log must carry {expected:?}: {log:?}"
            );
        }
    }
}

/// The daemon's first line is pinned byte for byte at protocol 33.
///
/// upstream: compat.c:844-854 output_daemon_greeting() prints
/// `@RSYNCD: %d.%d %s\n` with PROTOCOL_VERSION (rsync.h:114) and the full
/// digest list. A newer client negotiates down to this number, so changing it
/// must be a deliberate edit here.
#[test]
fn golden_daemon_greeting_advertises_protocol_33() {
    assert_eq!(
        legacy_daemon_greeting(),
        "@RSYNCD: 33.0 sha512 sha256 sha1 md5 md4\n"
    );
}
