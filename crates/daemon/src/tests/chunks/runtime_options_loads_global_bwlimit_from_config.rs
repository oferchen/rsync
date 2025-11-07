#[test]
fn runtime_options_loads_global_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "bwlimit = 3M:12M\n[docs]\npath = /srv/docs\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    assert_eq!(
        options.bandwidth_limit(),
        Some(NonZeroU64::new(3 * 1024 * 1024).unwrap())
    );
    assert_eq!(
        options.bandwidth_burst(),
        Some(NonZeroU64::new(12 * 1024 * 1024).unwrap())
    );
    assert!(options.bandwidth_limit_configured());

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert!(modules[0].bandwidth_limit().is_none());
}

