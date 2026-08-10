#[test]
fn read_trimmed_line_strips_multiple_cr_lf() {
    // Input with multiple trailing CR and LF characters
    let input: &[u8] = b"data\r\n\r\n";
    let mut reader = BufReader::new(input);

    // First line should be "data"
    let line = read_trimmed_line(&mut reader)
        .expect("read line")
        .expect("line available");
    assert_eq!(line, "data");

    // Second line should be empty string (empty line between the \n\r\n)
    let line2 = read_trimmed_line(&mut reader)
        .expect("read line")
        .expect("line available");
    assert_eq!(line2, "");

    // Now we should get None (EOF)
    let eof = read_trimmed_line(&mut reader).expect("eof read");
    assert!(eof.is_none());
}
