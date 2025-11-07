#[test]
fn runtime_options_loads_max_connections_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = 7\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("config parses");

    assert_eq!(options.modules[0].max_connections(), NonZeroU32::new(7));
}

