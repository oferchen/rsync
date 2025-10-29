#[test]
fn sanitize_module_identifier_preserves_clean_input() {
    let ident = "secure-module";
    match sanitize_module_identifier(ident) {
        Cow::Borrowed(value) => assert_eq!(value, ident),
        Cow::Owned(_) => panic!("clean identifiers should not allocate"),
    }
}

#[test]
fn sanitize_module_identifier_replaces_control_characters() {
    let ident = "bad\nname\t";
    let sanitized = sanitize_module_identifier(ident);
    assert_eq!(sanitized.as_ref(), "bad?name?");
}

#[test]
fn format_bandwidth_rate_prefers_largest_whole_unit() {
    let cases = [
        (NonZeroU64::new(512).unwrap(), "512 bytes/s"),
        (NonZeroU64::new(1024).unwrap(), "1 KiB/s"),
        (NonZeroU64::new(8 * 1024).unwrap(), "8 KiB/s"),
        (NonZeroU64::new(1024 * 1024).unwrap(), "1 MiB/s"),
        (NonZeroU64::new(1024 * 1024 * 1024).unwrap(), "1 GiB/s"),
        (NonZeroU64::new(1024u64.pow(4)).unwrap(), "1 TiB/s"),
        (NonZeroU64::new(2 * 1024u64.pow(5)).unwrap(), "2 PiB/s"),
    ];

    for (input, expected) in cases {
        assert_eq!(format_bandwidth_rate(input), expected);
    }
}

#[test]
fn connection_status_messages_describe_active_sessions() {
    assert_eq!(format_connection_status(0), "Idle; waiting for connections");
    assert_eq!(format_connection_status(1), "Serving 1 connection");
    assert_eq!(format_connection_status(3), "Serving 3 connections");
}

#[allow(unsafe_code)]
impl EnvGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

#[allow(unsafe_code)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            unsafe {
                std::env::set_var(self.key, value);
            }
        } else {
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[test]
fn first_existing_config_path_prefers_primary_candidate() {
    let dir = tempdir().expect("tempdir");
    let primary = dir.path().join("primary.conf");
    let legacy = dir.path().join("legacy.conf");
    fs::write(&primary, "# primary").expect("write primary");
    fs::write(&legacy, "# legacy").expect("write legacy");

    let expected = primary.as_os_str().to_os_string();
    let result = first_existing_config_path([primary.as_path(), legacy.as_path()]);

    assert_eq!(result, Some(expected));
}

#[test]
fn first_existing_config_path_falls_back_to_legacy_candidate() {
    let dir = tempdir().expect("tempdir");
    let legacy = dir.path().join("legacy.conf");
    fs::write(&legacy, "# legacy").expect("write legacy");

    let missing = dir.path().join("missing.conf");
    let expected = legacy.as_os_str().to_os_string();
    let result = first_existing_config_path([missing.as_path(), legacy.as_path()]);

    assert_eq!(result, Some(expected));
}

#[test]
fn first_existing_config_path_returns_none_when_absent() {
    let dir = tempdir().expect("tempdir");
    let missing_primary = dir.path().join("missing-primary.conf");
    let missing_legacy = dir.path().join("missing-legacy.conf");
    let result = first_existing_config_path([missing_primary.as_path(), missing_legacy.as_path()]);

    assert!(result.is_none());
}

#[test]
fn default_secrets_path_prefers_primary_candidate() {
    let dir = tempdir().expect("tempdir");
    let primary = dir.path().join("primary.secrets");
    let fallback = dir.path().join("fallback.secrets");
    fs::write(&primary, "alice:password\n").expect("write primary");
    fs::write(&fallback, "bob:password\n").expect("write fallback");

    let result = with_test_secrets_candidates(vec![primary.clone(), fallback.clone()], || {
        default_secrets_path_if_present(Brand::Oc)
    });

    assert_eq!(result, Some(primary.into_os_string()));
}

#[test]
fn default_secrets_path_falls_back_to_secondary_candidate() {
    let dir = tempdir().expect("tempdir");
    let fallback = dir.path().join("fallback.secrets");
    fs::write(&fallback, "bob:password\n").expect("write fallback");

    let missing = dir.path().join("missing.secrets");
    let result = with_test_secrets_candidates(vec![missing, fallback.clone()], || {
        default_secrets_path_if_present(Brand::Oc)
    });

    assert_eq!(result, Some(fallback.into_os_string()));
}

#[test]
fn default_secrets_path_returns_none_when_absent() {
    let dir = tempdir().expect("tempdir");
    let primary = dir.path().join("missing-primary.secrets");
    let secondary = dir.path().join("missing-secondary.secrets");

    let result = with_test_secrets_candidates(vec![primary, secondary], || {
        default_secrets_path_if_present(Brand::Oc)
    });

    assert!(result.is_none());
}

#[test]
fn default_config_candidates_prefer_oc_branding() {
    assert_eq!(
        Brand::Oc.config_path_candidate_strs(),
        [
            branding::OC_DAEMON_CONFIG_PATH,
            branding::LEGACY_DAEMON_CONFIG_PATH,
        ]
    );
}

#[test]
fn default_config_candidates_prefer_legacy_for_upstream_brand() {
    assert_eq!(
        Brand::Upstream.config_path_candidate_strs(),
        [
            branding::LEGACY_DAEMON_CONFIG_PATH,
            branding::OC_DAEMON_CONFIG_PATH,
        ]
    );
}

#[test]
fn configured_fallback_binary_defaults_to_rsync() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::remove(DAEMON_FALLBACK_ENV);
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert_eq!(configured_fallback_binary(), Some(OsString::from("rsync")));
}

#[test]
fn configured_fallback_binary_respects_primary_disable() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert!(configured_fallback_binary().is_none());
}

#[test]
fn configured_fallback_binary_respects_secondary_disable() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::remove(DAEMON_FALLBACK_ENV);
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("no"));
    assert!(configured_fallback_binary().is_none());
}

#[test]
fn configured_fallback_binary_supports_auto_value() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("auto"));
    let _secondary = EnvGuard::remove(CLIENT_FALLBACK_ENV);
    assert_eq!(configured_fallback_binary(), Some(OsString::from("rsync")));
}

#[cfg(unix)]
fn write_executable_script(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write script");
    let mut permissions = std::fs::metadata(path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("set script permissions");
}

#[test]
fn advertised_capability_lines_empty_without_modules() {
    assert!(advertised_capability_lines(&[]).is_empty());
}

#[test]
fn advertised_capability_lines_report_modules_without_auth() {
    let module = ModuleRuntime::from(base_module("docs"));

    assert_eq!(
        advertised_capability_lines(&[module]),
        vec![String::from("modules")]
    );
}

#[test]
fn advertised_capability_lines_include_authlist_when_required() {
    let mut definition = base_module("secure");
    definition.auth_users.push(String::from("alice"));
    definition.secrets_file = Some(PathBuf::from("secrets.txt"));
    let module = ModuleRuntime::from(definition);

    assert_eq!(
        advertised_capability_lines(&[module]),
        vec![String::from("modules authlist")]
    );
}

fn module_with_host_patterns(allow: &[&str], deny: &[&str]) -> ModuleDefinition {
    ModuleDefinition {
        name: String::from("module"),
        path: PathBuf::from("/srv/module"),
        comment: None,
        hosts_allow: allow
            .iter()
            .map(|pattern| HostPattern::parse(pattern).expect("parse allow pattern"))
            .collect(),
        hosts_deny: deny
            .iter()
            .map(|pattern| HostPattern::parse(pattern).expect("parse deny pattern"))
            .collect(),
        auth_users: Vec::new(),
        secrets_file: None,
        bandwidth_limit: None,
        bandwidth_limit_specified: false,
        bandwidth_burst: None,
        bandwidth_burst_specified: false,
        bandwidth_limit_configured: false,
        refuse_options: Vec::new(),
        read_only: true,
        numeric_ids: false,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        max_connections: None,
    }
}

fn run_with_args<I, S>(args: I) -> (i32, Vec<u8>, Vec<u8>)
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run(args, &mut stdout, &mut stderr);
    (code, stdout, stderr)
}

#[test]
fn module_definition_hostname_allow_matches_exact() {
    let module = module_with_host_patterns(&["trusted.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("trusted.example.com")));
    assert!(!module.permits(peer, Some("other.example.com")));
    assert!(!module.permits(peer, None));
}

#[test]
fn module_definition_hostname_suffix_matches() {
    let module = module_with_host_patterns(&[".example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("node.example.com")));
    assert!(module.permits(peer, Some("example.com")));
    assert!(!module.permits(peer, Some("example.net")));
    assert!(!module.permits(peer, Some("sampleexample.com")));
}

#[test]
fn module_definition_hostname_wildcard_matches() {
    let module = module_with_host_patterns(&["build?.example.*"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("build1.example.net")));
    assert!(module.permits(peer, Some("builda.example.org")));
    assert!(!module.permits(peer, Some("build12.example.net")));
}

#[test]
fn module_definition_hostname_deny_takes_precedence() {
    let module = module_with_host_patterns(&["*"], &["bad.example.com"]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(!module.permits(peer, Some("bad.example.com")));
    assert!(module.permits(peer, Some("good.example.com")));
}

#[test]
fn module_definition_hostname_wildcard_handles_multiple_asterisks() {
    let module = module_with_host_patterns(&["*build*node*.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("fastbuild-node1.example.com")));
    assert!(module.permits(peer, Some("build-node.example.com")));
    assert!(!module.permits(peer, Some("build.example.org")));
}

#[test]
fn module_definition_hostname_wildcard_treats_question_as_single_character() {
    let module = module_with_host_patterns(&["app??.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("app12.example.com")));
    assert!(!module.permits(peer, Some("app1.example.com")));
    assert!(!module.permits(peer, Some("app123.example.com")));
}

#[test]
fn module_definition_hostname_wildcard_collapses_consecutive_asterisks() {
    let module = module_with_host_patterns(&["**.example.com"], &[]);
    let peer = IpAddr::V4(Ipv4Addr::LOCALHOST);
    assert!(module.permits(peer, Some("node.example.com")));
    assert!(!module.permits(peer, Some("node.example.org")));
}

#[cfg(unix)]
#[test]
fn delegate_system_rsync_invokes_fallback_binary() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _guard = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());

    let (code, _stdout, stderr) = run_with_args([
        OsStr::new(RSYNCD),
        OsStr::new("--delegate-system-rsync"),
        OsStr::new("--config"),
        OsStr::new(branding::OC_DAEMON_CONFIG_PATH),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let recorded = fs::read_to_string(&log_path).expect("read invocation log");
    assert!(recorded.contains("--daemon"));
    assert!(recorded.contains("--no-detach"));
    assert!(recorded.contains(&format!("--config {}", branding::OC_DAEMON_CONFIG_PATH)));
}

#[cfg(unix)]
#[test]
fn delegate_system_rsync_propagates_exit_code() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    write_executable_script(&script_path, "#!/bin/sh\nexit 7\n");
    let _guard = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());

    let (code, _stdout, stderr) =
        run_with_args([OsStr::new(RSYNCD), OsStr::new("--delegate-system-rsync")]);

    assert_eq!(code, 7);
    let stderr_str = String::from_utf8_lossy(&stderr);
    assert!(stderr_str.contains("system rsync daemon exited"));
}

#[cfg(unix)]
#[test]
fn delegate_system_rsync_falls_back_to_client_override() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

    let (code, _stdout, stderr) = run_with_args([
        OsStr::new(RSYNCD),
        OsStr::new("--delegate-system-rsync"),
        OsStr::new("--port"),
        OsStr::new("1234"),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let recorded = fs::read_to_string(&log_path).expect("read invocation log");
    assert!(recorded.contains("--port 1234"));
}

#[cfg(unix)]
#[test]
fn delegate_system_rsync_env_triggers_fallback() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("rsync-wrapper.sh");
    let log_path = temp.path().join("invocation.log");
    let script = format!("#!/bin/sh\necho \"$@\" > {}\nexit 0\n", log_path.display());
    write_executable_script(&script_path, &script);
    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());
    let _auto = EnvGuard::set(DAEMON_AUTO_DELEGATE_ENV, OsStr::new("1"));

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
fn binary_session_delegates_to_configured_fallback() {
    use std::io::BufReader;

    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().expect("tempdir");

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

    let script_path = temp.path().join("binary-fallback.py");
    let marker_path = temp.path().join("fallback.marker");
    let script = "#!/usr/bin/env python3\n".to_string()
        + "import os, sys, binascii\n"
        + "marker = os.environ.get('FALLBACK_MARKER')\n"
        + "if marker:\n"
        + "    with open(marker, 'w', encoding='utf-8') as handle:\n"
        + "        handle.write('delegated')\n"
        + "sys.stdin.buffer.read(4)\n"
        + "payload = binascii.unhexlify(os.environ['BINARY_RESPONSE_HEX'])\n"
        + "sys.stdout.buffer.write(payload)\n"
        + "sys.stdout.buffer.flush()\n";
    write_executable_script(&script_path, &script);

    let _fallback = EnvGuard::set(DAEMON_FALLBACK_ENV, script_path.as_os_str());
    let _marker = EnvGuard::set("FALLBACK_MARKER", marker_path.as_os_str());
    let _hex = EnvGuard::set("BINARY_RESPONSE_HEX", OsStr::new(&expected_hex));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
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
    assert!(marker_path.exists());

    handle.join().expect("daemon thread").expect("daemon run");
}

