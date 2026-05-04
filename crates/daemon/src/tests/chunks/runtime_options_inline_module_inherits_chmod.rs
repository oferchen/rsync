#[test]
fn runtime_options_inline_module_inherits_chmod() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "incoming chmod = Duog\noutgoing chmod = Fugo\n[docs]\npath = /srv/docs\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from("cli=/srv/cli"),
    ])
    .expect("parse config and inline module");

    let cli_module = options
        .modules()
        .iter()
        .find(|module| module.name == "cli")
        .expect("find cli module");
    assert_eq!(cli_module.incoming_chmod(), Some("Duog"));
    assert_eq!(cli_module.outgoing_chmod(), Some("Fugo"));
}
