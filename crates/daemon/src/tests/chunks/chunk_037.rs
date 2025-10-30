#[cfg(unix)]
#[test]
fn binary_session_delegation_propagates_runtime_arguments() {
    use std::io::BufReader;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");

    let module_dir = temp.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = temp.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("[docs]\n    path = {}\n", module_dir.display()),
    )
    .expect("write config");

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

    let script_path = temp.path().join("binary-args.py");
    let args_log_path = temp.path().join("delegation-args.log");
    let script = "#!/usr/bin/env python3\n".to_string()
        + "import os, sys, binascii\n"
        + "args_log = os.environ.get('ARGS_LOG')\n"
        + "if args_log:\n"
        + "    with open(args_log, 'w', encoding='utf-8') as handle:\n"
        + "        handle.write(' '.join(sys.argv[1:]))\n"
        + "sys.stdin.buffer.read(4)\n"
        + "payload = binascii.unhexlify(os.environ['BINARY_RESPONSE_HEX'])\n"
        + "sys.stdout.buffer.write(payload)\n"
        + "sys.stdout.buffer.flush()\n";
    write_executable_script(&script_path, &script);

    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());
    let _hex = EnvGuard::set("BINARY_RESPONSE_HEX", OsStr::new(&expected_hex));
    let _args = EnvGuard::set("ARGS_LOG", args_log_path.as_os_str());

    let port = allocate_test_port();

    let log_path = temp.path().join("daemon.log");
    let pid_path = temp.path().join("daemon.pid");
    let lock_path = temp.path().join("daemon.lock");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.clone().into_os_string(),
            OsString::from("--log-file"),
            log_path.clone().into_os_string(),
            OsString::from("--pid-file"),
            pid_path.clone().into_os_string(),
            OsString::from("--lock-file"),
            lock_path.clone().into_os_string(),
            OsString::from("--bwlimit"),
            OsString::from("96"),
            OsString::from("--ipv4"),
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

    handle.join().expect("daemon thread").expect("daemon run");

    let recorded = fs::read_to_string(&args_log_path).expect("read args log");
    assert!(recorded.contains("--port"));
    assert!(recorded.contains(&port.to_string()));
    assert!(recorded.contains("--config"));
    assert!(recorded.contains(config_path.to_str().expect("utf8 config")));
    assert!(recorded.contains("--log-file"));
    assert!(recorded.contains(log_path.to_str().expect("utf8 log")));
    assert!(recorded.contains("--pid-file"));
    assert!(recorded.contains(pid_path.to_str().expect("utf8 pid")));
    assert!(recorded.contains("--lock-file"));
    assert!(recorded.contains(lock_path.to_str().expect("utf8 lock")));
    assert!(recorded.contains("--bwlimit"));
    assert!(recorded.contains("96"));
    assert!(recorded.contains("--ipv4"));
}

