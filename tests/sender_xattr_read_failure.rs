//! A sender whose xattr read fails leaves the entry out of the file list.
//!
//! upstream: flist.c:2058-2063 send_file_name() - when get_xattr() returns
//! -1 the sender sets `io_error |= IOERR_GENERAL` and returns NULL, so the
//! file never joins the list; get_xattr_data() has already printed
//! `get_xattr_data: lgetxattr(<full_fname>,"<name>",0) failed: ...`
//! (xattrs.c). The run ends with code 23 and the receiver never sees the file.
//! A sender that ignores the failure lists the file with no xattrs and only
//! fails later, opening its data, with a different message.
//!
//! The fixture is a user xattr on a mode-000 file: listing names needs no
//! permission, reading a `user.*` value does, so only the value read fails.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::{UpstreamVersion, require_upstream_rsync};

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// A remote shell that drops the host name and runs the rest locally, so
/// `--rsync-path` names the sender binary.
fn rsh_wrapper(dir: &Path) -> PathBuf {
    let wrapper = dir.join("rsh.sh");
    fs::write(&wrapper, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

#[test]
fn oc_sender_drops_a_file_whose_xattrs_cannot_be_read() {
    if let Some(reason) = test_support::Capabilities::probe()
        .skip_reason(&[test_support::capabilities::names::XATTRS])
    {
        eprintln!("skip: {reason}");
        return;
    }
    if !test_support::skip::require_command_on_path("setfattr") {
        return;
    }
    if fs::metadata("/proc/self").is_ok_and(|meta| std::os::unix::fs::MetadataExt::uid(&meta) == 0)
    {
        eprintln!("skip: root reads a mode-000 file's xattrs");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let src = temp.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(src.join("a"), b"a\n").unwrap();
    let locked = src.join("b");
    fs::write(&locked, b"b\n").unwrap();
    let tagged = Command::new("setfattr")
        .args(["-n", "user.x", "-v", "1"])
        .arg(&locked)
        .status()
        .is_ok_and(|s| s.success());
    if !tagged {
        eprintln!("skip: the temp filesystem does not take user xattrs");
        return;
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let wrapper = rsh_wrapper(temp.path());
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_1);
    let mut clients = vec![("oc", oc_rsync())];
    clients.extend(
        upstream
            .as_ref()
            .map(|u| ("upstream", u.binary().to_path_buf())),
    );
    for (client, client_bin) in &clients {
        let dest = temp.path().join(format!("by-{client}"));
        let out = Command::new(client_bin)
            .args(["-rX", "-e"])
            .arg(&wrapper)
            .arg(format!("--rsync-path={}", oc_rsync().display()))
            .arg(format!("lh:{}/", src.display()))
            .arg(format!("{}/", dest.display()))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(23), "{client} client: {out:?}");
        assert!(
            stderr.contains(&format!(
                "rsync: [sender] get_xattr_data: lgetxattr(\"{}\",\"user.x\",0) failed: Permission denied (13)",
                locked.display()
            )),
            "{client} client: the sender names the failed read: {stderr}"
        );
        assert!(
            !stderr.contains("send_files failed to open"),
            "{client} client: the file never reached the transfer: {stderr}"
        );
        assert!(
            dest.join("a").exists(),
            "{client} client: the readable file arrives"
        );
        assert!(
            !dest.join("b").exists(),
            "{client} client: the dropped file does not"
        );
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).unwrap();
}
