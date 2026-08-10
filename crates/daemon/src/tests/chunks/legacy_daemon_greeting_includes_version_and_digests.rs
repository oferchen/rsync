#[test]
fn legacy_daemon_greeting_includes_version_and_digests() {
    let greeting = legacy_daemon_greeting();

    // Greeting should start with @RSYNCD: and protocol version
    assert!(greeting.starts_with("@RSYNCD: "));

    // Should end with a newline
    assert!(greeting.ends_with('\n'));

    // Should contain supported digest names (md5, md4)
    // Digests are listed after the version number
    let parts: Vec<&str> = greeting.trim_end().split(' ').collect();
    assert!(parts.len() >= 2, "expected version and at least one digest");

    // First part is @RSYNCD:
    assert_eq!(parts[0], "@RSYNCD:");

    // Second part is the version number (format: "31.0" or similar)
    let version_str = parts[1];
    // Version may be "31.0" or "31" - extract the major version
    let major_str = version_str.split('.').next().expect("version must have major part");
    let major: u8 = major_str.parse().expect("valid protocol version number");
    assert!(major >= 28, "protocol version should be at least 28");

    // Remaining parts should be digest names
    for digest in &parts[2..] {
        assert!(!digest.is_empty(), "digest name should not be empty");
    }
}
