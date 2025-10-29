#[test]
fn module_bwlimit_cannot_raise_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(8 * 1024 * 1024),
        true,
        true,
        None,
        false,
    );

    assert_eq!(change, LimiterChange::Unchanged);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn module_bwlimit_can_lower_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(8 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(1024 * 1024),
        true,
        true,
        None,
        false,
    );

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(limiter.limit_bytes(), NonZeroU64::new(1024 * 1024).unwrap());
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn module_bwlimit_burst_does_not_raise_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(8 * 1024 * 1024),
        true,
        true,
        Some(NonZeroU64::new(256 * 1024).unwrap()),
        true,
    );

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        limiter.burst_bytes(),
        Some(NonZeroU64::new(256 * 1024).unwrap())
    );
}

#[test]
fn module_bwlimit_configures_unlimited_daemon() {
    let mut limiter = None;

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(2 * 1024 * 1024),
        true,
        true,
        None,
        false,
    );

    assert_eq!(change, LimiterChange::Enabled);

    let limiter = limiter.expect("limiter configured by module");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());

    let mut limiter = Some(limiter);
    let change = apply_module_bandwidth_limit(
        &mut limiter,
        None,
        false,
        true,
        Some(NonZeroU64::new(256 * 1024).unwrap()),
        true,
    );

    assert_eq!(change, LimiterChange::Updated);
    let limiter = limiter.expect("limiter preserved");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        limiter.burst_bytes(),
        Some(NonZeroU64::new(256 * 1024).unwrap())
    );
}

#[test]
fn module_without_bwlimit_inherits_daemon_cap() {
    let limit = NonZeroU64::new(3 * 1024 * 1024).unwrap();
    let mut limiter = Some(BandwidthLimiter::new(limit));

    let change = apply_module_bandwidth_limit(&mut limiter, None, false, false, None, false);

    assert_eq!(change, LimiterChange::Unchanged);

    let limiter = limiter.expect("limiter remains in effect");
    assert_eq!(limiter.limit_bytes(), limit);
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn module_bwlimit_updates_burst_without_lowering_limit() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(4 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(4 * 1024 * 1024),
        true,
        true,
        Some(NonZeroU64::new(512 * 1024).unwrap()),
        true,
    );

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(4 * 1024 * 1024).unwrap()
    );
    assert_eq!(
        limiter.burst_bytes(),
        Some(NonZeroU64::new(512 * 1024).unwrap())
    );
}

#[test]
fn module_bwlimit_zero_burst_clears_existing_burst() {
    let mut limiter = Some(BandwidthLimiter::with_burst(
        NonZeroU64::new(4 * 1024 * 1024).unwrap(),
        Some(NonZeroU64::new(512 * 1024).unwrap()),
    ));

    let change = apply_module_bandwidth_limit(
        &mut limiter,
        NonZeroU64::new(4 * 1024 * 1024),
        true,
        true,
        None,
        true,
    );

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("limiter remains configured");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(4 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn module_bwlimit_unlimited_clears_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, true, true, None, false);

    assert_eq!(change, LimiterChange::Disabled);

    assert!(limiter.is_none());
}

#[test]
fn module_bwlimit_unlimited_with_burst_override_clears_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, true, true, None, true);

    assert_eq!(change, LimiterChange::Disabled);

    assert!(limiter.is_none());
}

#[test]
fn module_bwlimit_configured_unlimited_without_specified_flag_clears_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, false, true, None, false);

    assert_eq!(change, LimiterChange::Disabled);

    assert!(limiter.is_none());
}

#[test]
fn module_bwlimit_configured_unlimited_with_burst_override_clears_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, false, true, None, true);

    assert_eq!(change, LimiterChange::Disabled);

    assert!(limiter.is_none());
}

#[test]
fn module_bwlimit_unlimited_with_explicit_burst_preserves_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(4 * 1024 * 1024).unwrap(),
    ));

    let burst = NonZeroU64::new(256 * 1024).unwrap();
    let change = apply_module_bandwidth_limit(&mut limiter, None, false, true, Some(burst), true);

    assert_eq!(change, LimiterChange::Updated);

    let limiter = limiter.expect("daemon cap should remain active");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(4 * 1024 * 1024).unwrap()
    );
    assert_eq!(limiter.burst_bytes(), Some(burst));
}

#[test]
fn module_bwlimit_unlimited_is_noop_when_no_cap() {
    let mut limiter: Option<BandwidthLimiter> = None;

    let change = apply_module_bandwidth_limit(&mut limiter, None, true, true, None, false);

    assert_eq!(change, LimiterChange::Unchanged);

    assert!(limiter.is_none());
}

#[test]
fn log_module_bandwidth_change_logs_updates() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path).expect("open log");
    let limiter = BandwidthLimiter::with_burst(
        NonZeroU64::new(8 * 1024).expect("limit"),
        Some(NonZeroU64::new(64 * 1024).expect("burst")),
    );

    log_module_bandwidth_change(
        &log,
        None,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "docs",
        Some(&limiter),
        LimiterChange::Enabled,
    );

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    assert!(contents.contains("enabled bandwidth limit 8 KiB/s with burst 64 KiB/s"));
    assert!(contents.contains("module 'docs'"));
    assert!(contents.contains("127.0.0.1"));
}

#[test]
fn log_module_bandwidth_change_logs_disable() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path).expect("open log");

    log_module_bandwidth_change(
        &log,
        Some("client.example"),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "docs",
        None,
        LimiterChange::Disabled,
    );

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    assert!(contents.contains("removed bandwidth limit"));
    assert!(contents.contains("client.example"));
}

#[test]
fn log_module_bandwidth_change_ignores_unchanged() {
    let dir = tempdir().expect("log dir");
    let path = dir.path().join("daemon.log");
    let log = open_log_sink(&path).expect("open log");

    let limiter = BandwidthLimiter::new(NonZeroU64::new(4 * 1024).expect("limit"));

    log_module_bandwidth_change(
        &log,
        None,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        "docs",
        Some(&limiter),
        LimiterChange::Unchanged,
    );

    drop(log);

    let contents = fs::read_to_string(&path).expect("read log");
    assert!(contents.is_empty());
}

#[test]
fn module_without_bwlimit_preserves_daemon_cap() {
    let mut limiter = Some(BandwidthLimiter::new(
        NonZeroU64::new(2 * 1024 * 1024).unwrap(),
    ));

    let change = apply_module_bandwidth_limit(&mut limiter, None, false, false, None, false);

    assert_eq!(change, LimiterChange::Unchanged);

    let limiter = limiter.expect("daemon cap should remain active");
    assert_eq!(
        limiter.limit_bytes(),
        NonZeroU64::new(2 * 1024 * 1024).unwrap()
    );
    assert!(limiter.burst_bytes().is_none());
}

#[test]
fn run_daemon_refuses_disallowed_module_options() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nrefuse options = compress\n",
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

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    line.clear();
    reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    stream
        .write_all(b"@RSYNCD: OPTION --compress\n")
        .expect("send refused option");
    stream.flush().expect("flush refused option");

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("refusal message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: The server is configured to refuse --compress",
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_denies_module_when_host_not_allowed() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nhosts allow = 10.0.0.0/8\n",).expect("write config");

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

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake response");
    stream.flush().expect("flush handshake response");

    line.clear();
    reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    stream.write_all(b"docs\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: access denied to module 'docs' from 127.0.0.1"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

