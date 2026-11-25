/// Runs a one-connection daemon whose `[mod]` section ends with `extra`,
/// requests `mod`, and returns the first line the daemon sent back (empty
/// when it closed the connection without one) and the daemon log split into
/// messages without their timestamp and pid prefix.
fn module_setup_refusal(dir: &Path, extra: &str) -> (String, Vec<String>) {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let log_path = dir.join("rsyncd.log");
    let config_path = dir.join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("log file = {}\n[mod]\n{extra}", log_path.display()),
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
    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\nmod\n")
        .expect("send module request");
    stream.flush().expect("flush module request");
    line.clear();
    reader.read_line(&mut line).expect("refusal");
    drop(reader);
    drop(stream);
    let _ = handle.join().expect("daemon thread");
    let log = fs::read_to_string(&log_path).expect("read daemon log");
    let messages = log
        .lines()
        .map(|entry| {
            entry
                .split_once("] ")
                .map_or(entry, |(_, body)| body)
                .to_owned()
        })
        .collect();
    (line.trim_end().to_owned(), messages)
}

/// The message logged right after the module's access line.
fn after_access_line(messages: &[String]) -> &str {
    let access = messages
        .iter()
        .position(|message| message.starts_with("rsync allowed access on module mod from "))
        .unwrap_or_else(|| panic!("no access line in {messages:?}"));
    messages
        .get(access + 1)
        .unwrap_or_else(|| panic!("nothing logged after the access line: {messages:?}"))
}

/// A module whose path cannot be entered tells the peer only that the chdir
/// failed; the log names the directory and the errno. The absolute path must
/// not reach the peer.
///
/// upstream: clientserver.c:692-699 path_failure(), :1063-1064.
#[test]
fn missing_module_path_refuses_with_upstream_chdir_lines() {
    let dir = tempdir().expect("config dir");
    let missing = dir.path().join("missing");
    let cause = fs::metadata(&missing).expect_err("path is missing");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!("use chroot = false\npath = {}\n", missing.display()),
    );
    assert_eq!(reply, "@ERROR: chdir failed");
    assert_eq!(
        after_access_line(&messages),
        logging::rsyserr_io(
            "rsync",
            "Receiver",
            format_args!("chdir {} failed", missing.display()),
            &cause,
        )
    );
}

/// A lock file that cannot be opened is logged with its path and errno, after
/// the access line, because upstream logs access before claiming the slot.
///
/// upstream: clientserver.c:787-800.
#[test]
fn unopenable_lock_file_refuses_with_upstream_lines() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let lock_file = dir.path().join("nodir").join("lock");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!(
            "use chroot = false\npath = {}\nmax connections = 1\nlock file = {}\n",
            module_dir.display(),
            lock_file.display()
        ),
    );
    assert_eq!(reply, "@ERROR: failed to open lock file");
    let logged = after_access_line(&messages);
    let prefix = format!(
        "rsync: [Receiver] failed to open lock file {}: ",
        lock_file.display()
    );
    assert!(
        logged.starts_with(&prefix),
        "expected {prefix:?}..., got {logged:?}; log: {messages:?}"
    );
}

/// An early exec hook that fails logs upstream's one line and nothing oc
/// adds; a hook that succeeds logs nothing at all.
///
/// upstream: clientserver.c:998-1013 - finish_pre_exec()'s description is
/// discarded and errno is the ECHILD left by wait_process() (main.c:152-166).
#[cfg(unix)]
#[test]
fn failing_early_exec_refuses_with_the_upstream_line() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!(
            "use chroot = false\npath = {}\nearly exec = /bin/false\n",
            module_dir.display()
        ),
    );
    assert_eq!(reply, "@ERROR: early exec failed");
    assert_eq!(
        after_access_line(&messages),
        logging::rsyserr("rsync", "Receiver", "early exec failed", 10)
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.contains("early exec command")),
        "oc's own hook description must not be logged: {messages:?}"
    );
}

/// A module filter that does not parse closes the connection with nothing
/// sent; the log has the parser's cause as is, then the trailer with the role
/// upstream reports before the server arguments are parsed.
///
/// upstream: exclude.c:136, cleanup.c:208-216, log.c:1025-1051.
#[test]
fn unparsable_module_filter_logs_the_cause_and_a_receiver_trailer() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!(
            "use chroot = false\npath = {}\nfilter = - foo clear,- keep\n",
            module_dir.display()
        ),
    );
    assert_eq!(reply, "", "the daemon must close without a reply");
    let cause = messages
        .iter()
        .position(|message| message == "'!' rule has trailing characters: clear,- keep")
        .unwrap_or_else(|| panic!("no bare cause line in {messages:?}"));
    let trailer = &messages[cause + 1];
    assert!(
        trailer.contains("syntax or usage error (code 1) at ")
            && trailer.ends_with(&format!("[Receiver={}]", env!("CARGO_PKG_VERSION"))),
        "expected the syntax-error trailer tagged [Receiver=...], got {trailer:?}"
    );
}

/// Under `use chroot = yes` a missing module path is refused at the chroot,
/// not at the chdir: the peer reads `@ERROR: chroot failed` and the log names
/// the chroot target. No privilege is needed, because the path is checked
/// before the chroot is attempted.
///
/// upstream: clientserver.c:1054-1058.
#[test]
fn missing_chrooted_module_path_refuses_at_the_chroot() {
    let dir = tempdir().expect("config dir");
    let missing = dir.path().join("missing");
    let cause = fs::metadata(&missing).expect_err("path is missing");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!("use chroot = yes\npath = {}\n", missing.display()),
    );
    assert_eq!(reply, "@ERROR: chroot failed");
    assert_eq!(
        after_access_line(&messages),
        logging::rsyserr_io(
            "rsync",
            "Receiver",
            format_args!("chroot(\"{}\") failed", missing.display()),
            &cause,
        )
    );
}

/// With a `/./` split, the chroot target exists and the missing inner half is
/// refused at the chdir, named as the jail sees it.
///
/// upstream: clientserver.c:901-912, :1063-1064.
#[test]
fn missing_inner_half_of_a_split_module_path_refuses_at_the_chdir() {
    let dir = tempdir().expect("config dir");
    let outer = dir.path().join("real");
    fs::create_dir_all(&outer).expect("outer dir");
    let cause = fs::metadata(outer.join("missing")).expect_err("inner is missing");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!("use chroot = yes\npath = {}/./missing\n", outer.display()),
    );
    assert_eq!(reply, "@ERROR: chdir failed");
    assert_eq!(
        after_access_line(&messages),
        logging::rsyserr_io("rsync", "Receiver", "chdir /missing failed", &cause)
    );
}

/// A module path that is a regular file is refused at the chdir with the
/// errno chdir() would report, not accepted and failed later.
///
/// upstream: clientserver.c:692-699, :1063-1064.
#[test]
fn module_path_that_is_a_file_refuses_at_the_chdir() {
    let dir = tempdir().expect("config dir");
    let file = dir.path().join("target");
    fs::write(&file, "not a directory\n").expect("write file");
    let cause = fs::read_dir(&file).expect_err("a file is not a directory");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!("use chroot = false\npath = {}\n", file.display()),
    );
    assert_eq!(reply, "@ERROR: chdir failed");
    assert_eq!(
        after_access_line(&messages),
        logging::rsyserr_io(
            "rsync",
            "Receiver",
            format_args!("chdir {} failed", file.display()),
            &cause,
        )
    );
}

/// Under `use chroot = yes` the same file is refused at the chroot.
///
/// upstream: clientserver.c:1054-1058.
#[test]
fn chrooted_module_path_that_is_a_file_refuses_at_the_chroot() {
    let dir = tempdir().expect("config dir");
    let file = dir.path().join("target");
    fs::write(&file, "not a directory\n").expect("write file");
    let cause = fs::read_dir(&file).expect_err("a file is not a directory");
    let (reply, messages) = module_setup_refusal(
        dir.path(),
        &format!("use chroot = yes\npath = {}\n", file.display()),
    );
    assert_eq!(reply, "@ERROR: chroot failed");
    assert_eq!(
        after_access_line(&messages),
        logging::rsyserr_io(
            "rsync",
            "Receiver",
            format_args!("chroot(\"{}\") failed", file.display()),
            &cause,
        )
    );
}
