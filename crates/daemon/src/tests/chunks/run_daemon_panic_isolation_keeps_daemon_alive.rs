/// Verifies that a malformed first connection does not kill the daemon.
///
/// Each connection handler runs inside `catch_unwind`, isolating panics and
/// I/O errors to the faulting session so the accept loop keeps running.
/// This test exercises that guarantee end-to-end over a real TCP socket:
///
/// 1. Sends a garbage version line (not `@RSYNCD:`) on the first connection.
/// 2. Verifies the daemon still accepts and greets a second valid connection.
#[test]
fn run_daemon_panic_isolation_keeps_daemon_alive() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    // Allow exactly 2 sessions so the daemon exits cleanly after the valid
    // connection completes, letting the test join the daemon thread.
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    drop(held_listener);
    let daemon_handle = thread::spawn(move || run_daemon(config));

    // --- Connection 1: send garbage instead of the @RSYNCD: version line ---
    // The daemon reads the greeting it sent, parses the garbage as a module
    // name, replies with @ERROR: and @RSYNCD: EXIT, then keeps the accept
    // loop running because catch_unwind isolates per-connection errors.
    {
        let mut bad = connect_with_retries(port);
        let mut bad_reader = BufReader::new(bad.try_clone().expect("clone bad stream"));

        // Drain the server greeting so the send buffer does not stall.
        let mut discard = String::new();
        bad_reader
            .read_line(&mut discard)
            .expect("drain greeting on bad connection");

        // Send an ASCII garbage line in place of a valid @RSYNCD: response.
        // The daemon will interpret this as a module request, discover no
        // such module exists, and reply with @ERROR: + @RSYNCD: EXIT.
        bad.write_all(b"NOT_A_VALID_RSYNCD_LINE\n")
            .expect("send garbage line");
        bad.flush().expect("flush garbage");

        // Drain the daemon response so the worker thread finishes before
        // the second connection arrives.
        discard.clear();
        loop {
            let n = bad_reader
                .read_line(&mut discard)
                .expect("read daemon response to garbage");
            if n == 0 || discard.contains("@RSYNCD: EXIT") {
                break;
            }
            discard.clear();
        }
    } // bad stream dropped here — worker thread has already finished

    // --- Connection 2: well-formed handshake ---
    // If catch_unwind isolation had failed the daemon would have exited and
    // connect_with_retries would time out.
    {
        let mut good = connect_with_retries(port);
        let mut good_reader = BufReader::new(good.try_clone().expect("clone good stream"));

        let mut line = String::new();
        good_reader
            .read_line(&mut line)
            .expect("greeting on good connection");
        assert_eq!(
            line,
            legacy_daemon_greeting(),
            "daemon must still be alive and send the greeting to the second connection"
        );

        // Complete the session so the daemon counts it toward max-sessions
        // and can exit cleanly.
        good.write_all(b"@RSYNCD: 32.0\n")
            .expect("send version");
        good.flush().expect("flush version");

        good.write_all(b"no_such_module\n")
            .expect("send module request");
        good.flush().expect("flush module request");

        line.clear();
        good_reader
            .read_line(&mut line)
            .expect("error from good connection");
        assert!(
            line.starts_with("@ERROR:"),
            "expected @ERROR: for unknown module, got: {line:?}"
        );

        line.clear();
        good_reader
            .read_line(&mut line)
            .expect("exit from good connection");
        assert_eq!(line, "@RSYNCD: EXIT\n");
    } // good stream dropped here

    let result = daemon_handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
}
