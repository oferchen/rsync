#[test]
fn read_bounded_line_strips_crlf_terminators() {
    let input: &[u8] = b"payload data\r\n";
    let mut reader = BufReader::new(input);

    let line = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ)
        .expect("read line")
        .expect("line available");

    assert_eq!(line, "payload data");

    let eof = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ).expect("eof read");
    assert!(eof.is_none());
}
