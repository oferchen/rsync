#[test]
fn runtime_options_load_modules_from_config_file() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\ncomment = Documentation\n\n[logs]\npath=/var/log\n"
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 2);
    assert_eq!(modules[0].name, "docs");
    assert_eq!(modules[0].path, PathBuf::from("/srv/docs"));
    assert_eq!(modules[0].comment.as_deref(), Some("Documentation"));
    assert!(modules[0].bandwidth_limit().is_none());
    assert!(modules[0].bandwidth_burst().is_none());
    assert!(modules[0].listable());
    assert_eq!(modules[1].name, "logs");
    assert_eq!(modules[1].path, PathBuf::from("/var/log"));
    assert!(modules[1].comment.is_none());
    assert!(modules[1].bandwidth_limit().is_none());
    assert!(modules[1].bandwidth_burst().is_none());
    assert!(modules[1].listable());
    assert!(modules.iter().all(ModuleDefinition::use_chroot));
}

