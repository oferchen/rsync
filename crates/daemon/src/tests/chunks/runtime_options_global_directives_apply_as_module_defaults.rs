#[test]
fn runtime_options_global_directives_apply_as_module_defaults() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "incoming chmod = Dg+s\n\
         outgoing chmod = Fo-w\n\
         \n\
         [alpha]\n\
         path = /srv/alpha\n\
         \n\
         [beta]\n\
         path = /srv/beta\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);

    // Both modules should inherit global chmod defaults
    assert_eq!(modules[0].incoming_chmod(), Some("Dg+s"));
    assert_eq!(modules[0].outgoing_chmod(), Some("Fo-w"));
    assert_eq!(modules[1].incoming_chmod(), Some("Dg+s"));
    assert_eq!(modules[1].outgoing_chmod(), Some("Fo-w"));
}
