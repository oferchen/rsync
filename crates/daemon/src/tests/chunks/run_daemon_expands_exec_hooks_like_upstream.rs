/// Starts a `--once` daemon with one module `[hooks]` at `module_dir` whose
/// extra directives are `hook_lines`, names the module, and returns everything
/// the daemon sent after the greeting until it closed or said `@RSYNCD: OK`.
#[cfg(unix)]
fn request_hooked_module(module_dir: &Path, config_dir: &Path, hook_lines: &str) -> String {
    let config_path = config_dir.join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[hooks]\npath = {}\nread only = false\nuse chroot = false\n{hook_lines}",
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
    stream
        .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\nhooks\n")
        .expect("send greeting and module");

    let mut reply = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        reply.push_str(&line);
        if line == "@RSYNCD: OK\n" {
            break;
        }
    }

    drop(reader);
    drop(stream);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
    reply
}

/// Hook templates expand only upstream's `%NAME%` references.
///
/// upstream: loadparm.c:250 expands `%` + uppercase letter + name + `%` through
/// getenv, escaping an `RSYNC_` value for its quote context; nothing else is
/// special. Measured on 3.5.1: `"%m %P %u %a %h %p %RSYNC_MODULE_NAME%
/// %RSYNC_PID%"` inside double quotes reaches the hook as
/// `%m %P %u %a %h %p 'pct' %RSYNC_PID%`.
#[cfg(unix)]
#[test]
fn run_daemon_expands_only_upstream_hook_references() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let _enabled = EnvGuard::remove("RSYNC_NO_XFER_EXEC");

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let out = dir.path().join("early.out");

    let reply = request_hooked_module(
        &module_dir,
        dir.path(),
        &format!(
            "early exec = echo \"%m %P %u %a %h %p %RSYNC_MODULE_NAME% %RSYNC_PID%\" > {}\n",
            out.display()
        ),
    );
    assert_eq!(reply, "@RSYNCD: OK\n");
    assert_eq!(
        fs::read_to_string(&out).expect("hook output"),
        "%m %P %u %a %h %p 'hooks' %RSYNC_PID%\n"
    );
}

/// A refused hook ends the session before `@RSYNCD: OK` with nothing sent, and
/// a pre-fork refusal never arms `post-xfer exec`.
///
/// upstream: loadparm.c:266-274 refuses an `RSYNC_` value holding a shell
/// metacharacter - a module path with a space included - by logging and
/// `exit_cleanup(RERR_UNSUPPORTED)`. The directives are read at
/// clientserver.c:959-967, before the post-xfer fork and the `@RSYNCD: OK` at
/// :1152. Measured on 3.5.1: the client sees the socket close ("didn't get
/// server startup line") and the post-xfer hook never runs.
#[cfg(unix)]
#[test]
fn run_daemon_refuses_an_unsafe_hook_before_ok_without_post_xfer() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let _enabled = EnvGuard::remove("RSYNC_NO_XFER_EXEC");

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module with space");
    fs::create_dir_all(&module_dir).expect("module dir");
    let pre = dir.path().join("pre.out");
    let post = dir.path().join("post.out");

    let reply = request_hooked_module(
        &module_dir,
        dir.path(),
        &format!(
            "pre-xfer exec = echo %RSYNC_MODULE_PATH% > {}\npost-xfer exec = echo ran > {}\n",
            pre.display(),
            post.display()
        ),
    );
    assert_eq!(reply, "", "a refused hook sends nothing, not even @ERROR");
    assert!(!pre.exists(), "the refused hook must not run");
    assert!(
        !post.exists(),
        "a refusal before the post-xfer fork must not run post-xfer exec"
    );
}

/// A module `path` expands upstream's `%RSYNC_*%` references and nothing else.
///
/// upstream: loadparm.c expand_vars() has no built-in names, so the old oc
/// `%MODULE%` alias is a literal directory name; `%RSYNC_MODULE_NAME%` is set
/// (clientserver.c:757) before `path` is read (:877). Measured on 3.5.1:
/// `path = <dir>/%MODULE%` serves the directory literally named `%MODULE%`.
/// Each fixture creates only the directory the upstream reading names, so a
/// wrong expansion answers `@ERROR: chdir failed`.
#[cfg(unix)]
#[test]
fn run_daemon_expands_module_path_like_upstream() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));
    let _module = EnvGuard::remove("MODULE");

    for (token, served) in [("%MODULE%", "%MODULE%"), ("%RSYNC_MODULE_NAME%", "hooks")] {
        let dir = tempdir().expect("config dir");
        fs::create_dir_all(dir.path().join(served)).expect("served dir");
        let reply = request_hooked_module(&dir.path().join(token), dir.path(), "");
        assert_eq!(
            reply, "@RSYNCD: OK\n",
            "path token {token} must serve {served}"
        );
    }
}
