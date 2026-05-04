#[test]
fn runtime_options_loads_bwlimit_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nbwlimit = 4M\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(
        module.bandwidth_limit(),
        Some(NonZeroU64::new(4 * 1024 * 1024).unwrap())
    );
    assert!(module.bandwidth_burst().is_none());
    assert!(!module.bandwidth_burst_specified());
}

