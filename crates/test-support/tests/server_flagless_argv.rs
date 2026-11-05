//! A push with no short option reaches the server as `--server . DEST`: no
//! compact flag string at all.
//!
//! ```c
//! /* options.c:2905-2907 server_options() */
//! if (x > 1)
//!         args[ac++] = argstr;
//! ```
//!
//! At protocol 30+ the client always has one, because `maybe_add_e_option()`
//! appends the `-e.` capability letters. Below 30 nothing is appended, so a
//! flagless push sends `rsync --server . DEST`. 3.5.1's server parses that argv
//! with the same popt table as any other, consumes `.` as its chdir slot and
//! DEST as the destination (main.c:1207-1214 `do_server_recv()`), and
//! transfers. The oc server used to refuse it with "missing rsync server flag
//! string", which the client reported as exit 12.
//!
//! Each cell records the server argv through a stub remote shell, so it also
//! proves the argv under test really has the shape it claims: no flag string
//! at protocol 28 and 29, and only the `-e.` capability string at protocol 32,
//! the control cell that shows where the flagless shape ends.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use test_support::upstream_compat::{UpstreamVersion, require_upstream_rsync};

const PAYLOAD: &[u8] = b"flagless payload\n";

/// Writes a remote shell that logs the server argv, one token per line, then
/// execs it - the `host` token is dropped like ssh does.
fn argv_logging_rsh(dir: &Path) -> std::path::PathBuf {
    let rsh = dir.join("rsh");
    let log = dir.join("argv.log");
    let script = format!(
        "#!/bin/sh\nshift\nprintf '%s\\n' \"$@\" > '{}'\nexec \"$@\"\n",
        log.display()
    );
    fs::write(&rsh, script).expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    rsh
}

/// Pushes one file with no option but `--protocol` from `client` to the oc
/// server and returns the server argv the remote shell saw.
fn flagless_push(client: &Path, protocol: u8) -> Vec<String> {
    let work = test_support::create_tempdir();
    let src = work.path().join("a");
    let dest = work.path().join("dest");
    fs::write(&src, PAYLOAD).expect("seed a");
    fs::create_dir(&dest).expect("mkdir dest");
    let rsh = argv_logging_rsh(work.path());

    let output = Command::new(client)
        .arg(format!("--protocol={protocol}"))
        .arg("-e")
        .arg(&rsh)
        .arg("--rsync-path")
        .arg(test_support::oc_rsync_bin())
        .arg(&src)
        .arg({
            let mut operand = OsString::from("fakehost:");
            operand.push(&dest);
            operand.push("/");
            operand
        })
        .env_remove("RSYNC_RSH")
        .env_remove("RSYNC_PROTECT_ARGS")
        .output()
        .expect("run push");

    assert_eq!(
        output.status.code(),
        Some(0),
        "flagless push at protocol {protocol} must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(dest.join("a")).expect("read pushed a"), PAYLOAD);

    let argv = fs::read_to_string(work.path().join("argv.log")).expect("read argv log");
    argv.lines().map(str::to_owned).collect()
}

/// The compact flag string the server received, if any.
fn compact_flag_string(argv: &[String]) -> Option<&str> {
    let server = argv.iter().position(|a| a == "--server").expect("--server");
    argv[server + 1..]
        .iter()
        .find(|a| a.starts_with('-') && !a.starts_with("--"))
        .map(String::as_str)
}

/// The argv must have the shape the cell claims, or it does not test the
/// defect: no flag string below protocol 30, where `maybe_add_e_option()`
/// adds nothing, and only the `-e.` capability string from 30 on.
fn assert_argv_shape(argv: &[String], protocol: u8) {
    let flags = compact_flag_string(argv);
    if protocol < 30 {
        assert_eq!(flags, None, "no compact flag string: {argv:?}");
    } else {
        assert!(
            flags.is_some_and(|f| f.starts_with("-e.")),
            "only the capability string: {argv:?}"
        );
    }
}

fn oc_client_cell(protocol: u8) {
    let argv = flagless_push(&test_support::oc_rsync_bin(), protocol);
    assert_argv_shape(&argv, protocol);
}

fn upstream_client_cell(protocol: u8) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let argv = flagless_push(upstream.binary(), protocol);
    assert_argv_shape(&argv, protocol);
}

#[test]
fn oc_client_flagless_push_at_protocol_28_is_served() {
    oc_client_cell(28);
}

#[test]
fn oc_client_flagless_push_at_protocol_29_is_served() {
    oc_client_cell(29);
}

#[test]
fn upstream_client_flagless_push_at_protocol_28_is_served() {
    upstream_client_cell(28);
}

#[test]
fn upstream_client_flagless_push_at_protocol_29_is_served() {
    upstream_client_cell(29);
}

#[test]
fn oc_client_flagless_push_at_protocol_32_is_served() {
    oc_client_cell(32);
}

#[test]
fn upstream_client_flagless_push_at_protocol_32_is_served() {
    upstream_client_cell(32);
}
