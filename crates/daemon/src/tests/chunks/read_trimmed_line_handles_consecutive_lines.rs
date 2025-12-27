#[test]
fn read_trimmed_line_handles_consecutive_lines() {
    let input: &[u8] = b"first line\nsecond line\nthird\n";
    let mut reader = BufReader::new(input);

    let line1 = read_trimmed_line(&mut reader)
        .expect("read line 1")
        .expect("line 1 available");
    assert_eq!(line1, "first line");

    let line2 = read_trimmed_line(&mut reader)
        .expect("read line 2")
        .expect("line 2 available");
    assert_eq!(line2, "second line");

    let line3 = read_trimmed_line(&mut reader)
        .expect("read line 3")
        .expect("line 3 available");
    assert_eq!(line3, "third");

    let eof = read_trimmed_line(&mut reader).expect("eof read");
    assert!(eof.is_none());
}
