/// Daemon defense-in-depth: a module with `munge symlinks = yes` must refuse
/// to serve when a `rsyncd-munged` directory already exists in the module
/// root, because an attacker who planted it could make munged
/// `/rsyncd-munged/...` symlinks resolve through real on-disk paths and so
/// escape the module - defeating the whole point of symlink munging.
///
/// The check runs after path validation and before `@RSYNCD: OK`, so the
/// daemon replies with the raw pre-OK line
/// `@ERROR: daemon security issue -- contact admin` and exits
/// `RERR_UNSUPPORTED` (4) instead of acknowledging the session.
///
/// # Upstream Reference
///
/// - `clientserver.c:rsync_module` (998-1009) - after `change_dir()` into the
///   module and resolving `munge_symlinks`, upstream stats the
///   `SYMLINK_PREFIX` name with its trailing slash trimmed (`rsyncd-munged`);
///   an existing directory triggers `@ERROR: daemon security issue --
///   contact admin` + `exit_cleanup(RERR_UNSUPPORTED)`.
/// - `rsync.h:36` - `SYMLINK_PREFIX "/rsyncd-munged/"`.
#[cfg(unix)]
#[test]
fn daemon_munge_symlinks_rejects_preexisting_munged_dir() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    // Plant the attacker-controlled directory the safety check must catch.
    fs::create_dir(module_dir.join("rsyncd-munged")).expect("plant rsyncd-munged dir");

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[mungemod]\npath = {}\nread only = false\nuse chroot = false\nmunge symlinks = yes\n",
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
    stream.flush().expect("flush handshake response");

    stream
        .write_all(b"mungemod\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    // The safety check fires before `@RSYNCD: OK`, so the very next line the
    // client sees is the security-issue error rather than an acknowledgement.
    line.clear();
    reader.read_line(&mut line).expect("security error message");
    assert_eq!(
        line, "@ERROR: daemon security issue -- contact admin\n",
        "a pre-existing `rsyncd-munged` directory must abort the session with \
         upstream's security-issue error before the transfer begins",
    );

    // upstream: the abort happens before `@RSYNCD: OK`, so no OK is ever sent
    // and the socket closes right after the error (next read is EOF).
    line.clear();
    let read = reader.read_line(&mut line).expect("eof after error");
    assert_eq!(
        read, 0,
        "no trailing line after the security error, got: {line:?}"
    );

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon returned error: {result:?}");
}

/// Control case for [`daemon_munge_symlinks_rejects_preexisting_munged_dir`]:
/// the identical `munge symlinks = yes` module with NO pre-existing
/// `rsyncd-munged` directory proceeds to `@RSYNCD: OK` normally. This pins the
/// safety check to the attacker-planted directory rather than to munging being
/// enabled, so enabling munging alone never breaks a legitimate module.
#[cfg(unix)]
#[test]
fn daemon_munge_symlinks_without_munged_dir_proceeds_to_ok() {
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
            "[mungemod]\npath = {}\nread only = false\nuse chroot = false\nmunge symlinks = yes\n",
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
    stream.flush().expect("flush handshake response");

    stream
        .write_all(b"mungemod\n")
        .expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("ok message");
    assert_eq!(
        line, "@RSYNCD: OK\n",
        "without an attacker-planted `rsyncd-munged` directory the module must \
         acknowledge the session as usual",
    );

    drop(reader);
    drop(stream);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon returned error: {result:?}");
}
