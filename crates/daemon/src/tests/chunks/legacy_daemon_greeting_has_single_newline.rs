#[test]
fn legacy_daemon_greeting_has_single_newline() {
    let greeting = legacy_daemon_greeting();

    // Greeting should end with exactly one newline
    assert!(greeting.ends_with('\n'));
    assert!(!greeting.ends_with("\n\n"), "should not have double newline");

    // The newline should be at the very end
    let newline_count = greeting.matches('\n').count();
    assert_eq!(newline_count, 1, "should have exactly one newline");
}
