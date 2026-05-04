#[test]
fn runtime_options_parse_no_bwlimit_argument() {
    let options =
        RuntimeOptions::parse(&[OsString::from("--no-bwlimit")]).expect("parse no-bwlimit");

    assert!(options.bandwidth_limit().is_none());
    assert!(options.bandwidth_burst().is_none());
    assert!(options.bandwidth_limit_configured());
}

