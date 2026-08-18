//! A perishable (`p`) filter rule must NOT make a receiver refuse a pre-30 peer.
//!
//! ```c
//! /* exclude.c:1872-1877, get_rule_prefix() */
//! if (rule->rflags & FILTRULE_PERISHABLE) {
//!     if (!for_xfer || protocol_version >= 30)
//!         *op++ = 'p';
//!     else if (am_sender)
//!         return NULL;
//! }
//! ```
//!
//! Below protocol 30 upstream never writes the `p` byte, so a perishable rule
//! cannot overflow `legal_len` and cannot be "too modern" on its own. The only
//! NULL is the sender-only `else if (am_sender)`.
//!
//! oc listed `perishable` among the modifiers that overflow `legal_len` in
//! `build_old_prefix`, which is role-blind - so a RECEIVING client refused a
//! pre-29 pull that upstream completes. This file pins the receiver side.
//!
//! MEASURED against rsync 3.5.0 (`--filter='-p bar.txt'`, remote via an rsh
//! shim, no `--delete`):
//!
//! ```text
//! pull --protocol=28  exit 0   foo.txt transferred   (oc exited 2 before)
//! pull --protocol=32  exit 0   foo.txt transferred
//! push --protocol=32  exit 0   foo.txt transferred
//! ```
//!
//! ⚠ THE SENDER HALF IS DELIBERATELY NOT TESTED HERE, because it is NOT
//! implemented. oc does not abort a protocol-28 PUSH carrying a perishable
//! rule where upstream would. Making it abort is NOT a one-line hoist: see
//! `send_rules` (exclude.c:1905-1912), where a rule elided as `LOCAL_RULE`
//! `continue`s BEFORE `get_rule_prefix` is ever called, and exclude.c:1499,
//! where upstream marks a rule perishable ONLY when `protocol_version >= 30`.
//! An earlier revision of this branch hoisted the sender check without either
//! condition and regressed the upstream `symlink-dirlink-basis` testsuite
//! case: `--partial-dir` at protocol 28 aborted where upstream exits 0.
//! That is tracked separately; do not "restore" the hoist without both gates.
//!
//! The protocol-32 rows are non-vacuity controls: without them a build that
//! refused every perishable rule, or refused on both sides, would still pass
//! the protocol-28 pull row alone.
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
/// real as `--server`.
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

#[derive(Clone, Copy)]
enum Direction {
    Push,
    Pull,
}

struct Outcome {
    code: Option<i32>,
    stderr: String,
    dest_entries: Vec<String>,
}

/// Runs one cell: a perishable exclude over the rsh shim at `protocol`.
///
/// Deliberately passes NO `--delete` and NO `--prune-empty-dirs` - those are
/// exactly the flags that make oc's write-suppression flag true and would have
/// masked the defect this test exists for.
fn run_cell(protocol: u32, direction: Direction) -> Outcome {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("foo.txt"), b"a\n").expect("write foo");
    fs::write(src.join("bar.txt"), b"b\n").expect("write bar");
    let dest = root.join("dest");

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
        .arg("--filter=-p bar.txt");

    match direction {
        Direction::Push => {
            command
                .arg(format!("{}/", src.display()))
                .arg(format!("lh:{}/", dest.display()));
        }
        Direction::Pull => {
            command
                .arg(format!("lh:{}/", src.display()))
                .arg(format!("{}/", dest.display()));
        }
    }

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

/// Non-vacuity across the PROTOCOL dimension: at 30 and above the `p` is
/// representable, so there is nothing to refuse.
#[test]
fn perishable_rule_transfers_on_push_at_protocol_32() {
    let outcome = run_cell(32, Direction::Push);
    assert_eq!(
        outcome.code,
        Some(0),
        "push at protocol 32 must succeed; stderr: {}",
        outcome.stderr
    );
    assert_eq!(
        outcome.dest_entries,
        vec!["foo.txt".to_string()],
        "the perishable exclude must still drop bar.txt"
    );
}

/// Non-vacuity across the SIDE dimension: upstream's refusal is gated on
/// `am_sender`, so a receiving client keeps the rule locally and transfers.
#[test]
fn perishable_rule_transfers_on_pull_below_protocol_30() {
    let outcome = run_cell(28, Direction::Pull);
    assert_eq!(
        outcome.code,
        Some(0),
        "pull at protocol 28 must succeed - the abort is sender-only; stderr: {}",
        outcome.stderr
    );
    assert_eq!(
        outcome.dest_entries,
        vec!["foo.txt".to_string()],
        "the perishable exclude must still drop bar.txt"
    );
}

/// The remaining cell of the 2x2, so the table is complete rather than
/// three points chosen to fit.
#[test]
fn perishable_rule_transfers_on_pull_at_protocol_32() {
    let outcome = run_cell(32, Direction::Pull);
    assert_eq!(
        outcome.code,
        Some(0),
        "pull at protocol 32 must succeed; stderr: {}",
        outcome.stderr
    );
    assert_eq!(
        outcome.dest_entries,
        vec!["foo.txt".to_string()],
        "the perishable exclude must still drop bar.txt"
    );
}
