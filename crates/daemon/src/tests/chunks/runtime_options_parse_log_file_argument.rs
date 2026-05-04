#[test]
fn runtime_options_parse_log_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--log-file"),
        OsString::from("/var/log/rsyncd.log"),
    ])
    .expect("parse log file argument");

    assert_eq!(
        options.log_file(),
        Some(&PathBuf::from("/var/log/rsyncd.log"))
    );
}

