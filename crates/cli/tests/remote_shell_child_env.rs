//! The environment a `-e` remote-shell child inherits from the client.
//!
//! Each case runs the client against a fake remote shell that records
//! `RSYNC_PORT` and `DISPLAY` and then fails, so no daemon is needed. Values
//! were measured against the rsync 3.5.2 reference binary.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// What the fake remote shell saw, as `PORT=<v> DISPLAY=<v>`. A local
/// destination is appended unless the run is a module listing.
fn child_env(args: &[&str], listing: bool, configure: impl FnOnce(&mut Command)) -> String {
    let dir = TempDir::new().expect("tempdir");
    let record = dir.path().join("record");
    let rsh = dir.path().join("rsh.sh");
    fs::write(
        &rsh,
        format!(
            "#!/bin/sh\necho \"PORT=${{RSYNC_PORT-unset}} DISPLAY=${{DISPLAY-unset}}\" > '{}'\nexit 1\n",
            record.display()
        ),
    )
    .expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    let mut cmd = Command::new(oc_rsync_bin());
    cmd.arg("-e").arg(&rsh).args(args);
    if !listing {
        cmd.arg(dir.path().join("dest"));
    }
    cmd.env_remove("RSYNC_PORT").env("DISPLAY", ":9");
    cmd.env_remove("SSH_ASKPASS");
    configure(&mut cmd);
    let output = cmd.output().expect("run oc-rsync");
    assert!(!output.status.success(), "the fake shell must fail the run");
    read_record(&record)
}

fn read_record(record: &Path) -> String {
    fs::read_to_string(record)
        .unwrap_or_else(|e| panic!("remote shell never ran ({e})"))
        .trim_end()
        .to_owned()
}

/// upstream: main.c:1474 + main.c:1613-1616 + main.c:1653-1654 - the child
/// sees `RSYNC_PORT` as the port the user named (`--port` or the operand's
/// `:port`), or 0 when none was named; the 873 fallback is for dialling only.
#[test]
fn daemon_over_rsh_exports_the_named_port_or_zero() {
    let port = |args: &[&str]| {
        child_env(args, false, |_| {})
            .split(' ')
            .next()
            .unwrap()
            .to_owned()
    };
    assert_eq!(port(&["h::mod/"]), "PORT=0");
    assert_eq!(port(&["rsync://h/mod/"]), "PORT=0");
    assert_eq!(port(&["rsync://h:2222/mod/"]), "PORT=2222");
    assert_eq!(port(&["--port=99", "h::mod/"]), "PORT=99");
}

/// The module listing reaches the daemon through the same remote shell, so it
/// exports the same `RSYNC_PORT`.
#[test]
fn module_listing_over_rsh_exports_zero_without_a_port() {
    let seen = child_env(&["h::"], true, |_| {});
    assert!(seen.starts_with("PORT=0 "), "{seen}");
}

/// upstream: main.c:1879-1881 - `DISPLAY` is dropped before any remote shell
/// starts unless `SSH_ASKPASS` is set, so ssh cannot reach an X display the
/// user did not ask it to use for a passphrase prompt.
#[test]
fn display_reaches_the_remote_shell_only_with_ssh_askpass() {
    let display = |set_askpass: bool| {
        child_env(&["h:src"], false, |cmd| {
            if set_askpass {
                cmd.env("SSH_ASKPASS", "/usr/bin/ssh-askpass");
            }
        })
        .split(' ')
        .nth(1)
        .unwrap()
        .to_owned()
    };
    assert_eq!(display(false), "DISPLAY=unset");
    assert_eq!(display(true), "DISPLAY=:9");
}
