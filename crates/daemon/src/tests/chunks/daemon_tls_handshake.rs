/// Integration tests for the TLS daemon handshake and protocol exchange.
///
/// Exercises the full TLS path: daemon-side acceptor (rustls `ServerConnection`)
/// and client-side connector (rustls `ClientConnection`) negotiate a TLS session
/// over a TCP loopback pair, then exchange the `@RSYNCD:` protocol greeting and
/// module listing over the encrypted channel.
///
/// Uses rcgen to generate ephemeral self-signed certificates for each test run,
/// avoiding any dependency on pre-existing certificate material.
///
/// # Upstream Reference
///
/// - stunnel-era `ssl cert`, `ssl key`, `ssl ca` global directives
/// - `clientserver.c:output_daemon_greeting()` - wire greeting format
/// - `clientserver.c:start_inband_exchange()` - module listing handshake

#[cfg(feature = "daemon-tls")]
#[test]
fn tls_handshake_exchanges_rsyncd_greeting() {
    use crate::tls::{TlsConfig, build_tls_acceptor};
    use std::net::TcpListener;

    let (cert_pem, key_pem, ca_pem) = generate_test_certificates("localhost");

    let dir = tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.pem");
    let key_path = dir.path().join("server.key");
    let ca_path = dir.path().join("ca.pem");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    let tls_config = TlsConfig {
        cert_path: cert_path.clone(),
        key_path: key_path.clone(),
        client_ca_path: None,
    };
    let acceptor = build_tls_acceptor(&tls_config).expect("build tls acceptor");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();

    let server_thread = thread::spawn(move || {
        let (tcp_stream, _addr) = listener.accept().expect("accept");
        let mut tls_stream =
            crate::tls::wrap_stream(&acceptor, tcp_stream).expect("tls handshake");

        // Send the @RSYNCD: greeting over the encrypted channel.
        let greeting = legacy_daemon_greeting();
        tls_stream
            .write_all(greeting.as_bytes())
            .expect("write greeting");
        tls_stream.flush().expect("flush greeting");

        // Read the client's version response.
        let mut reader = BufReader::new(&mut tls_stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read client version");
        assert!(
            line.starts_with("@RSYNCD:"),
            "client response must start with @RSYNCD:, got: {line}"
        );

        // Send the @RSYNCD: EXIT to close the session.
        let exit_msg = "@RSYNCD: EXIT\n";
        reader
            .get_mut()
            .write_all(exit_msg.as_bytes())
            .expect("write exit");
        reader.get_mut().flush().expect("flush exit");
    });

    // Client side: connect, wrap in TLS, exchange greeting.
    let client_tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    let mut client_tls = build_test_tls_client(client_tcp, &ca_path, "localhost");

    let mut reader = BufReader::new(&mut client_tls);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read server greeting");

    let expected_greeting = legacy_daemon_greeting();
    assert_eq!(
        line, expected_greeting,
        "TLS greeting must match plain-TCP greeting"
    );

    // Respond with client version.
    reader
        .get_mut()
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("write client version");
    reader.get_mut().flush().expect("flush client version");

    // Read the exit message.
    line.clear();
    reader.read_line(&mut line).expect("read exit");
    assert_eq!(line, "@RSYNCD: EXIT\n", "expected EXIT, got: {line}");

    drop(reader);
    server_thread.join().expect("server thread");
}

#[cfg(feature = "daemon-tls")]
#[test]
fn tls_handshake_module_listing_over_encrypted_channel() {
    use crate::tls::{TlsConfig, build_tls_acceptor};
    use std::net::TcpListener;

    let (cert_pem, key_pem, ca_pem) = generate_test_certificates("localhost");

    let dir = tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.pem");
    let key_path = dir.path().join("server.key");
    let ca_path = dir.path().join("ca.pem");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    let tls_config = TlsConfig {
        cert_path: cert_path.clone(),
        key_path: key_path.clone(),
        client_ca_path: None,
    };
    let acceptor = build_tls_acceptor(&tls_config).expect("build tls acceptor");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();

    let server_thread = thread::spawn(move || {
        let (tcp_stream, _addr) = listener.accept().expect("accept");
        let mut tls_stream =
            crate::tls::wrap_stream(&acceptor, tcp_stream).expect("tls handshake");

        // Send greeting.
        let greeting = legacy_daemon_greeting();
        tls_stream
            .write_all(greeting.as_bytes())
            .expect("write greeting");
        tls_stream.flush().expect("flush greeting");

        // Read client version.
        let mut reader = BufReader::new(&mut tls_stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read client version");

        // Read module selection (client sends #list).
        line.clear();
        reader.read_line(&mut line).expect("read module request");
        assert_eq!(line.trim(), "#list", "expected #list, got: {line}");

        // Send capability line.
        reader
            .get_mut()
            .write_all(b"@RSYNCD: CAP modules\n")
            .expect("write capabilities");

        // Send a module entry.
        reader
            .get_mut()
            .write_all(b"testmod        \tTest module\n")
            .expect("write module entry");

        // Send exit.
        reader
            .get_mut()
            .write_all(b"@RSYNCD: EXIT\n")
            .expect("write exit");
        reader.get_mut().flush().expect("flush listing");
    });

    // Client: connect, TLS wrap, negotiate, request module list.
    let client_tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    let mut client_tls = build_test_tls_client(client_tcp, &ca_path, "localhost");

    let mut reader = BufReader::new(&mut client_tls);

    // Read greeting.
    let mut line = String::new();
    reader.read_line(&mut line).expect("read greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "greeting must start with @RSYNCD:"
    );

    // Send client version.
    reader
        .get_mut()
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("write client version");
    reader.get_mut().flush().expect("flush client version");

    // Send module list request.
    reader
        .get_mut()
        .write_all(b"#list\n")
        .expect("write list request");
    reader.get_mut().flush().expect("flush list request");

    // Read capabilities.
    line.clear();
    reader.read_line(&mut line).expect("read capabilities");
    assert_eq!(line, "@RSYNCD: CAP modules\n");

    // Read module entry.
    line.clear();
    reader.read_line(&mut line).expect("read module entry");
    assert!(
        line.contains("testmod"),
        "module entry must contain 'testmod', got: {line}"
    );

    // Read exit.
    line.clear();
    reader.read_line(&mut line).expect("read exit");
    assert_eq!(line, "@RSYNCD: EXIT\n");

    drop(reader);
    server_thread.join().expect("server thread");
}

#[cfg(feature = "daemon-tls")]
#[test]
fn tls_config_roundtrip_from_rsyncd_conf() {
    let dir = tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.pem");
    let key_path = dir.path().join("server.key");
    let ca_path = dir.path().join("ca.pem");

    // Generate real certificates so the paths exist with valid PEM content.
    let (cert_pem, key_pem, ca_pem) = generate_test_certificates("localhost");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    let config_content = format!(
        "ssl cert = {}\n\
         ssl key = {}\n\
         ssl ca = {}\n\
         [testmod]\n\
         path = /tmp\n",
        cert_path.display(),
        key_path.display(),
        ca_path.display()
    );

    let config = crate::rsyncd_config::RsyncdConfig::parse(&config_content, Path::new("test.conf"))
        .expect("parse rsyncd config with TLS directives");

    let global = config.global();
    assert_eq!(
        global.ssl_cert(),
        Some(cert_path.as_path()),
        "ssl_cert must match configured path"
    );
    assert_eq!(
        global.ssl_key(),
        Some(key_path.as_path()),
        "ssl_key must match configured path"
    );
    assert_eq!(
        global.ssl_ca(),
        Some(ca_path.as_path()),
        "ssl_ca must match configured path"
    );

    // Verify the TlsConfig builder produces a usable config.
    let tls_config = global
        .tls_config()
        .expect("tls_config must be Some when ssl cert + key are set");
    assert_eq!(tls_config.cert_path, cert_path);
    assert_eq!(tls_config.key_path, key_path);
    assert_eq!(tls_config.client_ca_path.as_deref(), Some(ca_path.as_path()));

    // Verify the TlsConfig can build a working acceptor.
    let acceptor = crate::tls::build_tls_acceptor(&tls_config);
    assert!(
        acceptor.is_ok(),
        "build_tls_acceptor must succeed with valid cert material: {:?}",
        acceptor.err()
    );
}

#[cfg(feature = "daemon-tls")]
#[test]
fn tls_acceptor_rejects_plain_tcp_client() {
    use crate::tls::{TlsConfig, build_tls_acceptor};
    use std::io::Read;
    use std::net::TcpListener;

    let (cert_pem, key_pem, _ca_pem) = generate_test_certificates("localhost");

    let dir = tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.pem");
    let key_path = dir.path().join("server.key");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");

    let tls_config = TlsConfig {
        cert_path,
        key_path,
        client_ca_path: None,
    };
    let acceptor = build_tls_acceptor(&tls_config).expect("build tls acceptor");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();

    let server_thread = thread::spawn(move || {
        let (tcp_stream, _addr) = listener.accept().expect("accept");
        tcp_stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");

        // wrap_stream constructs the TLS session object (lazy handshake).
        // The actual handshake occurs on first I/O, which should fail when
        // the client sends plain-text bytes instead of a TLS ClientHello.
        let mut tls_stream =
            crate::tls::wrap_stream(&acceptor, tcp_stream).expect("wrap_stream succeeds lazily");

        let mut buf = [0u8; 256];
        let result = tls_stream.read(&mut buf);
        assert!(
            result.is_err(),
            "TLS read must fail when client sends plain text instead of TLS ClientHello"
        );
    });

    // Send plain-text rsync greeting (not TLS) to trigger the handshake error.
    let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    client
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("write plain greeting");
    client.flush().expect("flush");

    // The server should close the connection after the failed handshake.
    // Read until EOF to let the server side finish processing.
    let mut buf = [0u8; 256];
    let _ = client.read(&mut buf);
    drop(client);

    server_thread.join().expect("server thread");
}

#[cfg(feature = "daemon-tls")]
#[test]
fn tls_mutual_auth_requires_client_certificate() {
    use crate::tls::{TlsConfig, build_tls_acceptor};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let (cert_pem, key_pem, ca_pem) = generate_test_certificates("localhost");

    let dir = tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.pem");
    let key_path = dir.path().join("server.key");
    let ca_path = dir.path().join("ca.pem");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    // Enable mutual TLS by setting the client CA path.
    let tls_config = TlsConfig {
        cert_path,
        key_path,
        client_ca_path: Some(ca_path.clone()),
    };
    let acceptor = build_tls_acceptor(&tls_config).expect("build mtls acceptor");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();

    let server_thread = thread::spawn(move || {
        let (tcp_stream, _addr) = listener.accept().expect("accept");
        tcp_stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("set read timeout");

        // wrap_stream constructs the TLS session lazily. The mTLS handshake
        // failure manifests on first I/O when the server demands a client
        // certificate that the client cannot provide.
        let mut tls_stream =
            crate::tls::wrap_stream(&acceptor, tcp_stream).expect("wrap_stream succeeds lazily");

        // The server writes first, triggering the handshake. With mTLS
        // enabled, the server sends a CertificateRequest; the client
        // responds without a certificate, causing the server to reject.
        let write_result = tls_stream.write_all(b"@RSYNCD: 32.0\n");
        if write_result.is_ok() {
            let _ = tls_stream.flush();
            // Even if write buffered successfully, the read-back should
            // fail when the handshake alert propagates.
            let mut buf = [0u8; 256];
            let read_result = tls_stream.read(&mut buf);
            assert!(
                matches!(read_result, Ok(0) | Err(_)),
                "mTLS must reject client without certificate"
            );
        }
        // If write_result is Err, the rejection already happened.
    });

    // Connect with TLS but without presenting a client certificate.
    let client_tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    let mut client_tls = build_test_tls_client(client_tcp, &ca_path, "localhost");

    // Try to exchange data - the handshake failure manifests on first I/O.
    let mut buf = [0u8; 256];
    let read_result = client_tls.read(&mut buf);
    // The read may fail or return 0 bytes (connection reset) depending on
    // when the server rejects the handshake.
    match read_result {
        Ok(0) | Err(_) => { /* expected: handshake rejected */ }
        Ok(n) => panic!(
            "expected handshake rejection, but read {n} bytes: {:?}",
            &buf[..n]
        ),
    }

    drop(client_tls);
    server_thread.join().expect("server thread");
}

#[cfg(feature = "daemon-tls")]
#[test]
fn tls_daemon_stream_reports_is_tls() {
    use crate::tls::{TlsConfig, build_tls_acceptor};
    use std::net::TcpListener;

    let (cert_pem, key_pem, ca_pem) = generate_test_certificates("localhost");

    let dir = tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.pem");
    let key_path = dir.path().join("server.key");
    let ca_path = dir.path().join("ca.pem");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    let tls_config = TlsConfig {
        cert_path,
        key_path,
        client_ca_path: None,
    };
    let acceptor = build_tls_acceptor(&tls_config).expect("build tls acceptor");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();

    let server_thread = thread::spawn(move || {
        let (tcp_stream, _addr) = listener.accept().expect("accept");
        let tls_stream =
            crate::tls::wrap_stream(&acceptor, tcp_stream).expect("tls handshake");
        let daemon_stream = DaemonStream::Tls(Box::new(tls_stream));
        assert!(daemon_stream.is_tls(), "DaemonStream::Tls must report is_tls()");
    });

    // Client side: connect with TLS so the server handshake succeeds.
    let client_tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    let _client_tls = build_test_tls_client(client_tcp, &ca_path, "localhost");

    server_thread.join().expect("server thread");
}

#[cfg(feature = "daemon-tls")]
#[test]
fn tls_bidirectional_data_integrity() {
    use crate::tls::{TlsConfig, build_tls_acceptor};
    use std::io::Read;
    use std::net::TcpListener;

    let (cert_pem, key_pem, ca_pem) = generate_test_certificates("localhost");

    let dir = tempdir().expect("tempdir");
    let cert_path = dir.path().join("server.pem");
    let key_path = dir.path().join("server.key");
    let ca_path = dir.path().join("ca.pem");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    let tls_config = TlsConfig {
        cert_path,
        key_path,
        client_ca_path: None,
    };
    let acceptor = build_tls_acceptor(&tls_config).expect("build tls acceptor");

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("local addr").port();

    // Test that binary data (including zero bytes) roundtrips correctly
    // through the TLS layer, verifying that the encryption does not
    // corrupt payload bytes that the rsync wire protocol may emit
    // (e.g., file content, checksum bytes, multiplex headers).
    let test_payload: Vec<u8> = (0..=255).collect();

    let payload_clone = test_payload.clone();
    let server_thread = thread::spawn(move || {
        let (tcp_stream, _addr) = listener.accept().expect("accept");
        let mut tls_stream =
            crate::tls::wrap_stream(&acceptor, tcp_stream).expect("tls handshake");

        // Server sends the test payload.
        tls_stream
            .write_all(&payload_clone)
            .expect("write payload");
        tls_stream.flush().expect("flush payload");

        // Server reads the echoed payload back.
        let mut buf = vec![0u8; payload_clone.len()];
        tls_stream.read_exact(&mut buf).expect("read echo");
        assert_eq!(buf, payload_clone, "echoed payload must match");
    });

    let client_tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).expect("connect");
    let mut client_tls = build_test_tls_client(client_tcp, &ca_path, "localhost");

    // Client reads the payload.
    let mut received = vec![0u8; test_payload.len()];
    client_tls.read_exact(&mut received).expect("read payload");
    assert_eq!(received, test_payload, "received payload must match");

    // Client echoes it back.
    client_tls.write_all(&received).expect("write echo");
    client_tls.flush().expect("flush echo");

    drop(client_tls);
    server_thread.join().expect("server thread");
}

// ---------------------------------------------------------------------------
// Helper functions for TLS test certificate generation
// ---------------------------------------------------------------------------

/// Generates a self-signed CA certificate and a server certificate signed by
/// that CA. Returns `(server_cert_pem, server_key_pem, ca_cert_pem)`.
///
/// The server certificate includes `hostname` as a Subject Alternative Name
/// (SAN) so rustls client verification accepts it.
#[cfg(feature = "daemon-tls")]
fn generate_test_certificates(hostname: &str) -> (String, String, String) {
    // Generate CA key pair and self-signed certificate.
    let ca_key_pair = rcgen::KeyPair::generate().expect("generate CA key pair");
    let mut ca_params =
        rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA cert params");
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Test CA");
    let ca_cert = ca_params
        .self_signed(&ca_key_pair)
        .expect("self-sign CA cert");

    // Generate server key pair and certificate signed by the CA.
    let server_key_pair = rcgen::KeyPair::generate().expect("generate server key pair");
    let mut server_params = rcgen::CertificateParams::new(vec![hostname.to_owned()])
        .expect("server cert params");
    server_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, hostname);
    let server_cert = server_params
        .signed_by(&server_key_pair, &ca_cert, &ca_key_pair)
        .expect("sign server cert");

    let server_cert_pem = server_cert.pem();
    let server_key_pem = server_key_pair.serialize_pem();
    let ca_cert_pem = ca_cert.pem();

    (server_cert_pem, server_key_pem, ca_cert_pem)
}

/// Builds a rustls TLS client stream that trusts the given CA certificate
/// file. Used by tests to connect to the test TLS server.
#[cfg(feature = "daemon-tls")]
fn build_test_tls_client(
    tcp_stream: TcpStream,
    ca_path: &Path,
    hostname: &str,
) -> rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    use rustls::pki_types::{CertificateDer, ServerName};

    let ca_pem = fs::read(ca_path).expect("read CA cert");
    let ca_certs: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut ca_pem.as_slice())
            .collect::<Result<Vec<_>, _>>()
            .expect("parse CA cert PEM");

    let mut root_store = rustls::RootCertStore::empty();
    for cert in ca_certs {
        root_store.add(cert).expect("add CA cert to root store");
    }

    let client_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("protocol versions")
    .with_root_certificates(root_store)
    .with_no_client_auth();

    let server_name = ServerName::try_from(hostname.to_owned()).expect("valid hostname");
    let connection = rustls::ClientConnection::new(Arc::new(client_config), server_name)
        .expect("create client connection");

    rustls::StreamOwned::new(connection, tcp_stream)
}
