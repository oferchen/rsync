#[test]
fn runtime_options_parse_pid_file_argument() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--pid-file"),
        OsString::from("/var/run/rsyncd.pid"),
    ])
    .expect("parse pid file argument");

    assert_eq!(options.pid_file(), Some(Path::new("/var/run/rsyncd.pid")));
}

