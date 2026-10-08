/// A handshake line longer than upstream's line buffer ends the session with
/// no reply, instead of being read in full and answered.
///
/// upstream: `io.c:2635-2656` `read_line_old()` examines at most `bufsiz - 1`
/// bytes and fails when the buffer fills before a newline, and
/// `clientserver.c:1416,1427` reads the greeting into `char line[1024]`; on that
/// failure `exchange_protocols()` returns -1 without a word to a non-client
/// peer (`clientserver.c:203-206`). Measured on 3.5.1: a 64 MiB greeting gets
/// the connection reset and nothing else.
#[cfg(unix)]
#[test]
fn run_daemon_closes_on_an_oversized_handshake_line() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let (port, held_listener) = allocate_test_port();
    let temp = tempdir().expect("module dir");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from(format!("docs={}", temp.path().display())),
            OsString::from("--once"),
        ])
        .build();
    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    let mut greeting = b"@RSYNCD: 32.0 ".to_vec();
    greeting.resize(1 << 20, b'x');
    // The daemon may close mid-write; that is the behaviour under test.
    let _ = stream.write_all(&greeting);
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    drop(reader);
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "the daemon must survive the greeting");
    }
    assert!(
        rest.is_empty(),
        "an oversized greeting gets no reply, got {:?}",
        String::from_utf8_lossy(&rest)
    );
}
