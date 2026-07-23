//! End-to-end tests for the daemon module-listing client.
//!
//! Exercises [`run_module_list`] and its variants against in-process stub
//! daemons and HTTP `CONNECT` proxies, plus the proxy tunnel and handshake
//! error-mapping helpers. The stub daemons speak the legacy `@RSYNCD:`
//! protocol so the assertions verify wire behaviour end to end rather than
//! individual parser units (those live beside their sources).

use std::env;
#[cfg(unix)]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use protocol::{NegotiationError, ProtocolVersion};

use super::super::{
    AddressMode, FEATURE_UNAVAILABLE_EXIT_CODE, PARTIAL_TRANSFER_EXIT_CODE,
    PROTOCOL_INCOMPATIBLE_EXIT_CODE, SOCKET_IO_EXIT_CODE, TransferTimeout,
};
use super::auth::set_test_daemon_password;
use super::{
    DaemonAddress, DaemonAuthDigest, ModuleListOptions, ModuleListRequest, ProxyConfig,
    compute_daemon_auth_response, establish_proxy_tunnel, map_daemon_handshake_error,
    resolve_daemon_addresses, run_module_list, run_module_list_with_options,
    run_module_list_with_password,
};

const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

const DEFAULT_PROXY_STATUS_LINE: &str = "HTTP/1.0 200 Connection established";
const LOWERCASE_PROXY_STATUS_LINE: &str = "http/1.1 200 Connection Established";

static ENV_GUARD: OnceLock<Mutex<()>> = OnceLock::new();

fn env_lock() -> &'static Mutex<()> {
    ENV_GUARD.get_or_init(|| Mutex::new(()))
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        // SAFETY: Test-only EnvGuard serializes env access; no other threads read this key concurrently.
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    // Only the #[cfg(unix)] connect-program test uses set_os/remove (the
    // RSYNC_CONNECT_PROG/RSYNC_SHELL/RSYNC_PROXY env vars), so gate them to unix
    // to avoid a dead-code error on Windows under -D warnings.
    #[cfg(unix)]
    fn set_os(key: &'static str, value: &OsStr) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        // SAFETY: Test-only EnvGuard serializes env access; no other threads read this key concurrently.
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    #[cfg(unix)]
    fn remove(key: &'static str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        // SAFETY: Test-only EnvGuard serializes env access; no other threads read this key concurrently.
        unsafe {
            env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            #[allow(unsafe_code)]
            // SAFETY: Test-only EnvGuard restores prior value during Drop; no concurrent env access in test scope.
            unsafe {
                env::set_var(self.key, value);
            }
        } else {
            #[allow(unsafe_code)]
            // SAFETY: Test-only EnvGuard removes key during Drop; no concurrent env access in test scope.
            unsafe {
                env::remove_var(self.key);
            }
        }
    }
}

fn spawn_stub_daemon(
    responses: Vec<&'static str>,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_connection(stream, responses);
        }
    });

    (addr, handle)
}

fn handle_connection(mut stream: TcpStream, responses: Vec<&'static str>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    stream
        .write_all(LEGACY_DAEMON_GREETING.as_bytes())
        .expect("write greeting");
    stream.flush().expect("flush greeting");

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read client greeting");
    assert_eq!(line, LEGACY_DAEMON_GREETING);

    line.clear();
    reader.read_line(&mut line).expect("read request");
    assert_eq!(line, "#list\n");

    for response in responses {
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .expect("write response");
    }
    reader.get_mut().flush().expect("flush response");

    let stream = reader.into_inner();
    let _ = stream.shutdown(Shutdown::Both);
}

fn spawn_stub_proxy(
    target: std::net::SocketAddr,
    expected_header: Option<&'static str>,
    status_line: &'static str,
) -> (
    std::net::SocketAddr,
    mpsc::Receiver<String>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let addr = listener.local_addr().expect("proxy addr");
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut captured = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read request line") == 0 {
                    break;
                }
                captured.push_str(&line);
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }

            if let Some(expected) = expected_header {
                assert!(captured.contains(expected), "missing proxy header");
            }

            tx.send(captured).expect("send captured request");

            let mut client_stream = reader.into_inner();
            let mut server_stream = TcpStream::connect(target).expect("connect daemon");
            client_stream
                .write_all(status_line.as_bytes())
                .expect("write proxy response");
            client_stream
                .write_all(b"\r\n\r\n")
                .expect("terminate proxy status");

            let mut client_clone = client_stream.try_clone().expect("clone client");
            let mut server_clone = server_stream.try_clone().expect("clone server");

            let forward = thread::spawn(move || {
                let _ = io::copy(&mut client_clone, &mut server_stream);
            });
            let backward = thread::spawn(move || {
                let _ = io::copy(&mut server_clone, &mut client_stream);
            });

            let _ = forward.join();
            let _ = backward.join();
        }
    });

    (addr, rx, handle)
}

#[test]
fn run_module_list_reports_authentication_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abcdef";
    let expected = compute_daemon_auth_response(b"secret", challenge, DaemonAuthDigest::Sha512);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            reader
                .get_mut()
                .write_all(b"@RSYNCD: AUTHFAILED credentials rejected\n")
                .expect("write failure");
            reader
                .get_mut()
                .write_all(b"@RSYNCD: EXIT\n")
                .expect("write exit");
            reader.get_mut().flush().expect("flush failure");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let error = run_module_list(request).expect_err("auth failure surfaces");
    set_test_daemon_password(None);

    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("rejected provided credentials")
    );

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_reports_access_denied() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@RSYNCD: DENIED host rules\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("denied response should surface");
    assert_eq!(error.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("denied access"));
    assert!(rendered.contains("host rules"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_accepts_plaintext_motd_before_acknowledgment() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "-----\n",
        "Welcome to the stub rsync service\n",
        "@RSYNCD: OK\n",
        "public\tExample module\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");

    assert_eq!(
        list.motd_lines(),
        ["-----", "Welcome to the stub rsync service"]
    );
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "public");
    assert_eq!(list.entries()[0].comment(), Some("Example module"));

    handle.join().expect("daemon thread completes");
}

#[test]
fn run_module_list_suppresses_plaintext_motd_when_requested() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "Banner headline\n",
        "@RSYNCD: OK\n",
        "archive\tRotating snapshots\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let options = ModuleListOptions::default().suppress_motd(true);
    let list = run_module_list_with_options(request, options).expect("module list succeeds");

    assert!(list.motd_lines().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "archive");
    assert_eq!(list.entries()[0].comment(), Some("Rotating snapshots"));

    handle.join().expect("daemon thread completes");
}

#[test]
fn establish_proxy_tunnel_formats_ipv6_authority_without_brackets() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
    let addr = listener.local_addr().expect("proxy addr");
    let expected_line = "CONNECT fe80::1%eth0:873 HTTP/1.0\r\n";

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept proxy connection");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read CONNECT request");
        assert_eq!(line, expected_line);

        line.clear();
        reader.read_line(&mut line).expect("read blank line");
        assert!(line == "\r\n" || line == "\n");

        let mut stream = reader.into_inner();
        stream
            .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
            .expect("write proxy response");
        stream.flush().expect("flush proxy response");
    });

    let daemon_addr = DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("daemon addr");
    let proxy = ProxyConfig {
        host: String::from("proxy.example"),
        port: addr.port(),
        credentials: None,
    };

    let mut stream = TcpStream::connect(addr).expect("connect to proxy listener");
    establish_proxy_tunnel(&mut stream, &daemon_addr, &proxy).expect("tunnel negotiation succeeds");

    handle.join().expect("proxy thread completes");
}

#[test]
fn run_module_list_accepts_lowercase_proxy_status_line() {
    let responses = vec!["@RSYNCD: OK\n", "kappa\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let (proxy_addr, _request_rx, proxy_handle) =
        spawn_stub_proxy(daemon_addr, None, LOWERCASE_PROXY_STATUS_LINE);

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "kappa");

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

#[test]
fn run_module_list_reports_invalid_proxy_configuration() {
    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set("RSYNC_PROXY", "invalid-proxy");

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(String::from("localhost"), 873).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("invalid proxy should fail");
    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid proxy specification: should be HOST:PORT")
    );
}

#[test]
fn map_daemon_handshake_error_converts_error_payload() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@ERROR module unavailable".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("module unavailable"));
}

#[test]
fn map_daemon_handshake_error_converts_plain_invalid_data_error() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::InvalidData, "@ERROR daemon unavailable");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PARTIAL_TRANSFER_EXIT_CODE);
    assert!(mapped.message().to_string().contains("daemon unavailable"));
}

#[test]
fn map_daemon_handshake_error_converts_other_malformed_greetings() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(
        io::ErrorKind::InvalidData,
        NegotiationError::MalformedLegacyGreeting {
            input: "@RSYNCD? unexpected".to_string(),
        },
    );

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), PROTOCOL_INCOMPATIBLE_EXIT_CODE);
    assert!(mapped.message().to_string().contains("@RSYNCD? unexpected"));
}

#[test]
fn map_daemon_handshake_error_propagates_other_failures() {
    let addr = DaemonAddress::new("127.0.0.1".to_string(), 873).expect("address");
    let error = io::Error::new(io::ErrorKind::TimedOut, "timed out");

    let mapped = map_daemon_handshake_error(error, &addr);
    assert_eq!(mapped.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = mapped.message().to_string();
    assert!(rendered.contains("timed out"));
    assert!(rendered.contains("negotiate with"));
}

#[test]
fn run_module_list_reports_authentication_required() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec!["@RSYNCD: AUTHREQD modules\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("auth requirement should surface");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("requires authentication"));
    assert!(rendered.contains("username"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_requires_password_for_authentication() {
    let responses = vec!["@RSYNCD: AUTHREQD challenge\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(None);

    let error = run_module_list(request).expect_err("missing password should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(error.message().to_string().contains("RSYNC_PASSWORD"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abc123";
    let expected = compute_daemon_auth_response(b"secret", challenge, DaemonAuthDigest::Sha512);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "secured\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let list = run_module_list(request).expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "secured");

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_password_override() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind override daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "override";
    let expected =
        compute_daemon_auth_response(b"override-secret", challenge, DaemonAuthDigest::Sha512);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "override\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"wrong".to_vec()));
    let list = run_module_list_with_password(
        request,
        Some(b"override-secret".to_vec()),
        TransferTimeout::Default,
    )
    .expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "override");

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_authenticates_with_split_challenge() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind split auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "split123";
    let expected = compute_daemon_auth_response(b"secret", challenge, DaemonAuthDigest::Sha512);

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("read timeout");
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .expect("write timeout");

            stream
                .write_all(LEGACY_DAEMON_GREETING.as_bytes())
                .expect("write greeting");
            stream.flush().expect("flush greeting");

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read client greeting");
            assert_eq!(line, LEGACY_DAEMON_GREETING);

            line.clear();
            reader.read_line(&mut line).expect("read request");
            assert_eq!(line, "#list\n");

            reader
                .get_mut()
                .write_all(b"@RSYNCD: AUTHREQD\n")
                .expect("write authreqd");
            reader.get_mut().flush().expect("flush authreqd");

            reader
                .get_mut()
                .write_all(format!("@RSYNCD: AUTH {challenge}\n").as_bytes())
                .expect("write challenge");
            reader.get_mut().flush().expect("flush challenge");

            line.clear();
            reader.read_line(&mut line).expect("read credentials");
            let received = line.trim_end_matches(['\n', '\r']);
            assert_eq!(received, format!("user {expected}"));

            for response in ["@RSYNCD: OK\n", "protected\n", "@RSYNCD: EXIT\n"] {
                reader
                    .get_mut()
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
            reader.get_mut().flush().expect("flush response");
        }
    });

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        Some(String::from("user")),
        ProtocolVersion::NEWEST,
    );

    let _guard = env_lock().lock().unwrap();
    set_test_daemon_password(Some(b"secret".to_vec()));
    let list = run_module_list(request).expect("module list succeeds");
    set_test_daemon_password(None);

    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "protected");

    handle.join().expect("server thread");
}

#[test]
fn module_list_request_supports_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_supports_raw_ipv6_zone_identifier() {
    let operands = vec![OsString::from("[fe80::1%eth0]::")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request detected");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_rejects_truncated_percent_encoding_in_username() {
    let operands = vec![OsString::from("user%2@localhost::")];
    let error =
        ModuleListRequest::from_operands(&operands).expect_err("invalid encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon username")
    );
}

#[test]
fn resolve_daemon_addresses_filters_ipv4_mode() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv4).expect("ipv4 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv4));
}

#[test]
fn resolve_daemon_addresses_rejects_missing_ipv6_addresses() {
    let address = DaemonAddress::new(String::from("127.0.0.1"), 873).expect("address");
    let error = resolve_daemon_addresses(&address, AddressMode::Ipv6)
        .expect_err("IPv6 filtering should fail for IPv4-only host");

    assert_eq!(error.exit_code(), SOCKET_IO_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("does not have IPv6 addresses"));
}

#[test]
fn resolve_daemon_addresses_filters_ipv6_mode() {
    let address = DaemonAddress::new(String::from("::1"), 873).expect("address");
    let addresses =
        resolve_daemon_addresses(&address, AddressMode::Ipv6).expect("ipv6 resolution succeeds");

    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(std::net::SocketAddr::is_ipv6));
}

#[test]
fn daemon_address_accepts_ipv6_zone_identifier() {
    let address =
        DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("zone identifier accepted");
    assert_eq!(address.host(), "fe80::1%eth0");
    assert_eq!(address.port(), 873);

    let display = format!("{}", address.socket_addr_display());
    assert_eq!(display, "[fe80::1%eth0]:873");
}

#[test]
fn module_list_request_parses_ipv6_zone_identifier() {
    let operands = vec![OsString::from("rsync://fe80::1%eth0/")];
    let request = ModuleListRequest::from_operands(&operands)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);

    let bracketed = vec![OsString::from("rsync://[fe80::1%25eth0]/")];
    let request = ModuleListRequest::from_operands(&bracketed)
        .expect("parse succeeds")
        .expect("request present");
    assert_eq!(request.address().host(), "fe80::1%eth0");
    assert_eq!(request.address().port(), 873);
}

#[test]
fn module_list_request_rejects_truncated_percent_encoding() {
    let operands = vec![OsString::from("rsync://example%2/")];
    let error = ModuleListRequest::from_operands(&operands)
        .expect_err("truncated percent encoding should fail");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    assert!(
        error
            .message()
            .to_string()
            .contains("invalid percent-encoding in daemon host")
    );
}

#[test]
fn module_list_request_rejects_ipv6_module_transfer() {
    let operands = vec![OsString::from("[fe80::1]::module")];
    let request = ModuleListRequest::from_operands(&operands).expect("parse succeeds");
    assert!(request.is_none());
}

#[test]
fn run_module_list_collects_entries() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: MOTD Maintenance window at 02:00 UTC\n",
        "@RSYNCD: OK\n",
        "alpha\tPrimary module\n",
        "beta\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(
        list.motd_lines(),
        &[
            String::from("Welcome to the test daemon"),
            String::from("Maintenance window at 02:00 UTC"),
        ]
    );
    assert!(list.capabilities().is_empty());
    assert_eq!(list.entries().len(), 2);
    assert_eq!(list.entries()[0].name(), "alpha");
    assert_eq!(list.entries()[0].comment(), Some("Primary module"));
    assert_eq!(list.entries()[1].name(), "beta");
    assert_eq!(list.entries()[1].comment(), None);

    handle.join().expect("server thread");
}

// RSYNC_CONNECT_PROG execution spawns the daemon over a socketpair, a path only
// compiled on unix (connect/program.rs is #[cfg(unix)]-gated); gate the test to
// match so Windows builds neither reference the unix path nor invoke `sh`.
#[cfg(unix)]
#[test]
fn run_module_list_uses_connect_program_command() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let command = OsString::from(
        "sh -c 'CONNECT_HOST=%H\n\
         CONNECT_PORT=%P\n\
         printf \"@RSYNCD: 31.0\\n\"\n\
         read greeting\n\
         printf \"@RSYNCD: OK\\n\"\n\
         read request\n\
         printf \"example\\t$CONNECT_HOST:$CONNECT_PORT\\n@RSYNCD: EXIT\\n\"'",
    );

    let _prog_guard = EnvGuard::set_os("RSYNC_CONNECT_PROG", &command);
    let _shell_guard = EnvGuard::remove("RSYNC_SHELL");
    let _proxy_guard = EnvGuard::remove("RSYNC_PROXY");

    let request = ModuleListRequest::from_components(
        DaemonAddress::new("example.com".to_string(), 873).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("connect program listing succeeds");
    assert_eq!(list.entries().len(), 1);
    let entry = &list.entries()[0];
    assert_eq!(entry.name(), "example");
    assert_eq!(entry.comment(), Some("example.com:873"));
}

#[cfg(unix)]
#[test]
fn run_module_list_collects_motd_after_acknowledgement() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: OK\n",
        "@RSYNCD: MOTD: Post-acknowledgement notice\n",
        "gamma\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(
        list.motd_lines(),
        &[String::from("Post-acknowledgement notice")]
    );
    assert!(list.capabilities().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "gamma");
    assert!(list.entries()[0].comment().is_none());

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_suppresses_motd_when_requested() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: MOTD Welcome to the test daemon\n",
        "@RSYNCD: OK\n",
        "alpha\tPrimary module\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list =
        run_module_list_with_options(request, ModuleListOptions::default().suppress_motd(true))
            .expect("module list succeeds");
    assert!(list.motd_lines().is_empty());
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "alpha");
    assert_eq!(list.entries()[0].comment(), Some("Primary module"));

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_collects_warnings() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@WARNING: Maintenance scheduled\n",
        "@RSYNCD: OK\n",
        "delta\n",
        "@WARNING: Additional notice\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "delta");
    assert_eq!(
        list.warnings(),
        &[
            String::from("Maintenance scheduled"),
            String::from("Additional notice")
        ]
    );
    assert!(list.capabilities().is_empty());

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_collects_capabilities() {
    let _guard = env_lock().lock().expect("env mutex poisoned");

    let responses = vec![
        "@RSYNCD: CAP modules uid\n",
        "@RSYNCD: OK\n",
        "epsilon\n",
        "@RSYNCD: CAP compression\n",
        "@RSYNCD: EXIT\n",
    ];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "epsilon");
    assert_eq!(
        list.capabilities(),
        &[String::from("modules uid"), String::from("compression")]
    );

    handle.join().expect("server thread");
}

#[test]
fn run_module_list_via_proxy_connects_through_tunnel() {
    let responses = vec!["@RSYNCD: OK\n", "theta\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let (proxy_addr, request_rx, proxy_handle) =
        spawn_stub_proxy(daemon_addr, None, DEFAULT_PROXY_STATUS_LINE);

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "theta");

    let captured = request_rx.recv().expect("proxy request");
    assert!(
        captured
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("CONNECT "))
    );

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}

#[test]
fn run_module_list_via_proxy_includes_auth_header() {
    let responses = vec!["@RSYNCD: OK\n", "iota\n", "@RSYNCD: EXIT\n"];
    let (daemon_addr, daemon_handle) = spawn_stub_daemon(responses);
    let expected_header = "Proxy-Authorization: Basic dXNlcjpzZWNyZXQ=";
    let (proxy_addr, request_rx, proxy_handle) = spawn_stub_proxy(
        daemon_addr,
        Some(expected_header),
        DEFAULT_PROXY_STATUS_LINE,
    );

    let _env_lock = env_lock().lock().expect("env mutex poisoned");
    let _guard = EnvGuard::set(
        "RSYNC_PROXY",
        &format!("user:secret@{}:{}", proxy_addr.ip(), proxy_addr.port()),
    );

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(daemon_addr.ip().to_string(), daemon_addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let list = run_module_list(request).expect("module list succeeds");
    assert_eq!(list.entries().len(), 1);
    assert_eq!(list.entries()[0].name(), "iota");

    let captured = request_rx.recv().expect("proxy request");
    assert!(captured.contains(expected_header));

    proxy_handle.join().expect("proxy thread");
    daemon_handle.join().expect("daemon thread");
}
