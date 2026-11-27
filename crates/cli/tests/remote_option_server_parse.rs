//! Oracle cells: a server reads the options a client forwards with `-M` the
//! way rsync 3.5.2's server does.
//!
//! upstream: options.c:3251-3258 appends every `--remote-option` after the
//! other server arguments, and the server parses them with the client's own
//! long_options[] table (options.c:610-876, :1515). So `-M--log-file-format`
//! is accepted, and with no `--log-file` it changes nothing (options.c:
//! 2595-2596); `-M--chmod` tweaks each received mode (flist.c:1225-1226); a
//! switch given a value is popt's refusal, and a bad `--stderr` mode is
//! rsync's own (options.c:2061-2075).
//!
//! Each cell runs a 3.5.2 client against oc's server and against 3.5.2's,
//! through a remote shell that runs its command locally. Without a 3.5.2
//! build the oc client drives oc's server and the expected outcome stands
//! alone.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// A run's exit code, its stderr with the program name, source location and
/// version masked, and the mode of the transferred `a`.
#[derive(Debug, PartialEq)]
struct Outcome {
    code: Option<i32>,
    lines: Vec<String>,
    mode: Option<u32>,
}

fn normalise(line: &str) -> String {
    let line = line.strip_prefix("oc-").unwrap_or(line);
    let Some((head, tail)) = line.split_once(" at ") else {
        return line.to_owned();
    };
    let Some((_, trailer)) = tail.rsplit_once(" [") else {
        return line.to_owned();
    };
    let role = trailer.split_once('=').map_or(trailer, |(role, _)| role);
    format!("{head} at X [{role}=V]")
}

/// A source tree, an empty destination, and a remote shell that runs its
/// command locally, so `lh:PATH` reaches a server on this host.
fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    fs::write(root.join("src/a"), b"hello\n").unwrap();
    fs::set_permissions(root.join("src/a"), fs::Permissions::from_mode(0o644)).unwrap();
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// Pushes or pulls `src/` with `client`, starting `server` at the far end.
fn remote(client: &Path, server: &Path, options: &[&str], push: bool) -> Outcome {
    let dir = fixture();
    let root = dir.path().display().to_string();
    let (from, to) = if push {
        ("src/".to_owned(), format!("lh:{root}/dst/"))
    } else {
        (format!("lh:{root}/src/"), "dst/".to_owned())
    };
    let output = Command::new(client)
        .current_dir(dir.path())
        .arg("-a")
        .args(options)
        .arg("-e")
        .arg(dir.path().join("rsh"))
        .arg(format!("--rsync-path={}", server.display()))
        .args([from, to])
        .output()
        .expect("run rsync");
    Outcome {
        code: output.status.code(),
        lines: String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(normalise)
            .collect(),
        mode: fs::metadata(dir.path().join("dst/a"))
            .ok()
            .map(|meta| meta.permissions().mode() & 0o7777),
    }
}

/// Runs one cell: oc's server under a 3.5.2 client, 3.5.2's server under
/// the same client, or, with no 3.5.2 build, oc on both ends.
fn assert_cell(options: &[&str], push: bool, expected: &Outcome) {
    let oc = oc_rsync_bin();
    match require_upstream_rsync(UpstreamVersion::V3_5_2) {
        Some(upstream) => {
            let client = upstream.binary();
            assert_eq!(
                &remote(client, &oc, options, push),
                expected,
                "oc server, {options:?} push={push}"
            );
            assert_eq!(
                &remote(client, client, options, push),
                expected,
                "3.5.2 server, {options:?} push={push}"
            );
        }
        None => assert_eq!(
            &remote(&oc, &oc, options, push),
            expected,
            "{options:?} push={push}"
        ),
    }
}

fn completed(mode: u32) -> Outcome {
    Outcome {
        code: Some(0),
        lines: Vec::new(),
        mode: Some(mode),
    }
}

/// The server's refusal of `text`, then the client's report of the closed
/// stream.
fn refused(text: &str, push: bool) -> Outcome {
    let client = if push { "sender" } else { "Receiver" };
    Outcome {
        code: Some(12),
        lines: vec![
            format!("rsync: {text}"),
            "rsync error: syntax or usage error (code 1) at X [server=V]".to_owned(),
            format!("rsync: connection unexpectedly closed (0 bytes received so far) [{client}]"),
            format!("rsync error: error in rsync protocol data stream (code 12) at X [{client}=V]"),
        ],
        mode: None,
    }
}

#[test]
fn log_file_format_without_a_log_file_changes_nothing() {
    for push in [true, false] {
        assert_cell(&["-M--log-file-format=%o:%n"], push, &completed(0o644));
    }
}

/// The server tweaks the modes it receives on a push and the ones it sends
/// on a pull (flist.c:1225-1226, :1970-1971).
#[test]
fn chmod_tweaks_each_mode_the_server_handles() {
    for push in [true, false] {
        assert_cell(&["-M--chmod=go-rwx"], push, &completed(0o600));
    }
}

#[test]
fn malformed_remote_options_keep_3_5_2_refusals() {
    for push in [true, false] {
        assert_cell(
            &["-M--verbose=1"],
            push,
            &refused(
                "on remote machine: --verbose=1: option does not take an argument",
                push,
            ),
        );
        assert_cell(
            &["-M--stderr=bogus"],
            push,
            &refused(
                "--stderr mode \"bogus\" is not one of errors, all, or client",
                push,
            ),
        );
    }
}
