#[test]
fn run_module_list_reports_authentication_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abcdef";
    let expected = compute_daemon_auth_response(b"secret", challenge);

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
fn run_module_list_reports_daemon_listing_unavailable() {
    let responses = vec!["@RSYNCD: --daemon not enabled--\n", "@RSYNCD: EXIT\n"];
    let (addr, handle) = spawn_stub_daemon(responses);

    let request = ModuleListRequest::from_components(
        DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("address"),
        None,
        ProtocolVersion::NEWEST,
    );

    let error = run_module_list(request).expect_err("daemon refusal should surface");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("daemon refused module listing"));
    assert!(rendered.contains("--daemon not enabled--"));

    handle.join().expect("server thread");
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn set_os(key: &'static str, value: &OsStr) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
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
            unsafe {
                env::set_var(self.key, value);
            }
        } else {
            #[allow(unsafe_code)]
            unsafe {
                env::remove_var(self.key);
            }
        }
    }
}

const DEFAULT_PROXY_STATUS_LINE: &str = "HTTP/1.0 200 Connection established";
const LOWERCASE_PROXY_STATUS_LINE: &str = "http/1.1 200 Connection Established";

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
