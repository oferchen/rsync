#[test]
fn runtime_options_loads_modules_from_included_config() {
    let dir = tempdir().expect("tempdir");
    let include_path = dir.path().join("modules.conf");
    writeln!(
        File::create(&include_path).expect("create include"),
        "[docs]\npath = /srv/docs\n"
    )
    .expect("write include");

    let main_path = dir.path().join("rsyncd.conf");
    writeln!(
        File::create(&main_path).expect("create config"),
        "include = modules.conf\n"
    )
    .expect("write main config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        main_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with include");

    assert_eq!(options.modules().len(), 1);
    assert_eq!(options.modules()[0].name(), "docs");
}

