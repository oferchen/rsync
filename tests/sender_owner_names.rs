//! An INC_RECURSE sender names each owner inline once, from its id list.
//!
//! upstream: flist.c:787-800 - `user_name = add_uid(uid)` /
//! `group_name = add_gid(gid)`; uidlist.c:340-371 returns the name only on the
//! id's first sighting, and only then does `XMIT_USER_NAME_FOLLOWS` /
//! `XMIT_GROUP_NAME_FOLLOWS` carry it. Without inc_recurse no name goes
//! inline: it travels in the trailing id list (send_id_lists).
//!
//! The file entries themselves hold no names, so the sender must look them
//! up as each entry is written. The test captures the sender's output stream
//! and counts the length-prefixed owner name in it.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn id_name(flag: &str) -> Option<String> {
    let out = Command::new("id").arg(flag).output().ok()?;
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}

/// `rsync` invokes `$RSYNC_RSH <host> <command...>`; drop the host, run the
/// command locally and copy what it writes to `$CAPTURE`.
fn write_capturing_rsh(dir: &Path) -> PathBuf {
    let script = dir.join("capture_rsh.sh");
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
         \"$@\" | tee \"$CAPTURE\"\n",
    )
    .expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod");
    script
}

/// Occurrences of `name` written as an owner name: one length byte, then
/// the bytes (flist.c:send_file_entry() `write_byte(f, len); write_buf(...)`).
fn owner_name_count(stream: &[u8], name: &str) -> usize {
    let mut needle = vec![name.len() as u8];
    needle.extend_from_slice(name.as_bytes());
    stream
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Pulls `src/` with `extra` args and returns the sender's output stream.
fn sender_stream(root: &Path, extra: &[&str]) -> Vec<u8> {
    let rsh = write_capturing_rsh(root);
    let capture = root.join("sender.out");
    let dst = root.join("dst");
    let _ = fs::remove_dir_all(&dst);
    let out = test_support::OcRsyncCliRunner::new()
        .binary(oc_rsync_binary())
        .cwd(root)
        .env("CAPTURE", &capture)
        .arg("-a")
        .args(extra)
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", oc_rsync_binary().display()))
        .arg("h:src/")
        .arg("dst/")
        .run()
        .expect("transfer did not finish");
    out.assert_success();
    fs::read(&capture).expect("read capture")
}

#[test]
fn inc_recurse_sender_names_each_owner_inline_once() {
    let (Some(user), Some(group)) = (id_name("-un"), id_name("-gn")) else {
        eprintln!("skip: the test user or group has no name");
        return;
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let root = fs::canonicalize(temp.path()).expect("canonicalize");
    for dir in ["src/a", "src/b"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    for file in ["src/top", "src/a/f1", "src/a/f2", "src/b/f3"] {
        fs::write(root.join(file), file.as_bytes()).expect("write");
    }

    // INC_RECURSE is the default for a recursive transfer.
    let inc = sender_stream(&root, &[]);
    // A group named like the user matches the same bytes twice.
    let expected = if user == group { (2, 2) } else { (1, 1) };
    assert_eq!(
        (
            owner_name_count(&inc, &user),
            owner_name_count(&inc, &group)
        ),
        expected,
        "under INC_RECURSE the sender names each owner inline on its first \
         sighting only (flist.c:787-800, uidlist.c:340-371)"
    );
}
