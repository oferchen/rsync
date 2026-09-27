/// An empty `refuse options` loads and refuses nothing.
///
/// upstream: loadparm.c:do_parameter stores "" for an empty P_STRING, and an
/// empty refuse list refuses no option.
#[test]
fn runtime_options_accepts_empty_refuse_options_directive() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = /srv/docs\nrefuse options =   \n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("an empty refuse options must load");

    assert!(options.modules()[0].refuse_options.is_empty());
}
