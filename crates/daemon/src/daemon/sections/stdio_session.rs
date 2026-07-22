// Stdio daemon session - runs a single daemon session over stdin/stdout.
//
// upstream: main.c:1867-1868 - when both `am_server` and `am_daemon` are set,
// upstream rsync calls `start_daemon(STDIN_FILENO, STDOUT_FILENO)`. This runs
// the daemon protocol over the process's stdin and stdout, used by remote-shell
// daemon mode (`rsync -e ssh host::module`) and by `RSYNC_CONNECT_PROG`.
//
// upstream: clientserver.c:1546-1559 - `daemon_main()` detects
// `is_a_socket(STDIN_FILENO)` for inetd-style invocations and calls
// `start_daemon(STDIN_FILENO, STDIN_FILENO)`.

/// Runs a single daemon session over stdin/stdout.
///
/// Loads the configuration from the provided arguments (respecting `--config`),
/// builds the module table, and runs the legacy `@RSYNCD:` session handler
/// with a [`DaemonStream::Stdio`]. This is the Rust equivalent of upstream's
/// `start_daemon(STDIN_FILENO, STDOUT_FILENO)`.
///
/// When `is_rsh_daemon` is `true`, the daemon looks for `rsyncd.conf` in the
/// current working directory before falling back to the system default,
/// matching upstream's `RSYNCD_USERCONF` behavior.
///
/// # Errors
///
/// Returns a `DaemonError` if config loading fails or the session handler
/// encounters an I/O error.
pub fn run_stdio_session(
    arguments: &[OsString],
    is_rsh_daemon: bool,
) -> Result<(), DaemonError> {
    let mut options = RuntimeOptions {
        brand: Brand::Oc,
        ..Default::default()
    };
    let mut seen_modules = HashSet::new();
    let mut has_explicit_config = false;

    // Parse --config and --log-file from the provided arguments.
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        if let Some(value) = take_option_value(argument, &mut iter, "--config")
            .map_err(|e| DaemonError::new(1, rsync_error!(1, format!("{e}")).with_role(Role::Daemon)))?
        {
            options.load_config_modules(&value, &mut seen_modules)?;
            has_explicit_config = true;
        } else if let Some(value) = take_option_value(argument, &mut iter, "--log-file")
            .map_err(|e| DaemonError::new(1, rsync_error!(1, format!("{e}")).with_role(Role::Daemon)))?
        {
            options.set_log_file(PathBuf::from(value))?;
        }
    }

    // When no --config was given, search for a config file.
    // upstream: clientserver.c:1277-1281 - load_config() uses RSYNCD_USERCONF
    // ("rsyncd.conf" relative to CWD) for rsh-daemon non-root invocations,
    // and RSYNCD_SYSCONF ("/etc/rsyncd.conf") otherwise.
    if !has_explicit_config {
        let mut loaded = false;

        // For remote-shell daemon mode, try CWD "rsyncd.conf" first.
        // upstream: rsync.h:31 - RSYNCD_USERCONF = "rsyncd.conf"
        if is_rsh_daemon {
            let user_conf = OsString::from("rsyncd.conf");
            if PathBuf::from(&user_conf).exists()
                && options
                    .load_config_modules(&user_conf, &mut seen_modules)
                    .is_ok()
            {
                loaded = true;
            }
        }

        if !loaded {
            if let Some(path) = environment_config_override() {
                options.load_config_modules(&path, &mut seen_modules)?;
            } else if let Some(path) = default_config_path_if_present(Brand::Oc) {
                options.load_config_modules(&path, &mut seen_modules)?;
            }
        }
    }

    let RuntimeOptions {
        modules,
        motd_lines,
        bandwidth_limit,
        bandwidth_burst,
        log_file,
        reverse_lookup,
        ..
    } = options;

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, Brand::Oc)?)
    } else {
        None
    };

    let connection_limiter: Option<Arc<ConnectionLimiter>> = None;
    let modules: Vec<ModuleRuntime> = modules
        .into_iter()
        .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
        .collect();

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let pair = crate::daemon_stream::StdioPair::new(Box::new(stdin), Box::new(stdout));
    let stream = DaemonStream::stdio(pair);

    // upstream: clientserver.c:1300-1301 - for rsh-daemon, am_daemon is set
    // to -1 as a flag distinguishing it from a network daemon. We use a
    // loopback address as the synthetic peer for log messages and access checks.
    let peer_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    handle_legacy_session(
        stream,
        peer_addr,
        LegacySessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: bandwidth_limit,
            daemon_burst: bandwidth_burst,
            log_sink,
            peer_host: Some("localhost".to_owned()),
            reverse_lookup,
        },
    )
    .map_err(|error| {
        DaemonError::new(
            SOCKET_IO_EXIT_CODE,
            rsync_error!(SOCKET_IO_EXIT_CODE, format!("stdio daemon session failed: {error}"))
                .with_role(Role::Daemon),
        )
    })
}
