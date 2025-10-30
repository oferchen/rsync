#[test]
fn runtime_options_rejects_duplicate_reverse_lookup_directive() {
    let dir = tempdir().expect("config dir");
    let config_path = dir.path().join("rsyncd.conf");
    fs::write(
        &config_path,
        "reverse lookup = yes\nreverse lookup = no\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let args = [
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ];
    let error = RuntimeOptions::parse(&args).expect_err("duplicate reverse lookup");
    assert!(format!("{error}").contains("reverse lookup"));
}

