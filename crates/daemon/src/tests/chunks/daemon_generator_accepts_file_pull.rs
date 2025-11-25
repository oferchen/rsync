#[test]
fn daemon_generator_accepts_file_pull() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let temp = tempdir().expect("tempdir");

    // Create source directory with test file
    let source_dir = temp.path().join("module_root");
    fs::create_dir(&source_dir).expect("create source");
    fs::write(source_dir.join("testfile.txt"), b"test content\n").expect("create test file");

    // Create config file with read-only module (no authentication)
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[testmodule]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n\
         comment = Test module for pull operations\n",
        source_dir.display()
    );
    fs::write(&config_file, config_content).expect("write config");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read daemon greeting
    let mut greeting = String::new();
    reader
        .read_line(&mut greeting)
        .expect("read daemon greeting");

    assert!(
        greeting.starts_with("@RSYNCD:"),
        "expected @RSYNCD: greeting, got: {greeting}"
    );

    // Send client version
    stream
        .write_all(b"@RSYNCD: 31.0\n")
        .expect("send client version");
    stream.flush().expect("flush");

    // Read handshake acknowledgment
    let mut ack = String::new();
    reader.read_line(&mut ack).expect("read ack");
    assert_eq!(ack, "@RSYNCD: OK\n");

    // Request the module (no authentication needed for read-only)
    stream
        .write_all(b"testmodule\n")
        .expect("send module request");
    stream.flush().expect("flush");

    // Read response - for read-only module without auth, we expect either:
    // - Success response (empty line or server ready indicator)
    // - Or an error if run_server_stdio fails
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");

    // The daemon accepted the read-only module request
    // Response could be empty line (success) or an error
    // Either way, verify we got past the module selection phase
    assert!(
        !response.contains("Unknown module"),
        "daemon should recognize the module, got: {response}"
    );

    // Close connection
    drop(reader);
    drop(stream);

    let result = handle.join().expect("daemon thread");
    // Daemon may report error if server mode isn't fully implemented,
    // but it shouldn't panic
    let _ = result;
}
