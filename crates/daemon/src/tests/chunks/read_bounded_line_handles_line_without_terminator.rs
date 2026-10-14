/// upstream: `io.c:2640-2643` - with `eof_ok` zero, as at every daemon call
/// site, EOF before the newline fails the read instead of returning the
/// partial line.
#[test]
fn read_bounded_line_handles_line_without_terminator() {
    let input: &[u8] = b"#list";
    let mut reader = BufReader::new(input);
    let line = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ).expect("read line");
    assert_eq!(line, None);
}
