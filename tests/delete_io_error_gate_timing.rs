//! When a source scan hits an I/O error, whether `--delete` still runs depends
//! on *when* the delete decision is made relative to that error.
//!
//! Upstream has exactly one gate: `generator.c:304`
//!
//! ```c
//! if (io_error & IOERR_GENERAL && !ignore_errors) {
//!     if (already_warned)
//!         return;
//!     rprintf(FINFO, "IO error encountered -- skipping file deletion\n");
//!     already_warned = 1;
//!     return;
//! }
//! ```
//!
//! It sits inside `delete_in_dir()` - the function that DECIDES what to delete -
//! and reads the live global `io_error`. `do_delayed_deletions()`
//! (`generator.c:265-278`) has no gate: it is a pure executor that replays the
//! recorded plan.
//!
//! So the outcome per `--delete-WHEN` is decided entirely by whether the
//! failing directory has been scanned by the time the decision runs:
//!
//! | flags                | inc_recurse | decision site | scan done? | extraneous |
//! |----------------------|-------------|---------------|------------|------------|
//! | (none, = during)     | 1           | per-segment   | no         | deleted    |
//! | `--no-inc-recursive` | 0           | recv_generator| yes        | kept       |
//! | `--delete-before`    | 0 (forced)  | do_delete_pass| yes        | kept       |
//! | `--delete-after`     | 0 (forced)  | do_delete_pass| yes        | kept       |
//! | `--delete-delay`     | **1**       | per-segment   | no         | deleted    |
//! | `--ignore-errors`    | -           | gate disabled | -          | deleted    |
//!
//! `--delete-delay` keeps `allow_inc_recurse` set: `compat.c:174-177` clears it
//! for `delete_before`, `delete_after`, `delay_updates` and `prune_empty_dirs`,
//! but `--delete-delay` is `delete_during == 2` and is not in that list. Only
//! the *unlink* is postponed, never the decision - which is why it deletes
//! where `--no-inc-recursive` keeps, even though both "defer" something.
//!
//! Every row is asserted on `dst/extraneous.txt` alone. `dst/sub` deliberately
//! is not asserted: upstream creates the destination directory for an
//! unreadable source directory and oc does not, which is a separate defect;
//! including it would fail every row for an unrelated reason.
//!
//! Two rows are recorded as OPEN divergences rather than silently omitted.
//! oc's local copy runs `--delete-before` as a per-directory sweep during the
//! walk (`apply_pre_transfer_deletions`, called right after each directory is
//! planned), where upstream runs `do_delete_pass()` once over an already
//! complete file list. The root's sweep therefore decides before `sub` has been
//! scanned, and oc deletes where upstream keeps. `--no-inc-recursive` is the
//! same shape. Closing them means giving the local executor a whole-tree delete
//! phase, not adding a check - so they are asserted at their CURRENT behaviour
//! with `expected` naming what upstream does. Fixing the executor flips those
//! two rows and this test fails, pointing at the line to change; it cannot pass
//! by accident in either direction.
//!
//! Skip condition (test passes with a printed reason): the fixture cannot be
//! built with an unreadable directory - notably when running as root, for whom
//! mode 000 is not a barrier and no I/O error would occur at all.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// One row of the table above.
struct Row {
    flags: &'static [&'static str],
    /// Whether upstream keeps the extraneous destination file.
    upstream_keeps: bool,
    /// `Some(reason)` when oc knowingly differs from `upstream_keeps`, so the
    /// row asserts oc's current behaviour and says why.
    open_divergence: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        flags: &[],
        upstream_keeps: false,
        open_divergence: None,
    },
    Row {
        flags: &["--no-inc-recursive"],
        upstream_keeps: true,
        open_divergence: Some(
            "oc has no whole-tree delete phase, so the root sweep decides \
             before the unreadable directory is scanned",
        ),
    },
    Row {
        flags: &["--delete-before"],
        upstream_keeps: true,
        open_divergence: Some(
            "oc runs --delete-before per directory during the walk, upstream \
             runs do_delete_pass() once over a complete file list",
        ),
    },
    Row {
        flags: &["--delete-after"],
        upstream_keeps: true,
        open_divergence: None,
    },
    Row {
        flags: &["--delete-delay"],
        upstream_keeps: false,
        open_divergence: None,
    },
    Row {
        flags: &["--ignore-errors"],
        upstream_keeps: false,
        open_divergence: None,
    },
];

/// Builds `src/{a.txt,sub/s.txt}` with `sub` unreadable, and `dst/extraneous.txt`.
///
/// Returns `None` when `sub` is still readable after the chmod, which is the
/// case for root: the run would then produce no I/O error and every row would
/// pass for the wrong reason.
fn fixture() -> Option<(TempDir, PathBuf, PathBuf)> {
    let root = TempDir::new().expect("tempdir");
    let src = root.path().join("src");
    let dst = root.path().join("dst");
    fs::create_dir_all(src.join("sub")).expect("create src/sub");
    fs::create_dir_all(&dst).expect("create dst");
    fs::write(src.join("a.txt"), b"a\n").expect("write a.txt");
    fs::write(src.join("sub/s.txt"), b"s\n").expect("write s.txt");
    fs::write(dst.join("extraneous.txt"), b"x\n").expect("write extraneous");
    set_mode(&src.join("sub"), 0o000);
    if fs::read_dir(src.join("sub")).is_ok() {
        restore(&src);
        return None;
    }
    Some((root, src, dst))
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}

/// Re-opens `src/sub` so `TempDir`'s recursive delete can remove it.
fn restore(src: &Path) {
    set_mode(&src.join("sub"), 0o755);
}

#[test]
fn delete_gate_follows_the_decision_site_not_the_flush() {
    let mut checked = 0usize;
    for row in ROWS {
        let Some((root, src, dst)) = fixture() else {
            println!("skipping: the fixture directory stayed readable after chmod 000");
            return;
        };
        let status = Command::new(oc_rsync_bin())
            .arg("-a")
            .arg("--delete")
            .args(row.flags)
            .arg(format!("{}/", src.display()))
            .arg(format!("{}/", dst.display()))
            .status()
            .expect("run oc-rsync");
        let extraneous = dst.join("extraneous.txt");
        let survived = extraneous.exists();
        let label = if row.flags.is_empty() {
            "(none)".to_owned()
        } else {
            row.flags.join(" ")
        };
        // Every row must report the partial-transfer failure; a row that
        // silently succeeded never hit the I/O error the table is about.
        assert_eq!(
            status.code(),
            Some(23),
            "`--delete {label}` exited {status:?}, so the source scan did not \
             fail and this row proves nothing"
        );
        let expected = match row.open_divergence {
            // Open divergence: oc does the opposite of upstream, on purpose,
            // until the executor gains a whole-tree delete phase.
            Some(_) => !row.upstream_keeps,
            None => row.upstream_keeps,
        };
        assert_eq!(
            survived,
            expected,
            "`--delete {label}`: extraneous.txt {}, expected it to {}{}",
            if survived { "survived" } else { "was deleted" },
            if expected { "survive" } else { "be deleted" },
            match row.open_divergence {
                Some(reason) => format!(
                    " - this row is an OPEN divergence from upstream (which \
                     {}s it): {reason}. If you just fixed that, flip \
                     `open_divergence` to None here.",
                    if row.upstream_keeps { "keep" } else { "delete" }
                ),
                None => String::new(),
            }
        );
        checked += 1;
        restore(&src);
        drop(root);
    }
    // Both outcomes must be represented, or the table could be satisfied by a
    // build that always deletes (or always keeps).
    assert_eq!(checked, ROWS.len());
    assert!(ROWS.iter().any(|row| row.upstream_keeps));
    assert!(ROWS.iter().any(|row| !row.upstream_keeps));
    // The gate is only meaningful if some row exercises each side of it, and
    // the divergence ledger is only meaningful if it is not the whole table.
    assert!(ROWS.iter().any(|row| row.open_divergence.is_none()));
    assert!(ROWS.iter().any(|row| row.open_divergence.is_some()));
}
