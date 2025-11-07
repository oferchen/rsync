#[test]
fn runtime_options_loads_boolean_and_id_directives_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[docs]\npath = /srv/docs\nread only = yes\nnumeric ids = on\nuid = 1234\ngid = 4321\nlist = no\n",
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
    assert!(module.read_only());
    assert!(module.numeric_ids());
    assert_eq!(module.uid(), Some(1234));
    assert_eq!(module.gid(), Some(4321));
    assert!(!module.listable());
    assert!(module.use_chroot());
}

