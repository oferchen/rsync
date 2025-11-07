#[test]
fn runtime_options_loads_global_chmod_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "incoming chmod = Duog\noutgoing chmod = Fugo\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    let module = &modules[0];
    assert_eq!(module.name, "docs");
    assert_eq!(module.incoming_chmod(), Some("Duog"));
    assert_eq!(module.outgoing_chmod(), Some("Fugo"));
}
