//! A relative alt-dest dir is anchored on a single-file destination's parent.
//!
//! upstream: main.c:845-858 `get_local_name()` - copying one file to
//! `dst/g`, where `g` is not a directory, enters `dst` and returns `g` as the
//! local name. `check_alt_basis_dirs()` then anchors `--compare-dest=../alt`
//! on that `curr_dir` (main.c:898-911), so the basis is `dst/../alt`, and
//! `try_dests_reg()` looks for `alt/g`, the local name (generator.c:1083).
//!
//! oc warned `--compare-dest arg does not exist: .../dst/g/../alt` on push,
//! pull and local copies, because the check ran before the file list said the
//! destination was a single file, and a local copy looked for `alt/<source
//! name>` so it re-sent a file 3.5.1 skips.
//!
//! Each cell plants an `alt/g` identical to the source and asserts both
//! halves of upstream's outcome: no warning, and no `g` written (compare-dest
//! skips a level-3 match, generator.c:1149-1155).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use filetime::FileTime;

const PAYLOAD: &[u8] = b"single-file destination payload\n";

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

/// `src/f`, an identical `alt/g` with the same mtime, and an empty `dst/`.
fn seed(root: &Path) -> PathBuf {
    for dir in ["src", "dst", "alt"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    let mtime = FileTime::from_unix_time(1_577_836_800, 0);
    for file in [root.join("src/f"), root.join("alt/g")] {
        fs::write(&file, PAYLOAD).expect("write payload");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
        filetime::set_file_mtime(&file, mtime).expect("set mtime");
    }
    let shim = root.join("rsh.sh");
    fs::write(&shim, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    shim
}

fn assert_upstream_outcome(root: &Path, output: &Output, cell: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{cell}: expected exit 0, got {:?}\nstderr: {stderr}",
        output.status.code()
    );
    assert!(
        !stderr.contains("arg does not exist"),
        "{cell}: upstream anchors ../alt on the destination's parent and \
         finds it; oc warned:\n{stderr}"
    );
    assert!(
        !root.join("dst/g").exists(),
        "{cell}: the identical alt/g is a compare-dest match, so nothing is \
         written; dst/g was created\nstderr: {stderr}"
    );
}

#[test]
fn push_to_a_single_file_destination_anchors_on_its_parent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let shim = seed(root);
    let binary = oc_rsync_binary();
    let output = Command::new(&binary)
        .args(["-t", "--compare-dest=../alt", "-e"])
        .arg(&shim)
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(root.join("src/f"))
        .arg(format!("localhost:{}", root.join("dst/g").display()))
        .output()
        .expect("spawn oc-rsync");
    assert_upstream_outcome(root, &output, "push");
}

/// The push cell above cannot fail on its own: an oc server receiver used to
/// drop every setup warning when nothing was transferred, so a warning about
/// `dst/g/../alt` would never have reached the client either. A second,
/// genuinely missing basis makes the warning path observable: upstream prints
/// exactly one line for it, anchored on `dst`, and still skips `g`.
#[test]
fn push_warns_once_for_a_missing_basis_anchored_on_the_parent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let shim = seed(root);
    let binary = oc_rsync_binary();
    let output = Command::new(&binary)
        .args([
            "-t",
            "--compare-dest=../alt",
            "--compare-dest=../nope",
            "-e",
        ])
        .arg(&shim)
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(root.join("src/f"))
        .arg(format!("localhost:{}", root.join("dst/g").display()))
        .output()
        .expect("spawn oc-rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = format!(
        "--compare-dest arg does not exist: {}\n",
        root.join("dst/../nope").display()
    );
    assert_eq!(
        stderr, expected,
        "upstream (main.c:913-914) warns once, for ../nope on dst"
    );
    assert!(output.status.success(), "exit {:?}", output.status.code());
    assert!(
        !root.join("dst/g").exists(),
        "alt/g is still a compare-dest match, so nothing is written"
    );
}

#[test]
fn pull_into_a_single_file_destination_anchors_on_its_parent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let shim = seed(root);
    let binary = oc_rsync_binary();
    let output = Command::new(&binary)
        .args(["-t", "--compare-dest=../alt", "-e"])
        .arg(&shim)
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(format!("localhost:{}", root.join("src/f").display()))
        .arg(root.join("dst/g"))
        .output()
        .expect("spawn oc-rsync");
    assert_upstream_outcome(root, &output, "pull");
}

#[test]
fn local_copy_to_a_single_file_destination_anchors_on_its_parent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    seed(root);
    let output = Command::new(oc_rsync_binary())
        .args(["-t", "--compare-dest=../alt"])
        .arg(root.join("src/f"))
        .arg(root.join("dst/g"))
        .output()
        .expect("spawn oc-rsync");
    assert_upstream_outcome(root, &output, "local");
}
