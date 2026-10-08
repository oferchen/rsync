/// upstream: `io.c:2638-2655` - `read_line_old()` reserves the NUL with
/// `bufsiz--` and fails once the rest has gone by without a newline, so the
/// daemon never buffers more than one
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

/// upstream: `io.c:2649-2652` - a carriage return is dropped wherever it sits
/// and does not use up the buffer, so a full-length line padded with CRs still
/// fits.
#[test]
fn read_bounded_line_drops_carriage_returns_without_counting_them() {
    let mut input = b"#li\rst".to_vec();
    input.extend(std::iter::repeat_n(b'\r', HANDSHAKE_LINE_BUFSIZ * 4));
    input.push(b'\n');
    let mut reader = BufReader::new(input.as_slice());
    let line = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ)
        .expect("read")
        .expect("line");
    assert_eq!(line, "#list");
}

/// upstream: `io.c:2645-2646` - a NUL byte fails the read outright.
#[test]
fn read_bounded_line_refuses_a_nul_byte() {
    let input: &[u8] = b"#li\0st\n";
    let mut reader = BufReader::new(input);
    let line = read_bounded_line(&mut reader, HANDSHAKE_LINE_BUFSIZ).expect("read");
    assert_eq!(line, None);
}
