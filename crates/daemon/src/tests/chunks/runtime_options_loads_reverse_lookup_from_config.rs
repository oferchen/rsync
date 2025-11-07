#[test]
fn runtime_options_loads_reverse_lookup_from_config() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "reverse lookup = no\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ];
    let options = RuntimeOptions::parse(&args).expect("parse config");
    assert!(!options.reverse_lookup());
}

