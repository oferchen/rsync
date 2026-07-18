/// Tests for daemon mode module listing negotiation.
///
/// These tests verify the correct behavior of the daemon when handling
/// module listing requests (#list) during the daemon negotiation protocol.

#[test]
fn daemon_negotiation_module_list_sends_listing_directly() {
    // upstream: clientserver.c sends module listing directly after #list,
    // without @RSYNCD: CAP or @RSYNCD: OK preamble.
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    // Normalise to forward slashes for symmetry with the comment-bearing test
    // below; Windows accepts forward slashes, and this avoids any future
    // Windows-only parser surprises around `\` as an escape character.
    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from(format!("test={module_path}")),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "Expected greeting, got: {line}");

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // upstream: no @RSYNCD: OK or CAP before module listing - straight to modules.
    // Read module listing - upstream: clientserver.c:1254 uses %-15s\t%s\n format
    line.clear();
    reader.read_line(&mut line).expect("module listing");
    assert_eq!(line, "test           \t\n", "Expected %-15s aligned module, got: {line}");

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

    let (port, held_listener) = allocate_test_port();

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

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // upstream: no @RSYNCD: OK or CAP lines before module listing

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

    let (port, held_listener) = allocate_test_port();

    // The --module value uses `\` as an escape character, so on Windows we
    // normalise the temp-dir path to forward slashes (which Windows accepts
    // natively) to keep the comma separator that delimits the comment from
    // being swallowed by the escape state machine in
    // `daemon::sections::module_parsing::split_module_path_comment_and_options`.
    let module_path = std::env::temp_dir().display().to_string().replace('\\', "/");
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from(format!("mymod={module_path},This is a comment")),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // upstream: no @RSYNCD: OK or CAP lines before module listing

    line.clear();
    reader.read_line(&mut line).expect("module");

    // upstream: clientserver.c:1254 - %-15s\t%s\n format
    assert_eq!(
        line, "mymod          \tThis is a comment\n",
        "Module listing should use %-15s alignment, got: {line}"
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

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush");

    // upstream: no @RSYNCD: OK before module listing. When there are no
    // modules, the daemon goes straight to EXIT.

    // Expect EXIT immediately (no modules)
    line.clear();
    reader.read_line(&mut line).expect("exit");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
