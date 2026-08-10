#[test]
fn runtime_options_parse_bwlimit_argument() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8M")])
        .expect("parse bwlimit");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

