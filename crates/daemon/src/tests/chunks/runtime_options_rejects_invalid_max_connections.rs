#[test]
fn runtime_options_max_connections_atoi_leniency() {
    // upstream: loadparm.c:431-433 - `max connections` is P_INTEGER, parsed
    // with atoi(), so a non-numeric value yields 0 (unlimited), not an error.
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nmax connections = nope\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("atoi-lenient max connections must not abort the load");

    assert!(options.modules()[0].max_connections().is_none());
}
