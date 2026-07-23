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
fn parse_args_uses_rsync_partial_dir_env_with_partial() {
    // upstream: options.c:2448-2451 - RSYNC_PARTIAL_DIR is consulted only when
    // keep_partial is active (--partial/-P) and no explicit --partial-dir was
    // given.
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", OsStr::new(".env-partial"));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--partial"),
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
fn parse_args_ignores_rsync_partial_dir_env_without_partial() {
    // upstream: options.c:2448 `if (keep_partial && !partial_dir && !am_server)`
    // - without --partial/-P the env var is ignored and does not enable partial.
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", OsStr::new(".env-partial"));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert!(!parsed.partial);
    assert!(parsed.partial_dir.is_none());
}

#[test]
fn parse_args_treats_dot_rsync_partial_dir_env_as_unset() {
    // upstream: options.c:2452-2454 - a "." partial dir collapses to NULL.
    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let _guard = EnvGuard::set("RSYNC_PARTIAL_DIR", OsStr::new("."));

    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--partial"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    // --partial still enables partial, but the "." env value contributes no dir.
    assert!(parsed.partial);
    assert!(parsed.partial_dir.is_none());
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
