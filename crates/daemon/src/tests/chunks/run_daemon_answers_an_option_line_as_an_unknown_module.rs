/// A pre-module `@RSYNCD: OPTION ...` line is a module name, never a
/// configuration or refuse-options channel.
///
/// upstream: `start_daemon` reads exactly one request line after the greeting
/// (clientserver.c:1541-1575) and looks it up as a module, so an upstream 3.5.1
/// daemon answers `@ERROR: Unknown module '@RSYNCD: OPTION read only=no'` and
/// closes. Module parameters come only from the operator: the config file and
/// the daemon's own `--dparam` (loadparm.c:667 set_dparams()).
///
/// The bait is a read-only, chrooted anonymous module the peer asks to relax
/// before naming it. Were either line honoured, the session would reach
/// `@RSYNCD: OK` and accept a push into the module. Reaching the unknown-module
/// refusal instead means no module session ever starts, so neither the push
/// gate nor the chroot decision is taken from peer input.
#[test]
fn run_daemon_answers_an_option_line_as_an_unknown_module() {
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
            "[readonly]\npath = {}\nread only = true\nuse chroot = true\n",
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

    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "expected greeting, got: {line}"
    );

    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
        .expect("send handshake response");
    // Everything a peer would need to push into the module unconfined. The
    // daemon stops reading at the first line, so the rest may be discarded
    // with the socket; send it in one write so no write can fail midway.
    let _ = stream.write_all(
        b"@RSYNCD: OPTION read only=no\n\
          @RSYNCD: OPTION use chroot=no\n\
          readonly\n",
    );
    let _ = stream.flush();

    line.clear();
    reader.read_line(&mut line).expect("refusal line");
    assert_eq!(
        line, "@ERROR: Unknown module '@RSYNCD: OPTION read only=no'\n",
        "the OPTION line must be looked up as a module name, as upstream does"
    );

    // upstream: the client treats @ERROR as fatal and the daemon returns
    // without reading further, so nothing follows the refusal.
    let mut rest = Vec::new();
    let _ = reader.read_to_end(&mut rest);
    assert!(
        rest.is_empty(),
        "no @RSYNCD: OK or other output may follow the refusal, got {:?}",
        String::from_utf8_lossy(&rest)
    );

    drop(reader);
    drop(stream);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
    assert_eq!(
        fs::read_dir(&module_dir).expect("module dir").count(),
        0,
        "the read-only module must be untouched"
    );
}
