//! A receiver that cannot create, open or rename an output file skips that one
//! file and keeps going, ending the run with exit 23, exactly as rsync 3.5.1
//! does:
//!
//! ```c
//! /* receiver.c:465-468 - open_tmpfile() */
//!         rsyserr(FERROR_XFER, errno, "mkstemp %s failed", full_fname(fnametmp));
//!         return -1;
//! /* receiver.c:1242-1261 - recv_files() */
//!         rsyserr(FERROR_XFER, errno, "open %s failed", full_fname(fnametmp));
//!         ...
//!         discard_receive_data(f_in, file);
//!         continue;
//! /* rsync.c:918-927 - finish_transfer() */
//!         rsyserr(FERROR_XFER, errno, "%s %s -> \"%s\"", "rename", full_fname(fnametmp), fname);
//!         return 0;
//! ```
//!
//! Every cell pushes two file operands, `a` then `b`, through a local rsh shim
//! so the receiver is a real `--server` process. The destination (or the
//! `--temp-dir`) is 0555 and, being the operand directory rather than a file-list
//! entry, is never relaxed to u+w - so BOTH files fail at the same stage and the
//! expected stderr is two per-file lines in file-list order, each oracle line
//! copied from rsync 3.5.1 run against the identical fixture.
//!
//! The in-place cell is the one oc used to get wrong: its `EACCES` create is
//! retried without `O_CREAT`, the retry reports `ENOENT`, and oc aborted the
//! whole run with exit 3 on that non-`EACCES` errno instead of skipping.
//!
//! Skip conditions (the test passes with a printed reason): running as root,
//! which bypasses the 0555 mode the fixture relies on.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn running_as_root() -> bool {
    std::env::var("USER").is_ok_and(|u| u == "root")
}

/// An SSH-shaped remote-shell shim: drop the options and the host
/// placeholder, then exec the server command locally.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do case \"$1\" in -*) shift ;; *) break ;; esac; done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod fixture");
}

/// What one push reported, with the receiver's random temp suffixes masked.
struct Outcome {
    exit_code: Option<i32>,
    per_file_lines: Vec<String>,
}

/// Replaces the six-character `mkstemp` suffix (`.a.Xy12Zq"` -> `.a.XXXXXX"`)
/// so the line can be compared with the oracle text verbatim.
fn mask_temp_suffix(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(quote) = rest.find('"') {
        let (head, tail) = rest.split_at(quote);
        match head.rfind('.') {
            Some(dot) if head.len() - dot == 7 => {
                out.push_str(&head[..=dot]);
                out.push_str("XXXXXX");
            }
            _ => out.push_str(head),
        }
        out.push('"');
        rest = &tail[1..];
    }
    out.push_str(rest);
    out
}

/// How a cell routes the receiver's output.
#[derive(Clone, Copy)]
enum Route {
    /// Temp files beside the destination (upstream's default).
    Default,
    /// `--temp-dir=<root>/tmp`.
    TempDir,
    /// `--inplace`: write the destination itself.
    Inplace,
}

/// Pushes `a` and `b` into `<root>/dst` along `route`, with every directory in
/// `seal` made 0555 for the run, then restores write access for teardown.
fn push(cell: &str, route: Route, seal: &[&str]) -> Option<(TempDir, Outcome)> {
    if running_as_root() {
        eprintln!("skip {cell}: root bypasses the 0555 fixture");
        return None;
    }
    let dir = test_support::create_tempdir();
    let root = dir.path();
    for dir in ["src", "dst", "tmp"] {
        fs::create_dir_all(root.join(dir)).expect("create fixture dir");
    }
    fs::write(root.join("src/a"), b"payload-a\n").expect("seed a");
    fs::write(root.join("src/b"), b"payload-b\n").expect("seed b");
    for dir in seal {
        set_mode(&root.join(dir), 0o555);
    }

    let shim = write_rsh_shim(root);
    let binary = oc_binary();
    let mut command = Command::new(&binary);
    match route {
        Route::Default => {}
        Route::TempDir => {
            command.arg(format!("--temp-dir={}", root.join("tmp").display()));
        }
        Route::Inplace => {
            command.arg("--inplace");
        }
    }
    let output = command
        .arg("--rsh")
        .arg(&shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(root.join("src/a"))
        .arg(root.join("src/b"))
        .arg(format!("localhost:{}/", root.join("dst").display()))
        .output()
        .expect("run push");
    for dir in seal {
        set_mode(&root.join(dir), 0o755);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let per_file_lines = stderr
        .lines()
        .filter(|line| line.starts_with("rsync: [receiver]"))
        .map(mask_temp_suffix)
        .collect();
    Some((
        dir,
        Outcome {
            exit_code: output.status.code(),
            per_file_lines,
        },
    ))
}

fn assert_per_file_skips(cell: &str, root: &Path, outcome: &Outcome, expected: &[String]) {
    assert_eq!(
        outcome.per_file_lines, expected,
        "{cell}: both files report upstream's line, in file-list order"
    );
    assert_eq!(
        outcome.exit_code,
        Some(23),
        "{cell}: a per-file failure ends the run with RERR_PARTIAL, never a fatal code"
    );
    assert!(
        !root.join("dst/a").exists() && !root.join("dst/b").exists(),
        "{cell}: neither file may land"
    );
}

/// Oracle lines for `a` then `b`, built by `line(name)`.
fn per_file(line: impl Fn(&str) -> String) -> Vec<String> {
    ["a", "b"].into_iter().map(line).collect()
}

#[test]
fn a_denied_temp_create_skips_each_file_with_exit_23() {
    let Some((dir, outcome)) = push("mkstemp", Route::Default, &["dst"]) else {
        return;
    };
    let root = dir.path();
    let dst = root.join("dst");
    let expected = per_file(|name| {
        format!(
            "rsync: [receiver] mkstemp \"{}/.{name}.XXXXXX\" failed: Permission denied (13)",
            dst.display()
        )
    });
    assert_per_file_skips("mkstemp", root, &outcome, &expected);
}

#[test]
fn a_denied_temp_dir_create_skips_each_file_with_exit_23() {
    let Some((dir, outcome)) = push("mkstemp-tempdir", Route::TempDir, &["tmp"]) else {
        return;
    };
    let root = dir.path();
    let tmp = root.join("tmp");
    let expected = per_file(|name| {
        format!(
            "rsync: [receiver] mkstemp \"{}/{name}.XXXXXX\" failed: Permission denied (13)",
            tmp.display()
        )
    });
    assert_per_file_skips("mkstemp-tempdir", root, &outcome, &expected);
}

/// The in-place create fails `EACCES`, the retry without `O_CREAT` finds no
/// file, and upstream reports that last arm's `ENOENT` - the non-`EACCES`
/// errno oc used to treat as fatal (exit 3). The `O_CREAT`-less retry is
/// upstream's `#ifdef linux` arm, so the reported errno is Linux-specific.
#[cfg(target_os = "linux")]
#[test]
fn a_failed_inplace_open_skips_each_file_with_the_open_wording() {
    let Some((dir, outcome)) = push("inplace", Route::Inplace, &["dst"]) else {
        return;
    };
    let root = dir.path();
    let dst = root.join("dst");
    let expected = per_file(|name| {
        format!(
            "rsync: [receiver] open \"{}/{name}\" failed: No such file or directory (2)",
            dst.display()
        )
    });
    assert_per_file_skips("inplace", root, &outcome, &expected);
}

#[test]
fn a_denied_rename_into_place_skips_each_file_with_exit_23() {
    let Some((dir, outcome)) = push("rename", Route::TempDir, &["dst"]) else {
        return;
    };
    let root = dir.path();
    let tmp = root.join("tmp");
    let expected = per_file(|name| {
        format!(
            "rsync: [receiver] rename \"{}/{name}.XXXXXX\" -> \"{name}\": Permission denied (13)",
            tmp.display()
        )
    });
    assert_per_file_skips("rename", root, &outcome, &expected);
}
