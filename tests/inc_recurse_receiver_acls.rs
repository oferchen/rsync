//! ACLs of entries an INC_RECURSE receiver reads from sub-lists.
//!
//! Under INC_RECURSE every directory after the first arrives in its own
//! sub-list, and each sub-list's `send_acl()` calls add to the same growing
//! `access_acl_list`/`default_acl_list` the initial list started
//! (`// upstream: acls.c:94-95, acls.c:1016-1024 receive_acl()`). A
//! receiver that stops reading that list after the initial file list applies
//! nothing to a sub-list entry whose ACL is new, so the file keeps whatever
//! its parent's default ACL gave it and the run still exits 0.
//!
//! The fixture puts a default ACL on `sub/` and a different ACL on
//! `sub/b.log`, so a dropped ACL is visible as the inherited entries. Every
//! cell compares the destination's `getfacl` entries with the source's; the
//! oc-to-oc cells run everywhere, and the cells with an upstream 3.5.1 peer
//! run when that binary resolves.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::{UpstreamVersion, require_upstream_rsync};

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// `getfacl` entries of every path under `dir`, keyed by path. The owner
/// header lines are dropped: a non-root receiver cannot chown.
fn acl_snapshot(dir: &Path) -> BTreeMap<String, Vec<String>> {
    let out = Command::new("getfacl")
        .args(["-R", "-p", "."])
        .current_dir(dir)
        .output()
        .expect("run getfacl");
    assert!(out.status.success(), "getfacl {}: {out:?}", dir.display());
    let mut acls = BTreeMap::new();
    for block in String::from_utf8_lossy(&out.stdout).split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.is_empty());
        let Some(file) = lines.next() else { continue };
        let entries = lines
            .filter(|l| !l.starts_with("# owner:") && !l.starts_with("# group:"))
            .map(str::to_owned)
            .collect();
        acls.insert(file.to_owned(), entries);
    }
    acls
}

/// Applies `setfacl args` to `path`; `false` when the filesystem refuses.
fn setfacl(args: &[&str], path: &Path) -> bool {
    Command::new("setfacl")
        .args(args)
        .arg(path)
        .status()
        .is_ok_and(|s| s.success())
}

/// Builds `src/` with a sub-list directory carrying a default ACL and a
/// file inside it with an ACL of its own; `None` when ACLs are unavailable.
fn acl_tree(root: &Path) -> Option<PathBuf> {
    if !test_support::skip::require_command_on_path("setfacl")
        || !test_support::skip::require_command_on_path("getfacl")
    {
        return None;
    }
    let src = root.join("src");
    fs::create_dir_all(src.join("sub/deep")).unwrap();
    fs::write(src.join("a.txt"), b"alpha\n").unwrap();
    fs::write(src.join("sub/b.log"), b"beta\n").unwrap();
    fs::write(src.join("sub/deep/c.txt"), b"gamma\n").unwrap();
    if !setfacl(&["-m", "u:65534:r-x"], &src.join("a.txt")) {
        eprintln!("skip: the temp filesystem does not take a named ACL entry");
        return None;
    }
    for (args, path) in [
        (&["-m", "g:65534:r--"][..], "sub/b.log"),
        (&["-m", "u:65534:rw-"], "sub/deep/c.txt"),
        (&["-d", "-m", "u:65534:r-x,g:65534:rwx"], "sub"),
        (&["-m", "u:65534:rwx"], "sub/deep"),
    ] {
        assert!(setfacl(args, &src.join(path)), "setfacl {args:?} {path}");
    }
    Some(src)
}

/// A remote shell that drops the host name and runs the rest locally, so
/// `--rsync-path` names the server binary.
fn rsh_wrapper(dir: &Path) -> PathBuf {
    let wrapper = dir.join("rsh.sh");
    fs::write(&wrapper, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

/// upstream: acls.c:961-1013 recv_rsync_acl() appends each new ACL to the one
/// list every later index names, sub-lists included (and set_acl() reads it
/// at acls.c:1389). Every client/server pairing pulls and pushes, so oc is
/// the receiver as pulling client and as pushed-to server, and the
/// upstream-only cells are the control: under `--inc-recursive` the sub-list
/// entries must keep their own ACLs exactly as they do without it.
#[test]
fn inc_recurse_receiver_applies_sub_list_acls() {
    let temp = tempfile::tempdir().unwrap();
    let Some(src) = acl_tree(temp.path()) else {
        return;
    };
    let want = acl_snapshot(&src);
    let wrapper = rsh_wrapper(temp.path());
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_1);
    let mut peers = vec![("oc", oc_rsync())];
    peers.extend(upstream.as_ref().map(|u| ("upstream", u.binary().to_path_buf())));
    let mut cells = 0;
    for (client, client_bin) in &peers {
        for (server, server_bin) in &peers {
            for push in [false, true] {
                for mode in ["--inc-recursive", "--no-inc-recursive"] {
                    let dest = temp.path().join(format!(
                        "{client}-{server}-{}{mode}",
                        if push { "push" } else { "pull" }
                    ));
                    let (from, to) = if push {
                        (format!("{}/", src.display()), format!("lh:{}/", dest.display()))
                    } else {
                        (format!("lh:{}/", src.display()), format!("{}/", dest.display()))
                    };
                    let out = Command::new(client_bin)
                        .args(["-rA", mode, "-e"])
                        .arg(&wrapper)
                        .arg(format!("--rsync-path={}", server_bin.display()))
                        .args([&from, &to])
                        .output()
                        .unwrap();
                    let cell = format!("client={client} server={server} push={push} {mode}");
                    assert!(out.status.success(), "{cell}: {out:?}");
                    assert_eq!(acl_snapshot(&dest), want, "{cell}");
                    cells += 1;
                }
            }
        }
    }
    assert!(cells >= 4, "the oc-to-oc pull and push cells ran: {cells}");
}
