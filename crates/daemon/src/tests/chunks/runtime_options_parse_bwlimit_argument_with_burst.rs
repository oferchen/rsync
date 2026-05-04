#[test]
fn runtime_options_parse_bwlimit_argument_with_burst() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8M:12M")])
        .expect("parse bwlimit with burst");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(12 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_limit_configured());
}

