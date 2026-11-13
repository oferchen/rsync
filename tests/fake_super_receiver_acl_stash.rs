//! A `--fake-super` receiver stashes every received ACL, pinned against 3.5.1.
//!
//! upstream: acls.c:1228 set_rsync_acl() - with `am_root < 0` the ACL goes
//! into the `%aacl`/`%dacl` xattrs instead of `sys_acl_set_file()`. Every
//! set_file_attrs() caller reaches it: new and existing directories, and a
//! file the quick check finds up to date.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::{UpstreamVersion, require_upstream_rsync};

fn has_tool(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn setfacl(args: &[&str], path: &Path) -> bool {
    Command::new("setfacl")
        .args(args)
        .arg(path)
        .status()
        .unwrap()
        .success()
}

/// The real ACL and the ACL stashes of `rel` under `dest`, as one text block.
fn acl_state(dest: &Path, rel: &str) -> String {
    let path = dest.join(rel);
    let acl = Command::new("getfacl")
        .args(["-cp"])
        .arg(&path)
        .output()
        .unwrap();
    let stash = Command::new("getfattr")
        .args(["-d", "-e", "hex", "-m", r"^user\.rsync\.%[ad]acl$"])
        .arg(&path)
        .output()
        .unwrap();
    let stash: String = String::from_utf8_lossy(&stash.stdout)
        .lines()
        .filter(|l| l.starts_with("user."))
        .map(|l| format!("{l}\n"))
        .collect();
    format!("== {rel}\n{}{stash}", String::from_utf8_lossy(&acl.stdout))
}

/// A 3.5.1 client pushes `-rtA` to each server with `-M--fake-super`, then
/// pushes again after the source ACLs change. Both servers must leave the
/// same real ACLs and stashes; the second push reaches the existing
/// directory and the up-to-date file.
#[test]
fn fake_super_server_stashes_directory_and_up_to_date_file_acls() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    if !["setfacl", "getfacl", "getfattr"].into_iter().all(has_tool) {
        eprintln!("skipping: acl/attr tools not installed");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let src = root.join("src");
    fs::create_dir_all(src.join("d")).unwrap();
    fs::write(src.join("d/f"), "x\n").unwrap();
    if !setfacl(&["-m", "u:nobody:rx"], &src.join("d"))
        || !setfacl(&["-d", "-m", "u:nobody:rx"], &src.join("d"))
        || !setfacl(&["-m", "u:nobody:r"], &src.join("d/f"))
    {
        eprintln!("skipping: the filesystem refuses POSIX ACLs");
        return;
    }
    // A remote shell that runs the server locally, like support/lsh.sh.
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).unwrap();

    let servers: [(&str, PathBuf); 2] = [
        ("upstream", upstream.binary().to_path_buf()),
        ("oc", PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))),
    ];
    let push = |server: &Path, dest: &Path| {
        let out = Command::new(upstream.binary())
            .args(["-rtA", "-M--fake-super", "-e"])
            .arg(&rsh)
            .arg(format!("--rsync-path={}", server.display()))
            .arg(format!("{}/", src.display()))
            .arg(format!("localhost:{}/", dest.display()))
            .output()
            .unwrap();
        assert!(out.status.success(), "{}: {out:?}", server.display());
    };
    let state = |dest: &Path| format!("{}{}", acl_state(dest, "d"), acl_state(dest, "d/f"));

    for round in ["new", "changed"] {
        if round == "changed" {
            assert!(setfacl(&["-m", "u:nobody:rwx"], &src.join("d")));
            assert!(setfacl(&["-m", "u:nobody:rw"], &src.join("d/f")));
        }
        let mut seen = Vec::new();
        for (label, server) in &servers {
            let dest = root.join(format!("dst_{label}"));
            fs::create_dir_all(&dest).unwrap();
            push(server, &dest);
            seen.push(state(&dest));
        }
        assert!(
            !seen[0].contains("user:nobody") && seen[0].contains("user.rsync.%dacl"),
            "oracle ({round}): ACLs are stashed, not applied:\n{}",
            seen[0]
        );
        assert_eq!(seen[1], seen[0], "oc server differs from 3.5.1 ({round})");
    }
}
