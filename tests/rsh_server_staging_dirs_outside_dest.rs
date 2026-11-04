//! An rsh `--server` receiver must reach a `--backup-dir` or absolute
//! `--partial-dir` that lies outside the destination.
//!
//! upstream: backup.c:65-136 renames the replaced file into `--backup-dir`,
//! creating the directory on first use, and receiver.c:820-849 reads an
//! existing `--partial-dir` file as the basis and removes it once the file
//! lands. Neither directory has to be under the destination. The oc
//! `--server` receiver confines itself with Landlock, and a ruleset that
//! names only the destination turned every such backup into
//! `keep_backup failed: ... Permission denied (13)`, leaving the destination
//! file un-updated, and left a stale partial behind.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;

const NEW: &[u8] = b"the new contents the sender pushes, longer than the old ones\n";
const OLD: &[u8] = b"the old destination contents\n";

/// How a cell spells its `--backup-dir`, given the scratch root.
type Spelling = fn(&Path) -> String;

/// `CARGO_BIN_EXE_oc-rsync` is a compile-time variable; reading it at run
/// time would fall through to whatever stale binary is on disk.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Lays out `src/f` (new) and an older, differently sized `dst/f`.
fn seed(root: &Path) -> PathBuf {
    for dir in ["src", "dst"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    fs::write(root.join("src/f"), NEW).expect("write source");
    fs::write(root.join("dst/f"), OLD).expect("write destination");
    filetime::set_file_mtime(root.join("dst/f"), FileTime::from_unix_time(946_684_800, 0))
        .expect("backdate destination");
    let shim = root.join("rsh.sh");
    fs::write(&shim, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    shim
}

fn push(root: &Path, shim: &Path, extra: &[String]) {
    let binary = oc_rsync_binary();
    let output = Command::new(&binary)
        .arg("-rt")
        .args(extra)
        .arg("-e")
        .arg(shim)
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(format!("{}/", root.join("src").display()))
        .arg(format!("localhost:{}/", root.join("dst").display()))
        .output()
        .expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "{extra:?}: expected exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn a_backup_dir_outside_the_destination_receives_the_replaced_file() {
    let cells: [(&str, bool, Spelling); 3] = [
        ("absolute, existing", true, |root| {
            root.join("bak").display().to_string()
        }),
        ("absolute, missing", false, |root| {
            root.join("bak").display().to_string()
        }),
        ("relative ../bak", true, |_| "../bak".to_owned()),
    ];
    for (name, pre_create, spelling) in cells {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let shim = seed(root);
        if pre_create {
            fs::create_dir(root.join("bak")).expect("mkdir bak");
        }
        push(
            root,
            &shim,
            &["-b".to_owned(), format!("--backup-dir={}", spelling(root))],
        );

        assert_eq!(
            fs::read(root.join("dst/f")).expect("read destination"),
            NEW,
            "{name}: the destination was not updated"
        );
        assert_eq!(
            fs::read(root.join("bak/f")).expect("read backup"),
            OLD,
            "{name}: upstream backup.c renames the replaced file into --backup-dir"
        );
    }
}

#[test]
fn an_absolute_partial_dir_outside_the_destination_is_consumed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let shim = seed(root);
    fs::create_dir(root.join("part")).expect("mkdir part");
    fs::write(root.join("part/f"), &NEW[..20]).expect("write old partial");

    push(
        root,
        &shim,
        &[format!("--partial-dir={}", root.join("part").display())],
    );

    assert_eq!(fs::read(root.join("dst/f")).expect("read destination"), NEW);
    assert!(
        !root.join("part/f").exists(),
        "upstream receiver.c removes the partial once the file lands; it was left behind"
    );
}
