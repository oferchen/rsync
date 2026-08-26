#[test]
fn runtime_options_global_bwlimit_respects_cli_override() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 3M\n[docs]\npath = /srv/docs\n").expect("write config");

    // upstream: options.c:862 - the daemon --bwlimit is a bare KiB integer;
    // 8192 KiB = 8 MiB/s. The CLI value overrides the config `bwlimit`.
    let options = RuntimeOptions::parse(&[
        OsString::from("--bwlimit"),
        OsString::from("8192"),
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config with cli override");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(8 * 1024 * 1024).unwrap())
    );
}
