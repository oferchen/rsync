/// The test daemon holds no copy of a client's socket, so a client's close
/// reaches its session as EOF, as it does when the client is another process.
///
/// Sessions inherit their descriptors from the daemon process, so checking the
/// daemon covers every session it forks. A daemon run inside the test process,
/// or forked after the client connected, holds the client socket, and a
/// session waiting for input outlives the test.
///
/// upstream: main.c:1954 `daemon_main()` runs the daemon as its own process.
#[cfg(target_os = "linux")]
#[test]
fn no_daemon_process_holds_a_client_socket() {
    use std::os::fd::AsRawFd as _;

    let (port, held_listener) = allocate_test_port();
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            OsString::from("--port"),
            OsString::from(port.to_string()),
            OsString::from("--once"),
        ])
        .build();
    let (stream, handle) = start_daemon(config, port, held_listener);
    let client_socket = fs::read_link(format!("/proc/self/fd/{}", stream.as_raw_fd()))
        .expect("resolve the client socket");

    let daemon_fds = fs::read_dir(format!("/proc/{}/fd", handle.pid())).expect("daemon fds");
    let held: Vec<_> = daemon_fds
        .filter_map(|entry| fs::read_link(entry.expect("daemon fd").path()).ok())
        .filter(|target| *target == client_socket)
        .collect();
    assert!(
        held.is_empty(),
        "the daemon holds the client socket {client_socket:?}, so the client's close cannot end its session"
    );

    drop(stream);
    let result = finish_daemon(handle).expect("the daemon ends after its one session");
    assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
}
