//! A commit-path backup that cannot be placed must STOP the transfer with
//! `RERR_FILEIO` (11), not skip the file and carry on.
//!
//! upstream `rsync.c:897-900`, `finish_transfer()`:
//!
//! ```text
//! if (make_backups > 0 && overwriting_basis) {
//!         int ok = make_backup(fname, False);
//!         if (!ok)
//!                 exit_cleanup(RERR_FILEIO);
//! ```
//!
//! `_exit_cleanup()` is `NORETURN` (`cleanup.c:103`), so this is an abort, and
//! the errno never enters the decision - `EACCES` is as fatal as anything else.
//! The code is 11 rather than the 23 a per-file error would produce because
//! `cleanup.c:113-117` keeps the first code handed to `exit_cleanup()`, and the
//! `RERR_PARTIAL` fallback at `cleanup.c:210-218` is guarded on
//! `exit_code == 0`. Contrast the per-file failure oc already matched: a denied
//! `mkstemp()` (`receiver.c:452-455`) is reported and `recv_files()` moves on.
//!
//! Why the abort is the half that matters: `make_backup()` is what preserves
//! the destination's pre-image. A receiver that treats "the backup could not be
//! placed" as a per-file skip keeps walking a batch whose backup area is known
//! to be unusable, so every later `-b` file is one lost pre-image away from the
//! operator's only copy. Upstream refuses to make that trade even once.
//!
//! Measured against rsync 3.5.0 over this fixture (pull and push alike):
//! `rc=11`, exactly ONE `backup mkdir bak/sub failed: Permission denied (13)`
//! on stderr for a 20-file batch, and every destination file left at its
//! pre-transfer contents. oc emitted a diagnostic for all 20 and exited 23.
//!
//! The cell is a PULL through a real `--server` process: the local side is then
//! the receiver, so the commit path under test is the network receiver's
//! (`crates/transfer/src/pipeline/receiver.rs`), not the engine's local-copy
//! executor, which has its own backup path and would exercise none of this.
//! A pull is also the only direction that can pin the code today: a push puts
//! the receiver in the remote `--server` process, whose fatal exit is not
//! carried back to the client - `crates/cli/src/frontend/server/run.rs` returns
//! a flat 1 for every server error and oc emits no `MSG_ERROR_EXIT`, so the
//! client grades the run by the truncated stream it observes instead. A push
//! over this fixture does stop at the first file, which is the half with data
//! consequences, but reports 12.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Enough files that "stopped at the first failure" and "walked the whole
/// batch" cannot be confused by however many commits the pipeline had in
/// flight.
const FILE_COUNT: usize = 20;

const OLD: &str = "pre-transfer contents\n";
const NEW: &str = "replacement contents that are longer\n";

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Root ignores the mode bits that make the backup `mkdir` fail, so the fixture
/// would report the post-fix state on a broken binary. Skip instead of emitting
/// a false pass on the root CI leg.
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

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

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

/// `src/sub/fNN` holds new content; `dst/sub/fNN` holds older, backdated
/// content, so the quick check sees a genuine update and every file has a
/// pre-image to back up.
///
/// The backup lives at `dst/bak/sub/fNN` (upstream clears the suffix when a
/// `--backup-dir` is named, `options.c:2438-2439`). Nesting the files one level
/// down is load-bearing: the step that fails is the leaf `mkdir` of
/// `dst/bak/sub`, so top-level files would need no new backup subdirectory and
/// the fixture would never fail.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src/sub")).expect("create src");
    fs::create_dir_all(root.join("dst/sub")).expect("create dst");
    fs::create_dir_all(root.join("dst/bak")).expect("create backup root");
    for name in file_names() {
        fs::write(root.join("src/sub").join(&name), NEW).expect("write src");
        let dest = root.join("dst/sub").join(&name);
        fs::write(&dest, OLD).expect("write dst");
        filetime::set_file_mtime(&dest, filetime::FileTime::from_unix_time(1_000_000, 0))
            .expect("backdate dst");
    }
    let shim = write_rsh_shim(&root);
    Fixture {
        _temp: temp,
        root,
        shim,
    }
}

/// Zero-padded so the flist sort order matches the numeric order, which is what
/// makes `last_file()` genuinely the last file the receiver would reach.
fn file_names() -> Vec<String> {
    (0..FILE_COUNT).map(|i| format!("f{i:02}")).collect()
}

fn last_file() -> String {
    file_names().pop().expect("FILE_COUNT is non-zero")
}

/// Pulls through the shim from an external `--server` sender, so the local
/// process is the receiver that runs the commit path.
fn pull(fx: &Fixture) -> (Option<i32>, String) {
    let binary = oc_rsync_binary();
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .args(["-r", "--backup", "--backup-dir=bak"])
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("bkhost:{}/src/", fx.root.display()))
        .arg(format!("{}/dst/", fx.root.display()))
        .run()
        .expect("pull did not finish");
    (
        out.status,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn backup_failures(stderr: &str) -> usize {
    stderr
        .lines()
        .filter(|line| line.contains("keep_backup failed"))
        .count()
}

/// The headline, asserting both halves of upstream's answer at once: the exit
/// code is `RERR_FILEIO`, AND the run stopped instead of walking the rest of
/// the batch.
#[test]
fn a_failed_commit_backup_aborts_with_file_io() {
    if running_as_root() {
        return;
    }
    let fx = fixture();
    let bak = fx.root.join("dst/bak");
    fs::set_permissions(&bak, fs::Permissions::from_mode(0o555)).expect("chmod backup root");

    let (status, stderr) = pull(&fx);

    fs::set_permissions(&bak, fs::Permissions::from_mode(0o755)).expect("restore backup root");

    let failures = backup_failures(&stderr);
    assert!(
        failures >= 1,
        "the fixture is only meaningful while the backup genuinely cannot be \
         placed; stderr was: {stderr}"
    );

    // The abort half. Upstream stops at the FIRST unplaceable backup
    // (rsync.c:900 -> NORETURN cleanup.c:103), so the last file in the batch is
    // never reached. Treating the failure as a per-file skip instead reports
    // one line per file and leaves the transfer running against a backup area
    // already known to be unusable.
    assert!(
        !stderr.contains(&last_file()),
        "the transfer must stop at the first unplaceable backup, but it \
         reached {}, the last file of the batch ({failures} of {FILE_COUNT} \
         files reported). stderr was: {stderr}",
        last_file()
    );
    assert!(
        failures < FILE_COUNT,
        "{failures} of {FILE_COUNT} files reported a failed backup, so the run \
         walked the whole batch instead of aborting. stderr was: {stderr}"
    );

    // The exit-code half. 11 (RERR_FILEIO), not the 23 (RERR_PARTIAL) a
    // per-file error accumulates: upstream hands 11 straight to exit_cleanup()
    // (rsync.c:900) and cleanup.c:210-218 only reaches for 23 when no code has
    // been claimed. Measured identical against rsync 3.5.0 on this fixture.
    assert_eq!(status, Some(11), "stderr was: {stderr}");

    // Nothing may be committed on the way out: the destination still holds the
    // pre-image the backup was asked to preserve.
    for name in file_names() {
        assert_eq!(
            fs::read_to_string(fx.root.join("dst/sub").join(&name)).expect("read dst"),
            OLD,
            "{name} was overwritten even though its backup could not be placed"
        );
    }
}

/// Non-vacuity for the cell above: the SAME fixture with a writable
/// `--backup-dir` transfers every file and exits 0, so "the run stopped" and
/// "exit 11" cannot be passing because the transfer never moved anything.
#[test]
fn a_placeable_backup_transfers_the_whole_batch() {
    if running_as_root() {
        return;
    }
    let fx = fixture();

    let (status, stderr) = pull(&fx);

    assert_eq!(status, Some(0), "stderr was: {stderr}");
    for name in file_names() {
        assert_eq!(
            fs::read_to_string(fx.root.join("dst/sub").join(&name)).expect("read dst"),
            NEW,
            "{name} must be updated when its backup can be placed"
        );
        assert_eq!(
            fs::read_to_string(fx.root.join("dst/bak/sub").join(&name)).expect("read backup"),
            OLD,
            "the pre-image of {name} belongs under the named backup dir"
        );
    }
}
