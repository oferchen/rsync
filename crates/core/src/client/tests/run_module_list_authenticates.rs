use super::prelude::*;


#[test]
fn run_module_list_authenticates_with_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");
    let challenge = "abc123";
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
    let expected = compute_daemon_auth_response(b"override-secret", challenge);

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

