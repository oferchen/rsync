/// upstream: daemon-parm.txt:34 installs DEFAULT_LOCK_FILE - rsync.h:33
/// `"/var/run/rsyncd.lock"` - as the `lock file` default, and
/// connection.c:26-46 claims every `max connections` slot through that file.
/// Sessions run in a process per connection (forked on Unix, spawned on
/// Windows), so without this default a module cap would be counted inside each
/// per-session child and never refuse anyone. Upstream uses the same string on
/// every platform, Cygwin included.
#[test]
fn runtime_options_defaults_lock_file_to_upstream_path() {
    let options = RuntimeOptions::parse(&[OsString::from("--port"), OsString::from("873")])
        .expect("parse arguments");

    assert_eq!(options.lock_file(), Some(Path::new("/var/run/rsyncd.lock")));
}

/// Daemon path options are stored as written, as loadparm.c stores them; the
/// Cygwin-style mapping applies only when the daemon opens the file
/// (`crate::posix_path`).
#[test]
fn runtime_options_store_daemon_paths_as_written() {
    let options = RuntimeOptions::parse(&[
        OsString::from("--log-file"),
        OsString::from("/var/log/oc.log"),
        OsString::from("--pid-file"),
        OsString::from("/var/run/oc.pid"),
    ])
    .expect("parse arguments");

    assert_eq!(
        options.log_file().map(PathBuf::as_path),
        Some(Path::new("/var/log/oc.log"))
    );
    assert_eq!(options.pid_file(), Some(Path::new("/var/run/oc.pid")));
}
