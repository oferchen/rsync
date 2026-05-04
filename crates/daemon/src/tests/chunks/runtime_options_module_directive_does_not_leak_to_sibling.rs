#[test]
fn runtime_options_module_directive_does_not_leak_to_sibling() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[customized]\n\
         path = /srv/customized\n\
         read only = no\n\
         use chroot = no\n\
         numeric ids = yes\n\
         fake super = yes\n\
         timeout = 120\n\
         \n\
         [defaults]\n\
         path = /srv/defaults\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);

    // The `customized` module has explicit overrides
    let customized = modules.iter().find(|m| m.name == "customized").unwrap();
    assert!(!customized.read_only());
    assert!(!customized.use_chroot());
    assert!(customized.numeric_ids());
    assert!(customized.fake_super());
    assert_eq!(customized.timeout().map(|t| t.get()), Some(120));

    // The `defaults` module retains all defaults, unaffected by `customized`
    let defaults = modules.iter().find(|m| m.name == "defaults").unwrap();
    assert!(defaults.read_only());
    assert!(defaults.use_chroot());
    assert!(!defaults.numeric_ids());
    assert!(!defaults.fake_super());
    assert!(defaults.timeout().is_none());
}
