use crate::client::{module_list::apply_socket_options, FEATURE_UNAVAILABLE_EXIT_CODE};

fn connected_stream() -> (std::net::TcpStream, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let addr = listener.local_addr().expect("addr");

    let handle = std::thread::spawn(move || {
        let _ = listener.accept();
    });

    let stream = std::net::TcpStream::connect(addr).expect("connect");
    (stream, handle)
}

#[test]
fn apply_socket_options_sets_send_buffer_size() {
    let (stream, handle) = connected_stream();

    apply_socket_options(&stream, std::ffi::OsStr::new("SO_SNDBUF=32768"))
        .expect("sockopts applied");

    let sockref = socket2::SockRef::from(&stream);
    let reported = sockref
        .send_buffer_size()
        .expect("query send buffer size");
    assert!(reported >= 32768);

    drop(stream);
    handle.join().expect("accept thread completes");
}

#[test]
fn apply_socket_options_rejects_unknown_names() {
    let (stream, handle) = connected_stream();

    let error = apply_socket_options(&stream, std::ffi::OsStr::new("SO_NOTREAL=1"))
        .expect_err("unknown option should be rejected");
    assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    let rendered = error.message().to_string();
    assert!(rendered.contains("Unknown socket option SO_NOTREAL"));

    drop(stream);
    handle.join().expect("accept thread completes");
}
