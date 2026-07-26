/// Spins up a one-shot daemon serving a single auth-protected module named
/// `protected` whose secrets file grants `alice:correctpassword`, and returns the
/// connected client socket plus the daemon thread handle.
///
/// The caller has already consumed nothing: the server greeting is still queued.
fn start_auth_digest_daemon(dir: &Path) -> (TcpStream, thread::JoinHandle<Result<(), crate::DaemonError>>) {
    let module_dir = dir.join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.join("secrets.txt");
    fs::write(&secrets_path, "alice:correctpassword\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let config_path = dir.join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[protected]\npath = {}\nauth users = alice\nsecrets file = {}\nuse chroot = false\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let (port, held_listener) = allocate_test_port();
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--no-detach"),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    start_daemon(config, port, held_listener)
}

/// Drives the handshake up to the point where the daemon has answered the
/// `protected` module request, returning that answer line.
///
/// `greeting` is the client's verbatim `@RSYNCD:` line including its digest list
/// (or lack of one).
fn negotiate_protected_module(
    stream: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    greeting: &str,
) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).expect("server greeting");
    assert!(line.starts_with("@RSYNCD:"), "expected greeting, got: {line}");

    stream
        .write_all(greeting.as_bytes())
        .expect("send client greeting");
    stream.write_all(b"protected\n").expect("send module request");
    stream.flush().expect("flush handshake");

    line.clear();
    reader.read_line(&mut line).expect("daemon answer");
    line.trim_end().to_owned()
}

/// The digest the daemon negotiated must drive the challenge it sends *and* the
/// response it accepts. With `md5 sha512` offered, upstream's server-side rule
/// takes the client's first acceptable name - MD5 - not the strongest one.
///
/// upstream: compat.c:354 - `if (best == 1 || am_server) break;` stops the walk of
/// the client's list at its first supported entry; authenticate.c:76 and :90 then
/// both hash with `valid_auth_checksums.negotiated_nni`.
#[test]
fn daemon_auth_uses_the_first_client_offered_digest_it_supports() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_daemon(dir.path());
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let answer = negotiate_protected_module(&mut stream, &mut reader, "@RSYNCD: 32.0 md5 sha512\n");
    let challenge = answer
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .unwrap_or_else(|| panic!("expected auth challenge, got: {answer}"));

    // The challenge itself is MD5-sized, proving the negotiated digest - not the
    // strongest mutually supported one - generated it.
    assert_eq!(
        challenge.len(),
        DaemonAuthDigest::Md5.base64_len(),
        "challenge must come from the negotiated MD5, not SHA-512"
    );

    let response =
        core::auth::compute_daemon_auth_response(b"correctpassword", challenge, DaemonAuthDigest::Md5);
    stream
        .write_all(format!("alice {response}\n").as_bytes())
        .expect("send response");
    stream.flush().expect("flush response");

    let mut line = String::new();
    reader.read_line(&mut line).expect("auth verdict");
    assert_eq!(line.trim_end(), "@RSYNCD: OK");

    drop(reader);
    drop(stream);
    // Bounded join: an unconditional join() can wedge on Windows, where a
    // blocking accept on a re-bound listener may linger (see finish_daemon).
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
    }
}

/// A client that negotiated a weak digest cannot present a response computed with
/// a different one, and vice versa. This is the downgrade case: before the digest
/// was pinned at negotiation the verifier inferred the algorithm from the
/// response *length*, so an 86-character SHA-512 response was checked as SHA-512
/// no matter what the greeting had negotiated - letting the client choose.
///
/// upstream: authenticate.c:90 `generate_hash()` only ever hashes with
/// `valid_auth_checksums.negotiated_nni`.
#[test]
fn daemon_auth_rejects_a_response_computed_with_a_non_negotiated_digest() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_daemon(dir.path());
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    // Offer SHA-512 first so it is what gets negotiated.
    let answer = negotiate_protected_module(&mut stream, &mut reader, "@RSYNCD: 32.0 sha512 md5\n");
    let challenge = answer
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .unwrap_or_else(|| panic!("expected auth challenge, got: {answer}"));
    assert_eq!(challenge.len(), DaemonAuthDigest::Sha512.base64_len());

    // Answer with the correct password but the *wrong* (shorter) digest.
    let short_response =
        core::auth::compute_daemon_auth_response(b"correctpassword", challenge, DaemonAuthDigest::Md5);
    assert_eq!(short_response.len(), DaemonAuthDigest::Md5.base64_len());
    stream
        .write_all(format!("alice {short_response}\n").as_bytes())
        .expect("send response");
    stream.flush().expect("flush response");

    let mut line = String::new();
    reader.read_line(&mut line).expect("auth verdict");
    assert_eq!(
        line.trim_end(),
        "@ERROR: auth failed on module protected",
        "a digest other than the negotiated one must never authenticate"
    );

    drop(reader);
    drop(stream);
    // Bounded join: an unconditional join() can wedge on Windows, where a
    // blocking accept on a re-bound listener may linger (see finish_daemon).
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
    }
}

/// When the client's list names nothing this build implements, the daemon refuses
/// with upstream's exact wording - echoing its *own* list - and never sends a
/// challenge.
///
/// upstream: compat.c:871-875 - `@ERROR: your client does not support one of our
/// daemon-auth checksums: %s\n` followed by `exit_cleanup(RERR_UNSUPPORTED)`.
/// Verified against rsync 3.4.4, which answers exactly this line and exits 4.
#[test]
fn daemon_auth_refuses_a_client_with_no_mutual_digest() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_daemon(dir.path());
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let answer =
        negotiate_protected_module(&mut stream, &mut reader, "@RSYNCD: 32.0 sponge blake3\n");
    assert_eq!(
        answer,
        "@ERROR: your client does not support one of our daemon-auth checksums: \
         sha512 sha256 sha1 md5 md4"
    );

    // The refusal replaces the challenge: nothing else reaches the client.
    let mut line = String::new();
    let read = reader.read_line(&mut line).expect("eof after error");
    assert_eq!(read, 0, "no challenge may follow the refusal, got: {line:?}");

    drop(reader);
    drop(stream);
    // Bounded join: an unconditional join() can wedge on Windows, where a
    // blocking accept on a re-bound listener may linger (see finish_daemon).
    if let Some(result) = finish_daemon(handle) {
        assert!(result.is_ok(), "the listener survives a refused client: {result:?}");
    }
}

/// An unprotected module never negotiates, so a foreign digest list is harmless
/// there.
///
/// upstream: authenticate.c:239-240 - `if (!users || !*users) return "";` returns
/// before `negotiate_daemon_auth()` is ever called.
#[test]
fn daemon_skips_digest_negotiation_when_the_module_needs_no_auth() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("open");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[open]\npath = {}\nuse chroot = false\n",
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
            OsString::from("--no-detach"),
            OsString::from("--once"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();
    let (mut stream, handle) = start_daemon(config, port, held_listener);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("server greeting");
    stream
        .write_all(b"@RSYNCD: 32.0 sponge blake3\nopen\n")
        .expect("send greeting and module");
    stream.flush().expect("flush");

    line.clear();
    reader.read_line(&mut line).expect("daemon answer");
    assert_eq!(line.trim_end(), "@RSYNCD: OK");

    drop(reader);
    drop(stream);
    let _ = finish_daemon(handle);
}

/// A protocol > 31 greeting that carries no digest list at all is refused before
/// any module is looked up.
///
/// upstream: clientserver.c:203-211 - `daemon_auth_choices` is NULL and
/// `remote_protocol > 31`, so `exchange_protocols()` answers
/// `@ERROR: your client omitted the digest name list: %s` echoing the raw line and
/// returns -1. Verified against rsync 3.4.4.
#[test]
fn daemon_refuses_a_protocol_32_greeting_without_a_digest_list() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let (mut stream, handle) = start_auth_digest_daemon(dir.path());
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let mut line = String::new();
    reader.read_line(&mut line).expect("server greeting");

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send bare greeting");
    stream.flush().expect("flush greeting");

    line.clear();
    reader.read_line(&mut line).expect("refusal");
    assert_eq!(
        line.trim_end(),
        "@ERROR: your client omitted the digest name list: @RSYNCD: 32.0"
    );

    drop(reader);
    drop(stream);
    let _ = finish_daemon(handle);
}

/// The single-session entry points (stdio, inetd) surface a session-fatal refusal
/// as the process exit status, the way upstream's forked child does.
///
/// upstream: compat.c:875 `exit_cleanup(RERR_UNSUPPORTED)`. Confirmed against
/// rsync 3.4.4 driven as `rsync --server --daemon`: it prints the refusal and
/// exits 4.
#[test]
fn single_session_exit_maps_a_refusal_to_upstream_exit_code_4() {
    let clean = single_session_exit(Ok(None), "stdio daemon session failed");
    assert!(clean.is_ok());

    let refused = single_session_exit(
        Ok(Some(UNSUPPORTED_AUTH_DIGEST_EXIT_CODE)),
        "stdio daemon session failed",
    );
    let error = refused.expect_err("a refusal must not report success");
    assert_eq!(error.exit_code(), ExitCode::Unsupported.as_i32());
    assert_eq!(error.exit_code(), 4);
}
