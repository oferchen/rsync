#[test]
fn runtime_options_allows_relative_path_when_use_chroot_disabled() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "[docs]\npath = data/docs\nuse chroot = no\n",).expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
    ])
    .expect("parse config modules");

    let modules = options.modules();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].path, PathBuf::from("data/docs"));
    assert!(!modules[0].use_chroot());
}

