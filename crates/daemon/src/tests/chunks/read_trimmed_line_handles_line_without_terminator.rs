#[test]
fn read_trimmed_line_handles_line_without_terminator() {
    // Input without trailing newline
    let input: &[u8] = b"no terminator";
    let mut reader = BufReader::new(input);

    let line = read_trimmed_line(&mut reader)
        .expect("read line")
        .expect("line available");

    assert_eq!(line, "no terminator");

    // Next read should return None (EOF)
    let eof = read_trimmed_line(&mut reader).expect("eof read");
    assert!(eof.is_none());
}
