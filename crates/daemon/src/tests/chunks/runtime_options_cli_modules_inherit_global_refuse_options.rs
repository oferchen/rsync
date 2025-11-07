#[test]
fn runtime_options_cli_modules_inherit_global_refuse_options() {
    let mut file = NamedTempFile::new().expect("config file");
    writeln!(file, "refuse options = compress\n").expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        file.path().as_os_str().to_os_string(),
        OsString::from("--module"),
        OsString::from("extra=/srv/extra"),
    ])
    .expect("parse config with cli module");

    assert_eq!(options.modules()[0].refused_options(), ["compress"]);
}

