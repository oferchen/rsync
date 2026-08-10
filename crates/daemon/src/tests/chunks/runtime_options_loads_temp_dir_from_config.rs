#[test]
fn runtime_options_loads_temp_dir_from_config() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(
        file,
        "[staging]\npath = /srv/staging\nuse chroot = no\ntemp dir = /tmp/rsync-temp\n"
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
    assert_eq!(module.temp_dir(), Some("/tmp/rsync-temp"));
}
