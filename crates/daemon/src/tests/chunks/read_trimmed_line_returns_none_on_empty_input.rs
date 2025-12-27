#[test]
fn read_trimmed_line_returns_none_on_empty_input() {
    let input: &[u8] = b"";
    let mut reader = BufReader::new(input);

    let result = read_trimmed_line(&mut reader).expect("read should succeed");
    assert!(result.is_none(), "empty input should return None");
}
