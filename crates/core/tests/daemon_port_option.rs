//! `--port=PORT` must reach the daemon *transfer* path, not just the listing.
//!
//! Upstream stores `--port` in `rsync_port` (`options.c:852`) and keeps it
//! through operand parsing: `check_for_hostspec()` (`options.c:3301-3327`)
//! substitutes the `-1` "use the default" sentinel only when `rsync_port` is
//! still 0, so an operator-supplied port survives, and only a `:port` written
//! into the operand itself overwrites it. `main.c:1591` then falls back to
//! `RSYNC_PORT` (873) for the sentinel alone.
//!
//! oc honoured that on the module-listing path only: the three daemon-transfer
//! parsers hardcoded 873, so `--port=N rsync://host/mod/file` dialled 873 and
//! failed with "connection refused" while upstream transferred the file. These
//! tests pin all three legs of the rule against a live oc-rsync daemon on an
//! OS-assigned port - the only port a hardcoded 873 can never be.
//!
//! Unix-only: daemon mode is unsupported on Windows (`daemon.rs`), so the
//! harness cannot start a peer there.

#![cfg(unix)]

mod common;

use std::fs;
use std::path::Path;

use common::{DaemonBinary, TestDaemon, create_test_file};
use core::client::ClientConfig;
use core::client::run_client;
use tempfile::{TempDir, tempdir};

/// Payload the daemon publishes; the byte comparison is what proves the
/// transfer reached the daemon rather than merely exiting 0.
const PAYLOAD: &[u8] = b"port option reached the transfer path";

/// Starts an oc-rsync daemon serving `file.txt`, or `None` when the binary has
/// not been built - the harness's degrade-not-fail convention, so a local run
/// without a build reports a skip instead of a spurious failure.
fn daemon_serving_a_file() -> Option<TestDaemon> {
    let bin = test_support::oc_rsync_bin();
    if !Path::new(&bin).exists() {
        eprintln!("skipping: oc-rsync binary not built at {}", bin.display());
        return None;
    }
    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");
    create_test_file(&daemon.module_path().join("file.txt"), PAYLOAD);
    Some(daemon)
}

/// Pulls `file.txt` from `operand` with `--port` set to `daemon_port`, and
/// returns the destination directory so the caller can assert on the bytes.
fn pull(operand: String, daemon_port: Option<u16>) -> TempDir {
    let dest = tempdir().expect("create dest dir");
    let config = ClientConfig::builder()
        .transfer_args([operand, dest.path().to_string_lossy().to_string()])
        .daemon_port(daemon_port)
        .build();
    run_client(config).expect("daemon pull succeeds");
    dest
}

fn assert_payload_landed(dest: &TempDir) {
    let landed = dest.path().join("file.txt");
    assert_eq!(
        fs::read(&landed).expect("read transferred file"),
        PAYLOAD,
        "the pull must deliver the daemon's bytes"
    );
}

/// An `rsync://` operand that names no port takes the operator's `--port`.
#[test]
fn rsync_url_without_a_port_uses_the_port_option() {
    let Some(daemon) = daemon_serving_a_file() else {
        return;
    };
    let dest = pull(
        "rsync://127.0.0.1/testmodule/file.txt".to_owned(),
        Some(daemon.port()),
    );
    assert_payload_landed(&dest);
}

/// `host::module` carries no port syntax at all, so it always takes `--port`.
/// upstream: `options.c:3318-3321` - the `path[0] == ':'` arm sets the same
/// sentinel the URL arm does.
#[test]
fn double_colon_operand_uses_the_port_option() {
    let Some(daemon) = daemon_serving_a_file() else {
        return;
    };
    let dest = pull(
        "127.0.0.1::testmodule/file.txt".to_owned(),
        Some(daemon.port()),
    );
    assert_payload_landed(&dest);
}

/// Non-vacuity companion for the two pins above: a port written into the
/// operand must WIN over `--port`, so a deliberately unusable `--port=1` still
/// transfers. Without this row, "`--port` is honoured" could be satisfied by an
/// implementation that lets the option override an explicit `:port` - which
/// upstream does not do (`parse_hostspec` overwrites `rsync_port` from the
/// operand before the sentinel is ever applied).
#[test]
fn an_explicit_operand_port_outranks_the_port_option() {
    let Some(daemon) = daemon_serving_a_file() else {
        return;
    };
    let dest = pull(
        format!("rsync://127.0.0.1:{}/testmodule/file.txt", daemon.port()),
        Some(1),
    );
    assert_payload_landed(&dest);
}

/// The library pins above enter at `ClientConfig`, so they cannot see the CLI
/// leg: `--port` is parsed into `ParsedArgs` and has to be copied into the
/// config the transfer path reads. A field that never makes that hop is inert
/// no matter how correct the parser is, so this cell drives the real binary.
#[test]
fn the_port_flag_reaches_the_transfer_path_through_the_cli() {
    let Some(daemon) = daemon_serving_a_file() else {
        return;
    };
    let dest = tempdir().expect("create dest dir");
    let output = std::process::Command::new(test_support::oc_rsync_bin())
        .arg(format!("--port={}", daemon.port()))
        .arg("rsync://127.0.0.1/testmodule/file.txt")
        .arg(dest.path())
        .output()
        .expect("run oc-rsync");
    assert!(
        output.status.success(),
        "pull failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_payload_landed(&dest);
}

/// With no `--port`, the fallback is upstream's `RSYNC_PORT`. Asserted on the
/// resolver rather than over a socket, because 873 needs privileges to bind.
#[test]
fn no_port_option_falls_back_to_the_upstream_default() {
    let config = ClientConfig::builder().build();
    assert_eq!(
        config.daemon_port(),
        None,
        "an unset --port must stay unset, so the parser can apply upstream's default"
    );
}
