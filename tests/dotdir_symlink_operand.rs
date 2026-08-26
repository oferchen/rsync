//! A source operand carrying upstream's DOTDIR marker must follow a
//! symlink-to-directory, with no `--copy-dirlinks` required.
//!
//! Upstream sets `name_type = DOTDIR_NAME` for an operand ending in `/`
//! (`flist.c:2589-2594`) or `/.` (`:2604`), and `flist.c:2697` passes
//! `copy_dirlinks || name_type != NORMAL_NAME` as `link_stat()`'s
//! follow_dirlinks argument. The marker therefore does two things: it selects
//! "contents of", and it resolves the operand before the walk decides what it
//! is.
//!
//! oc gated that follow on `--copy-dirlinks` alone, so
//! `oc-rsync -a host:current/ dest/` - where `current -> releases/2026-08-25`,
//! an entirely ordinary layout - DELIVERED NOTHING and exited 0. The `/.`
//! spelling failed differently: it transmitted the symlink itself, which under
//! a restricted shell publishes the link's absolute target to the client.
//!
//! These cells run over a real two-process remote-shell transfer because the
//! defect is in the NETWORK sender's file-list walk. The local-copy executor
//! was measured correct on the same operands throughout, so a local fixture
//! would pass without exercising the fix at all.

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

/// Builds `tree/sub/deep/f2` plus `tree/alias -> sub`, pulls `tree/<operand>`
/// through the shim, and returns the delivered entries, sorted.
fn pull(operand: &str, extra: &[&str]) -> Vec<String> {
    let temp = tempfile::tempdir().expect("tempdir");
    let tree = temp.path().join("tree");
    fs::create_dir_all(tree.join("sub/deep")).expect("create tree");
    fs::write(tree.join("sub/deep/f2"), b"payload").expect("write f2");
    std::os::unix::fs::symlink("sub", tree.join("alias")).expect("symlink alias -> sub");

    let dest = temp.path().join("dest");
    fs::create_dir_all(&dest).expect("create dest");
    let binary = oc_rsync_binary();
    let shim = write_rsh_shim(temp.path());

    let output = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .arg("-a")
        .args(extra)
        .arg("--rsh")
        .arg(&shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("dotdirhost:{}/{operand}", tree.display()))
        .arg(format!("{}/", dest.display()))
        .run()
        .expect("pull did not finish");
    output.assert_success();

    let mut found = Vec::new();
    let mut stack = vec![dest.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read dest") {
            let path = entry.expect("dir entry").path();
            let rel = path.strip_prefix(&dest).expect("under dest");
            found.push(rel.to_string_lossy().into_owned());
            if path.symlink_metadata().expect("stat entry").is_dir() {
                stack.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The headline defect: a trailing `/` on a symlink-to-directory delivered
/// NOTHING and still exited 0.
#[test]
fn trailing_slash_on_a_symlinked_dir_transfers_its_contents() {
    assert_eq!(
        pull("alias/", &[]),
        vec!["deep".to_string(), "deep/f2".to_string()],
        "a DOTDIR operand must resolve the symlink and transfer the contents; \
         delivering nothing here is silent data loss, not an empty directory"
    );
}

/// The `/.` spelling is DOTDIR too, and its old failure was different in kind:
/// it transmitted the symlink itself rather than the contents.
#[test]
fn trailing_dot_on_a_symlinked_dir_transfers_its_contents() {
    assert_eq!(
        pull("alias/.", &[]),
        vec!["deep".to_string(), "deep/f2".to_string()],
        "`/.` is upstream's other DOTDIR spelling (flist.c:2604); transmitting \
         `alias` itself publishes the link instead of the tree it names"
    );
}

/// Non-vacuity for the fixture: the same tree through a REAL directory already
/// worked, so it cannot be what makes the two cells above pass.
#[test]
fn trailing_slash_on_a_real_dir_is_unchanged() {
    assert_eq!(
        pull("sub/", &[]),
        vec!["deep".to_string(), "deep/f2".to_string()],
        "the real-directory control must keep behaving exactly as before"
    );
}

/// The other side of the gate: WITHOUT the marker the symlink stays a symlink.
/// Without this, a fix that simply always followed dirlinks would pass every
/// cell above while breaking `-a` on an ordinary symlink.
#[test]
fn no_marker_leaves_the_symlink_a_symlink() {
    assert_eq!(
        pull("alias", &[]),
        vec!["alias".to_string()],
        "a bare operand is NORMAL_NAME: upstream transmits the link itself"
    );
}
