/// Regression test for the post-`@RSYNCD: OK` refused-options wire shape.
///
/// upstream: clientserver.c:1160-1200 - once the daemon has acknowledged
/// the module, the multiplex framing on the client side starts
/// immediately after `setup_protocol()` completes. Refused options
/// detected by `parse_arguments()` flow through `rwrite(FERROR, ...)`,
/// which encodes them as a `MSG_ERROR_XFER` (tag `MPLEX_BASE + 1 = 8`)
/// frame followed by `MSG_ERROR_EXIT` (tag `MPLEX_BASE + 86 = 93`).
///
/// Before this fix the daemon skipped the post-OK `setup_protocol()`
/// step entirely, so the client decoded the first error-frame bytes as
/// the compat-flags varint and the checksum seed and only flipped to
/// multiplex input after consuming them. The framing then resynchronised
/// partway into our `MSG_ERROR_XFER` payload, decoding the letter `A`
/// of `@ERROR: ...` as tag `MPLEX_BASE + 65 = 72`, which upstream rsync
/// rejected with `unexpected tag 72 [Receiver]`. Pinning the byte stream
/// here prevents that regression from reappearing.
#[test]
fn run_daemon_post_ok_refused_option_uses_multiplexed_error() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let module_path = std::env::temp_dir()
        .display()
        .to_string()
        .replace('\\', "/");
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[no-compress]\npath = {module_path}\nrefuse options = compress\n",
    )
    .expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            file.path().as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Drain the daemon greeting and complete the version handshake.
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "greeting mismatch: {line:?}");

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    // Skip optional capability advertisements that the daemon emits before
    // it expects the module name. Stop as soon as we read a non-`@RSYNCD:`
    // line, which in the happy path would be `@RSYNCD: OK`.
    stream
        .write_all(b"no-compress\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    loop {
        line.clear();
        reader.read_line(&mut line).expect("daemon response line");
        if line.trim_end() == "@RSYNCD: OK" {
            break;
        }
        if line.starts_with("@ERROR:") {
            panic!("daemon refused module before post-OK handoff: {line:?}");
        }
    }

    // Send a packed flag string that hides `-z` (compress) inside the
    // bundled short options upstream's client emits for `rsync -avz`. The
    // refused-options matcher must spot the `z` letter inside the bundle
    // and reject the transfer.
    let post_ok_args: &[&str] = &[
        "--server",
        "--sender",
        "-vlogDtprez.iLsfxCIvu",
        ".",
        "no-compress/",
    ];
    for arg in post_ok_args {
        stream.write_all(arg.as_bytes()).expect("write client arg");
        stream.write_all(&[0]).expect("write arg terminator");
    }
    stream.write_all(&[0]).expect("terminate args list");
    stream.flush().expect("flush client args");

    // The daemon must now emit the post-OK `setup_protocol()` writes
    // (compat-flags varint + 4-byte checksum seed) before flipping to
    // multiplexed output. Decode the varint with the protocol-crate
    // helper so the wire shape mirrors upstream `read_varint()` byte for
    // byte.
    let compat_flags =
        protocol::read_varint(&mut reader).expect("read compat-flags varint after @RSYNCD: OK");
    assert!(
        compat_flags > 0,
        "daemon must advertise at least one compat flag, got {compat_flags}",
    );
    // upstream: compat.c:535-565 - leaving CF_VARINT_FLIST_FLAGS unset is
    // load-bearing on the abort path so the client does not try to read
    // negotiated vstrings that we never write.
    let compat_bits = compat_flags as u32;
    assert_eq!(
        compat_bits & protocol::CompatibilityFlags::VARINT_FLIST_FLAGS.bits(),
        0,
        "post-OK refused-options abort must NOT advertise CF_VARINT_FLIST_FLAGS",
    );
    assert_ne!(
        compat_bits & protocol::CompatibilityFlags::CHECKSUM_SEED_FIX.bits(),
        0,
        "post-OK refused-options abort must advertise CF_CHECKSUM_SEED_FIX",
    );

    // 4-byte checksum seed (any value).
    let mut seed_buf = [0u8; 4];
    reader
        .read_exact(&mut seed_buf)
        .expect("read checksum seed");

    // First multiplexed frame: MSG_ERROR_XFER (tag = MPLEX_BASE + 1 = 8)
    // carrying the refused-option error text.
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
        "post-OK refused-options error must use MSG_ERROR_XFER (tag = MPLEX_BASE + 1 = 8); \
         got raw tag {err_tag} which decodes to msg_code {} - this surfaces upstream as \
         `unexpected tag {err_tag} [Receiver]`",
        err_tag.wrapping_sub(protocol::MPLEX_BASE),
    );

    let mut err_body = vec![0u8; err_len];
    reader
        .read_exact(&mut err_body)
        .expect("read MSG_ERROR_XFER payload");
    let err_text = String::from_utf8(err_body).expect("UTF-8 error payload");
    assert!(
        err_text.contains("--compress"),
        "error payload must name the refused option: {err_text:?}",
    );
    assert!(
        err_text.starts_with("@ERROR: The server is configured to refuse"),
        "error payload must mirror upstream's refuse-options message: {err_text:?}",
    );

    // Second multiplexed frame: MSG_ERROR_EXIT (tag = MPLEX_BASE + 86 = 93)
    // carrying the 4-byte exit code.
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
        "post-OK refused-options exit must use MSG_ERROR_EXIT (tag = MPLEX_BASE + 86 = 93)",
    );
    assert_eq!(exit_len, 4, "MSG_ERROR_EXIT payload must carry an i32");
    let mut exit_buf = [0u8; 4];
    reader
        .read_exact(&mut exit_buf)
        .expect("read MSG_ERROR_EXIT payload");
    assert_eq!(
        i32::from_le_bytes(exit_buf),
        FEATURE_UNAVAILABLE_EXIT_CODE,
        "refused-options exit code must mirror upstream's unsupported-feature path",
    );

    drop(reader);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon thread returned: {result:?}");
}
