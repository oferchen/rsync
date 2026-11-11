//! A local copy whose destination write fails ends like upstream 3.5.1's.
//!
//! upstream: receiver.c:658-660 - `rsyserr(FERROR_XFER, errno, "write failed
//! on %s", full_fname(fname))` then `exit_cleanup(RERR_FILEIO)`, so the copy
//! prints the destination's name, not the temp file's, and exits 11 from the
//! receiver. A file written in place keeps what was written: receiver.c:1242-1246
//! registers no cleanup name for it, and cleanup.c only unlinks the temp file.
//!
//! Each cell runs the same copy with 3.5.1 and with oc and requires the same
//! exit code, the same cause line and the same destination left behind.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// What a failed copy left: its exit code, its first stderr line with the
/// run's directory replaced by `<D>`, its last stderr line, and the size of
/// the destination file, if any.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    cause: String,
    trailer: String,
    kept: Option<u64>,
}

fn outcome(output: &Output, root: &Path, kept: &Path) -> Outcome {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut lines = stderr.lines();
    let cause = lines
        .next()
        .unwrap_or_default()
        .replace(&root.display().to_string(), "<D>");
    let trailer = stderr.lines().last().unwrap_or_default().to_owned();
    Outcome {
        code: output.status.code(),
        cause,
        trailer,
        kept: fs::metadata(kept).ok().map(|meta| meta.len()),
    }
}

/// A directory copy of a file larger than the `ulimit -f` cap, so the write
/// fails with `EFBIG`. `SIGXFSZ` is ignored, as the shell's `trap` leaves it
/// for the exec'd copy, so `write(2)` fails instead of the process dying.
fn file_size_limited_copy(binary: &Path, args: &[&str]) -> Outcome {
    let temp = TempDir::new().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    fs::create_dir_all(root.join("src")).expect("source dir");
    fs::create_dir_all(root.join("dst")).expect("destination dir");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    fs::write(root.join("src/big"), data).expect("write big");
    let output = Command::new("sh")
        .current_dir(&root)
        .arg("-c")
        .arg("trap '' XFSZ; ulimit -f 100; exec \"$@\"")
        .arg("sh")
        .arg(binary)
        .arg("-r")
        .args(args)
        .arg("src/")
        .arg("dst/")
        .output()
        .expect("run copy");
    outcome(&output, &root, &root.join("dst/big"))
}

fn assert_same(case: &str, upstream: &Outcome, oc: &Outcome) {
    assert_eq!(upstream.code, Some(11), "{case}: 3.5.1 {upstream:?}");
    assert_eq!(oc.code, upstream.code, "{case}: oc {oc:?}");
    assert_eq!(oc.cause, upstream.cause, "{case}");
    assert!(
        oc.trailer.contains("error in file IO (code 11)"),
        "{case}: {oc:?}"
    );
    assert!(oc.trailer.contains("[receiver="), "{case}: {oc:?}");
    assert_eq!(oc.kept, upstream.kept, "{case}: destination left behind");
}

#[test]
fn a_failed_destination_write_reports_the_receivers_line_and_exits_11() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let oc: PathBuf = oc_rsync_bin();
    for (case, args) in [
        ("temp file", &[][..]),
        ("whole file", &["-W"][..]),
        ("in place", &["--inplace"][..]),
    ] {
        let expected = file_size_limited_copy(upstream.binary(), args);
        let actual = file_size_limited_copy(&oc, args);
        assert_same(case, &expected, &actual);
    }
}

/// The in-place cell must be able to tell a kept partial from a removed one.
#[test]
fn an_in_place_write_failure_keeps_the_partial_file() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let kept = file_size_limited_copy(upstream.binary(), &["--inplace"]).kept;
    assert!(
        kept.is_some_and(|len| len > 0 && len < 300_000),
        "3.5.1 keeps a partial in-place file: {kept:?}"
    );
}

/// `/dev/full` fails every write with `ENOSPC`.
#[cfg(target_os = "linux")]
#[test]
fn a_write_to_dev_full_reports_the_receivers_line_and_exits_11() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let run = |binary: &Path, args: &[&str]| {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        fs::write(root.join("f"), vec![0x5a; 70_000]).expect("write f");
        let output = Command::new(binary)
            .current_dir(&root)
            .arg("--write-devices")
            .args(args)
            .arg("f")
            .arg("/dev/full")
            .output()
            .expect("run copy");
        outcome(&output, &root, Path::new("/nonexistent"))
    };
    let oc = oc_rsync_bin();
    for (case, args) in [("device", &[][..]), ("device in place", &["--inplace"][..])] {
        assert_same(case, &run(upstream.binary(), args), &run(&oc, args));
    }
}
