//! Without `-r` or `-d`, a directory source operand transfers NOTHING.
//!
//! Upstream's argument loop stats each operand and then refuses a directory
//! outright when `xfer_dirs` is off:
//!
//! ```c
//! /* rsync-3.5.0/flist.c:2723-2726 */
//! if (S_ISDIR(st.st_mode) && !xfer_dirs) {
//!         rprintf(FINFO, "skipping directory %s\n", fbuf);
//!         continue;
//! }
//! ```
//!
//! `xfer_dirs` resolves to `-d`, or `-r`, or `--list-only` when neither was
//! given (`options.c:2314-2320`); `--files-from` forces it on
//! (`options.c:2307-2308`). The `continue` is total: no directory entry, no
//! children, and - under `--relative` - none of the operand's implied parents.
//!
//! The DOTDIR spellings are NOT exempt. `dir/` and `dir/.` are rewritten to
//! `dir/.` with `name_type = DOTDIR_NAME` (`flist.c:2586-2604`), but they still
//! `link_stat` as a directory, so they hit this same skip. The marker decides
//! "contents of" only once directories are being transferred at all.
//!
//! oc's NETWORK sender had no such gate. `oc-rsync -a --no-r host:tree/sub/
//! dest/` delivered a whole level of children upstream refuses to send, and a
//! `-R` operand additionally materialised every implied parent directory. The
//! local-copy executor had the gate but lost the message for the DOTDIR
//! spelling, because the name upstream prints there is a bare `.` (the `fn`
//! half of its `dir`/`fn` split) and oc passed `None`.
//!
//! Every expectation below was pinned against the real rsync 3.5.0 binary on
//! the identical fixture, in both directions.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// SSH-shaped shim: drop the options and the host placeholder, exec the rest.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    tree: PathBuf,
    dest: PathBuf,
    shim: PathBuf,
}

/// Builds `tree/sub/{top.txt,deep/f2}` plus `tree/alias -> sub`.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let tree = temp.path().join("tree");
    fs::create_dir_all(tree.join("sub/deep")).expect("create tree");
    fs::write(tree.join("sub/deep/f2"), b"payload").expect("write f2");
    fs::write(tree.join("sub/top.txt"), b"top").expect("write top.txt");
    std::os::unix::fs::symlink("sub", tree.join("alias")).expect("symlink alias -> sub");
    let dest = temp.path().join("dest");
    fs::create_dir_all(&dest).expect("create dest");
    let shim = write_rsh_shim(temp.path());
    Fixture {
        _temp: temp,
        tree,
        dest,
        shim,
    }
}

fn delivered(dest: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![dest.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read dest") {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(dest).expect("under dest");
            found.push(rel.to_string_lossy().into_owned());
            if path.symlink_metadata().expect("stat entry").is_dir() {
                stack.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Pulls `tree/<operand>` through the remote-shell shim, returning
/// `(delivered entries, stdout)`.
fn pull(operand: &str, extra: &[&str]) -> (Vec<String>, String) {
    let fx = fixture();
    let binary = oc_rsync_binary();
    let output = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .args(extra)
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("xferdirshost:{}/{operand}", fx.tree.display()))
        .arg(format!("{}/", fx.dest.display()))
        .run()
        .expect("pull did not finish");
    output.assert_success();
    (delivered(&fx.dest), output.stdout_str().into_owned())
}

/// Copies `tree/<operand>` with the local-copy executor (no remote shell).
fn local(operand: &str, extra: &[&str]) -> (Vec<String>, String) {
    let fx = fixture();
    let output = test_support::OcRsyncCliRunner::new()
        .binary(oc_rsync_binary())
        .args(extra)
        .arg(format!("{}/{operand}", fx.tree.display()))
        .arg(format!("{}/", fx.dest.display()))
        .run()
        .expect("local copy did not finish");
    output.assert_success();
    (delivered(&fx.dest), output.stdout_str().into_owned())
}

/// The headline defect: a DOTDIR operand without `-r`/`-d` shipped a level of
/// children upstream never sends.
#[test]
fn a_dotdir_operand_without_xfer_dirs_transfers_nothing() {
    for operand in ["sub/", "sub/.", "alias/"] {
        let (entries, stdout) = pull(operand, &["-lptgoD"]);
        assert!(
            entries.is_empty(),
            "`{operand}` without -r/-d must deliver nothing (upstream flist.c:2723 \
             `continue`s the argument); got {entries:?}"
        );
        assert!(
            stdout.contains("skipping directory .\n"),
            "upstream prints the `fn` half of its dir/fn split, which is a bare \
             `.` for a DOTDIR operand; got stdout {stdout:?}"
        );
    }
}

/// The non-DOTDIR spelling is skipped too, and names itself.
#[test]
fn a_bare_directory_operand_without_xfer_dirs_transfers_nothing() {
    let (entries, stdout) = pull("sub", &["-lptgoD"]);
    assert!(
        entries.is_empty(),
        "a bare directory operand must not even create the directory; got {entries:?}"
    );
    assert!(
        stdout.contains("skipping directory sub\n"),
        "upstream names the operand's `fn` half; got stdout {stdout:?}"
    );
}

/// `--relative` makes the skip louder: upstream emits none of the implied
/// parents either, because `send_implied_dirs()` sits after the `continue`.
#[test]
fn a_relative_directory_operand_emits_no_implied_parents() {
    let (entries, _) = pull("sub/", &["-lptgoD", "-R"]);
    assert!(
        entries.is_empty(),
        "the flist.c:2723 `continue` runs BEFORE send_implied_dirs (flist.c:2735), \
         so a skipped operand contributes no ancestor chain either; got {entries:?}"
    );
}

/// Non-vacuity for the fixture: `-d` turns `xfer_dirs` on and the very same
/// operand delivers one level. Without this cell a sender that transferred
/// nothing at all would pass every assertion above.
#[test]
fn dirs_flag_restores_the_one_level_walk() {
    let (entries, stdout) = pull("sub/", &["-lptgoD", "-d"]);
    assert_eq!(
        entries,
        vec!["deep".to_string(), "top.txt".to_string()],
        "-d sets xfer_dirs, so the DOTDIR operand walks one level again"
    );
    assert!(
        !stdout.contains("skipping directory"),
        "nothing is skipped once xfer_dirs is on; got stdout {stdout:?}"
    );
}

/// The other non-vacuity leg: `-r` must still deliver the full subtree.
#[test]
fn recursion_still_delivers_the_whole_subtree() {
    let (entries, _) = pull("sub/", &["-lptgoD", "-r"]);
    assert_eq!(
        entries,
        vec![
            "deep".to_string(),
            "deep/f2".to_string(),
            "top.txt".to_string()
        ],
        "-r is the other xfer_dirs source; the recursive walk must be untouched"
    );
}

/// `--list-only` is upstream's third `xfer_dirs` source (`options.c:2320`), so
/// the same operand with no `-r`/`-d` still lists.
#[test]
fn list_only_keeps_xfer_dirs_on() {
    let fx = fixture();
    let binary = oc_rsync_binary();
    let output = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .arg("-lptgoD")
        .arg("--list-only")
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("xferdirshost:{}/sub/", fx.tree.display()))
        .run()
        .expect("list did not finish");
    output.assert_success();
    let stdout = output.stdout_str();
    assert!(
        stdout.contains("top.txt"),
        "--list-only sets xfer_dirs (options.c:2320), so the listing must still \
         happen; got stdout {stdout:?}"
    );
    assert!(
        !stdout.contains("skipping directory"),
        "got stdout {stdout:?}"
    );
}

/// A push puts the sender in the local client process, where the FINFO goes
/// straight to stdout instead of riding an `MSG_INFO` frame.
#[test]
fn a_pushed_dotdir_operand_is_skipped_and_named_too() {
    let fx = fixture();
    let binary = oc_rsync_binary();
    let output = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .arg("-lptgoD")
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/sub/", fx.tree.display()))
        .arg(format!("xferdirshost:{}/", fx.dest.display()))
        .run()
        .expect("push did not finish");
    output.assert_success();
    assert!(delivered(&fx.dest).is_empty(), "a push must skip it too");
    assert!(
        output.stdout_contains("skipping directory .\n"),
        "upstream log.c:322-330 puts FINFO on stdout, not stderr; got stdout {:?}",
        output.stdout_str()
    );
}

/// `--quiet` silences it. Upstream applies that on `rwrite()`'s FINFO arm
/// (`log.c:344-346`), which sits BEFORE the `am_server` frame send at
/// `log.c:357` and is met again when the peer's frame is delivered through
/// `read_a_msg()`. Both ends therefore drop the line.
#[test]
fn quiet_suppresses_the_skip_line() {
    let (entries, stdout) = pull("sub/", &["-lptgoD", "--quiet"]);
    assert!(entries.is_empty(), "got {entries:?}");
    assert!(
        !stdout.contains("skipping directory"),
        "--quiet returns early on the FINFO arm; got stdout {stdout:?}"
    );
}

/// The local-copy executor already refused the transfer; what it lost was the
/// name. Upstream prints `.` for a DOTDIR operand, not an empty line and not
/// silence.
#[test]
fn the_local_executor_names_a_skipped_dotdir_operand() {
    for operand in ["sub/", "sub/.", "alias/"] {
        let (entries, stdout) = local(operand, &["-lptgoD"]);
        assert!(entries.is_empty(), "got {entries:?}");
        assert!(
            stdout.contains("skipping directory .\n"),
            "`{operand}` must name the skip `.` (upstream flist.c:2725 with the \
             `fn` half of the dir/fn split); got stdout {stdout:?}"
        );
    }
}

/// Non-vacuity for the local cell: the non-DOTDIR spelling already printed its
/// name, and must keep printing exactly that.
#[test]
fn the_local_executor_still_names_a_bare_directory_operand() {
    let (entries, stdout) = local("sub", &["-lptgoD"]);
    assert!(entries.is_empty(), "got {entries:?}");
    assert!(
        stdout.contains("skipping directory sub\n"),
        "got stdout {stdout:?}"
    );
}
