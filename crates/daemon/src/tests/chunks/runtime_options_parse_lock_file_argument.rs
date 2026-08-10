#[test]
fn runtime_options_parse_lock_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--lock-file"),
        OsString::from("/var/run/rsyncd.lock"),
    ])
    .expect("parse lock file argument");

    assert_eq!(options.lock_file(), Some(Path::new("/var/run/rsyncd.lock")));
}

