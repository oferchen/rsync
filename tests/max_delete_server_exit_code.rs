//! A `--server` process whose `--max-delete` limit cut the delete pass short
//! must exit `RERR_DEL_LIMIT` (25), not 0.
//!
//! upstream `generator.c:2900-2905` sets `IOERR_DEL_LIMIT` when deletions were
//! skipped, and `cleanup.c:210-218` turns the accumulated `io_error` bits into
//! the process exit code for EVERY role, a server included: a clean finish with
//! `IOERR_DEL_LIMIT` set exits 25. The remote shell hands that status to the
//! client (`main.c:1412`), which is how a push learns the limit was hit.
//!
//! Measured against rsync 3.5.1 over this fixture: the server exits 25. oc's
//! server returned a flat 0 for every successful run, so a push through an oc
//! server reported success while leaving extraneous files behind.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const EXTRAS: usize = 3;

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Remote-shell stand-in that runs the server command locally and records its
/// exit status, so the test reads the server's own code rather than whatever
/// the client derives from the stream.
fn write_rsh_shim(dir: &Path, rc_file: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             while [ $# -gt 0 ]; do\n\
             case \"$1\" in\n\
             -*) shift ;;\n\
             *) break ;;\n\
             esac\n\
             done\n\
             shift || true\n\
             \"$@\"\n\
             rc=$?\n\
             echo $rc > '{}'\n\
             exit $rc\n",
            rc_file.display()
        ),
    )
    .expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
    rc_file: PathBuf,
}

/// `src/` holds one file; `dst/` holds the same file plus `EXTRAS` files the
/// source lacks, so `--delete` has exactly `EXTRAS` candidates.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    fs::write(root.join("src/keep"), "keep\n").expect("write src");
    for i in 0..EXTRAS {
        fs::write(root.join(format!("dst/extra{i}")), "extra\n").expect("write extra");
    }
    let rc_file = root.join("server.rc");
    let shim = write_rsh_shim(&root, &rc_file);
    Fixture {
        _temp: temp,
        root,
        shim,
        rc_file,
    }
}

/// Pushes through the shim, so the remote `--server` is the receiver that runs
/// the delete pass. Returns the server's exit status and the client's stderr.
fn push(fx: &Fixture, max_delete: usize) -> (i32, String) {
    let binary = oc_rsync_binary();
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .args(["-r", "--delete"])
        .arg(format!("--max-delete={max_delete}"))
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/src/", fx.root.display()))
        .arg(format!("mdhost:{}/dst/", fx.root.display()))
        .run()
        .expect("push did not finish");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let rc = fs::read_to_string(&fx.rc_file)
        .unwrap_or_else(|e| panic!("server status not recorded ({e}); stderr was: {stderr}"));
    (rc.trim().parse().expect("numeric server status"), stderr)
}

fn remaining_extras(fx: &Fixture) -> usize {
    (0..EXTRAS)
        .filter(|i| fx.root.join(format!("dst/extra{i}")).exists())
        .count()
}

#[test]
fn a_server_that_hit_the_delete_limit_exits_25() {
    let fx = fixture();
    let (rc, stderr) = push(&fx, 1);
    // The limit really cut the pass short: one deletion, the rest skipped.
    assert_eq!(remaining_extras(&fx), EXTRAS - 1, "stderr was: {stderr}");
    assert_eq!(rc, 25, "stderr was: {stderr}");
}

/// Non-vacuity: with a limit the pass never reaches, the same server deletes
/// every extra and exits 0, so 25 above is caused by the limit and nothing else.
#[test]
fn a_server_under_the_delete_limit_exits_0() {
    let fx = fixture();
    let (rc, stderr) = push(&fx, EXTRAS + 1);
    assert_eq!(remaining_extras(&fx), 0, "stderr was: {stderr}");
    assert_eq!(rc, 0, "stderr was: {stderr}");
}
