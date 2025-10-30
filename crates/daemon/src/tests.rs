use super::*;
use rsync_core::fallback::DAEMON_AUTO_DELEGATE_ENV;
use rsync_core::version::VersionInfoReport;
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use tempfile::{NamedTempFile, tempdir};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const RSYNCD: &str = branding::daemon_program_name();
const OC_RSYNC_D: &str = branding::oc_daemon_program_name();

fn base_module(name: &str) -> ModuleDefinition {
    ModuleDefinition {
        name: String::from(name),
        path: PathBuf::from("/srv/module"),
        comment: None,
        hosts_allow: Vec::new(),
        hosts_deny: Vec::new(),
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

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_test_secrets_candidates<F, R>(candidates: Vec<PathBuf>, func: F) -> R
where
    F: FnOnce() -> R,
{
    TEST_SECRETS_CANDIDATES.with(|cell| {
        let previous = cell.replace(Some(candidates));
        let result = func();
        cell.replace(previous);
        result
    })
}

fn with_test_secrets_env<F, R>(override_value: Option<TestSecretsEnvOverride>, func: F) -> R
where
    F: FnOnce() -> R,
{
    TEST_SECRETS_ENV.with(|cell| {
        let previous = cell.replace(override_value);
        let result = func();
        cell.replace(previous);
        result
    })
}

fn allocate_test_port() -> u16 {
    const START: u16 = 40_000;
    const RANGE: u32 = 20_000;
    const STATE_SIZE: u64 = 4;

    let mut path = std::env::temp_dir();
    path.push("rsync-daemon-test-port.lock");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .expect("open port allocator state");

    file.lock_exclusive().expect("lock port allocator state");
    file.seek(SeekFrom::Start(0))
        .expect("rewind port allocator state");

    let mut counter_bytes = [0u8; STATE_SIZE as usize];
    let read = file
        .read(&mut counter_bytes)
        .expect("read port allocator state");
    let mut counter = if read == counter_bytes.len() {
        u32::from_le_bytes(counter_bytes)
    } else {
        0
    };

    for _ in 0..RANGE {
        let offset = (counter % RANGE) as u16;
        counter = counter.wrapping_add(1);

        file.seek(SeekFrom::Start(0))
            .expect("rewind port allocator state");
        file.write_all(&counter.to_le_bytes())
            .expect("persist port allocator state");
        file.set_len(STATE_SIZE)
            .expect("truncate port allocator state");
        file.flush().expect("flush port allocator state");

        let candidate = START + offset;
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, candidate)) {
            drop(listener);
            return candidate;
        }
    }

    panic!("failed to allocate a free test port");
}

#[test]
fn parse_auth_user_list_trims_and_deduplicates_case_insensitively() {
    let users = parse_auth_user_list(" alice,BOB, alice ,  Carol ")
        .expect("parse non-empty user list");
    assert_eq!(users, ["alice", "BOB", "Carol"]);

    let err = parse_auth_user_list(" , ,  ").expect_err("blank list rejected");
    assert_eq!(err, "must specify at least one username");
}

#[test]
fn parse_refuse_option_list_normalises_and_deduplicates() {
    let options = parse_refuse_option_list("delete, ICONV ,compress, delete")
        .expect("parse refuse option list");
    assert_eq!(options, ["delete", "iconv", "compress"]);

    let err = parse_refuse_option_list("   ,").expect_err("blank option list rejected");
    assert_eq!(err, "must specify at least one option");
}

#[test]
fn parse_boolean_directive_interprets_common_forms() {
    for value in ["1", "true", "YES", " On "] {
        assert_eq!(parse_boolean_directive(value), Some(true));
    }

    for value in ["0", "false", "No", " off "] {
        assert_eq!(parse_boolean_directive(value), Some(false));
    }

    assert_eq!(parse_boolean_directive("maybe"), None);
}

#[test]
fn parse_numeric_identifier_rejects_blank_or_invalid_input() {
    assert_eq!(parse_numeric_identifier("  42  "), Some(42));
    assert_eq!(parse_numeric_identifier(""), None);
    assert_eq!(parse_numeric_identifier("not-a-number"), None);
}

#[test]
fn parse_timeout_seconds_supports_zero_and_non_zero_values() {
    assert_eq!(parse_timeout_seconds(""), None);
    assert_eq!(parse_timeout_seconds("  "), None);
    assert_eq!(parse_timeout_seconds("0"), Some(None));

    let expected = NonZeroU64::new(30).expect("non-zero timeout");
    assert_eq!(parse_timeout_seconds("30"), Some(Some(expected)));
    assert_eq!(parse_timeout_seconds("invalid"), None);
}

#[test]
fn parse_max_connections_directive_handles_zero_and_positive() {
    assert_eq!(parse_max_connections_directive(""), None);
    assert_eq!(parse_max_connections_directive("  "), None);
    assert_eq!(parse_max_connections_directive("0"), Some(None));

    let expected = NonZeroU32::new(25).expect("non-zero");
    assert_eq!(
        parse_max_connections_directive("25"),
        Some(Some(expected))
    );

    assert_eq!(parse_max_connections_directive("-1"), None);
    assert_eq!(parse_max_connections_directive("invalid"), None);
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

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

#[test]
fn runtime_options_parse_pid_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        OsString::from("/var/run/rsyncd.pid"),
    ])
    .expect("parse pid file argument");

    assert_eq!(options.pid_file(), Some(Path::new("/var/run/rsyncd.pid")));
}

#[test]
fn runtime_options_reject_duplicate_pid_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        OsString::from("/var/run/one.pid"),
        OsString::from("--pid-file"),
        OsString::from("/var/run/two.pid"),
    ])
    .expect_err("duplicate pid file should fail");

    assert!(error.message().to_string().contains("--pid-file"));
}

#[test]
fn runtime_options_ipv6_sets_default_bind_address() {
    let options =
        RuntimeOptions::parse(&[OsString::from("--ipv6")]).expect("parse --ipv6 succeeds");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

#[test]
fn runtime_options_ipv6_accepts_ipv6_bind_address() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("::1"),
        OsString::from("--ipv6"),
    ])
    .expect("ipv6 bind succeeds");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

#[test]
fn runtime_options_bind_accepts_bracketed_ipv6() {
    let options = RuntimeOptions::parse(&[OsString::from("--bind"), OsString::from("[::1]")])
        .expect("parse bracketed ipv6");

    assert_eq!(options.bind_address(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    assert_eq!(options.address_family(), Some(AddressFamily::Ipv6));
}

#[test]
fn runtime_options_bind_resolves_hostnames() {
    let options = RuntimeOptions::parse(&[OsString::from("--bind"), OsString::from("localhost")])
        .expect("parse hostname bind");

    let address = options.bind_address();
    assert!(
        address == IpAddr::V4(Ipv4Addr::LOCALHOST) || address == IpAddr::V6(Ipv6Addr::LOCALHOST),
        "unexpected resolved address {address}",
    );
}

#[test]
fn runtime_options_ipv6_rejects_ipv4_bind_address() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("127.0.0.1"),
        OsString::from("--ipv6"),
    ])
    .expect_err("ipv4 bind with --ipv6 should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot use --ipv6 with an IPv4 bind address")
    );
}

#[test]
fn runtime_options_ipv4_rejects_ipv6_bind_address() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bind"),
        OsString::from("::1"),
        OsString::from("--ipv4"),
    ])
    .expect_err("ipv6 bind with --ipv4 should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot use --ipv4 with an IPv6 bind address")
    );
}

#[test]
fn runtime_options_rejects_ipv4_ipv6_combo() {
    let error = RuntimeOptions::parse(&[OsString::from("--ipv4"), OsString::from("--ipv6")])
        .expect_err("conflicting address families should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("cannot combine --ipv4 with --ipv6")
    );
}

#[test]
fn runtime_options_load_modules_from_config_file() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\ncomment = Documentation\n\n[logs]\npath=/var/log\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].name, "docs");
    assert_eq!(modules[0].path, PathBuf::from("/srv/docs"));
    assert_eq!(modules[0].comment.as_deref(), Some("Documentation"));
    assert!(modules[0].bandwidth_limit().is_none());
    assert!(modules[0].bandwidth_burst().is_none());
    assert!(modules[0].listable());
    assert_eq!(modules[1].name, "logs");
    assert_eq!(modules[1].path, PathBuf::from("/var/log"));
    assert!(modules[1].comment.is_none());
    assert!(modules[1].bandwidth_limit().is_none());
    assert!(modules[1].bandwidth_burst().is_none());
    assert!(modules[1].listable());
    assert!(modules.iter().all(ModuleDefinition::use_chroot));
}

#[test]
fn runtime_options_loads_pid_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "pid file = daemon.pid\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with pid file");

    let expected = dir.path().join("daemon.pid");
    assert_eq!(options.pid_file(), Some(expected.as_path()));
}

#[test]
fn runtime_options_config_pid_file_respects_cli_override() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "pid file = config.pid\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let cli_pid = PathBuf::from("/var/run/override.pid");
    let options = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        cli_pid.as_os_str().to_os_string(),
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with cli override");

    assert_eq!(options.pid_file(), Some(cli_pid.as_path()));
}

#[test]
fn runtime_options_loads_lock_file_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "lock file = daemon.lock\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with lock file");

    let expected = dir.path().join("daemon.lock");
    assert_eq!(options.lock_file(), Some(expected.as_path()));
}

#[test]
fn runtime_options_config_lock_file_respects_cli_override() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&config_path).expect("create config"),
        "lock file = config.lock\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let cli_lock = PathBuf::from("/var/run/override.lock");
    let options = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        cli_lock.as_os_str().to_os_string(),
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with cli lock override");

    assert_eq!(options.lock_file(), Some(cli_lock.as_path()));
}

#[test]
fn runtime_options_loads_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = 4M\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(
        module.bandwidth_limit(),
        Some(NonZeroU64::new(4 * 1024 * 1024).unwrap())
    );
    assert!(module.bandwidth_burst().is_none());
    assert!(!module.bandwidth_burst_specified());
}

#[test]
fn runtime_options_loads_bwlimit_burst_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = 4M:16M\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(
        module.bandwidth_limit(),
        Some(NonZeroU64::new(4 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        module.bandwidth_burst(),
        Some(NonZeroU64::new(16 * 1024 * 1024).unwrap())
    );
    assert!(module.bandwidth_burst_specified());
}

#[test]
fn runtime_options_loads_global_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 3M:12M\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(3 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(12 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_limit_configured());

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert!(modules[0].bandwidth_limit().is_none());
}

#[test]
fn runtime_options_global_bwlimit_respects_cli_override() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 3M\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--bwlimit"),
        OsString::from("8M:32M"),
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config with cli override");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(32 * 1024 * 1024).unwrap())
    );
}

#[test]
fn runtime_options_loads_unlimited_global_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 0\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_loads_refuse_options_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nrefuse options = delete, compress progress\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(
        module.refused_options(),
        &["delete", "compress", "progress"]
    );
}

#[test]
fn runtime_options_loads_boolean_and_id_directives_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nread only = yes\nnumeric ids = on\nuid = 1234\ngid = 4321\nlist = no\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert!(module.read_only());
    assert!(module.numeric_ids());
    assert_eq!(module.uid(), Some(1234));
    assert_eq!(module.gid(), Some(4321));
    assert!(!module.listable());
    assert!(module.use_chroot());
}

#[test]
fn runtime_options_loads_use_chroot_directive_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nuse chroot = no\n",).expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert!(!modules[0].use_chroot());
}

#[test]
fn runtime_options_allows_relative_path_when_use_chroot_disabled() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = no\n",).expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].path, PathBuf::from("data/docs"));
    assert!(!modules[0].use_chroot());
}

#[test]
fn runtime_options_loads_timeout_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = 120\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.timeout().map(NonZeroU64::get), Some(120));
}

#[test]
fn runtime_options_allows_timeout_zero_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = 0\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert!(module.timeout().is_none());
}

#[test]
fn runtime_options_rejects_invalid_boolean_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nread only = maybe\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid boolean should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid boolean value 'maybe'")
    );
}

#[test]
fn runtime_options_rejects_duplicate_use_chroot_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nuse chroot = yes\nuse chroot = no\n",
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate directive should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'use chroot' directive")
    );
}

#[test]
fn runtime_options_rejects_relative_path_with_chroot_enabled() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = yes\n",).expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("relative path with chroot should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("requires an absolute path when 'use chroot' is enabled")
    );
}

#[test]
fn runtime_options_rejects_invalid_list_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nlist = maybe\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid list boolean should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid boolean value 'maybe' for 'list'")
    );
}

#[test]
fn runtime_options_apply_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "refuse options = compress, delete\n[docs]\npath = /srv/docs\n[logs]\npath = /srv/logs\nrefuse options = stats\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config with global refuse options");

    assert_eq!(
        options.modules()[0].refused_options(),
        ["compress", "delete"]
    );
    assert_eq!(options.modules()[1].refused_options(), ["stats"]);
}

#[test]
fn runtime_options_cli_modules_inherit_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "refuse options = compress\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from("extra=/srv/extra"),
    ])
    .expect("parse config with cli module");

    assert_eq!(options.modules()[0].refused_options(), ["compress"]);
}

#[test]
fn runtime_options_loads_modules_from_included_config() {
    let dir = tempdir().expect("tempdir");
    let include_path = dir.path().join("modules.conf");
    writeln!(
        File::create(&include_path).expect("create include"),
        "[docs]\npath = /srv/docs\n"
    )
    .expect("write include");

    let main_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&main_path).expect("create config"),
        "include = modules.conf\n"
    )
    .expect("write main config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        main_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with include");

    assert_eq!(options.modules().len(), 1);
    assert_eq!(options.modules()[0].name(), "docs");
}

#[test]
fn parse_config_modules_detects_recursive_include() {
    let dir = tempdir().expect("tempdir");
    let first = dir.path().join("first.conf");
    let second = dir.path().join("second.conf");

    writeln!(
        File::create(&first).expect("create first"),
        "include = second.conf\n"
    )
    .expect("write first");
    writeln!(
        File::create(&second).expect("create second"),
        "include = first.conf\n"
    )
    .expect("write second");

    let error = parse_config_modules(&first).expect_err("recursive include should fail");
    assert!(error.message().to_string().contains("recursive include"));
}

#[test]
fn runtime_options_rejects_duplicate_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "refuse options = compress\nrefuse options = delete\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate global refuse options should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'refuse options' directive")
    );
}

#[test]
fn runtime_options_rejects_duplicate_global_bwlimit() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "bwlimit = 1M\nbwlimit = 2M\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate global bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'bwlimit' directive in global section")
    );
}

#[test]
fn runtime_options_rejects_invalid_uid() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nuid = alpha\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid uid should fail");

    assert!(error.message().to_string().contains("invalid uid"));
}

#[test]
fn runtime_options_rejects_invalid_timeout() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = never\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid timeout should fail");

    assert!(error.message().to_string().contains("invalid timeout"));
}

#[test]
fn runtime_options_rejects_invalid_bwlimit_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = nope\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid 'bwlimit' value 'nope'")
    );
}

#[test]
fn runtime_options_rejects_duplicate_bwlimit_in_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nbwlimit = 1M\nbwlimit = 2M\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'bwlimit' directive")
    );
}

#[test]
fn runtime_options_loads_max_connections_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = 7\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("config parses");

    assert_eq!(options.modules[0].max_connections(), NonZeroU32::new(7));
}

#[test]
fn runtime_options_loads_unlimited_max_connections_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = 0\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("config parses");

    assert!(options.modules[0].max_connections().is_none());
}

#[test]
fn runtime_options_rejects_invalid_max_connections() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = nope\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("invalid max connections should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("invalid max connections value")
    );
}

#[test]
fn runtime_options_rejects_duplicate_refuse_options_directives() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nrefuse options = delete\nrefuse options = compress\n"
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("duplicate refuse options should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate 'refuse options' directive")
    );
}

#[test]
fn runtime_options_rejects_empty_refuse_options_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nrefuse options =   \n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("empty refuse options should fail");

    let rendered = error.message().to_string();
    assert!(rendered.contains("must specify at least one option"));
}

#[test]
fn runtime_options_parse_bwlimit_argument() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8M")])
        .expect("parse bwlimit");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_parse_bwlimit_argument_with_burst() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8M:12M")])
        .expect("parse bwlimit with burst");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(12 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_reject_whitespace_wrapped_bwlimit_argument() {
    let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from(" 8M \n")])
        .expect_err("whitespace-wrapped bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("--bwlimit= 8M \n is invalid")
    );
}

#[test]
fn runtime_options_parse_bwlimit_unlimited() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("0")])
        .expect("parse unlimited");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_parse_bwlimit_unlimited_ignores_burst() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("0:512K")])
        .expect("parse unlimited with burst");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_parse_no_bwlimit_argument() {
    let options =
        RuntimeOptions::parse(&[OsString::from("--no-bwlimit")]).expect("parse no-bwlimit");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

#[test]
fn runtime_options_reject_invalid_bwlimit() {
    let error = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("foo")])
        .expect_err("invalid bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("--bwlimit=foo is invalid")
    );
}

#[test]
fn runtime_options_reject_duplicate_bwlimit() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--bwlimit"),
        OsString::from("8M"),
        OsString::from("--bwlimit"),
        OsString::from("16M"),
    ])
    .expect_err("duplicate bwlimit should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--bwlimit'")
    );
}

#[test]
fn runtime_options_parse_log_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--log-file"),
        OsString::from("/var/log/rsyncd.log"),
    ])
    .expect("parse log file argument");

    assert_eq!(
        options.log_file(),
        Some(&PathBuf::from("/var/log/rsyncd.log"))
    );
}

#[test]
fn runtime_options_reject_duplicate_log_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--log-file"),
        OsString::from("/tmp/one.log"),
        OsString::from("--log-file"),
        OsString::from("/tmp/two.log"),
    ])
    .expect_err("duplicate log file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--log-file'")
    );
}

#[test]
fn runtime_options_parse_lock_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        OsString::from("/var/run/rsyncd.lock"),
    ])
    .expect("parse lock file argument");

    assert_eq!(options.lock_file(), Some(Path::new("/var/run/rsyncd.lock")));
}

#[test]
fn runtime_options_reject_duplicate_lock_file_argument() {
    let error = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        OsString::from("/tmp/one.lock"),
        OsString::from("--lock-file"),
        OsString::from("/tmp/two.lock"),
    ])
    .expect_err("duplicate lock file should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--lock-file'")
    );
}

#[test]
fn runtime_options_parse_motd_sources() {
    let dir = tempdir().expect("motd dir");
    let motd_path = dir.path().join("motd.txt");
    fs::write(&motd_path, "Welcome to rsyncd\nSecond line\n").expect("write motd");

    let options = RuntimeOptions::parse(&[
        OsString::from("--motd-file"),
        motd_path.as_os_str().to_os_string(),
        OsString::from("--motd-line"),
        OsString::from("Trailing notice"),
    ])
    .expect("parse motd options");

    let expected = vec![
        String::from("Welcome to rsyncd"),
        String::from("Second line"),
        String::from("Trailing notice"),
    ];

    assert_eq!(options.motd_lines(), expected.as_slice());
}

#[test]
fn runtime_options_loads_motd_from_config_directives() {
    let dir = tempdir().expect("motd dir");
    let config_path = dir.path().join("rsyncd.conf");
    let motd_path = dir.path().join("motd.txt");
    fs::write(&motd_path, "First line\nSecond line\r\n").expect("write motd file");

    fs::write(
        &config_path,
        "motd file = motd.txt\nmotd = Inline note\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with motd directives");

    let expected = vec![
        String::from("First line"),
        String::from("Second line"),
        String::from("Inline note"),
    ];

    assert_eq!(options.motd_lines(), expected.as_slice());
}

#[test]
fn runtime_options_default_enables_reverse_lookup() {
    let options = RuntimeOptions::parse(&[]).expect("parse defaults");
    assert!(options.reverse_lookup());
}

#[test]
fn runtime_options_loads_config_from_branded_environment_variable() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = dir.path().join("oc-rsyncd.conf");
    fs::write(
        &config_path,
        format!("[data]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let _env = EnvGuard::set(BRANDED_CONFIG_ENV, config_path.as_os_str());
    let options = RuntimeOptions::parse(&[]).expect("parse env config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "data");
    assert_eq!(module.path, module_dir);
    assert_eq!(
        &options.delegate_arguments,
        &[
            OsString::from("--config"),
            config_path.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_loads_config_from_legacy_environment_variable() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("[legacy]\npath = {}\n", module_dir.display()),
    )
    .expect("write config");

    let _env = EnvGuard::set(LEGACY_CONFIG_ENV, config_path.as_os_str());
    let options = RuntimeOptions::parse(&[]).expect("parse env config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "legacy");
    assert_eq!(module.path, module_dir);
    assert_eq!(
        &options.delegate_arguments,
        &[
            OsString::from("--config"),
            config_path.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_branded_config_env_overrides_legacy_env() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let branded_dir = dir.path().join("branded");
    let legacy_dir = dir.path().join("legacy");
    fs::create_dir_all(&branded_dir).expect("branded module dir");
    fs::create_dir_all(&legacy_dir).expect("legacy module dir");

    let branded_config = dir.path().join("oc.conf");
    fs::write(
        &branded_config,
        format!("[branded]\npath = {}\n", branded_dir.display()),
    )
    .expect("write branded config");

    let legacy_config = dir.path().join("legacy.conf");
    fs::write(
        &legacy_config,
        format!("[legacy]\npath = {}\n", legacy_dir.display()),
    )
    .expect("write legacy config");

    let _legacy = EnvGuard::set(LEGACY_CONFIG_ENV, legacy_config.as_os_str());
    let _branded = EnvGuard::set(BRANDED_CONFIG_ENV, branded_config.as_os_str());
    let options = RuntimeOptions::parse(&[]).expect("parse env config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "branded");
    assert_eq!(module.path, branded_dir);
    assert_eq!(
        &options.delegate_arguments,
        &[
            OsString::from("--config"),
            branded_config.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_default_secrets_path_updates_delegate_arguments() {
    let dir = tempdir().expect("config dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let options =
        with_test_secrets_candidates(vec![secrets_path.clone()], || RuntimeOptions::parse(&[]))
            .expect("parse defaults with secrets override");

    assert_eq!(
        options.delegate_arguments,
        [
            OsString::from("--secrets-file"),
            secrets_path.into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_loads_secrets_from_branded_environment_variable() {
    let dir = tempdir().expect("secrets dir");
    let secrets_path = dir.path().join("branded.txt");
    fs::write(&secrets_path, "alice:secret\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: Some(secrets_path.clone().into_os_string()),
            legacy: None,
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("parse env secrets");

    assert_eq!(
        options.delegate_arguments,
        [
            OsString::from("--secrets-file"),
            secrets_path.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_loads_secrets_from_legacy_environment_variable() {
    let dir = tempdir().expect("secrets dir");
    let secrets_path = dir.path().join("legacy.txt");
    fs::write(&secrets_path, "bob:secret\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: None,
            legacy: Some(secrets_path.clone().into_os_string()),
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("parse env secrets");

    assert_eq!(
        options.delegate_arguments,
        [
            OsString::from("--secrets-file"),
            secrets_path.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_branded_secrets_env_overrides_legacy_env() {
    let dir = tempdir().expect("secrets dir");
    let branded_path = dir.path().join("branded.txt");
    let legacy_path = dir.path().join("legacy.txt");
    fs::write(&branded_path, "carol:secret\n").expect("write branded secrets");
    fs::write(&legacy_path, "dave:secret\n").expect("write legacy secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&branded_path, PermissionsExt::from_mode(0o600))
            .expect("chmod branded secrets");
        fs::set_permissions(&legacy_path, PermissionsExt::from_mode(0o600))
            .expect("chmod legacy secrets");
    }

    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: Some(branded_path.clone().into_os_string()),
            legacy: Some(legacy_path.clone().into_os_string()),
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("parse env secrets");

    let delegate = &options.delegate_arguments;
    let expected_tail = [
        OsString::from("--secrets-file"),
        branded_path.clone().into_os_string(),
    ];
    assert!(delegate.ends_with(&expected_tail));
    assert!(
        !delegate.iter().any(|arg| arg == legacy_path.as_os_str()),
        "legacy secrets path should not be forwarded"
    );
}

#[test]
fn runtime_options_rejects_missing_secrets_from_environment() {
    let missing = OsString::from("/nonexistent/secrets.txt");
    let options = with_test_secrets_env(
        Some(TestSecretsEnvOverride {
            branded: Some(missing.clone()),
            legacy: None,
        }),
        || RuntimeOptions::parse(&[]),
    )
    .expect("missing secrets should be ignored");
    assert!(
        !options
            .delegate_arguments
            .iter()
            .any(|arg| arg == "--secrets-file"),
        "no secrets override should be forwarded when the environment path is missing"
    );
}

#[test]
fn runtime_options_cli_config_overrides_environment_variable() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let dir = tempdir().expect("config dir");
    let env_module_dir = dir.path().join("env-module");
    let cli_module_dir = dir.path().join("cli-module");
    fs::create_dir_all(&env_module_dir).expect("env module dir");
    fs::create_dir_all(&cli_module_dir).expect("cli module dir");

    let env_config = dir.path().join("env.conf");
    fs::write(
        &env_config,
        format!("[env]\npath = {}\n", env_module_dir.display()),
    )
    .expect("write env config");

    let cli_config = dir.path().join("cli.conf");
    fs::write(
        &cli_config,
        format!("[cli]\npath = {}\n", cli_module_dir.display()),
    )
    .expect("write cli config");

    let _env = EnvGuard::set(LEGACY_CONFIG_ENV, env_config.as_os_str());
    let args = [
        OsString::from("--config"),
        cli_config.clone().into_os_string(),
    ];
    let options = RuntimeOptions::parse(&args).expect("parse cli config");

    assert_eq!(options.modules().len(), 1);
    let module = &options.modules()[0];
    assert_eq!(module.name, "cli");
    assert_eq!(module.path, cli_module_dir);
    assert_eq!(
        options.delegate_arguments,
        vec![
            OsString::from("--config"),
            cli_config.clone().into_os_string(),
        ]
    );
}

#[test]
fn runtime_options_loads_reverse_lookup_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "reverse lookup = no\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ];
    let options = RuntimeOptions::parse(&args).expect("parse config");
    assert!(!options.reverse_lookup());
}

#[test]
fn runtime_options_rejects_duplicate_reverse_lookup_directive() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "reverse lookup = yes\nreverse lookup = no\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ];
    let error = RuntimeOptions::parse(&args).expect_err("duplicate reverse lookup");
    assert!(format!("{error}").contains("reverse lookup"));
}

#[test]
fn runtime_options_parse_hosts_allow_and_deny() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nhosts allow = 127.0.0.1,192.168.0.0/24\nhosts deny = 192.168.0.5\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse hosts directives");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert_eq!(module.hosts_allow.len(), 2);
    assert!(matches!(
        module.hosts_allow[0],
        HostPattern::Ipv4 { prefix: 32, .. }
    ));
    assert!(matches!(
        module.hosts_allow[1],
        HostPattern::Ipv4 { prefix: 24, .. }
    ));
    assert_eq!(module.hosts_deny.len(), 1);
    assert!(matches!(
        module.hosts_deny[0],
        HostPattern::Ipv4 { prefix: 32, .. }
    ));
}

#[test]
fn runtime_options_parse_hostname_patterns() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nhosts allow = trusted.example.com,.example.org\nhosts deny = bad?.example.net\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse hostname hosts directives");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);

    let module = &modules[0];
    assert_eq!(module.hosts_allow.len(), 2);
    assert!(matches!(module.hosts_allow[0], HostPattern::Hostname(_)));
    assert!(matches!(module.hosts_allow[1], HostPattern::Hostname(_)));
    assert_eq!(module.hosts_deny.len(), 1);
    assert!(matches!(module.hosts_deny[0], HostPattern::Hostname(_)));
}

#[test]
fn runtime_options_parse_auth_users_and_secrets_file() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[secure]\npath = {}\nauth users = alice, bob\nsecrets file = {}\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse auth users");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(
        module.auth_users(),
        &[String::from("alice"), String::from("bob")]
    );
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

#[test]
fn runtime_options_inherits_global_secrets_file_from_config() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "secrets file = {}\n[secure]\npath = {}\nauth users = alice\n",
            secrets_path.display(),
            module_dir.display()
        ),
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.auth_users(), &[String::from("alice")]);
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

#[test]
fn runtime_options_inline_module_uses_global_secrets_file() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!("secrets file = {}\n", secrets_path.display()),
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from(format!(
            "secure={}{}auth users=alice",
            module_dir.display(),
            ';'
        )),
    ];

    let options = RuntimeOptions::parse(&args).expect("parse inline module");
    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "secure");
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

#[test]
fn runtime_options_inline_module_uses_default_secrets_file() {
    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let args = [
        OsString::from("--module"),
        OsString::from(format!(
            "secure={}{}auth users=alice",
            module_dir.display(),
            ';'
        )),
    ];

    let options =
        with_test_secrets_candidates(vec![secrets_path.clone()], || RuntimeOptions::parse(&args))
            .expect("parse inline module with default secrets");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "secure");
    assert_eq!(module.secrets_file(), Some(secrets_path.as_path()));
}

#[test]
fn runtime_options_require_secrets_file_with_auth_users() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[secure]\npath = /srv/secure\nauth users = alice\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("missing secrets file should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("missing the required 'secrets file' directive")
    );
}

#[cfg(unix)]
#[test]
fn runtime_options_rejects_world_readable_secrets_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");
    fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o644)).expect("chmod secrets");

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("world-readable secrets file should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("must not be accessible to group or others")
    );
}

#[test]
fn runtime_options_rejects_config_missing_path() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\ncomment = sample\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect_err("missing path should error");

    assert!(
        error
            .message()
            .to_string()
            .contains("missing required 'path' directive")
    );
}

#[test]
fn runtime_options_rejects_duplicate_module_across_config_and_cli() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\n").expect("write config");

    let error = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from("docs=/other/path"),
    ])
    .expect_err("duplicate module should fail");

    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate module definition 'docs'")
    );
}

#[test]
fn run_daemon_serves_single_legacy_connection() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

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

    stream.write_all(b"module\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("error message");
    assert!(line.starts_with("@ERROR:"));

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_handles_binary_negotiation() {
    use rsync_protocol::{BorrowedMessageFrames, MessageCode};

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

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
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    let advertisement = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
    stream
        .write_all(&advertisement)
        .expect("send client advertisement");
    stream.flush().expect("flush advertisement");

    let mut response = [0u8; 4];
    stream
        .read_exact(&mut response)
        .expect("read server advertisement");
    assert_eq!(response, advertisement);

    let mut frames = Vec::new();
    stream.read_to_end(&mut frames).expect("read frames");

    let mut iter = BorrowedMessageFrames::new(&frames);
    let first = iter.next().expect("first frame").expect("decode frame");
    assert_eq!(first.code(), MessageCode::Error);
    assert_eq!(first.payload(), HANDSHAKE_ERROR_PAYLOAD.as_bytes());
    let second = iter.next().expect("second frame").expect("decode frame");
    assert_eq!(second.code(), MessageCode::ErrorExit);
    assert_eq!(
        second.payload(),
        u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE)
            .expect("feature unavailable exit code fits")
            .to_be_bytes()
    );
    assert!(iter.next().is_none());

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_requests_authentication_for_protected_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

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

    stream.write_all(b"secure\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules authlist\n");

    line.clear();
    reader.read_line(&mut line).expect("auth request");
    assert!(line.starts_with("@RSYNCD: AUTHREQD "));
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");
    assert!(!challenge.is_empty());

    stream.write_all(b"\n").expect("send empty credentials");
    stream.flush().expect("flush empty credentials");

    line.clear();
    reader.read_line(&mut line).expect("denied message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: access denied to module 'secure' from 127.0.0.1"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_enforces_module_connection_limit() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let config_path = dir.path().join("rsyncd.conf");
    writeln!(
        fs::File::create(&config_path).expect("create config"),
        "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\nmax connections = 1\n",
        module_dir.display(),
        secrets_path.display()
    )
    .expect("write config");

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
            OsString::from("--config"),
            config_path.as_os_str().to_os_string(),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let mut first_stream = connect_with_retries(port);
    let mut first_reader = BufReader::new(first_stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    first_reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    first_stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send handshake");
    first_stream.flush().expect("flush handshake");

    line.clear();
    first_reader.read_line(&mut line).expect("handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    first_stream
        .write_all(b"secure\n")
        .expect("send module request");
    first_stream.flush().expect("flush module");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("capabilities for first client");
    assert_eq!(line.trim_end(), "@RSYNCD: CAP modules authlist");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("auth request for first client");
    assert!(line.starts_with("@RSYNCD: AUTHREQD"));

    let mut second_stream = connect_with_retries(port);
    let mut second_reader = BufReader::new(second_stream.try_clone().expect("clone second"));

    line.clear();
    second_reader.read_line(&mut line).expect("second greeting");
    assert_eq!(line, expected_greeting);

    second_stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send second handshake");
    second_stream.flush().expect("flush second handshake");

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second handshake ack");
    assert_eq!(line, "@RSYNCD: OK\n");

    second_stream
        .write_all(b"secure\n")
        .expect("send second module");
    second_stream.flush().expect("flush second module");

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second capabilities");
    assert_eq!(line.trim_end(), "@RSYNCD: CAP modules authlist");

    line.clear();
    second_reader.read_line(&mut line).expect("limit error");
    assert_eq!(
        line.trim_end(),
        "@ERROR: max connections (1) reached -- try again later"
    );

    line.clear();
    second_reader
        .read_line(&mut line)
        .expect("second exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    first_stream
        .write_all(b"\n")
        .expect("send empty credentials to first client");
    first_stream.flush().expect("flush first credentials");

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("first denial message");
    assert!(line.starts_with("@ERROR: access denied"));

    line.clear();
    first_reader
        .read_line(&mut line)
        .expect("first exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(second_reader);
    drop(second_stream);
    drop(first_reader);
    drop(first_stream);

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_accepts_valid_credentials() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let dir = tempdir().expect("config dir");
    let module_dir = dir.path().join("module");
    fs::create_dir_all(&module_dir).expect("module dir");
    let secrets_path = dir.path().join("secrets.txt");
    fs::write(&secrets_path, "alice:password\n").expect("write secrets");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&secrets_path, PermissionsExt::from_mode(0o600))
            .expect("chmod secrets");
    }

    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        format!(
            "[secure]\npath = {}\nauth users = alice\nsecrets file = {}\n",
            module_dir.display(),
            secrets_path.display()
        ),
    )
    .expect("write config");

    let port = allocate_test_port();

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

    stream.write_all(b"secure\n").expect("send module request");
    stream.flush().expect("flush module request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules authlist\n");

    line.clear();
    reader.read_line(&mut line).expect("auth request");
    let challenge = line
        .trim_end()
        .strip_prefix("@RSYNCD: AUTHREQD ")
        .expect("challenge prefix");

    let mut hasher = Md5::new();
    hasher.update(b"password");
    hasher.update(challenge.as_bytes());
    let digest = STANDARD_NO_PAD.encode(hasher.finalize());
    let response_line = format!("alice {digest}\n");
    stream
        .write_all(response_line.as_bytes())
        .expect("send credentials");
    stream.flush().expect("flush credentials");

    line.clear();
    reader
        .read_line(&mut line)
        .expect("post-auth acknowledgement");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("unavailable message");
    assert_eq!(
        line.trim_end(),
        "@ERROR: module 'secure' transfers are not yet implemented in this build"
    );

    line.clear();
    reader.read_line(&mut line).expect("exit message");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_honours_max_sessions() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let expected_greeting = legacy_daemon_greeting();
    for _ in 0..2 {
        let mut stream = connect_with_retries(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

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

        stream.write_all(b"module\n").expect("send module request");
        stream.flush().expect("flush module request");

        line.clear();
        reader.read_line(&mut line).expect("error message");
        assert!(line.starts_with("@ERROR:"));

        line.clear();
        reader.read_line(&mut line).expect("exit message");
        assert_eq!(line, "@RSYNCD: EXIT\n");
    }

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_handles_parallel_sessions() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let handle = thread::spawn(move || run_daemon(config));

    let barrier = Arc::new(Barrier::new(2));
    let mut clients = Vec::new();

    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        clients.push(thread::spawn(move || {
            barrier.wait();
            let mut stream = connect_with_retries(port);
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

            let mut line = String::new();
            reader.read_line(&mut line).expect("greeting");
            assert_eq!(line, legacy_daemon_greeting());

            stream
                .write_all(b"@RSYNCD: 32.0\n")
                .expect("send handshake response");
            stream.flush().expect("flush handshake response");

            line.clear();
            reader.read_line(&mut line).expect("handshake ack");
            assert_eq!(line, "@RSYNCD: OK\n");

            stream.write_all(b"module\n").expect("send module request");
            stream.flush().expect("flush module request");

            line.clear();
            reader.read_line(&mut line).expect("error message");
            assert!(line.starts_with("@ERROR:"));

            line.clear();
            reader.read_line(&mut line).expect("exit message");
            assert_eq!(line, "@RSYNCD: EXIT\n");
        }));
    }

    for client in clients {
        client.join().expect("client thread");
    }

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_lists_modules_on_request() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--module"),
            OsString::from("docs=/srv/docs,Documentation"),
            OsString::from("--module"),
            OsString::from("logs=/var/log"),
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line.trim_end(), "docs\tDocumentation");

    line.clear();
    reader.read_line(&mut line).expect("second module");
    assert_eq!(line.trim_end(), "logs");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_writes_and_removes_pid_file() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let temp = tempdir().expect("pid dir");
    let pid_path = temp.path().join("rsyncd.pid");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--pid-file"),
            pid_path.as_os_str().to_os_string(),
            OsString::from("--once"),
        ])
        .build();

    let pid_clone = pid_path.clone();
    let handle = thread::spawn(move || run_daemon(config));

    let start = Instant::now();
    while !pid_clone.exists() {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("pid file not created");
        }
        thread::sleep(Duration::from_millis(20));
    }

    let mut stream = connect_with_retries(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

    let expected_greeting = legacy_daemon_greeting();
    let mut line = String::new();
    reader.read_line(&mut line).expect("greeting");
    assert_eq!(line, expected_greeting);

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    drop(reader);
    drop(stream);

    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
    assert!(!pid_path.exists());
}

#[test]
fn run_daemon_enforces_bwlimit_during_module_list() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let mut recorder = rsync_bandwidth::recorded_sleep_session();
    recorder.clear();

    let port = allocate_test_port();

    let comment = "x".repeat(4096);
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--bwlimit"),
            OsString::from("1K"),
            OsString::from("--module"),
            OsString::from(format!("docs=/srv/docs,{}", comment)),
            OsString::from("--module"),
            OsString::from("logs=/var/log"),
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    let mut total_bytes = 0usize;

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line.trim_end(), format!("docs\t{}", comment));
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("second module");
    assert_eq!(line.trim_end(), "logs");
    total_bytes += line.len();

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");
    total_bytes += line.len();

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());

    let recorded = recorder.take();
    assert!(
        !recorded.is_empty(),
        "expected bandwidth limiter to record sleep intervals"
    );
    let total_sleep = recorded
        .into_iter()
        .fold(Duration::ZERO, |acc, duration| acc + duration);
    let expected = Duration::from_secs_f64(total_bytes as f64 / 1024.0);
    let tolerance = Duration::from_millis(250);
    let diff = total_sleep.abs_diff(expected);
    assert!(
        diff <= tolerance,
        "expected sleep around {:?}, got {:?}",
        expected,
        total_sleep
    );
}

#[test]
fn run_daemon_omits_unlisted_modules_from_listing() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[visible]\npath = /srv/visible\n\n[hidden]\npath = /srv/hidden\nlist = no\n",
    )
    .expect("write config");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--bwlimit"),
            OsString::from("1K"),
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("first module");
    assert_eq!(line.trim_end(), "visible");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

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

#[test]
fn run_daemon_filters_modules_during_list_request() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[public]\npath = /srv/public\n\n[private]\npath = /srv/private\nhosts allow = 10.0.0.0/8\n",
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("public module");
    assert_eq!(line.trim_end(), "public");

    line.clear();
    reader
        .read_line(&mut line)
        .expect("exit line after accessible modules");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_lists_modules_with_motd_lines() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let dir = tempdir().expect("motd dir");
    let motd_path = dir.path().join("motd.txt");
    fs::write(
        &motd_path,
        "Welcome to rsyncd\nRemember to sync responsibly\n",
    )
    .expect("write motd");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--motd-file"),
            motd_path.as_os_str().to_os_string(),
            OsString::from("--motd-line"),
            OsString::from("Additional notice"),
            OsString::from("--module"),
            OsString::from("docs=/srv/docs"),
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

    stream.write_all(b"#list\n").expect("send list request");
    stream.flush().expect("flush list request");

    line.clear();
    reader.read_line(&mut line).expect("capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    line.clear();
    reader.read_line(&mut line).expect("motd line 1");
    assert_eq!(line.trim_end(), "@RSYNCD: MOTD Welcome to rsyncd");

    line.clear();
    reader.read_line(&mut line).expect("motd line 2");
    assert_eq!(
        line.trim_end(),
        "@RSYNCD: MOTD Remember to sync responsibly"
    );

    line.clear();
    reader.read_line(&mut line).expect("motd line 3");
    assert_eq!(line.trim_end(), "@RSYNCD: MOTD Additional notice");

    line.clear();
    reader.read_line(&mut line).expect("ok line");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("module line");
    assert_eq!(line.trim_end(), "docs");

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());
}

#[test]
fn run_daemon_records_log_file_entries() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let port = allocate_test_port();

    let temp = tempdir().expect("log dir");
    let log_path = temp.path().join("rsyncd.log");

    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--log-file"),
            log_path.as_os_str().to_os_string(),
            OsString::from("--module"),
            OsString::from("docs=/srv/docs"),
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
    reader.read_line(&mut line).expect("module acknowledgement");
    assert_eq!(line, "@RSYNCD: OK\n");

    line.clear();
    reader.read_line(&mut line).expect("module response");
    assert!(line.starts_with("@ERROR:"));

    line.clear();
    reader.read_line(&mut line).expect("exit line");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok());

    let log_contents = fs::read_to_string(&log_path).expect("read log file");
    assert!(log_contents.contains("connect from"));
    assert!(log_contents.contains("127.0.0.1"));
    assert!(log_contents.contains("module 'docs'"));
}

#[test]
fn read_trimmed_line_strips_crlf_terminators() {
    let input: &[u8] = b"payload data\r\n";
    let mut reader = BufReader::new(input);

    let line = read_trimmed_line(&mut reader)
        .expect("read line")
        .expect("line available");

    assert_eq!(line, "payload data");

    let eof = read_trimmed_line(&mut reader).expect("eof read");
    assert!(eof.is_none());
}

#[test]
fn version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_daemon_brand(Brand::Upstream).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_version_flag_renders_report() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC_D), OsStr::new("--version")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = VersionInfoReport::for_daemon_brand(Brand::Oc).human_readable();
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn help_flag_renders_static_help_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNCD), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::Rsyncd);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_flag_renders_branded_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC_D), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::OcRsyncd);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn run_daemon_rejects_unknown_argument() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--unknown")])
        .build();

    let error = run_daemon(config).expect_err("unknown argument should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("unsupported daemon argument")
    );
}

#[test]
fn run_daemon_rejects_invalid_port() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--port"), OsString::from("not-a-number")])
        .build();

    let error = run_daemon(config).expect_err("invalid port should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid value for --port")
    );
}

#[test]
fn run_daemon_rejects_invalid_max_sessions() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([OsString::from("--max-sessions"), OsString::from("0")])
        .build();

    let error = run_daemon(config).expect_err("invalid max sessions should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("--max-sessions must be greater than zero")
    );
}

#[test]
fn run_daemon_rejects_duplicate_session_limits() {
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--once"),
            OsString::from("--max-sessions"),
            OsString::from("2"),
        ])
        .build();

    let error = run_daemon(config).expect_err("duplicate session limits should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("duplicate daemon argument '--max-sessions'")
    );
}

#[test]
fn clap_parse_error_is_reported_via_message() {
    let command = clap_command(Brand::Upstream.daemon_program_name());
    let error = command
        .try_get_matches_from(vec!["rsyncd", "--version=extra"])
        .unwrap_err();

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        [OsString::from(RSYNCD), OsString::from("--version=extra")],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(status, 1);
    assert!(stdout.is_empty());

    let rendered = String::from_utf8(stderr).expect("diagnostic is valid UTF-8");
    assert!(rendered.contains(error.to_string().trim()));
}

fn connect_with_retries(port: u16) -> TcpStream {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(20);
    const MAX_BACKOFF: Duration = Duration::from_millis(200);
    const TIMEOUT: Duration = Duration::from_secs(15);

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + TIMEOUT;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        match TcpStream::connect_timeout(&target, backoff) {
            Ok(stream) => return stream,
            Err(error) => {
                if Instant::now() >= deadline {
                    panic!("failed to connect to daemon within timeout: {error}");
                }

                thread::sleep(backoff);
                backoff = (backoff.saturating_mul(2)).min(MAX_BACKOFF);
            }
        }
    }
}
