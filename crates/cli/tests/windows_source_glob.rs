//! Native Windows has no shell globbing, so `oc-rsync` expands wildcard
//! source operands itself. `std::process::Command` passes argv verbatim,
//! which reproduces what `cmd.exe` and PowerShell hand the binary.

#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use test_support::oc_rsync_bin;

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(oc_rsync_bin())
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn oc-rsync")
}

fn source_tree() -> tempfile::TempDir {
    let tmp = test_support::create_tempdir();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("sub")).expect("mkdir");
    for name in ["a.txt", "b.txt", "c.log", "s.txt", r"sub\d.txt"] {
        fs::write(src.join(name), name.as_bytes()).expect("write");
    }
    tmp
}

#[test]
fn relative_wildcard_source_copies_only_matches() {
    let tmp = source_tree();
    let out = run(tmp.path(), &["-r", r"src\*.txt", r"dst\"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dst = tmp.path().join("dst");
    assert!(dst.join("a.txt").is_file());
    assert!(dst.join("b.txt").is_file());
    assert!(!dst.join("c.log").exists(), "non-matching file was copied");
    assert!(
        !dst.join("sub").exists(),
        "non-matching directory was copied"
    );
}

#[test]
fn absolute_wildcard_with_trailing_separator_selects_directories() {
    let tmp = source_tree();
    let pattern = format!(r"{}\src\s*\", tmp.path().display());
    let out = run(tmp.path(), &["-r", &pattern, r"dst\"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `s.txt` also matches `s*` but is not a directory, so the trailing
    // separator must drop it; `sub\` copies the directory's contents.
    assert!(tmp.path().join(r"dst\d.txt").is_file());
}

#[test]
fn unmatched_wildcard_reports_the_literal_missing_source() {
    let tmp = source_tree();
    let out = run(tmp.path(), &["-r", r"src\*.nope", r"dst\"]);
    assert_eq!(
        out.status.code(),
        Some(23),
        "partial-transfer exit expected"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("*.nope"),
        "missing-source error must name the literal pattern: {stderr}"
    );
}
