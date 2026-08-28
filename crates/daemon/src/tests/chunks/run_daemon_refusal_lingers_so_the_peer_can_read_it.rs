/// Regression test: the daemon must not close a refused connection the instant
/// it has written the error.
///
/// The peer normally still has unread bytes sitting in the daemon's receive
/// queue when a refusal is decided - a real client has already pushed its whole
/// argument vector. Closing a socket in that state lets the kernel discard the
/// pending send queue and answer with RST, so the `MSG_ERROR_XFER` +
/// `MSG_ERROR_EXIT` frames the daemon just flushed are thrown away and the
/// client reports `Connection reset by peer` instead of the refusal. Whether
/// the peer's read won that race decided the outcome, which is why it surfaced
/// as an intermittent `daemon-refuse-delete-alias` failure on the upstream
/// 3.5.0 testsuite rather than a hard one.
///
/// upstream never closes immediately: `cleanup.c:263-264` sleeps 100 ms for any
/// server exiting non-zero, and `clientserver.c:1266` adds 400 ms on the
/// post-`@RSYNCD: OK` error path before `exit_cleanup(RERR_UNSUPPORTED)`.
///
/// What this pins is the observable half of that contract: the frames arrive
/// intact AND the connection is still open for at least the server-exit wait
/// after the refusal was written. Asserting a lower bound on a sleep is sound
/// in a way that asserting an upper bound would not be - scheduler jitter only
/// ever pushes the measurement further past the bound, never under it.
///
/// Whether the close is seen as FIN or RST is platform-dependent and is
/// deliberately not asserted. Measured: macOS loopback still delivers the queued
/// frames even with a zero-length linger, while Windows answers the same close
/// with a reset that surfaces as `ConnectionReset` on the final drain. Both are
/// the connection ending, which is the only thing the linger governs.
#[test]
fn run_daemon_refusal_lingers_so_the_peer_can_read_it() {
    /// Lower bound on how long the connection must stay open after the refusal
    /// is written. Deliberately the smaller of upstream's two waits, so the
    /// assertion holds no matter which one a future refactor routes through.
    const MIN_LINGER: Duration = Duration::from_millis(100);

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let module_path = std::env::temp_dir()
        .display()
        .to_string()
        .replace('\\', "/");
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[no-compress]\npath = {module_path}\nrefuse options = compress\n",
    )
    .expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "greeting mismatch: {line:?}");

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");
    stream
        .write_all(b"no-compress\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    loop {
        line.clear();
        reader.read_line(&mut line).expect("daemon response line");
        if line.trim_end() == "@RSYNCD: OK" {
            break;
        }
        assert!(
            !line.starts_with("@ERROR:"),
            "daemon refused module before post-OK handoff: {line:?}",
        );
    }

    // `-z` hidden inside the bundle is the refusal trigger, exactly as in the
    // sibling wire-shape test.
    for arg in [
        "--server",
        "--sender",
        "-vlogDtprez.iLsfxCIvu",
        ".",
        "no-compress/",
    ] {
        stream.write_all(arg.as_bytes()).expect("write client arg");
        stream.write_all(&[0]).expect("write arg terminator");
    }
    stream.write_all(&[0]).expect("terminate args list");
    // The bytes the daemon will never consume. This is what turns its close
    // into an RST, and it is not artificial: a real client has already pushed
    // its filter list and protocol chatter by the time the refusal is decided.
    stream
        .write_all(&[0u8; 512])
        .expect("queue unread bytes at the daemon");
    stream.flush().expect("flush client args");

    let refusal_written = Instant::now();

    let compat_flags = protocol::read_varint(&mut reader)
        .expect("compat-flags varint must survive the daemon's close");
    assert!(compat_flags > 0, "compat flags: {compat_flags}");
    let mut seed_buf = [0u8; 4];
    reader
        .read_exact(&mut seed_buf)
        .expect("checksum seed must survive the daemon's close");

    let mut err_header = [0u8; 4];
    reader
        .read_exact(&mut err_header)
        .expect("MSG_ERROR_XFER header must survive the daemon's close");
    let err_raw = u32::from_le_bytes(err_header);
    assert_eq!(
        (err_raw >> 24) as u8,
        protocol::MPLEX_BASE + protocol::MessageCode::ErrorXfer.as_u8(),
        "refusal must arrive as MSG_ERROR_XFER",
    );
    let mut err_body = vec![0u8; (err_raw & 0x00FF_FFFF) as usize];
    reader
        .read_exact(&mut err_body)
        .expect("MSG_ERROR_XFER payload must survive the daemon's close");
    let err_text = String::from_utf8(err_body).expect("UTF-8 error payload");
    assert!(
        err_text.starts_with("@ERROR: The server is configured to refuse"),
        "error payload must name the refusal: {err_text:?}",
    );

    let mut exit_header = [0u8; 4];
    reader
        .read_exact(&mut exit_header)
        .expect("MSG_ERROR_EXIT header must survive the daemon's close");
    let exit_raw = u32::from_le_bytes(exit_header);
    assert_eq!(
        (exit_raw >> 24) as u8,
        protocol::MPLEX_BASE + protocol::MessageCode::ErrorExit.as_u8(),
        "refusal must be terminated by MSG_ERROR_EXIT",
    );
    let mut exit_buf = [0u8; 4];
    reader
        .read_exact(&mut exit_buf)
        .expect("MSG_ERROR_EXIT payload must survive the daemon's close");
    assert_eq!(
        i32::from_le_bytes(exit_buf),
        RERR_UNSUPPORTED_EXIT_CODE,
        "refusal exit code must be RERR_UNSUPPORTED (4), matching the \
         exit_cleanup(RERR_UNSUPPORTED) this test's own header cites",
    );

    // Drain until the connection ends: the daemon closes only after its linger
    // elapses. A reset counts as the end just as a clean EOF does - the daemon
    // still holds 512 unread bytes from us, and on Windows closing in that state
    // makes the kernel answer RST rather than FIN. That is the very phenomenon
    // the linger exists to outrun, so the measurement is *when* the connection
    // ended, never *how*.
    let mut trailing = Vec::new();
    match reader.read_to_end(&mut trailing) {
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ) => {}
        Err(error) => panic!("read to end of stream after the refusal: {error}"),
    }
    let open_for = refusal_written.elapsed();
    assert!(
        open_for >= MIN_LINGER,
        "daemon closed {open_for:?} after writing the refusal, before the {MIN_LINGER:?} linger \
         upstream keeps (cleanup.c:263-264); a close that prompt lets the kernel answer with RST \
         and discard the error frames",
    );

    drop(reader);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon thread returned: {result:?}");
}
