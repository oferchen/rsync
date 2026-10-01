/// Starts a `--once` daemon serving one read-only module `[xfertest]` whose
/// `pre-xfer exec` is `hook`, requests it, and returns the stream positioned
/// right after `@RSYNCD: OK` with a push argv already sent.
#[cfg(unix)]
fn pre_xfer_push_session(
    hook: &str,
) -> (
    tempfile::TempDir,
    TcpStream,
    BufReader<TcpStream>,
    std::thread::JoinHandle<Result<(), crate::DaemonError>>,
) {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[xfertest]\npath = {}\nread only = true\nuse chroot = false\npre-xfer exec = {hook}\n",
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
    // Every read is bounded, so a regression fails the test instead of hanging it.
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\nxfertest\n")
        .expect("send greeting and module");
    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(line, "@RSYNCD: OK\n");

    // No --sender: the client pushes, so the daemon would be the receiver.
    stream
        .write_all(b"--server\0-logDtpr\0.\0xfertest/\0\0")
        .expect("send client args");
    stream.flush().expect("flush client args");

    (dir, stream, reader, handle)
}

/// Reads the post-OK `setup_protocol()` prefix, then `MSG_ERROR_XFER` frames
/// up to the closing `MSG_ERROR_EXIT`, returning each frame's text and the
/// exit code.
#[cfg(unix)]
fn read_framed_post_ok_error(reader: &mut BufReader<TcpStream>) -> (Vec<String>, i32) {
    protocol::read_varint(reader).expect("compat-flags varint");
    let mut seed = [0u8; 4];
    reader.read_exact(&mut seed).expect("checksum seed");

    let mut frames = Vec::new();
    loop {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header).expect("frame header");
        let raw = u32::from_le_bytes(header);
        let mut body = vec![0u8; (raw & 0x00FF_FFFF) as usize];
        reader.read_exact(&mut body).expect("frame payload");
        let tag = (raw >> 24) as u8;
        if tag == protocol::MPLEX_BASE + protocol::MessageCode::ErrorExit.as_u8() {
            let code = i32::from_le_bytes(body.try_into().expect("4-byte exit code"));
            return (frames, code);
        }
        assert_eq!(
            tag,
            protocol::MPLEX_BASE + protocol::MessageCode::ErrorXfer.as_u8(),
            "the error must travel as MSG_ERROR_XFER, never as raw text after OK"
        );
        frames.push(String::from_utf8(body).expect("UTF-8 error text"));
    }
}

/// A failing `pre-xfer exec` reports upstream's text, framed, with exit 4, and
/// is decided before the read-only check.
///
/// upstream: clientserver.c:637-673 finish_pre_exec() builds
/// `"pre-xfer exec returned failure (%d)%s\n%s"` from the raw wait status and
/// the hook's stdout (CRLF folded to LF; stderr is never captured), and
/// :1229-1267 sends it as FERROR after setup_protocol() and
/// io_start_multiplex_out(), then exits RERR_UNSUPPORTED. The read-only refusal
/// lives later, in main.c do_server_recv(), so it never gets a say. Measured on
/// 3.5.1: a push prints `pre-xfer exec returned failure (768):`, `line1`,
/// `line2` and exits 4.
#[cfg(unix)]
#[test]
fn daemon_pre_xfer_exec_rejects_on_nonzero_exit() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (_dir, _stream, mut reader, handle) =
        pre_xfer_push_session(r"printf 'line1\r\nline2\n'; echo on-stderr >&2; exit 3");

    // One frame per line: a client prints a newline inside a frame escaped.
    let (frames, code) = read_framed_post_ok_error(&mut reader);
    assert_eq!(
        frames,
        [
            "pre-xfer exec returned failure (768):\n",
            "line1\n",
            "line2\n"
        ]
    );
    assert_eq!(code, 4, "upstream exits RERR_UNSUPPORTED");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}
