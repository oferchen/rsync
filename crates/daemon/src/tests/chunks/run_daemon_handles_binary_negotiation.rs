#[test]
fn run_daemon_handles_binary_negotiation() {
    use rsync_protocol::{BorrowedMessageFrames, MessageCode};

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    let advertisement = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    stream
        .write_all(&advertisement)
        .expect("send client advertisement");
    stream.flush().expect("flush advertisement");

    let mut response = [0u8; 4];
    stream
        .read_exact(&mut response)
        .expect("read server advertisement");
    assert_eq!(response, advertisement);

    let mut frames = Vec::new();
    stream.read_to_end(&mut frames).expect("read frames");

    let mut iter = BorrowedMessageFrames::new(&frames);
    let first = iter.next().expect("first frame").expect("decode frame");
    assert_eq!(first.code(), MessageCode::Error);
    assert_eq!(first.payload(), HANDSHAKE_ERROR_PAYLOAD.as_bytes());
    let second = iter.next().expect("second frame").expect("decode frame");
    assert_eq!(second.code(), MessageCode::ErrorExit);
    assert_eq!(
        second.payload(),
        u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE)
            .expect("feature unavailable exit code fits")
            .to_be_bytes()
    );
    assert!(iter.next().is_none());

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

