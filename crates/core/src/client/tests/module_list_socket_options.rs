use crate::client::module_list::apply_socket_options;

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

    apply_socket_options(&stream, std::ffi::OsStr::new("SO_SNDBUF=32768"));

    let sockref = socket2::SockRef::from(&stream);
    let reported = sockref
        .send_buffer_size()
        .expect("query send buffer size");
    assert!(reported >= 32768);

    drop(stream);
    handle.join().expect("accept thread completes");
}

/// upstream: socket.c:704-707 - an unknown option name warns
/// (`Unknown socket option %s`) and `continue`s; `set_socket_options()` is
/// `void`, so a bogus name must never abort the connection. A later valid
/// option in the same string must still be applied, proving the loop
/// continued past the unknown token instead of bailing out.
#[test]
fn apply_socket_options_warns_and_continues_on_unknown_name() {
    let (stream, handle) = connected_stream();

    apply_socket_options(&stream, std::ffi::OsStr::new("SO_NOTREAL=1,SO_SNDBUF=32768"));

    let sockref = socket2::SockRef::from(&stream);
    assert!(
        sockref.send_buffer_size().expect("query send buffer size") >= 32768,
        "valid option after an unknown one must still apply"
    );

    drop(stream);
    handle.join().expect("accept thread completes");
}

/// upstream: socket.c:717-727 - an OPT_ON option (e.g. `IPTOS_LOWDELAY`) given
/// a value warns (`syntax error -- %s does not take a value`) but still applies
/// its fixed value. The value must not turn the option into a fatal error.
#[cfg(not(target_family = "windows"))]
#[test]
fn apply_socket_options_opt_on_with_value_still_applies() {
    let (stream, handle) = connected_stream();

    // IPTOS_LOWDELAY is an IPv4 IP_TOS preset; supplying `=5` is a user error
    // upstream warns about but still applies the 0x10 preset on an AF_INET
    // socket. This must return normally (no panic / no fatal error).
    apply_socket_options(&stream, std::ffi::OsStr::new("IPTOS_LOWDELAY=5"));

    drop(stream);
    handle.join().expect("accept thread completes");
}
