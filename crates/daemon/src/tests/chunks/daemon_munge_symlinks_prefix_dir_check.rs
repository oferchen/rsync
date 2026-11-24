/// Daemon `munge symlinks = yes` refuses a session only for the
/// `/rsyncd-munged` directory a munged link would resolve through: the host's
/// root entry for an unchrooted module. A `rsyncd-munged` directory inside the
/// module is ordinary data and must not refuse the session.
///
/// The check runs after path validation and before `@RSYNCD: OK`; on a hit the
/// daemon replies with the raw pre-OK line
/// `@ERROR: daemon security issue -- contact admin` and exits
/// `RERR_UNSUPPORTED` (4).
///
/// # Upstream Reference
///
/// - `clientserver.c:1076-1084` - after the chroot (`:1044-1052`) upstream
///   stats `SYMLINK_PREFIX` with its trailing slash trimmed, the absolute
///   `/rsyncd-munged`, so an unchrooted module consults the host root.
/// - `rsync.h:36` - `SYMLINK_PREFIX "/rsyncd-munged/"`.
#[cfg(unix)]
#[test]
fn daemon_munge_symlinks_ignores_a_munged_dir_inside_the_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");

    // A directory named like the prefix, but inside the module: not the one
    // a munged `/rsyncd-munged/...` link resolves through.
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

    // Only the host's `/rsyncd-munged` decides; the module's entry never does.
    let host_has_prefix_dir = Path::new("/rsyncd-munged").is_dir();
    line.clear();
    reader.read_line(&mut line).expect("module reply");
    let expected = if host_has_prefix_dir {
        "@ERROR: daemon security issue -- contact admin\n"
    } else {
        "@RSYNCD: OK\n"
    };
    assert_eq!(
        line, expected,
        "an unchrooted module's own `rsyncd-munged` directory must not decide \
         the munging safety check; only the host's /rsyncd-munged does",
    );

    drop(reader);
    drop(stream);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon returned error: {result:?}");
}

/// Control case for [`daemon_munge_symlinks_ignores_a_munged_dir_inside_the_module`]:
/// the identical `munge symlinks = yes` module with NO pre-existing
/// `rsyncd-munged` directory proceeds to `@RSYNCD: OK` normally. Enabling
/// munging alone never breaks a legitimate module.
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
