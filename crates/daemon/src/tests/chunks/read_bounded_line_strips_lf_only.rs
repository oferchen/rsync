#[test]
fn read_bounded_line_strips_lf_only() {
    let input: &[u8] = b"line content\n";
    let mut reader = BufReader::new(input);

    let line = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ)
        .expect("read line")
        .expect("line available");

    assert_eq!(line, "line content");
}
