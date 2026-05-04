#[test]
fn runtime_options_parse_bwlimit_unlimited_ignores_burst() {
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("0:512K")])
        .expect("parse unlimited with burst");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

