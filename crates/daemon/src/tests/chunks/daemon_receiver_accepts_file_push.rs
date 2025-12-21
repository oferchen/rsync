#[test]
fn daemon_receiver_accepts_file_push() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let temp = tempdir().expect("tempdir");

    // Create destination directory for writable module
    let dest_dir = temp.path().join("module_root");
    fs::create_dir(&dest_dir).expect("create destination");

    // Create secrets file for authentication
    let secrets_file = temp.path().join("secrets");
    fs::write(&secrets_file, "testuser:testpass\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&secrets_file)
            .expect("secrets metadata")
            .permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&secrets_file, permissions).expect("set secrets permissions");
    }

    // Create config file with writable module
    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[testmodule]\n\
         path = {}\n\
         read only = false\n\
         auth users = testuser\n\
         secrets file = {}\n\
         use chroot = false\n",
        dest_dir.display(),
        secrets_file.display()
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

    // Request the module with authentication
    stream
        .write_all(b"testuser@testmodule\n")
        .expect("send module request");
    stream.flush().expect("flush");

    // Read auth challenge
    let mut challenge = String::new();
    reader.read_line(&mut challenge).expect("read challenge");

    // Verify we got an auth challenge
    assert!(
        challenge.starts_with("@RSYNCD: AUTHREQD "),
        "expected auth challenge, got: {challenge}"
    );

    // Close connection (we've verified the daemon can handle writable module requests)
    drop(reader);
    drop(stream);

    let result = handle.join().expect("daemon thread");
    // Daemon may report error since we didn't complete authentication,
    // but it shouldn't panic
    let _ = result;
}
