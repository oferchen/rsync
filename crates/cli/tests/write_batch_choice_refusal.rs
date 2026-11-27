//! Oracle cells: a batch recording refuses a forced checksum or compress
//! choice the way rsync 3.5.2 does.
//!
//! upstream: compat.c:414-416 getenv_nstr() pins both negotiation lists to the
//! old-style choice (`md5`, `md4` below protocol 30, and `zlib`) while
//! `write_batch` is set. Only a server sees the pin: a local copy's forked
//! child shares the client's `write_batch`, and a push forwards
//! `--only-write-batch=X` to its receiver (options.c:3091-3092). That server
//! refuses any other forced choice (compat.c:448-451) with exit 4, and the
//! client, meeting the closed stream, exits 12 (io.c:299-303). A remote server
//! never told about the batch accepts the choice.
//!
//! oc leaves no batch file behind a refused local copy, where 3.5.2 leaves a
//! truncated one; the cells compare lines and exit codes only.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// The receiving server's refusal of `kind` `choice`, then the sender's report
/// of the closed stream, as 3.5.2 prints them once normalised.
fn refusal(kind: &str, choice: &str, server: &str) -> Vec<String> {
    vec![
        format!("Your --{kind}-choice value ({choice}) was refused by the server."),
        format!("rsync error: requested action not supported (code 4) at X [{server}=V]"),
        "rsync: connection unexpectedly closed (0 bytes received so far) [sender]".to_owned(),
        "rsync error: error in rsync protocol data stream (code 12) at X [sender=V]".to_owned(),
    ]
}

/// A run's exit code and stderr, with the program name, source location and
/// version that differ between the two implementations replaced.
#[derive(Debug, PartialEq)]
struct Outcome {
    code: Option<i32>,
    lines: Vec<String>,
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

/// A source tree, a destination, and a remote shell that runs its command
/// locally, so `lh:PATH` reaches a server on this host.
fn fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("src/sub")).unwrap();
    fs::create_dir_all(root.join("dst")).unwrap();
    fs::write(root.join("src/a"), b"hello\n").unwrap();
    fs::write(root.join("src/sub/b"), vec![7u8; 5000]).unwrap();
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// Runs `binary -a OPTIONS src/ dst/` in a fresh fixture, recording batch `B`.
fn local(binary: &Path, options: &[&str]) -> (Outcome, TempDir) {
    let dir = fixture();
    let output = Command::new(binary)
        .current_dir(dir.path())
        .arg("-a")
        .args(options)
        .args(["src/", "dst/"])
        .output()
        .expect("run rsync");
    (outcome(&output), dir)
}

/// Runs a push or pull through the local remote shell, `binary` on both ends.
fn remote(binary: &Path, options: &[&str], push: bool) -> Outcome {
    let dir = fixture();
    let root = dir.path().display().to_string();
    let (from, to) = if push {
        ("src/".to_owned(), format!("lh:{root}/dst/"))
    } else {
        (format!("lh:{root}/src/"), "dst/".to_owned())
    };
    let output = Command::new(binary)
        .current_dir(dir.path())
        .arg("-a")
        .args(options)
        .arg("-e")
        .arg(dir.path().join("rsh"))
        .arg(format!("--rsync-path={}", binary.display()))
        .args([from, to])
        .output()
        .expect("run rsync");
    outcome(&output)
}

fn outcome(output: &std::process::Output) -> Outcome {
    Outcome {
        code: output.status.code(),
        lines: String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(normalise)
            .collect(),
    }
}

/// Forced choices the pinned lists lack, and the value each refusal names.
/// Every one is built into any 3.5.2, so the oracle can run them all.
const REFUSED: &[(&[&str], &str, &str)] = &[
    (&["--zc=zlibx"], "compress", "zlibx"),
    (&["-zz"], "compress", "zlibx"),
    (&["--zc=none"], "compress", "none"),
    (&["--cc=sha1"], "checksum", "sha1"),
    (&["--cc=md4"], "checksum", "md4"),
    (&["--cc=md5,sha1"], "checksum", "md5,sha1"),
    (&["--protocol=29", "--cc=md5"], "checksum", "md5"),
    // upstream: compat.c:839-840 - the checksum is validated first.
    (&["--cc=sha1", "--zc=zlibx"], "checksum", "sha1"),
];

/// Forced choices the pinned lists hold, and dry runs, which record nothing
/// (options.c:2318-2319).
const ACCEPTED: &[&[&str]] = &[
    &["--zc=zlib"],
    &["--old-compress"],
    &["--cc=md5"],
    &["--cc=MD5,auto"],
    &["--protocol=29", "--cc=md4"],
    &["-n", "--zc=zlibx"],
];

fn with_batch<'a>(options: &[&'a str], batch: &'a str) -> Vec<&'a str> {
    options.iter().copied().chain([batch]).collect()
}

#[test]
fn local_copy_refuses_a_choice_the_batch_cannot_record() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for &(options, kind, choice) in REFUSED {
        let options = with_batch(options, "--write-batch=B");
        let (oc, dir) = local(&oc_rsync_bin(), &options);
        let expected = Outcome {
            code: Some(12),
            lines: refusal(kind, choice, "Receiver"),
        };
        assert_eq!(oc, expected, "{options:?}");
        assert!(!dir.path().join("B").exists(), "{options:?} left a batch");
        assert!(
            !dir.path().join("B.sh").exists(),
            "{options:?} left a script"
        );
        assert!(!dir.path().join("dst/a").exists(), "{options:?} copied");
        if let Some(upstream) = &upstream {
            assert_eq!(
                local(upstream.binary(), &options).0,
                expected,
                "{options:?}"
            );
        }
    }
}

/// zstd is absent from some 3.5.2 builds, so this cell has no oracle; the
/// zlibx cells above pin the same path.
#[cfg(feature = "zstd")]
#[test]
fn local_copy_refuses_zstd_under_write_batch() {
    let (oc, _dir) = local(
        &oc_rsync_bin(),
        &["-z", "--zc=zstd", "--only-write-batch=B"],
    );
    assert_eq!(
        oc,
        Outcome {
            code: Some(12),
            lines: refusal("compress", "zstd", "Receiver"),
        }
    );
}

#[test]
fn local_copy_records_a_choice_the_batch_can_hold() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for &options in ACCEPTED {
        let options = with_batch(options, "--write-batch=B");
        let (oc, _dir) = local(&oc_rsync_bin(), &options);
        let expected = Outcome {
            code: Some(0),
            lines: Vec::new(),
        };
        assert_eq!(oc, expected, "{options:?}");
        if let Some(upstream) = &upstream {
            assert_eq!(
                local(upstream.binary(), &options).0,
                expected,
                "{options:?}"
            );
        }
    }
}

/// upstream: options.c:3091-3092 - only a push forwards `--only-write-batch=X`,
/// so only its receiver refuses, while setup_protocol() still names the
/// process "server" (rsync.c:987-990).
#[test]
fn only_write_batch_push_receiver_refuses_the_choice() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    for (option, kind, choice) in [
        ("--cc=sha1", "checksum", "sha1"),
        ("--zc=zlibx", "compress", "zlibx"),
    ] {
        let options = [option, "--only-write-batch=B"];
        let expected = Outcome {
            code: Some(12),
            lines: refusal(kind, choice, "server"),
        };
        assert_eq!(
            remote(&oc_rsync_bin(), &options, true),
            expected,
            "{options:?}"
        );
        if let Some(upstream) = &upstream {
            assert_eq!(
                remote(upstream.binary(), &options, true),
                expected,
                "{options:?}"
            );
        }
    }
}

/// A remote server not told about the batch validates against its own
/// environment, so a `--write-batch` push or pull, and an
/// `--only-write-batch` pull, accept the choice.
#[test]
fn remote_server_not_told_about_the_batch_accepts_the_choice() {
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_2);
    let cells: [(&[&str], bool); 3] = [
        (&["--cc=sha1", "--write-batch=B"], true),
        (&["--cc=sha1", "--write-batch=B"], false),
        (&["--zc=zlibx", "--only-write-batch=B"], false),
    ];
    for (options, push) in cells {
        let expected = Outcome {
            code: Some(0),
            lines: Vec::new(),
        };
        assert_eq!(
            remote(&oc_rsync_bin(), options, push),
            expected,
            "{options:?}"
        );
        if let Some(upstream) = &upstream {
            assert_eq!(
                remote(upstream.binary(), options, push),
                expected,
                "{options:?}"
            );
        }
    }
}
