#[test]
fn runtime_options_timeout_atoi_leniency() {
    // upstream: loadparm.c:431-433 - `timeout` is P_INTEGER, parsed with
    // atoi(), so a non-numeric value yields 0 (disabled) rather than an error.
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\ntimeout = never\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("atoi-lenient timeout must not abort the load");

    assert!(options.modules()[0].timeout().is_none());
}
