#[test]
fn runtime_options_parse_bwlimit_argument() {
    // upstream: options.c:862 `{"bwlimit", 0, POPT_ARG_INT, &daemon_bwlimit, ...}`
    // - the daemon --bwlimit is a bare KiB integer (no size suffix, no :BURST).
    // 8192 KiB = 8 MiB/s.
    let options = RuntimeOptions::parse(&[OsString::from("--bwlimit"), OsString::from("8192")])
        .expect("parse bwlimit");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_limit_configured());
}

