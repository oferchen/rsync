//! `--delay-updates --backup`: a failed backup must NOT be followed by the
//! staged rename.
//!
//! upstream `receiver.c:694`:
//!
//! ```text
//! if (make_backups > 0 && !make_backup(fname, False))
//!         continue;
//! ```
//!
//! The `continue` is the whole contract: when the backup cannot be placed, the
//! delayed rename is skipped, the destination keeps its pre-transfer contents,
//! and the staged file stays behind in the `.~tmp~` partial dir. oc renamed
//! anyway, so the pre-image the backup was meant to preserve was overwritten
//! and existed nowhere afterwards - and the `create_dir_all` that actually
//! failed was discarded with `let _ =`, so the diagnostic named the derived
//! `ENOENT` from the rename instead of the real `EACCES`.
//!
//! Measured against rsync 3.5.0 (push and pull, `--backup-dir` under a mode
//! 555 directory): `rc=0`, `backup mkdir <dir> failed: Permission denied
//! (13)` on stderr, destination file UNCHANGED, `dst/sub/.~tmp~` left in
//! place. `make_backup()` reports through `FERROR`, which `log.c:336-341`
//! routes to stderr without setting `got_xfer_error`, so the exit status stays
//! 0 - the destination content, not the exit code, is what distinguishes a
//! honoured backup failure from a swallowed one.
//!
//! ⚠ This must run over a REAL `--server` process: `handle_delayed_updates`
//! belongs to the network receiver. A local transfer takes the engine's
//! local-copy executor, which has its own backup path and would exercise none
//! of this.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Root ignores the mode bits that make the backup `mkdir` fail, so the
/// fixture would report the post-fix state on a broken binary. Skip instead of
/// emitting a false pass on the root CI leg.
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

const OLD: &str = "pre-transfer contents\n";
const NEW: &str = "replacement contents that are longer\n";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

/// `src/sub/f` holds new content; `dst/sub/f` holds older, backdated content,
/// so the quick check sees a genuine update and the sweep has something to
/// back up.
///
/// The backup lives at `dst/bak/sub/f` (upstream clears the suffix when a
/// `--backup-dir` is named, options.c:2438-2439), and `dst/bak` is mode 555: the leaf
/// `mkdir` of `dst/bak/sub` is the step that fails with `EACCES`, which is
/// upstream's `backup mkdir %s failed` (`backup.c:128-139`). Nesting the file
/// one level down is load-bearing - a top-level file would need no new backup
/// subdirectory and the fixture would never fail.
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src/sub")).expect("create src");
    fs::create_dir_all(root.join("dst/sub")).expect("create dst");
    fs::create_dir_all(root.join("dst/bak")).expect("create backup root");
    fs::write(root.join("src/sub/f"), NEW).expect("write src");
    fs::write(root.join("dst/sub/f"), OLD).expect("write dst");
    filetime::set_file_mtime(
        root.join("dst/sub/f"),
        filetime::FileTime::from_unix_time(1_000_000, 0),
    )
    .expect("backdate dst");
    let shim = write_rsh_shim(&root);
    Fixture {
        _temp: temp,
        root,
        shim,
    }
}

/// Pushes through the shim against an external `--server` receiver.
fn push(fx: &Fixture, backup_dir: &str) -> (Option<i32>, String) {
    let binary = oc_rsync_binary();
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .args(["-r", "--delay-updates", "--backup"])
        .arg(format!("--backup-dir={backup_dir}"))
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/src/", fx.root.display()))
        .arg(format!("bkhost:{}/dst/", fx.root.display()))
        .run()
        .expect("push did not finish");
    (
        out.status,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The headline. A backup that cannot be placed must leave the destination
/// alone; overwriting it destroys the only copy of the pre-image.
#[test]
fn a_failed_backup_leaves_the_destination_untouched() {
    if running_as_root() {
        return;
    }
    let fx = fixture();
    let bak = fx.root.join("dst/bak");
    fs::set_permissions(&bak, fs::Permissions::from_mode(0o555)).expect("chmod backup root");

    let (status, stderr) = push(&fx, "bak");

    fs::set_permissions(&bak, fs::Permissions::from_mode(0o755)).expect("restore backup root");

    assert_eq!(
        fs::read_to_string(fx.root.join("dst/sub/f")).expect("read dst"),
        OLD,
        "upstream's receiver.c:694 `continue` skips the delayed rename when the \
         backup fails; overwriting the destination here leaves the pre-image \
         nowhere - not at dst/sub/f, and not in the backup dir either"
    );
    assert!(
        fx.root.join("dst/sub/.~tmp~").exists(),
        "upstream leaves the staged file in the partial dir when it skips the \
         rename (measured against 3.5.0); an absent .~tmp~ means the rename ran"
    );
    assert!(
        !fx.root.join("dst/bak/sub").exists(),
        "the fixture is only meaningful while the backup genuinely cannot be \
         placed"
    );
    assert!(
        stderr.contains("backup failed for") && stderr.contains("Permission denied"),
        "the diagnostic must name the error that actually stopped the backup. \
         Swallowing the create_dir_all reported the rename's derived `No such \
         file or directory` instead. stderr was: {stderr}"
    );
    // upstream: log.c:336-341 - make_backup()'s FERROR reaches stderr without
    // setting got_xfer_error, so 3.5.0 exits 0 for this cell (measured, push
    // and pull alike). Pinned so a future io_error bit here is a deliberate,
    // visible divergence rather than a drift.
    assert_eq!(status, Some(0), "stderr was: {stderr}");
}

/// Non-vacuity for the cell above: with a writable `--backup-dir` the SAME
/// fixture backs up and renames, so "dst/sub/f is unchanged" cannot be passing
/// because the transfer never moved anything.
#[test]
fn a_placeable_backup_still_updates_the_destination() {
    if running_as_root() {
        return;
    }
    let fx = fixture();

    let (status, stderr) = push(&fx, "bak");

    assert_eq!(status, Some(0), "stderr was: {stderr}");
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/sub/f")).expect("read dst"),
        NEW,
        "the delayed rename must still land when the backup succeeds"
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/bak/sub/f")).expect("read backup"),
        OLD,
        "the pre-image belongs under the named backup dir"
    );
    assert!(
        !fx.root.join("dst/sub/.~tmp~").exists(),
        "the sweep removes the emptied staging dir on the success path"
    );
}
