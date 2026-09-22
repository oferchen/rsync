/// upstream: daemon-parm.h:178 installs DEFAULT_LOCK_FILE - rsync.h:33
/// `"/var/run/rsyncd.lock"` - as the `lock file` default, and
/// connection.c:26-46 claims every `max connections` slot through that file.
/// Sessions fork per connection on Unix, so without this default a module cap
/// would be counted inside each per-session child and never refuse anyone.
#[cfg(unix)]
#[test]
fn runtime_options_defaults_lock_file_to_upstream_path() {
    let options = RuntimeOptions::parse(&[OsString::from("--port"), OsString::from("873")])
        .expect("parse arguments");

    assert_eq!(options.lock_file(), Some(Path::new("/var/run/rsyncd.lock")));
}
