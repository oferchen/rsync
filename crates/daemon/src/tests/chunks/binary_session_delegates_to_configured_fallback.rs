#[cfg(unix)]
#[test]
fn binary_session_delegates_to_configured_fallback() {
    use std::io::BufReader;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");

    let mut frames = Vec::new();
    MessageFrame::new(
        MessageCode::Error,
        HANDSHAKE_ERROR_PAYLOAD.as_bytes().to_vec(),
    )
    .expect("frame")
    .encode_into_writer(&mut frames)
    .expect("encode error frame");
    let exit_code = u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE).unwrap_or_default();
    MessageFrame::new(MessageCode::ErrorExit, exit_code.to_be_bytes().to_vec())
        .expect("exit frame")
        .encode_into_writer(&mut frames)
        .expect("encode exit frame");

    let mut expected = Vec::new();
    expected.extend_from_slice(&u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes());
    expected.extend_from_slice(&frames);
    let expected_hex: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();

    let script_path = temp.path().join("binary-fallback.py");
    let marker_path = temp.path().join("fallback.marker");
    let script = "#!/usr/bin/env python3\n".to_string()
        + "import os, sys, binascii\n"
        + "marker = os.environ.get('FALLBACK_MARKER')\n"
        + "if marker:\n"
        + "    with open(marker, 'w', encoding='utf-8') as handle:\n"
        + "        handle.write('delegated')\n"
        + "sys.stdin.buffer.read(4)\n"
        + "payload = binascii.unhexlify(os.environ['BINARY_RESPONSE_HEX'])\n"
        + "sys.stdout.buffer.write(payload)\n"
        + "sys.stdout.buffer.flush()\n";
    write_executable_script(&script_path, &script);

    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());
    let _marker = EnvGuard::set("FALLBACK_MARKER", marker_path.as_os_str());
    let _hex = EnvGuard::set("BINARY_RESPONSE_HEX", OsStr::new(&expected_hex));

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
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    stream
        .write_all(&u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes())
        .expect("send handshake");
    stream.flush().expect("flush handshake");

    let mut response = Vec::new();
    reader.read_to_end(&mut response).expect("read response");

    assert_eq!(response, expected);
    assert!(marker_path.exists());

    handle.join().expect("daemon thread").expect("daemon run");
}

