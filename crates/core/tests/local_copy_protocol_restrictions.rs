//! A local copy still runs upstream's `setup_protocol()` at the requested
//! `--protocol=N`, so options the protocol cannot carry are refused before any
//! file is touched. `local_server` exempts only `--acls`/`--xattrs`.
//!
//! upstream: compat.c:663-676 (local_server exemption), compat.c:684-713.

use core::client::{ClientConfig, ClientConfigBuilder, run_client};
use protocol::ProtocolVersion;
use std::fs;
use tempfile::tempdir;

/// Runs `src/ -> dst/` at `protocol` and returns the exit code and message,
/// or `None` when the copy succeeds.
fn run_at(
    protocol: u8,
    set: fn(ClientConfigBuilder) -> ClientConfigBuilder,
) -> Option<(i32, String)> {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("src");
    fs::create_dir(&src).expect("src");
    fs::write(src.join("f"), b"x").expect("file");
    let mut source = src.into_os_string();
    source.push("/");
    let builder = ClientConfig::builder()
        .transfer_args([source, dir.path().join("dst").into_os_string()])
        .recursive(true)
        .protocol_version(Some(ProtocolVersion::try_from(protocol).expect("protocol")));
    run_client(set(builder).build())
        .err()
        .map(|e| (e.exit_code(), e.message().to_string()))
}

#[test]
fn prune_empty_dirs_below_protocol_29_is_refused_with_rerr_protocol() {
    let (code, message) =
        run_at(28, |b| b.prune_empty_dirs(true)).expect("protocol 28 must refuse -m");
    assert_eq!(code, 2);
    assert!(
        message.contains("--prune-empty-dirs requires protocol 29 or higher (negotiated 28)."),
        "{message}"
    );
    assert!(run_at(29, |b| b.prune_empty_dirs(true)).is_none());
}

#[test]
fn fuzzy_below_protocol_29_is_refused_with_rerr_protocol() {
    let (code, message) = run_at(28, |b| b.fuzzy_level(1)).expect("protocol 28 must refuse -y");
    assert_eq!(code, 2);
    assert!(
        message.contains("--fuzzy requires protocol 29 or higher (negotiated 28)."),
        "{message}"
    );
}

/// Opposed control: `local_server` exempts a local copy from the ACL gate.
#[cfg(all(unix, feature = "acl"))]
#[test]
fn acls_below_protocol_30_are_allowed_locally() {
    assert!(run_at(29, |b| b.acls(true)).is_none());
}
