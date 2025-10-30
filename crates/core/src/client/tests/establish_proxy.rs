use super::prelude::*;


#[test]
fn establish_proxy_tunnel_formats_ipv6_authority_without_brackets() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy listener");
    let addr = listener.local_addr().expect("proxy addr");
    let expected_line = "CONNECT fe80::1%eth0:873 HTTP/1.0\r\n";

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept proxy connection");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read CONNECT request");
        assert_eq!(line, expected_line);

        line.clear();
        reader.read_line(&mut line).expect("read blank line");
        assert!(line == "\r\n" || line == "\n");

        let mut stream = reader.into_inner();
        stream
            .write_all(b"HTTP/1.0 200 Connection established\r\n\r\n")
            .expect("write proxy response");
        stream.flush().expect("flush proxy response");
    });

    let daemon_addr = DaemonAddress::new(String::from("fe80::1%eth0"), 873).expect("daemon addr");
    let proxy = ProxyConfig {
        host: String::from("proxy.example"),
        port: addr.port(),
        credentials: None,
    };

    let mut stream = TcpStream::connect(addr).expect("connect to proxy listener");
    establish_proxy_tunnel(&mut stream, &daemon_addr, &proxy).expect("tunnel negotiation succeeds");

    handle.join().expect("proxy thread completes");
}

