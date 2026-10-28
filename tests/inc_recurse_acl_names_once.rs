//! Under INC_RECURSE a sender names an ACL entry's user only on the id's first
//! sighting in the transfer, across every sub-list.
//!
//! upstream: acls.c:821-826 send_ida_entries() sends the name `add_uid()`
//! returns, and uidlist.c:340-354 add_uid() returns one only when it first
//! inserts the id. rsync 3.5.1 therefore names `root` once below, although
//! each file's ACL is a different, literal one in its own sub-list.
//!
//! A push with `--write-batch` tees the sender's stream into the batch file,
//! so the test counts the name in exactly what crossed the wire. The network
//! sender reuses one file-list writer across sub-lists, which is what keeps
//! one sighting set for the whole transfer.
//!
//! Skip conditions (test passes with a printed reason): no `setfacl`, or a
//! filesystem without POSIX ACLs.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// A remote-shell stand-in: drops the leading options and the host token, then
/// execs the server command.
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
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

/// Builds `src/{a,b,c}/f`, each with a distinct ACL naming user `root`.
/// Returns `false` when ACLs cannot be set here.
fn build_tree(src: &Path) -> bool {
    for (dir, perms) in [("a", "r"), ("b", "rw"), ("c", "rwx")] {
        let file = src.join(dir).join("f");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, dir).unwrap();
        let set = Command::new("setfacl")
            .arg("-m")
            .arg(format!("u:root:{perms}"))
            .arg(&file)
            .status();
        if !matches!(set, Ok(status) if status.success()) {
            return false;
        }
    }
    true
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

#[test]
fn acl_names_cross_the_wire_once_per_id() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let src = root.join("src");
    if !build_tree(&src) {
        eprintln!("skip: setfacl unavailable or ACLs unsupported here");
        return;
    }
    let rsh = write_rsh_shim(root);
    let oc = oc_binary();
    let src_arg = format!("{}/", src.display());
    let batch = root.join("bf");
    let out = Command::new(&oc)
        .arg("-rA")
        .arg(format!("--write-batch={}", batch.display()))
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", oc.display()))
        .arg(&src_arg)
        .arg(format!("localhost:{}/", root.join("dst").display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "push: {out:?}");
    let bytes = fs::read(&batch).unwrap();
    assert_eq!(
        count(&bytes, b"\x04root"),
        1,
        "`root` is named on its first sighting only"
    );
}
