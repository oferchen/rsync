//! Cross-implementation oracle: `--fake-super` destinations, oc-rsync against
//! upstream rsync 3.5.1 on the same fixtures.
//!
//! # Why this test exists
//!
//! Upstream's generator reads every destination through `x_lstat()`, which
//! folds the `user.rsync.%stat` record into the stat it compares and itemizes
//! against (xattrs.c:1372, generator.c:1745). oc compared the owner-writable
//! placeholder mode instead, so a rerun itemized `.f...p.....` for every
//! 0444, setuid and setgid file, and a record that did not parse was ignored
//! with exit 0 where upstream exits `RERR_FILEIO` (11).
//!
//! # The contract (Rule 9)
//!
//! For each cell, oc's itemize lines, exit code and diagnostic lines equal
//! upstream's. Only what cannot match by construction is masked: the program
//! name and version in the trailer, the source location, and upstream's
//! `received SIGUSR1 (code 19)` lines, which its forked sibling processes
//! print as they are killed and oc's threads have no counterpart for.
//!
//! # Gating
//!
//! Self-skips when `OC_RSYNC_UPSTREAM_COMPAT` is unset, when no upstream
//! rsync 3.5.1 or oc-rsync binary is available, and when the filesystem
//! refuses user xattrs. Once those hold the cells must run and match.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;
use metadata::FAKE_SUPER_XATTR;
use tempfile::tempdir;
use test_support::{UpstreamVersion, require_upstream_rsync, upstream_compat_enabled};

/// What one run printed and how it ended, with the unmatchable parts masked.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    stdout: String,
    stderr: Vec<String>,
    code: Option<i32>,
}

/// The two implementations under comparison, each run from its own tree.
struct Pair {
    upstream: PathBuf,
    oc: PathBuf,
}

fn pair() -> Option<Pair> {
    if !upstream_compat_enabled() {
        return None;
    }
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_1)?
        .binary()
        .to_path_buf();
    let oc = test_support::oc_rsync_bin();
    if !oc.is_file() {
        eprintln!("skipping: oc-rsync binary not built at {}", oc.display());
        return None;
    }
    Some(Pair { upstream, oc })
}

/// Masks the trailer's program name, location and version, and drops the
/// forked-process SIGUSR1 lines (see the module docs).
fn normalise(stderr: &[u8], root: &Path) -> Vec<String> {
    let root = root.display().to_string();
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| !line.contains("received SIGUSR1 (code 19)"))
        .map(|line| {
            let line = line.replace(&root, "<root>");
            let line = line.strip_prefix("oc-").unwrap_or(&line).to_owned();
            match (line.find(" at "), line.rfind(" [")) {
                (Some(at), Some(role)) if at < role => {
                    let role_name = line[role + 2..].split('=').next().unwrap_or_default();
                    format!("{} [{role_name}]", &line[..at])
                }
                _ => line,
            }
        })
        .collect()
}

/// Runs `binary` with `args` from `root` and masks the result.
fn run(binary: &Path, root: &Path, args: &[&str]) -> Outcome {
    let output = Command::new(binary)
        .args(args)
        .current_dir(root)
        .output()
        .expect("spawn rsync");
    Outcome {
        stdout: String::from_utf8_lossy(&output.stdout)
            .replace(&root.display().to_string(), "<root>"),
        stderr: normalise(&output.stderr, root),
        code: output.status.code(),
    }
}

/// A remote shell that runs the server command locally, as upstream's
/// testsuite `lsh` does.
fn local_shell(root: &Path) -> PathBuf {
    let shell = root.join("lsh");
    fs::write(&shell, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write lsh");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod lsh");
    shell
}

/// Pull arguments: `binary` is both the client and, via `--rsync-path`, the
/// server, so each implementation is compared against itself end to end.
fn pull_args<'a>(binary: &'a str, shell: &'a str, flags: &'a str, src: &'a str) -> Vec<&'a str> {
    vec![
        flags,
        "--fake-super",
        "-M--fake-super",
        "-e",
        shell,
        "--rsync-path",
        binary,
        src,
        "dst/",
    ]
}

/// Builds the #164 source tree: modes the placeholder cannot carry, plus a
/// plain file, all with a fixed mtime.
fn mode_tree(root: &Path) -> bool {
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    if xattr::set(&src, "user.probe", b"1").is_err() {
        eprintln!("skipping: the filesystem rejects user xattrs");
        return false;
    }
    let _ = xattr::remove(&src, "user.probe");
    for (name, mode) in [
        ("ro", 0o444),
        ("su", 0o4755),
        ("sg", 0o2755),
        ("plain", 0o644),
    ] {
        let path = src.join(name);
        fs::write(&path, name).expect("write source");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod source");
        filetime::set_file_mtime(&path, FileTime::from_unix_time(1_577_836_800, 0)).expect("mtime");
    }
    true
}

/// Moves every destination mtime back so the quick check cannot skip.
fn backdate(dst: &Path) {
    for entry in fs::read_dir(dst).expect("read dst") {
        let path = entry.expect("entry").path();
        filetime::set_file_mtime(&path, FileTime::from_unix_time(1_546_300_800, 0)).expect("mtime");
    }
}

/// Runs the #164 sequence (first run, rerun, backdated run) with `driver`
/// and collects every outcome.
fn mode_sequence(root: &Path, driver: &dyn Fn(&Path) -> Outcome) -> Vec<Outcome> {
    let _ = fs::remove_dir_all(root.join("dst"));
    let first = driver(root);
    let rerun = driver(root);
    backdate(&root.join("dst"));
    let backdated = driver(root);
    vec![first, rerun, backdated]
}

/// upstream: generator.c:1745 -> xattrs.c:1372 x_lstat(). A rerun itemizes
/// nothing; a backdated run reports only the time.
#[test]
fn a_fake_super_rerun_itemizes_like_upstream_locally_and_over_a_pull() {
    let Some(pair) = pair() else { return };
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    if !mode_tree(root) {
        return;
    }
    let shell = local_shell(root);
    let shell = shell.to_str().expect("utf-8 path");
    let src_url = format!("localhost:{}/src/", root.display());
    let local_args = ["-ai", "--fake-super", "src/", "dst/"];
    let mut sequences = Vec::new();
    for binary in [&pair.upstream, &pair.oc] {
        let bin = binary.to_str().expect("utf-8 path");
        let pull_args = pull_args(bin, shell, "-ai", &src_url);
        let local = mode_sequence(root, &|root: &Path| run(binary, root, &local_args));
        let pull = mode_sequence(root, &|root: &Path| run(binary, root, &pull_args));
        sequences.push((local, pull));
    }
    let (upstream, oc) = (&sequences[0], &sequences[1]);
    assert!(
        upstream.0[1].stdout.is_empty(),
        "upstream's local rerun itemizes nothing"
    );
    assert_eq!(oc.0, upstream.0, "local -ai --fake-super sequence");
    assert_eq!(oc.1, upstream.1, "pull -ai --fake-super sequence");
}

/// Plants a fresh `src/f` and an older `dst/f`, with `garbage` as the
/// `%stat` of `corrupt`.
fn corrupt_fixture(root: &Path, corrupt: &str) {
    for dir in ["src", "dst"] {
        let _ = fs::remove_dir_all(root.join(dir));
        fs::create_dir_all(root.join(dir)).expect("create dir");
    }
    fs::write(root.join("src/f"), b"data").expect("write src");
    fs::write(root.join("dst/f"), b"old").expect("write dst");
    filetime::set_file_mtime(
        root.join("dst/f"),
        FileTime::from_unix_time(1_546_300_800, 0),
    )
    .expect("mtime");
    xattr::set(root.join(corrupt), FAKE_SUPER_XATTR, b"garbage").expect("plant %stat");
}

/// upstream: xattrs.c:1253-1258 - a record that does not parse is printed
/// with its full_fname() and ends the run with RERR_FILEIO (11), tagged with
/// the role that read it: the generator for a destination, the sender for a
/// local source.
#[test]
fn a_corrupt_fake_super_record_fails_like_upstream() {
    let Some(pair) = pair() else { return };
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    if !mode_tree(root) {
        return;
    }
    let shell = local_shell(root);
    let shell = shell.to_str().expect("utf-8 path");
    let src_url = format!("localhost:{}/src/", root.display());
    let cells: [(&str, &str, bool); 3] = [
        ("local copy onto a corrupt destination", "dst/f", false),
        ("pull onto a corrupt destination", "dst/f", true),
        ("local copy of a corrupt source", "src/f", false),
    ];
    for (cell, corrupt, is_pull) in cells {
        let mut outcomes = Vec::new();
        for binary in [&pair.upstream, &pair.oc] {
            corrupt_fixture(root, corrupt);
            let bin = binary.to_str().expect("utf-8 path");
            let outcome = if is_pull {
                run(binary, root, &pull_args(bin, shell, "-a", &src_url))
            } else {
                run(binary, root, &["-a", "--fake-super", "src/", "dst/"])
            };
            outcomes.push(outcome);
        }
        assert_eq!(
            outcomes[0].code,
            Some(11),
            "{cell}: upstream exits RERR_FILEIO"
        );
        assert_eq!(outcomes[1], outcomes[0], "{cell}");
    }
}
