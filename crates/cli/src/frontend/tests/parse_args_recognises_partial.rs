use super::common::*;
use super::*;
use std::ffi::OsStr;
use std::path::Path;

#[test]
fn parse_args_recognises_partial_dir_and_enables_partial() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--partial-dir=.rsync-partial"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.partial);
    assert_eq!(
        parsed.partial_dir.as_deref(),
        Some(Path::new(".rsync-partial"))
    );
}

#[test]
fn parse_args_uses_rsync_partial_dir_env() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", OsStr::new(".env-partial"));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(parsed.partial);
    assert_eq!(
        parsed.partial_dir.as_deref(),
        Some(Path::new(".env-partial"))
    );
}

#[test]
fn parse_args_no_partial_overrides_rsync_partial_dir_env() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", OsStr::new(".env-partial"));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--no-partial"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.partial);
    assert!(parsed.partial_dir.is_none());
}

#[test]
fn parse_args_ignores_empty_rsync_partial_dir_env() {
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", OsStr::new(""));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.partial);
    assert!(parsed.partial_dir.is_none());
}
