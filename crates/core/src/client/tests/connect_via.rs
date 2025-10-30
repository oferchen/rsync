use super::prelude::*;


#[test]
fn connect_via_proxy_applies_io_timeout() {
    let proxy_listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let proxy_addr = proxy_listener.local_addr().expect("proxy addr");
    let proxy = ProxyConfig {
        host: proxy_addr.ip().to_string(),
        port: proxy_addr.port(),
        credentials: None,
    };

    let target = DaemonAddress::new(String::from("daemon.example"), 873).expect("daemon addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = proxy_listener.accept() {
            let mut reader = BufReader::new(stream);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read request") == 0 {
                    return;
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }

            let mut stream = reader.into_inner();
            stream
                .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
                .expect("respond to connect");
            let _ = stream.flush();
        }
    });

    let timeout = Some(Duration::from_secs(6));
    let stream = connect_via_proxy(&target, &proxy, Some(Duration::from_secs(9)), timeout, None)
        .expect("proxy connect");

    assert_eq!(stream.read_timeout().expect("read timeout"), timeout);
    assert_eq!(stream.write_timeout().expect("write timeout"), timeout);

    drop(stream);
    handle.join().expect("proxy thread");
}

