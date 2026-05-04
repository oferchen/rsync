#[test]
fn runtime_options_module_overrides_chmod_directives() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "incoming chmod = Duog\noutgoing chmod = Fugo\n[docs]\npath = /srv/docs\nincoming chmod = Fu\noutgoing chmod = Do\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let module = &options.modules()[0];
    assert_eq!(module.incoming_chmod(), Some("Fu"));
    assert_eq!(module.outgoing_chmod(), Some("Do"));
}
