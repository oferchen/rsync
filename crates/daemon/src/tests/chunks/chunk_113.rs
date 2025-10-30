#[test]
fn runtime_options_parse_motd_sources() {
    let dir = tempdir().expect("motd dir");
    let motd_path = dir.path().join("motd.txt");
    fs::write(&motd_path, "Welcome to rsyncd\nSecond line\n").expect("write motd");

    let options = RuntimeOptions::parse(&[
        OsString::from("--motd-file"),
        motd_path.as_os_str().to_os_string(),
        OsString::from("--motd-line"),
        OsString::from("Trailing notice"),
    ])
    .expect("parse motd options");

    let expected = vec![
        String::from("Welcome to rsyncd"),
        String::from("Second line"),
        String::from("Trailing notice"),
    ];

    assert_eq!(options.motd_lines(), expected.as_slice());
}

