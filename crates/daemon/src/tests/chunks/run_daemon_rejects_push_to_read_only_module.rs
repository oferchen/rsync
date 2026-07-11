#[test]
fn run_daemon_rejects_push_to_read_only_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[readonly]\npath = {}\nread only = true\nuse chroot = false\n",
            module_dir.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Read daemon greeting
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "expected greeting, got: {line}");

    // Send client version
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    // Request the read-only module
    stream
        .write_all(b"readonly\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // Daemon sends @RSYNCD: OK after module selection for unauthenticated modules
    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(line, "@RSYNCD: OK\n");

    // Send client arguments that indicate a push (no --sender flag means
    // the server must act as receiver, which conflicts with read-only).
    // upstream: options.c:server_options() - server args are null-terminated
    // for protocol >= 30.
    stream
        .write_all(b"--server\0-logDtpr\0.\0readonly/\0\0")
        .expect("send client args");
    stream.flush().expect("flush client args");

    // #227: the read-only push rejection fires after `@RSYNCD: OK`, so the
    // client has already switched to multiplexed input. The daemon must first
    // finish the post-OK protocol setup (compat-flags varint + 4-byte checksum
    // seed) and then deliver the error inside a `MSG_ERROR_XFER` frame, exactly
    // like upstream `do_server_recv()` (main.c:1166-1169) does after
    // `io_start_multiplex_out()`. A raw `@ERROR: ...\n` line here would be
    // decoded as a 4-byte frame header and desync the stream (the regression
    // upstream reports as `invalid multi-message 102 (code 12)`).
    assert_read_only_multiplexed_rejection(&mut reader);

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

/// Decodes the post-`@RSYNCD: OK` protocol prefix and asserts the read-only
/// rejection arrives as a framed `MSG_ERROR_XFER` (text `ERROR: module is read
/// only`) followed by `MSG_ERROR_EXIT` carrying `RERR_SYNTAX` (exit 1).
///
/// upstream: main.c:1166-1169 - `rprintf(FERROR, "ERROR: module is read
/// only\n")` + `exit_cleanup(RERR_SYNTAX)`; io.c encodes the FERROR message as
/// a `MSG_ERROR_XFER` frame and the exit as `MSG_ERROR_EXIT`.
fn assert_read_only_multiplexed_rejection(reader: &mut BufReader<TcpStream>) {
    // Post-OK `setup_protocol()` prefix: compat-flags varint + 4-byte seed.
    let compat_flags =
        protocol::read_varint(reader).expect("read compat-flags varint after @RSYNCD: OK");
    assert!(
        compat_flags > 0,
        "daemon must advertise at least one compat flag, got {compat_flags}",
    );
    let mut seed_buf = [0u8; 4];
    reader.read_exact(&mut seed_buf).expect("read checksum seed");

    // MSG_ERROR_XFER frame with the plain `ERROR:` text (no `@ERROR:` prefix).
    let mut err_header = [0u8; 4];
    reader
        .read_exact(&mut err_header)
        .expect("read MSG_ERROR_XFER header");
    let err_raw = u32::from_le_bytes(err_header);
    let err_tag = (err_raw >> 24) as u8;
    let err_len = (err_raw & 0x00FF_FFFF) as usize;
    assert_eq!(
        err_tag,
        protocol::MPLEX_BASE + protocol::MessageCode::ErrorXfer.as_u8(),
        "read-only rejection must use MSG_ERROR_XFER (tag = MPLEX_BASE + 1 = 8); \
         a raw line would surface as `invalid multi-message`/`unexpected tag`",
    );
    let mut err_body = vec![0u8; err_len];
    reader
        .read_exact(&mut err_body)
        .expect("read MSG_ERROR_XFER payload");
    let err_text = String::from_utf8(err_body).expect("UTF-8 error payload");
    assert_eq!(
        err_text.trim_end(),
        "ERROR: module is read only",
        "read-only rejection text must mirror upstream FERROR wording",
    );

    // MSG_ERROR_EXIT frame carrying the 4-byte RERR_SYNTAX exit code.
    let mut exit_header = [0u8; 4];
    reader
        .read_exact(&mut exit_header)
        .expect("read MSG_ERROR_EXIT header");
    let exit_raw = u32::from_le_bytes(exit_header);
    let exit_tag = (exit_raw >> 24) as u8;
    let exit_len = (exit_raw & 0x00FF_FFFF) as usize;
    assert_eq!(
        exit_tag,
        protocol::MPLEX_BASE + protocol::MessageCode::ErrorExit.as_u8(),
        "read-only exit must use MSG_ERROR_EXIT (tag = MPLEX_BASE + 86 = 93)",
    );
    assert_eq!(exit_len, 4, "MSG_ERROR_EXIT payload must carry an i32");
    let mut exit_buf = [0u8; 4];
    reader
        .read_exact(&mut exit_buf)
        .expect("read MSG_ERROR_EXIT payload");
    assert_eq!(
        i32::from_le_bytes(exit_buf),
        RERR_SYNTAX_EXIT_CODE,
        "read-only rejection exit code must be RERR_SYNTAX (1)",
    );
}
