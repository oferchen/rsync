#[test]
fn runtime_options_loads_motd_from_config_directives() {
    let dir = tempdir().expect("motd dir");
    let config_path = dir.path().join("rsyncd.conf");
    let motd_path = dir.path().join("motd.txt");
    fs::write(&motd_path, "First line\nSecond line\r\n").expect("write motd file");

    fs::write(
        &config_path,
        "motd file = motd.txt\nmotd = Inline note\n[docs]\npath = /srv/docs\n",
    )
    .expect("write config");

    let options = RuntimeOptions::parse(&[
        OsString::from("--config"),
        config_path.as_os_str().to_os_string(),
    ])
    .expect("parse config with motd directives");

    let expected = vec![
        String::from("First line"),
        String::from("Second line"),
        String::from("Inline note"),
    ];

    assert_eq!(options.motd_lines(), expected.as_slice());
}

