use super::prelude::*;


#[test]
fn connect_direct_applies_io_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let daemon_addr = DaemonAddress::new(addr.ip().to_string(), addr.port()).expect("daemon addr");

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1];
            let _ = stream.read(&mut buf);
        }
    });

    let timeout = Some(Duration::from_secs(4));
    let mut stream = connect_direct(
        &daemon_addr,
        Some(Duration::from_secs(10)),
        timeout,
        AddressMode::Default,
        None,
    )
    .expect("connect directly");

    assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
    assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

    // Wake the accept loop and close cleanly.
    let _ = stream.write_all(&[0]);
    handle.join().expect("listener thread");
}

