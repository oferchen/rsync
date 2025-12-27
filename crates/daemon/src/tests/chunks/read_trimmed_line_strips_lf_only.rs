#[test]
fn read_trimmed_line_strips_lf_only() {
    let input: &[u8] = b"line content\n";
    let mut reader = BufReader::new(input);

    let line = read_trimmed_line(&mut reader)
        .expect("read line")
        .expect("line available");

    assert_eq!(line, "line content");
}
