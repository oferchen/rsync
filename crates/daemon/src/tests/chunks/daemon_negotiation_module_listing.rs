/// Tests for daemon mode module listing negotiation.
///
/// These tests verify the correct behavior of the daemon when handling
/// module listing requests (#list) during the daemon negotiation protocol.

#[test]
fn daemon_negotiation_module_list_sends_capabilities_before_ok() {
    // Verify that when a client requests a module listing, the daemon
    // sends capabilities before the OK acknowledgment.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from("test=/tmp"),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "Expected greeting, got: {line}");

    // Send list request
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // Should receive CAP before OK
    line.clear();
    reader.read_line(&mut line).expect("capabilities line");
    assert!(
        line.starts_with("@RSYNCD: CAP"),
        "Expected CAP response, got: {line}"
    );

    // Now should receive OK
    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    // Read module listing
    line.clear();
    reader.read_line(&mut line).expect("module listing");
    assert!(line.starts_with("test"), "Expected module, got: {line}");

    // Read exit
    line.clear();
    reader.read_line(&mut line).expect("exit");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn daemon_negotiation_module_list_respects_listable_flag() {
    // Verify that modules marked as not listable are excluded from the listing.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("temp dir");
    let module_dir = dir.path().join("visible");
    fs::create_dir_all(&module_dir).expect("module dir");
    let hidden_dir = dir.path().join("hidden");
    fs::create_dir_all(&hidden_dir).expect("hidden dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[visible]\npath = {}\nlist = true\n\n[hidden]\npath = {}\nlist = false\n",
            module_dir.display(),
            hidden_dir.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // Skip CAP and OK lines
    line.clear();
    reader.read_line(&mut line).expect("cap");
    line.clear();
    reader.read_line(&mut line).expect("ok");

    // Read all module lines until EXIT
    let mut modules = Vec::new();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("module or exit");
        if line == "@RSYNCD: EXIT\n" {
            break;
        }
        modules.push(line.trim().to_string());
    }

    // Should only see 'visible', not 'hidden'
    assert!(
        modules.iter().any(|m| m.starts_with("visible")),
        "visible module should be listed"
    );
    assert!(
        !modules.iter().any(|m| m.starts_with("hidden")),
        "hidden module should not be listed"
    );

    drop(reader);
    let _result = handle.join().expect("daemon thread");
}

#[test]
fn daemon_negotiation_module_list_includes_comments() {
    // Verify that module comments are included in the listing.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from("mymod=/tmp,This is a comment"),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // Skip CAP and OK
    line.clear();
    reader.read_line(&mut line).expect("cap");
    line.clear();
    reader.read_line(&mut line).expect("ok");

    // Read module line
    line.clear();
    reader.read_line(&mut line).expect("module");

    // Module line should contain name and comment separated by tab
    assert!(
        line.contains("mymod") && line.contains("This is a comment"),
        "Module listing should include comment, got: {line}"
    );

    drop(reader);
    let _result = handle.join().expect("daemon thread");
}

#[test]
fn daemon_negotiation_module_list_empty_when_no_modules() {
    // Verify that an empty module list sends only OK and EXIT (no modules to list CAP for).
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
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    // Send list request
    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // Read response - may be CAP (if modules present) or OK (if no modules)
    line.clear();
    reader.read_line(&mut line).expect("response");

    // If CAP line, read the next line (OK)
    if line.starts_with("@RSYNCD: CAP") {
        line.clear();
        reader.read_line(&mut line).expect("ok");
    }
    assert_eq!(line, "@RSYNCD: OK\n", "Expected OK, got: {line}");

    // Expect EXIT immediately (no modules)
    line.clear();
    reader.read_line(&mut line).expect("exit");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
