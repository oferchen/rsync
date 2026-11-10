//! A server whose own argv carries `-M`/`--remote-option` or a stray `-s`.
//!
//! upstream: a server only records a `-M` value (options.c:1762-1777); a value
//! without a leading dash is a syntax error (options.c:1764-1767). A server
//! told `-s` reads its argument list from the stream and, finding no
//! operands there, prints its usage and exits RERR_SYNTAX (main.c:1956-1969).
//! oc used to read the `-M` value as its flag string, and an `s` in it turned
//! on secluded args, which swallowed the client's protocol version and hung
//! both sides. Every run here is deadlined so a hang fails as a TIMEOUT.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined};

const BUDGET: Duration = Duration::from_secs(30);

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// The clients to drive each server with: always oc, plus 3.5.1 when present.
fn clients() -> Vec<(&'static str, PathBuf)> {
    let mut clients = vec![("oc", oc_rsync())];
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) {
        clients.push(("upstream", upstream.binary().to_path_buf()));
    }
    clients
}

struct Push {
    _temp: tempfile::TempDir,
    status: Option<i32>,
    stderr: String,
    dest: PathBuf,
}

/// Pushes `src/` to an oc server whose argv starts with `extra`.
fn push(client: &Path, extra: &[&str]) -> Push {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("src/d")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    fs::write(root.join("src/d/f"), "x\n").unwrap();
    // A remote shell that runs the server locally with `extra` placed right
    // after its program name, as `--rsync-path="oc-rsync <extra>"` would.
    let quoted: Vec<String> = extra.iter().map(|a| format!("'{a}'")).collect();
    let rsh = root.join("rsh");
    fs::write(
        &rsh,
        format!(
            "#!/bin/sh\nshift\nprog=$1\nshift\nexec \"$prog\" {} \"$@\"\n",
            quoted.join(" ")
        ),
    )
    .unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    let dest = root.join("dst");
    let mut command = Command::new(client);
    command
        .arg("-r")
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", oc_rsync().display()))
        .arg(format!("{}/", root.join("src").display()))
        .arg(format!("localhost:{}/", dest.display()));
    match run_deadlined(&mut command, BUDGET).unwrap() {
        Deadlined::Finished { status, stderr, .. } => Push {
            _temp: temp,
            status: status.code(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            dest,
        },
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "TIMEOUT: push with server argv {extra:?} still running after {budget:?}: {}",
            String::from_utf8_lossy(&stderr)
        ),
    }
}

fn has_stash(path: &Path) -> bool {
    Command::new("getfattr")
        .args(["-d", "-m", r"^user\.rsync\."])
        .arg(path)
        .output()
        .is_ok_and(|out| !out.stdout.is_empty())
}

/// Every spelling of a remote option is recorded and ignored: the push
/// completes and the value never takes effect on the server.
#[test]
fn remote_options_on_the_server_argv_are_ignored() {
    for (label, client) in clients() {
        for extra in [
            &["-M--fake-super"][..],
            &["-M", "--fake-super"],
            &["--remote-option=--fake-super"],
            &["--remote-option", "--fake-super"],
        ] {
            let push = push(&client, extra);
            assert_eq!(
                push.status,
                Some(0),
                "{label} client, server argv {extra:?}: {}",
                push.stderr
            );
            let file = push.dest.join("d/f");
            assert_eq!(fs::read_to_string(&file).unwrap(), "x\n", "{extra:?}");
            assert!(
                !has_stash(&file),
                "{label} client, {extra:?}: the -M value took effect on the server"
            );
        }
    }
}

/// A `-M` value without a leading dash is refused before the handshake.
#[test]
fn remote_option_without_a_dash_is_a_syntax_error() {
    for (label, client) in clients() {
        let push = push(&client, &["-Mfoo"]);
        assert_ne!(push.status, Some(0), "{label}: {}", push.stderr);
        assert!(
            push.stderr
                .contains("Remote option must start with a dash: foo"),
            "{label}: {}",
            push.stderr
        );
        assert!(!push.dest.join("d").exists(), "{label}: transferred anyway");
    }
}

/// A stray `-s` makes the server read its argument list from the stream; the
/// client sent none, so the server must fail with a syntax error, not wait.
#[test]
fn stray_secluded_args_flag_fails_instead_of_hanging() {
    for (label, client) in clients() {
        let push = push(&client, &["-s"]);
        assert_ne!(push.status, Some(0), "{label}: {}", push.stderr);
        assert!(
            push.stderr.contains("syntax or usage error (code 1)"),
            "{label}: {}",
            push.stderr
        );
    }
}
