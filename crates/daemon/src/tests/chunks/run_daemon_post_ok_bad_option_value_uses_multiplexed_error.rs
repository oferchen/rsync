/// Regression test for the post-`@RSYNCD: OK` client-argument parse failure.
///
/// upstream: clientserver.c:1258-1266 - `parse_arguments()` returning 0 sends
/// the parser's own `err_buf` text through `option_error()`
/// (options.c:915 `rprintf(FERROR, ...)`) and then
/// `exit_cleanup(RERR_UNSUPPORTED)`. Both run *after* the
/// `setup_protocol()` + `io_start_multiplex_out()` pair at
/// clientserver.c:1229-1251, which upstream performs precisely so it "can get
/// the error back to the client".
///
/// oc delivered this one rejection with the pre-handshake raw `@ERROR:` writer
/// while its two sibling post-OK refusals (refused options, read-only access)
/// already used the multiplexed path. The client had switched to multiplex
/// input at `@RSYNCD: OK`, so it decoded the plaintext as a frame header and
/// aborted with `unknown multiplexed message code 38` - the `-` of
/// `--max-size` landing at the tag position, minus `MPLEX_BASE = 7`. The
/// operator never learned which option was rejected. Pinning the byte stream
/// here keeps the three post-OK refusals on one framing.
#[test]
fn run_daemon_post_ok_bad_option_value_uses_multiplexed_error() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (port, held_listener) = allocate_test_port();

    let module_path = std::env::temp_dir()
        .display()
        .to_string()
        .replace('\\', "/");
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[sizemod]\npath = {module_path}\nread only = yes\n").expect("write config");

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

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(line.starts_with("@RSYNCD:"), "greeting mismatch: {line:?}");

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    stream
        .write_all(b"sizemod\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

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

    // A value far wider than any size the parser can represent. The client
    // never sends this itself - it parses --max-size locally and would reject
    // it first - so upstream's testsuite forwards it with `-M` and so does
    // this pin, exercising the daemon's own option parser.
    let oversized = "9".repeat(5000);
    let max_size_arg = format!("--max-size={oversized}");
    let post_ok_args: &[&str] = &["--server", "--sender", "-logDtpre.LsfxCIvu", &max_size_arg];
    for arg in post_ok_args {
        stream.write_all(arg.as_bytes()).expect("write client arg");
        stream.write_all(&[0]).expect("write arg terminator");
    }
    for arg in [".", "sizemod/"] {
        stream.write_all(arg.as_bytes()).expect("write operand");
        stream.write_all(&[0]).expect("write operand terminator");
    }
    stream.write_all(&[0]).expect("terminate args list");
    stream.flush().expect("flush client args");

    // The post-OK `setup_protocol()` prefix: compat-flags varint then the
    // 4-byte checksum seed. Without these the client would consume the first
    // error-frame bytes in their place.
    let compat_flags =
        protocol::read_varint(&mut reader).expect("read compat-flags varint after @RSYNCD: OK");
    assert!(
        compat_flags > 0,
        "daemon must advertise at least one compat flag, got {compat_flags}",
    );
    let mut seed_buf = [0u8; 4];
    reader
        .read_exact(&mut seed_buf)
        .expect("read checksum seed");

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
        "post-OK option-value rejection must use MSG_ERROR_XFER (tag = MPLEX_BASE + 1 = 8); \
         got raw tag {err_tag}, which is what surfaces on the client as \
         `unknown multiplexed message code {}`",
        err_tag.wrapping_sub(protocol::MPLEX_BASE),
    );

    let mut err_body = vec![0u8; err_len];
    reader
        .read_exact(&mut err_body)
        .expect("read MSG_ERROR_XFER payload");
    let err_text = String::from_utf8(err_body).expect("UTF-8 error payload");
    // Upstream echoes the rejected value back verbatim (options.c:1254
    // `"--%s=%s is %s"`), which is what lets an operator see which option was
    // refused. Assert on the value, not just the option name.
    assert!(
        err_text.contains(&oversized),
        "error payload must echo the rejected value back to the client: {err_text:?}",
    );

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
        "post-OK option-value rejection must use MSG_ERROR_EXIT (tag = MPLEX_BASE + 86 = 93)",
    );
    assert_eq!(exit_len, 4, "MSG_ERROR_EXIT payload must carry an i32");
    let mut exit_buf = [0u8; 4];
    reader
        .read_exact(&mut exit_buf)
        .expect("read MSG_ERROR_EXIT payload");
    assert_eq!(
        i32::from_le_bytes(exit_buf),
        crate::daemon::RERR_UNSUPPORTED_EXIT_CODE,
        "upstream exits RERR_UNSUPPORTED after option_error() (clientserver.c:1266)",
    );

    drop(reader);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon thread returned: {result:?}");
}
