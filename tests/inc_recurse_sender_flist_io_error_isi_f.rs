//! ISI.f - INC_RECURSE sender with an unreadable directory.
//!
//! The source holds `a/readable_one`, `a/readable_two` and `a/forbidden`,
//! the last made mode 0000 so the sender's opendir fails. rsync 3.5.2's
//! sender reports `opendir ... failed: Permission denied (13)`, sends the
//! rest of the list with the I/O error flag set (flist.c `send_file_list()`
//! writes `io_error` after the list), and the client exits 23 (RERR_PARTIAL)
//! after transferring every readable file.
//!
//! An rsync 3.5.2 client pulls the tree from an oc-rsync sender and from a
//! 3.5.2 sender. Both must exit 23, land the same tree, and print the same
//! stderr lines. Root bypasses the mode, so the cell cannot provoke the
//! failure as root and says so.

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use integration::isi_harness::{Entry, peers, pin_mtimes, pull_both, write_fixture_file};
use test_support::{UpstreamVersion, require_upstream_rsync};

const SIZES: [usize; 5] = [0, 1, 64, 1024, 4097];
const RERR_PARTIAL: i32 = 23;

/// Restores the poisoned directory's mode so the temp dir can be removed.
struct Restore(PathBuf);

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
}

fn sorted_lines(text: &str) -> Vec<&str> {
    let mut lines: Vec<&str> = text.lines().collect();
    lines.sort_unstable();
    lines
}

#[test]
fn unreadable_directory_pull_from_oc_sender_matches_rsync_3_5_2() {
    if is_root() {
        eprintln!("skipped: root bypasses the 0000 mode that makes opendir fail");
        return;
    }
    let Some(peers) = peers(require_upstream_rsync(UpstreamVersion::V3_5_2)) else {
        return;
    };
    let work = tempfile::tempdir().expect("work dir");
    let src = work.path().join("src");
    for (dir_idx, dir) in ["a/readable_one", "a/readable_two", "a/forbidden"]
        .iter()
        .enumerate()
    {
        let path = src.join(dir);
        fs::create_dir_all(&path).expect("create dir");
        for (idx, size) in SIZES.iter().enumerate() {
            let seed = (dir_idx * SIZES.len() + idx) as u32;
            write_fixture_file(&path.join(format!("file_{idx}.bin")), seed, *size);
        }
    }
    pin_mtimes(&src);
    let forbidden = src.join("a/forbidden");
    fs::set_permissions(&forbidden, fs::Permissions::from_mode(0o000)).expect("chmod 0000");
    let _restore = Restore(forbidden);

    let ((oc, oc_tree), (up, up_tree)) = pull_both(&peers, work.path(), &src, &["-rt"]);
    assert_eq!(up.code, Some(RERR_PARTIAL), "3.5.2 sender: {}", up.stderr);
    assert!(
        up.stderr.contains("Permission denied (13)"),
        "3.5.2 sender must report the opendir failure: {}",
        up.stderr
    );
    for dir in ["a/readable_one", "a/readable_two"] {
        for idx in 0..SIZES.len() {
            let rel = PathBuf::from(format!("{dir}/file_{idx}.bin"));
            assert!(
                matches!(up_tree.get(&rel), Some(Entry::File(_))),
                "3.5.2 sender must transfer {}",
                rel.display()
            );
        }
    }
    assert_eq!(
        oc.code, up.code,
        "exit code, oc sender vs 3.5.2 sender: {}",
        oc.stderr
    );
    assert_eq!(oc_tree, up_tree, "destination, oc sender vs 3.5.2 sender");
    assert_eq!(
        sorted_lines(&oc.stderr),
        sorted_lines(&up.stderr),
        "client stderr, oc sender vs 3.5.2 sender"
    );
}
