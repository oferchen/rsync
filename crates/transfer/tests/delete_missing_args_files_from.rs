//! UTS-NEXTEST-EDGE.h: nextest sanity for the upstream
//! `delete-missing-args-files-from` scenario.
//!
//! Combines `--files-from=LIST` with `--delete-missing-args` and exercises
//! the end-to-end mode-0 sentinel path that the production fix landed under
//! UTS-19 / UTS-DD-files-from.3. The sender emits a mode-0 entry for each
//! vanished `--files-from` operand; the receiver consumes the entry and
//! removes the corresponding destination path.
//!
//! Without this guard the destination retains the stale sibling and the
//! transfer exits cleanly, which is the silent-incompatibility upstream's
//! `flist.c:2436-2443` and `generator.c:1348-1354` were written to avoid.
//!
//! # Upstream Reference
//!
//! - `flist.c:2436-2443` - sender emits mode-0 sentinel when
//!   `missing_args == 2` and `link_stat()` fails with `ENOENT`.
//! - `generator.c:1348-1354` - receiver-side `delete_item()` branch on
//!   `file->mode == 0 && missing_args == 2`.
//! - `options.c:765` / `options.c:768` - `--delete-missing-args` and
//!   `--ignore-missing-args` flag definitions.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::tempdir;

/// Locates the workspace `oc-rsync` binary the test runner built.
///
/// Mirrors the lookup logic in
/// `tests/remove_source_files_local_copy.rs`: prefer Cargo's
/// `CARGO_BIN_EXE_oc-rsync` when set, otherwise walk up from the test
/// executable until a `target/` directory is found.
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    let name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    while !dir.ends_with("target") {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    for sub in ["debug", "release"] {
        let candidate = dir.join(sub).join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Builds the upstream-shaped fixture: a source directory with `keep.txt`
/// present, a `files-from` list referencing both `keep.txt` and a vanished
/// sibling `ghost.txt`, and a pre-populated destination that holds the
/// stale `ghost.txt`.
struct Fixture {
    _root: tempfile::TempDir,
    source: PathBuf,
    dest: PathBuf,
    files_from: PathBuf,
}

fn build_fixture() -> Fixture {
    let root = tempdir().expect("tempdir");
    let source = root.path().join("source");
    let dest = root.path().join("dest");
    fs::create_dir_all(&source).expect("mkdir source");
    fs::create_dir_all(&dest).expect("mkdir dest");

    // Present source operand: must transfer.
    fs::write(source.join("keep.txt"), b"fresh source content").expect("write keep.txt");

    // Pre-populate destination with the about-to-be-deleted sibling and
    // a stale copy of keep.txt. Distinct content avoids the quick-check
    // size+mtime skip path so the transfer is observable.
    fs::write(dest.join("ghost.txt"), b"stale destination ghost").expect("write dest ghost.txt");
    fs::write(dest.join("keep.txt"), b"old").expect("write dest keep.txt");

    // files-from list references both the present source AND a vanished
    // sibling. Upstream's `flist.c:2436` raises the mode-0 sentinel for
    // the ENOENT entry when --delete-missing-args is in effect.
    let files_from = root.path().join("filelist");
    fs::write(&files_from, "keep.txt\nghost.txt\n").expect("write filelist");

    Fixture {
        _root: root,
        source,
        dest,
        files_from,
    }
}

/// `--delete-missing-args` + `--files-from`: the present file transfers
/// AND the vanished entry causes the destination sibling to be deleted.
/// Mirrors upstream `delete-missing-args-files-from` test scenario and
/// pins the production fix from UTS-19 / UTS-DD-files-from.3.
#[test]
fn delete_missing_args_files_from_removes_vanished_destination() {
    let Some(rsync_bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };

    let fx = build_fixture();
    let source_arg = {
        let mut s = fx.source.clone().into_os_string();
        s.push("/");
        s
    };

    let status = Command::new(&rsync_bin)
        .arg("-a")
        .arg("--delete-missing-args")
        .arg(format!("--files-from={}", fx.files_from.display()))
        .arg(&source_arg)
        .arg(&fx.dest)
        .status()
        .expect("spawn oc-rsync");
    assert!(
        status.success(),
        "oc-rsync exited with {status:?} (expected success on mode-0 sentinel path)"
    );

    // Present operand must have been transferred (content updated).
    let keep = fs::read(fx.dest.join("keep.txt")).expect("read dest keep.txt");
    assert_eq!(
        keep, b"fresh source content",
        "keep.txt destination content was not refreshed by the transfer"
    );

    // Vanished operand: receiver must have consumed the mode-0 sentinel
    // and deleted the stale destination sibling.
    assert!(
        !fx.dest.join("ghost.txt").exists(),
        "ghost.txt should be deleted from destination via mode-0 sentinel"
    );
}

/// Companion: `--ignore-missing-args` must NOT delete the destination
/// sibling. Upstream gates `delete_item()` on `missing_args == 2`;
/// `--ignore-missing-args` sets `missing_args == 1`, which skips the
/// sentinel entirely (`flist.c:2436-2437`).
#[test]
fn ignore_missing_args_files_from_preserves_destination() {
    let Some(rsync_bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };

    let fx = build_fixture();
    let source_arg = {
        let mut s = fx.source.clone().into_os_string();
        s.push("/");
        s
    };

    let status = Command::new(&rsync_bin)
        .arg("-a")
        .arg("--ignore-missing-args")
        .arg(format!("--files-from={}", fx.files_from.display()))
        .arg(&source_arg)
        .arg(&fx.dest)
        .status()
        .expect("spawn oc-rsync");
    assert!(
        status.success(),
        "oc-rsync exited with {status:?} (--ignore-missing-args must exit 0)"
    );

    // Present operand still transfers.
    let keep = fs::read(fx.dest.join("keep.txt")).expect("read dest keep.txt");
    assert_eq!(
        keep, b"fresh source content",
        "keep.txt destination content was not refreshed by the transfer"
    );

    // Vanished operand: stale sibling MUST survive under
    // --ignore-missing-args (no mode-0 sentinel is sent).
    assert!(
        fx.dest.join("ghost.txt").is_file(),
        "ghost.txt must survive when --ignore-missing-args replaces --delete-missing-args"
    );
    let ghost = fs::read(fx.dest.join("ghost.txt")).expect("read dest ghost.txt");
    assert_eq!(
        ghost, b"stale destination ghost",
        "ghost.txt content must be untouched under --ignore-missing-args"
    );
}
