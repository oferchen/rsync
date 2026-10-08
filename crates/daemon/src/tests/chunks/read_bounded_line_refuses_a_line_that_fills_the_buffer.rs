/// upstream: `io.c:2638-2655` - `read_line_old()` fails once `bufsiz - 1` bytes
/// have gone by without a newline, so the daemon never buffers more than one
/// line's worth of a peer that does not stop.
#[test]
fn read_bounded_line_refuses_a_line_that_fills_the_buffer() {
    let mut input = vec![b'x'; HANDSHAKE_LINE_BUFSIZ - 1];
    input.extend_from_slice(b"\nnext\n");
    let mut reader = BufReader::new(input.as_slice());
    let line = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ).expect("read");
    assert_eq!(line, None);
}

/// Boundary: `bufsiz - 2` bytes plus the newline is the longest line upstream
/// accepts, and it still reads whole.
#[test]
fn read_bounded_line_accepts_the_longest_line_that_fits() {
    let mut input = vec![b'x'; HANDSHAKE_LINE_BUFSIZ - 2];
    input.push(b'\n');
    let mut reader = BufReader::new(input.as_slice());
    let line = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ)
        .expect("read")
        .expect("line");
    assert_eq!(line.len(), HANDSHAKE_LINE_BUFSIZ - 2);
}
