/// Integration coverage for the hybrid async accept path
/// ([`crate::run_async_daemon`]).
///
/// Boots the async daemon on an ephemeral loopback port with a single
/// non-privileged (`use chroot = false`) read-write module, then drives a real
/// push and pull through it via `core::client::run_client`. The transferred
/// bytes must be identical in both directions, proving the async accept path
/// serves genuine sessions with the same wire behaviour as the sync daemon
/// (only accept + dispatch is async; the per-connection worker is the existing
/// synchronous session handler run under `spawn_blocking`).
///
/// A companion test pins the fail-closed behaviour: a module that requests a
/// privileged setting (`use chroot`, the upstream default) must be rejected at
/// startup rather than served without the privilege drop.
#[cfg(all(unix, feature = "async-daemon"))]
#[test]
fn daemon_async_accept_push_pull_byte_identical() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _primary = EnvGuard::set(DAEMON_FALLBACK_ENV, OsStr::new("0"));
    let _secondary = EnvGuard::set(CLIENT_FALLBACK_ENV, OsStr::new("0"));

    let temp = tempdir().expect("tempdir");

    // Module directory seeded with a small file for the pull leg.
    let module_dir = temp.path().join("module");
    fs::create_dir_all(&module_dir).expect("create module dir");
    let payload = b"async-daemon byte-identity payload\n";
    fs::write(module_dir.join("seed.txt"), payload).expect("seed module file");

    let config_file = temp.path().join("rsyncd.conf");
    let config_content = format!(
        "[async]\n\
         path = {}\n\
         read only = false\n\
         use chroot = false\n",
        module_dir.display()
    );
    fs::write(&config_file, config_content).expect("write daemon config");

    // Reserve an ephemeral port, then release it so the async daemon can bind
    // its own tokio listener on the same address (it does not accept a
    // pre-bound listener).
    let (port, held_listener) = allocate_test_port();
    drop(held_listener);

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--no-detach"),
            OsString::from("--address"),
            OsString::from("127.0.0.1"),
            OsString::from("--port"),
            OsString::from(port.to_string()),
        ])
        .build();

    let daemon_handle = thread::spawn(move || crate::run_async_daemon(daemon_config));

    // Wait for the async listener to accept connections. The port was just
    // released, so retry until connect succeeds or the daemon exits.
    let target = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + Duration::from_secs(10);
    let ready = loop {
        if daemon_handle.is_finished() {
            let result = daemon_handle.join().expect("daemon thread");
            panic!("async daemon exited before accepting connections: {result:?}");
        }
        if TcpStream::connect_timeout(&target, Duration::from_millis(100)).is_ok() {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(50));
    };
    assert!(ready, "async daemon did not become ready in time");

    let rsync_url = format!("rsync://127.0.0.1:{port}/async/");

    // Pull: seed.txt from the module into a fresh local directory.
    let pull_dest = temp.path().join("pull_dest");
    fs::create_dir_all(&pull_dest).expect("create pull dest");
    let mut pull_dest_arg = pull_dest.clone().into_os_string();
    pull_dest_arg.push("/");
    let mut pull_src_arg = OsString::from(&rsync_url);
    pull_src_arg.push("seed.txt");

    let pull_config = core::client::ClientConfig::builder()
        .transfer_args([pull_src_arg, pull_dest_arg])
        .build();
    match core::client::run_client(pull_config) {
        Ok(_) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("async daemon pull failed: {e}");
        }
    }
    assert_eq!(
        fs::read(pull_dest.join("seed.txt")).expect("read pulled file"),
        payload,
        "pulled bytes must be identical to the module source"
    );

    // Push: a new file from a local source dir into the module.
    let push_src = temp.path().join("push_src");
    fs::create_dir_all(&push_src).expect("create push src");
    let push_payload = b"pushed through the async accept path\n";
    fs::write(push_src.join("pushed.txt"), push_payload).expect("write push src file");
    let mut push_src_arg = push_src.clone().into_os_string();
    push_src_arg.push("/");

    let push_config = core::client::ClientConfig::builder()
        .transfer_args([push_src_arg, OsString::from(&rsync_url)])
        .build();
    match core::client::run_client(push_config) {
        Ok(_) => {}
        Err(e) => {
            let _ = daemon_handle.join();
            panic!("async daemon push failed: {e}");
        }
    }
    assert_eq!(
        fs::read(module_dir.join("pushed.txt")).expect("read pushed file"),
        push_payload,
        "pushed bytes must land in the module byte-for-byte"
    );

    // The tokio accept loop only stops on a shutdown signal, which this test
    // does not raise; nextest runs each test in its own process, so the still
    // -running daemon thread is reaped when the process exits. Detach it after
    // asserting success in both directions.
    drop(daemon_handle);
}

/// The async accept path fails closed on privileged modules: a module that
/// leaves `use chroot` at its default (true) must be refused at startup, since
/// chroot / setuid / setgid are not plumbed through the async worker.
#[cfg(all(unix, feature = "async-daemon"))]
#[test]
fn daemon_async_rejects_privileged_module() {
    let _lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let module_dir = temp.path().join("module");
    fs::create_dir_all(&module_dir).expect("create module dir");

    let config_file = temp.path().join("rsyncd.conf");
    // `use chroot` defaults to true, so omitting it configures a privileged
    // module that the async path must reject.
    let config_content = format!("[priv]\npath = {}\n", module_dir.display());
    fs::write(&config_file, config_content).expect("write daemon config");

    let (port, held_listener) = allocate_test_port();
    drop(held_listener);

    let daemon_config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--config"),
            config_file.as_os_str().to_owned(),
            OsString::from("--no-detach"),
            OsString::from("--address"),
            OsString::from("127.0.0.1"),
            OsString::from("--port"),
            OsString::from(port.to_string()),
        ])
        .build();

    let result = crate::run_async_daemon(daemon_config);
    let error = result.expect_err("privileged module must be rejected");
    let rendered = error.message().to_string();
    assert!(
        rendered.contains("async-daemon does not support privileged"),
        "expected privileged-module refusal, got: {rendered}"
    );
}

/// The async accept loop's worker-thread cap must never throttle below the
/// operator's configured `max connections`, while still applying the
/// flood-protection floor when the limit is unset or lower than the floor.
#[cfg(feature = "async-daemon")]
#[test]
fn async_max_inflight_honors_configured_limit() {
    use crate::async_listener::DEFAULT_MAX_INFLIGHT_WORKERS;
    use crate::daemon::async_max_inflight;

    // Unbounded (sync default): the flood floor alone applies.
    assert_eq!(async_max_inflight(None), DEFAULT_MAX_INFLIGHT_WORKERS);

    // A limit below the floor keeps the floor: the per-session admission
    // semaphore refuses excess connections before the floor ever binds.
    assert_eq!(async_max_inflight(Some(10)), DEFAULT_MAX_INFLIGHT_WORKERS);
    assert_eq!(
        async_max_inflight(Some(DEFAULT_MAX_INFLIGHT_WORKERS)),
        DEFAULT_MAX_INFLIGHT_WORKERS
    );

    // A limit above the floor raises the cap so all configured sessions run
    // concurrently instead of being silently capped at the floor.
    assert_eq!(async_max_inflight(Some(1000)), 1000);
    assert_eq!(async_max_inflight(Some(5000)), 5000);
}
