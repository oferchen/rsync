/// End-to-end interop tests verifying that rsync transfers work correctly
/// through a TLS-encrypted channel, emulating the upstream stunnel deployment
/// model.
///
/// Architecture:
///
/// ```text
/// oc-rsync client --plain TCP--> TLS proxy --plain TCP--> oc-rsync daemon
///                                 (port P)                  (port D)
/// ```
///
/// The proxy accepts plain TCP from the client, opens a plain TCP connection
/// to the daemon, and bridges the two through an internal TLS tunnel on a
/// per-connection TCP loopback. All rsync wire bytes pass through TLS
/// encrypt and decrypt, verifying that the rsync protocol is fully
/// transparent to TLS framing - the exact property that a stunnel deployment
/// relies on.
///
/// The TLS layer is exercised using the daemon crate's `TlsAcceptor` /
/// `wrap_stream()` on the server side and a rustls `ClientConnection` on the
/// client side, connected through TCP loopback. These are the same code paths
/// that will be used when the daemon's native TLS support is fully wired.
///
/// Uses rcgen for ephemeral self-signed certificates.
///
/// # Upstream Reference
///
/// - stunnel.conf `[rsync]` section - TLS termination for rsync daemons
/// - `rsync-ssl` script - client-side stunnel wrapper
/// - `clientserver.c:start_daemon_client()` - daemon connection entry point

#[cfg(all(unix, feature = "daemon-tls"))]
#[test]
fn tls_upstream_interop_push_through_encrypted_proxy() {
    use std::net::{Ipv4Addr, TcpListener};

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (cert_pem, key_pem, ca_pem) = tls_interop_generate_certificates("localhost");

    let cert_dir = tempdir().expect("tempdir for certs");
    let cert_path = cert_dir.path().join("server.pem");
    let key_path = cert_dir.path().join("server.key");
    let ca_path = cert_dir.path().join("ca.pem");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    let temp = tempdir().expect("tempdir for data");
    let source_dir = temp.path().join("source");
    fs::create_dir(&source_dir).expect("create source");
    fs::write(source_dir.join("hello.txt"), b"hello through TLS\n").expect("write hello");
    fs::write(
        source_dir.join("binary.dat"),
        &(0..=255).collect::<Vec<u8>>(),
    )
    .expect("write binary");

    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[tlsmod]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (daemon_port, daemon_listener) = allocate_test_port();
    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(daemon_port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("5"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, daemon_port, daemon_listener);
    drop(probe_stream);

    let proxy_listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind proxy");
    let proxy_port = proxy_listener.local_addr().expect("addr").port();

    let proxy_handle = thread::spawn({
        let cert = cert_path.clone();
        let key = key_path.clone();
        let ca = ca_path.clone();
        move || tls_proxy_accept_loop(proxy_listener, daemon_port, &cert, &key, &ca)
    });

    {
        let mut source_arg = source_dir.clone().into_os_string();
        source_arg.push("/");
        let rsync_url = format!("rsync://127.0.0.1:{proxy_port}/tlsmod/");

        let client_config = core::client::ClientConfig::builder()
            .transfer_args([source_arg, OsString::from(&rsync_url)])
            .build();

        let result = core::client::run_client(client_config);
        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 2,
                    "push through TLS must copy at least 2 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("push through TLS proxy failed: {e}");
            }
        }
    }

    assert_eq!(
        fs::read(module_dir.join("hello.txt")).expect("read hello"),
        b"hello through TLS\n",
        "hello.txt content mismatch after TLS push"
    );
    assert_eq!(
        fs::read(module_dir.join("binary.dat")).expect("read binary"),
        (0..=255).collect::<Vec<u8>>(),
        "binary.dat content mismatch after TLS push"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
    let _ = proxy_handle.join();
}

/// Pull test: verifies file transfer FROM a daemon module through TLS.
#[cfg(all(unix, feature = "daemon-tls"))]
#[test]
fn tls_upstream_interop_pull_through_encrypted_proxy() {
    use std::net::{Ipv4Addr, TcpListener};

    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let (cert_pem, key_pem, ca_pem) = tls_interop_generate_certificates("localhost");

    let cert_dir = tempdir().expect("tempdir for certs");
    let cert_path = cert_dir.path().join("server.pem");
    let key_path = cert_dir.path().join("server.key");
    let ca_path = cert_dir.path().join("ca.pem");
    fs::write(&cert_path, &cert_pem).expect("write cert");
    fs::write(&key_path, &key_pem).expect("write key");
    fs::write(&ca_path, &ca_pem).expect("write ca");

    let temp = tempdir().expect("tempdir for data");
    let module_dir = temp.path().join("module");
    fs::create_dir(&module_dir).expect("create module dir");
    fs::write(module_dir.join("report.txt"), b"quarterly results\n").expect("write report");
    fs::write(module_dir.join("logo.png"), b"\x89PNG\r\n\x1a\n\x00\x00")
        .expect("write fake png");

    let dest_dir = temp.path().join("dest");
    fs::create_dir(&dest_dir).expect("create dest");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[pullmod]\n\
         path = {}\n\
         read only = true\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    let (daemon_port, daemon_listener) = allocate_test_port();
    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--port"),
            OsString::from(daemon_port.to_string()),
            OsString::from("--max-sessions"),
            OsString::from("5"),
        ])
        .build();

    let (probe_stream, daemon_handle) = start_daemon(daemon_config, daemon_port, daemon_listener);
    drop(probe_stream);

    let proxy_listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind proxy");
    let proxy_port = proxy_listener.local_addr().expect("addr").port();

    let proxy_handle = thread::spawn({
        let cert = cert_path.clone();
        let key = key_path.clone();
        let ca = ca_path.clone();
        move || tls_proxy_accept_loop(proxy_listener, daemon_port, &cert, &key, &ca)
    });

    {
        let rsync_url = format!("rsync://127.0.0.1:{proxy_port}/pullmod/");
        let client_config = core::client::ClientConfig::builder()
            .transfer_args([
                OsString::from(&rsync_url),
                OsString::from(dest_dir.as_os_str()),
            ])
            .build();

        let result = core::client::run_client(client_config);
        match &result {
            Ok(summary) => {
                assert!(
                    summary.files_copied() >= 2,
                    "pull through TLS must copy at least 2 files, got {}",
                    summary.files_copied()
                );
            }
            Err(e) => {
                let _ = daemon_handle.join();
                panic!("pull through TLS proxy failed: {e}");
            }
        }
    }

    assert_eq!(
        fs::read(dest_dir.join("report.txt")).expect("read report"),
        b"quarterly results\n",
        "report.txt content mismatch after TLS pull"
    );
    assert_eq!(
        fs::read(dest_dir.join("logo.png")).expect("read logo"),
        b"\x89PNG\r\n\x1a\n\x00\x00",
        "logo.png content mismatch after TLS pull"
    );

    let daemon_result = daemon_handle.join().expect("daemon thread");
    let _ = daemon_result;
    let _ = proxy_handle.join();
}

// ---------------------------------------------------------------------------
// TLS proxy infrastructure
// ---------------------------------------------------------------------------

/// Accepts client connections and proxies them to the daemon through a TLS
/// tunnel on a per-connection TCP loopback.
///
/// For each connection, creates an internal TLS loopback (client+server) and
/// bridges data between the external client, the TLS tunnel, and the daemon.
/// Uses `rustls::StreamOwned` behind `Arc<Mutex>` with short read timeouts
/// to allow bidirectional multiplexing from separate threads without deadlock.
#[cfg(feature = "daemon-tls")]
fn tls_proxy_accept_loop(
    listener: std::net::TcpListener,
    daemon_port: u16,
    cert_path: &Path,
    key_path: &Path,
    ca_path: &Path,
) {
    use std::io;
    use std::net::{Ipv4Addr, TcpListener, TcpStream};

    let tls_config = crate::tls::TlsConfig {
        cert_path: cert_path.to_owned(),
        key_path: key_path.to_owned(),
        client_ca_path: None,
    };
    let acceptor = crate::tls::build_tls_acceptor(&tls_config).expect("build acceptor");

    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");

    let mut handles: Vec<thread::JoinHandle<()>> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(60);

    while Instant::now() < deadline {
        match listener.accept() {
            Ok((client_tcp, _)) => {
                client_tcp.set_nonblocking(false).expect("blocking");
                client_tcp
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .ok();

                let daemon_tcp =
                    match TcpStream::connect((Ipv4Addr::LOCALHOST, daemon_port)) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                daemon_tcp
                    .set_read_timeout(Some(Duration::from_secs(30)))
                    .ok();

                // Create a TCP loopback for the internal TLS session.
                let loopback =
                    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind loopback");
                let loopback_port = loopback.local_addr().expect("addr").port();

                let acceptor_clone = acceptor.clone();
                let ca_owned = ca_path.to_owned();

                // Server side: TLS accept on loopback, bridge to daemon.
                let h_server = thread::spawn(move || {
                    let (tcp, _) = match loopback.accept() {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    // Short read timeout to prevent holding the mutex while
                    // blocked on I/O - allows the write thread to interleave.
                    tcp.set_read_timeout(Some(Duration::from_millis(50))).ok();

                    let tls =
                        match crate::tls::wrap_stream(&acceptor_clone, tcp) {
                            Ok(s) => s,
                            Err(_) => return,
                        };

                    let tls_mutex = std::sync::Arc::new(std::sync::Mutex::new(tls));

                    let daemon_read = daemon_tcp.try_clone().expect("clone daemon");
                    daemon_read.set_read_timeout(Some(Duration::from_millis(50))).ok();
                    let daemon_write = daemon_tcp;

                    tls_bridge_bidirectional(tls_mutex, daemon_read, daemon_write);
                });

                // Client side: TLS connect to loopback, bridge to client.
                let h_client = thread::spawn(move || {
                    let tcp =
                        match TcpStream::connect((Ipv4Addr::LOCALHOST, loopback_port)) {
                            Ok(s) => s,
                            Err(_) => return,
                        };
                    tcp.set_read_timeout(Some(Duration::from_millis(50))).ok();

                    let conn =
                        match tls_interop_build_client_connection(&ca_owned, "localhost") {
                            Ok(c) => c,
                            Err(_) => return,
                        };

                    let tls = rustls::StreamOwned::new(conn, tcp);
                    let tls_mutex = std::sync::Arc::new(std::sync::Mutex::new(tls));

                    let client_read = client_tcp.try_clone().expect("clone client");
                    client_read.set_read_timeout(Some(Duration::from_millis(50))).ok();
                    let client_write = client_tcp;

                    tls_bridge_bidirectional(tls_mutex, client_read, client_write);
                });

                handles.push(h_server);
                handles.push(h_client);
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }

    for h in handles {
        let _ = h.join();
    }
}

/// Bidirectional bridge between a TLS stream (behind `Arc<Mutex>`) and a pair
/// of plain read/write streams.
///
/// Spawns two threads:
/// - TLS read -> plain write
/// - plain read -> TLS write
///
/// Both threads use short read timeouts and release the TLS mutex between
/// iterations to prevent deadlock. The bridge runs until both sides reach EOF
/// or an unrecoverable error.
#[cfg(feature = "daemon-tls")]
fn tls_bridge_bidirectional<T>(
    tls: std::sync::Arc<std::sync::Mutex<T>>,
    mut plain_read: std::net::TcpStream,
    mut plain_write: std::net::TcpStream,
) where
    T: std::io::Read + std::io::Write + Send + 'static,
{
    use std::io::{ErrorKind, Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};

    let done = std::sync::Arc::new(AtomicBool::new(false));

    // TLS -> plain
    let tls_r = std::sync::Arc::clone(&tls);
    let done_r = std::sync::Arc::clone(&done);
    let h_read = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while !done_r.load(Ordering::Relaxed) {
            let n = {
                let mut guard = tls_r.lock().expect("lock");
                match guard.read(&mut buf) {
                    Ok(0) => {
                        done_r.store(true, Ordering::Relaxed);
                        break;
                    }
                    Ok(n) => n,
                    Err(ref e)
                        if e.kind() == ErrorKind::WouldBlock
                            || e.kind() == ErrorKind::TimedOut =>
                    {
                        continue;
                    }
                    Err(_) => {
                        done_r.store(true, Ordering::Relaxed);
                        break;
                    }
                }
            };
            if plain_write.write_all(&buf[..n]).is_err() {
                done_r.store(true, Ordering::Relaxed);
                break;
            }
            let _ = plain_write.flush();
        }
    });

    // plain -> TLS
    let tls_w = std::sync::Arc::clone(&tls);
    let done_w = std::sync::Arc::clone(&done);
    let h_write = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while !done_w.load(Ordering::Relaxed) {
            let n = match plain_read.read(&mut buf) {
                Ok(0) => {
                    done_w.store(true, Ordering::Relaxed);
                    break;
                }
                Ok(n) => n,
                Err(ref e)
                    if e.kind() == ErrorKind::WouldBlock
                        || e.kind() == ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(_) => {
                    done_w.store(true, Ordering::Relaxed);
                    break;
                }
            };
            {
                let mut guard = tls_w.lock().expect("lock");
                if guard.write_all(&buf[..n]).is_err() {
                    done_w.store(true, Ordering::Relaxed);
                    break;
                }
                if guard.flush().is_err() {
                    done_w.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
    });

    let _ = h_read.join();
    let _ = h_write.join();
}

/// Builds a rustls `ClientConnection` that trusts the given CA certificate.
#[cfg(feature = "daemon-tls")]
fn tls_interop_build_client_connection(
    ca_path: &Path,
    hostname: &str,
) -> Result<rustls::ClientConnection, std::io::Error> {
    use rustls::pki_types::{CertificateDer, ServerName};

    let ca_pem = fs::read(ca_path)?;
    let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut ca_pem.as_slice())
        .collect::<Result<Vec<_>, _>>()?;

    let mut root_store = rustls::RootCertStore::empty();
    for cert in ca_certs {
        root_store
            .add(cert)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    }

    let client_config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
    .with_root_certificates(root_store)
    .with_no_client_auth();

    let server_name = ServerName::try_from(hostname.to_owned()).expect("valid hostname");
    rustls::ClientConnection::new(std::sync::Arc::new(client_config), server_name)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Generates ephemeral test certificates for TLS interop tests.
///
/// Returns `(server_cert_pem, server_key_pem, ca_cert_pem)`.
#[cfg(feature = "daemon-tls")]
fn tls_interop_generate_certificates(hostname: &str) -> (String, String, String) {
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

    let server_key_pair = rcgen::KeyPair::generate().expect("generate server key pair");
    let mut server_params =
        rcgen::CertificateParams::new(vec![hostname.to_owned()]).expect("server cert params");
    server_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, hostname);
    let server_cert = server_params
        .signed_by(&server_key_pair, &ca_cert, &ca_key_pair)
        .expect("sign server cert");

    (
        server_cert.pem(),
        server_key_pair.serialize_pem(),
        ca_cert.pem(),
    )
}
