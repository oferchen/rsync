//! `--delete-excluded` must survive the `--server` argv parse.
//!
//! Both ends of a transfer compute the same predicate before either touches the
//! filter list:
//!
//! ```c
//! /* exclude.c:1947-1948, send_filter_list()  */
//! /* exclude.c:1976-1977, recv_filter_list()  */
//! int receiver_wants_list = prune_empty_dirs
//!     || (delete_mode && (!delete_excluded || protocol_version >= 29));
//! ```
//!
//! It is a pure function of four inputs, so the two ends agree only while they
//! agree on every input. `--delete-excluded` was collapsed into the generic
//! `delete` flag by the real subprocess argv parser and never reached
//! `config.deletion.delete_excluded`, which therefore stayed `false` on the
//! server. Below protocol 29 that flips exactly one operand:
//!
//! - client, true  delete_excluded: `!true  || 28 >= 29` -> false, sends nothing
//! - server, false delete_excluded: `!false || 28 >= 29` -> true, reads a list
//!
//! The server then consumed the start of the file list as a 4-byte rule length,
//! producing a nonsense count and a truncated stream. At protocol 29 and above
//! the `protocol_version >= 29` disjunct makes the predicate true on both ends
//! regardless, which is why this only ever broke legacy peers.
//!
//! NOTE: this has nothing to do with filter rules - no `--filter` is involved
//! in any cell below. The defect reproduces on the bare option pair.
//!
//! MEASURED against rsync 3.5.0 and oc over an rsh shim, pushing a two-file
//! tree with no filter:
//!
//! ```text
//!                                   upstream        oc (before)
//! --protocol=28 --delete --delete-excluded   exit 0, 2 files   exit 12, 0 files
//! --protocol=28 --delete                     exit 0, 2 files   exit 0,  2 files
//! --protocol=32 --delete --delete-excluded   exit 0, 2 files   exit 0,  2 files
//! ```
//!
//! Exactly one cell diverged. Both agreeing rows are kept as controls and each
//! isolates one variable: dropping `--delete-excluded` at the same protocol
//! shows the option is the trigger, and keeping it at protocol 32 shows the
//! protocol is. Without them a build that broke every `--delete` push, or every
//! protocol, would still satisfy the failing row alone.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the remote-shell shim uses `/bin/sh`).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// A remote-shell stand-in: drops the leading options and the host token, then
/// execs the server command, so the "remote" is this same binary running for
/// real as `--server`. The defect lives in that subprocess argv parser, so an
/// in-process harness cannot reach it.
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

struct Outcome {
    code: Option<i32>,
    stderr: String,
    dest_entries: Vec<String>,
}

/// Pushes a two-file tree over the rsh shim at `protocol` with `delete_flags`.
fn push(protocol: u32, delete_flags: &[&str]) -> Outcome {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("foo.txt"), b"a\n").expect("write foo");
    fs::write(src.join("bar.txt"), b"b\n").expect("write bar");
    let dest = root.join("dest");
    fs::create_dir_all(&dest).expect("create dest");

    let shim = write_rsh_shim(root);
    let bin = oc_binary();
    let mut command = Command::new(&bin);
    command
        .arg("-r")
        .arg(format!("--protocol={protocol}"))
        .arg("-e")
        .arg(&shim)
        .arg("--rsync-path")
        .arg(&bin)
        .args(delete_flags)
        .arg(format!("{}/", src.display()))
        .arg(format!("lh:{}/", dest.display()));

    let output = command.output().expect("spawn oc-rsync");
    let mut dest_entries: Vec<String> = fs::read_dir(&dest)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    dest_entries.sort();

    Outcome {
        code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        dest_entries,
    }
}

fn both_files() -> Vec<String> {
    vec!["bar.txt".to_string(), "foo.txt".to_string()]
}

/// The defect: the two ends disagreed about `receiver_wants_list`, so the
/// server read a filter list the client never sent.
#[test]
fn delete_excluded_push_below_protocol_29_transfers() {
    let outcome = push(28, &["--delete", "--delete-excluded"]);
    assert_eq!(
        outcome.code,
        Some(0),
        "protocol 28 --delete --delete-excluded must succeed; stderr: {}",
        outcome.stderr
    );
    assert_eq!(
        outcome.dest_entries,
        both_files(),
        "nothing is excluded here - both files must land"
    );
    // The failure mode was a desynced stream, not a clean refusal. Pinning the
    // symptom keeps a future regression recognisable even if the exit code
    // changes.
    assert!(
        !outcome.stderr.contains("invalid filter rule length"),
        "the server must not read the file list as a filter-rule length; stderr: {}",
        outcome.stderr
    );
}

/// Control isolating the OPTION: the same protocol without
/// `--delete-excluded` agrees on both ends even with the defect present, so a
/// build that broke every `--delete` push would fail here too.
#[test]
fn plain_delete_push_below_protocol_29_transfers() {
    let outcome = push(28, &["--delete"]);
    assert_eq!(
        outcome.code,
        Some(0),
        "protocol 28 --delete must succeed; stderr: {}",
        outcome.stderr
    );
    assert_eq!(outcome.dest_entries, both_files());
}

/// Control isolating the PROTOCOL: at 29 and above the `protocol_version >= 29`
/// disjunct makes the predicate true on both ends regardless of
/// `delete_excluded`, so this cell was always green.
#[test]
fn delete_excluded_push_at_protocol_32_transfers() {
    let outcome = push(32, &["--delete", "--delete-excluded"]);
    assert_eq!(
        outcome.code,
        Some(0),
        "protocol 32 --delete --delete-excluded must succeed; stderr: {}",
        outcome.stderr
    );
    assert_eq!(outcome.dest_entries, both_files());
}
