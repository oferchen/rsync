#[cfg(unix)]
#[test]
fn binary_session_delegation_propagates_runtime_arguments() {
    use std::io::BufReader;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");

    let module_dir = temp.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = temp.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("[docs]\n    path = {}\n", module_dir.display()),
    )
    .expect("write config");

    let mut frames = Vec::new();
    MessageFrame::new(
        MessageCode::Error,
        HANDSHAKE_ERROR_PAYLOAD.as_bytes().to_vec(),
    )
    .expect("frame")
    .encode_into_writer(&mut frames)
    .expect("encode error frame");
    let exit_code = u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE).unwrap_or_default();
    MessageFrame::new(MessageCode::ErrorExit, exit_code.to_be_bytes().to_vec())
        .expect("exit frame")
        .encode_into_writer(&mut frames)
        .expect("encode exit frame");

    let mut expected = Vec::new();
    expected.extend_from_slice(&u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes());
    expected.extend_from_slice(&frames);
    let expected_hex: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();

    let script_path = temp.path().join("binary-args.py");
    let args_log_path = temp.path().join("delegation-args.log");
    let script = "#!/usr/bin/env python3\n".to_string()
        + "import os, sys, binascii\n"
        + "args_log = os.environ.get('ARGS_LOG')\n"
        + "if args_log:\n"
        + "    with open(args_log, 'w', encoding='utf-8') as handle:\n"
        + "        handle.write(' '.join(sys.argv[1:]))\n"
        + "sys.stdin.buffer.read(4)\n"
        + "payload = binascii.unhexlify(os.environ['BINARY_RESPONSE_HEX'])\n"
        + "sys.stdout.buffer.write(payload)\n"
        + "sys.stdout.buffer.flush()\n";
    write_executable_script(&script_path, &script);

    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());
    let _hex = EnvGuard::set("BINARY_RESPONSE_HEX", OsStr::new(&expected_hex));
    let _args = EnvGuard::set("ARGS_LOG", args_log_path.as_os_str());

    let port = allocate_test_port();

    let log_path = temp.path().join("daemon.log");
    let pid_path = temp.path().join("daemon.pid");
    let lock_path = temp.path().join("daemon.lock");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--config"),
            config_path.clone().into_os_string(),
            OsString::from("--log-file"),
            log_path.clone().into_os_string(),
            OsString::from("--pid-file"),
            pid_path.clone().into_os_string(),
            OsString::from("--lock-file"),
            lock_path.clone().into_os_string(),
            OsString::from("--bwlimit"),
            OsString::from("96"),
            OsString::from("--ipv4"),
            OsString::from("--once"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    stream
        .write_all(&u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes())
        .expect("send handshake");
    stream.flush().expect("flush handshake");

    let mut response = Vec::new();
    reader.read_to_end(&mut response).expect("read response");
    assert_eq!(response, expected);

    handle.join().expect("daemon thread").expect("daemon run");

    let recorded = fs::read_to_string(&args_log_path).expect("read args log");
    assert!(recorded.contains("--port"));
    assert!(recorded.contains(&port.to_string()));
    assert!(recorded.contains("--config"));
    assert!(recorded.contains(config_path.to_str().expect("utf8 config")));
    assert!(recorded.contains("--log-file"));
    assert!(recorded.contains(log_path.to_str().expect("utf8 log")));
    assert!(recorded.contains("--pid-file"));
    assert!(recorded.contains(pid_path.to_str().expect("utf8 pid")));
    assert!(recorded.contains("--lock-file"));
    assert!(recorded.contains(lock_path.to_str().expect("utf8 lock")));
    assert!(recorded.contains("--bwlimit"));
    assert!(recorded.contains("96"));
    assert!(recorded.contains("--ipv4"));
}

#[cfg(unix)]
#[test]
fn delegate_system_rsync_fallback_env_triggers_delegation() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _fallback = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

    let (code, _stdout, stderr) = run_with_args([
        OsStr::new(RSYNCD),
        OsStr::new("--config"),
        OsStr::new(branding::OC_DAEMON_CONFIG_PATH),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let recorded = fs::read_to_string(&log_path).expect("read invocation log");
    assert!(recorded.contains("--daemon"));
    assert!(recorded.contains(&format!("--config {}", branding::OC_DAEMON_CONFIG_PATH)));
}

#[cfg(unix)]
#[test]
fn delegate_system_rsync_daemon_fallback_env_triggers_delegation() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());

    let (code, _stdout, stderr) = run_with_args([
        OsStr::new(RSYNCD),
        OsStr::new("--config"),
        OsStr::new(branding::OC_DAEMON_CONFIG_PATH),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let recorded = fs::read_to_string(&log_path).expect("read invocation log");
    assert!(recorded.contains("--daemon"));
    assert!(recorded.contains(&format!("--config {}", branding::OC_DAEMON_CONFIG_PATH)));
}

#[cfg(unix)]
#[test]
fn delegate_system_rsync_env_false_skips_fallback() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho invoked > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());
    let _auto = EnvGuard::set(DAEMON_AUTO_DELEGATE_ENV, OsStr::new("0"));

    let (code, stdout, _stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(!stdout.is_empty());
    assert!(!log_path.exists());
}

#[test]
fn module_peer_hostname_uses_override() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    set_test_hostname_override(peer, Some("Trusted.Example.Com"));
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    assert_eq!(resolved, Some("trusted.example.com"));
    assert!(module.permits(peer, resolved));
    clear_test_hostname_overrides();
}

#[test]
fn module_peer_hostname_missing_resolution_denies_hostname_only_rules() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, true);
    if let Some(host) = resolved {
        assert_ne!(host, "trusted.example.com");
    }
    assert!(!module.permits(peer, resolved));
}

#[test]
fn module_peer_hostname_skips_lookup_when_disabled() {
    clear_test_hostname_overrides();
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    set_test_hostname_override(peer, Some("trusted.example.com"));
    let mut cache = None;
    let resolved = module_peer_hostname(&module, &mut cache, peer, false);
    assert!(resolved.is_none());
    assert!(!module.permits(peer, resolved));
    clear_test_hostname_overrides();
}

#[test]
fn connection_limiter_enforces_limits_across_guards() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    let limiter = Arc::new(ConnectionLimiter::open(lock_path).expect("open lock file"));
    let limit = NonZeroU32::new(2).expect("non-zero");

    let first = limiter
        .acquire("docs", limit)
        .expect("first connection allowed");
    let second = limiter
        .acquire("docs", limit)
        .expect("second connection allowed");
    assert!(matches!(
        limiter.acquire("docs", limit),
        Err(ModuleConnectionError::Limit(l)) if l == limit
    ));

    drop(second);
    let third = limiter
        .acquire("docs", limit)
        .expect("slot released after guard drop");

    drop(third);
    drop(first);
    assert!(limiter.acquire("docs", limit).is_ok());
}

#[test]
fn connection_limiter_open_preserves_existing_counts() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    fs::write(&lock_path, b"docs 1\nother 2\n").expect("seed lock file");

    let limiter = ConnectionLimiter::open(lock_path.clone()).expect("open lock file");
    drop(limiter);

    let contents = fs::read_to_string(&lock_path).expect("read lock file");
    assert_eq!(contents, "docs 1\nother 2\n");
}

#[test]
fn connection_limiter_propagates_io_errors() {
    let temp = tempdir().expect("lock dir");
    let lock_path = temp.path().join("daemon.lock");
    let limiter = Arc::new(ConnectionLimiter::open(lock_path.clone()).expect("open lock"));

    fs::remove_file(&lock_path).expect("remove original lock file");
    fs::create_dir(&lock_path).expect("replace lock file with directory");

    match limiter.acquire("docs", NonZeroU32::new(1).unwrap()) {
        Err(ModuleConnectionError::Io(_)) => {}
        Err(other) => panic!("expected io error, got {other:?}"),
        Ok(_) => panic!("expected io error, got success"),
    }
}

#[test]
fn builder_collects_arguments() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            OsString::from("/tmp/rsyncd.conf"),
        ])
        .build();

    assert_eq!(
        config.arguments(),
        &[
            OsString::from("--config"),
            OsString::from("/tmp/rsyncd.conf")
        ]
    );
    assert!(config.has_runtime_request());
    assert_eq!(config.brand(), Brand::Oc);
}

#[test]
fn builder_allows_brand_override() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .brand(Brand::Upstream)
        .arguments([OsString::from("--daemon")])
        .build();

    assert_eq!(config.brand(), Brand::Upstream);
    assert_eq!(config.arguments(), &[OsString::from("--daemon")]);
}

#[test]
fn runtime_options_parse_module_definitions() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs,Documentation"),
        OsString::from("--module"),
        OsString::from("logs=/var/log"),
    ])
    .expect("parse modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].name, "docs");
    assert_eq!(modules[0].path, PathBuf::from("/srv/docs"));
    assert_eq!(modules[0].comment.as_deref(), Some("Documentation"));
    assert!(modules[0].bandwidth_limit().is_none());
    assert!(modules[0].bandwidth_burst().is_none());
    assert!(modules[0].refused_options().is_empty());
    assert!(modules[0].read_only());
    assert_eq!(modules[1].name, "logs");
    assert_eq!(modules[1].path, PathBuf::from("/var/log"));
    assert!(modules[1].comment.is_none());
    assert!(modules[1].bandwidth_limit().is_none());
    assert!(modules[1].bandwidth_burst().is_none());
    assert!(modules[1].refused_options().is_empty());
    assert!(modules[1].read_only());
}

#[test]
fn runtime_options_module_definition_supports_escaped_commas() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs\\,archive,Project\\, Docs"),
    ])
    .expect("parse modules with escapes");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "docs");
    assert_eq!(modules[0].path, PathBuf::from("/srv/docs,archive"));
    assert_eq!(modules[0].comment.as_deref(), Some("Project, Docs"));
    assert!(modules[0].read_only());
}

#[test]
fn runtime_options_module_definition_preserves_escaped_backslash() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("logs=/var/log\\\\files,Log share"),
    ])
    .expect("parse modules with escaped backslash");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].path, PathBuf::from("/var/log\\files"));
    assert_eq!(modules[0].comment.as_deref(), Some("Log share"));
    assert!(modules[0].read_only());
}

#[test]
fn runtime_options_module_definition_parses_inline_options() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from(
            "mirror=./data;use-chroot=no;read-only=yes;list=no;numeric-ids=yes;hosts-allow=192.0.2.0/24;auth-users=alice,bob;secrets-file=/etc/oc-rsyncd/oc-rsyncd.secrets;bwlimit=1m;refuse-options=compress;uid=1000;gid=2000;timeout=600;max-connections=5",
        ),
    ])
    .expect("parse module with inline options");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name(), "mirror");
    assert_eq!(module.path, PathBuf::from("./data"));
    assert!(module.read_only());
    assert!(!module.listable());
    assert!(module.numeric_ids());
    assert!(!module.use_chroot());
    assert!(module.permits(
        IpAddr::V4(Ipv4Addr::new(192, 0, 2, 42)),
        Some("host.example")
    ));
    assert_eq!(
        module.auth_users(),
        &[String::from("alice"), String::from("bob")]
    );
    assert_eq!(
        module
            .secrets_file()
            .map(|path| path.to_string_lossy().into_owned()),
        Some(String::from(branding::OC_DAEMON_SECRETS_PATH))
    );
    assert_eq!(
        module.bandwidth_limit().map(NonZeroU64::get),
        Some(1_048_576)
    );
    assert!(module.bandwidth_burst().is_none());
    assert!(!module.bandwidth_burst_specified());
    assert_eq!(module.refused_options(), [String::from("compress")]);
    assert_eq!(module.uid(), Some(1000));
    assert_eq!(module.gid(), Some(2000));
    assert_eq!(module.timeout().map(NonZeroU64::get), Some(600));
    assert_eq!(module.max_connections().map(NonZeroU32::get), Some(5));
}

#[test]
fn runtime_options_module_definition_parses_inline_bwlimit_burst() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("mirror=./data;use-chroot=no;bwlimit=2m:8m"),
    ])
    .expect("parse module with inline bwlimit burst");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(
        module.bandwidth_limit().map(NonZeroU64::get),
        Some(2_097_152)
    );
    assert_eq!(
        module.bandwidth_burst().map(NonZeroU64::get),
        Some(8_388_608)
    );
    assert!(module.bandwidth_burst_specified());
}

#[test]
fn runtime_options_module_definition_rejects_unknown_inline_option() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs;unknown=true"),
    ])
    .expect_err("unknown option should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("unsupported module option")
    );
}

#[test]
fn runtime_options_module_definition_requires_secrets_for_inline_auth_users() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("logs=/var/log;auth-users=alice"),
    ])
    .expect_err("missing secrets file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("did not supply a secrets file")
    );
}

#[test]
fn runtime_options_module_definition_rejects_duplicate_inline_option() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--module"),
        OsString::from("docs=/srv/docs;read-only=yes;read-only=no"),
    ])
    .expect_err("duplicate inline option should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate module option")
    );
}

