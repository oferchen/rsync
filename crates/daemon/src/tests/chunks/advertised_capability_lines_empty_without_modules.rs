#[test]
fn advertised_capability_lines_empty_without_modules() {
    assert!(advertised_capability_lines(&[]).is_empty());
}

