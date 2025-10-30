#[test]
fn runtime_options_parse_bwlimit_unlimited() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("0")])
        .expect("parse unlimited");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

