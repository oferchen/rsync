#[test]
fn read_trimmed_line_strips_crlf_terminators() {
    let input: &[u8] = b"payload data\r\n";
    let mut reader = BufReader::new(input);

    let line = read_trimmed_line(&mut reader)
        .expect("read line")
        .expect("line available");

    assert_eq!(line, "payload data");

    let eof = read_trimmed_line(&mut reader).expect("eof read");
    assert!(eof.is_none());
}

