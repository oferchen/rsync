//! A directory standing where a regular file, symlink, or special file has to
//! be written is removed, never backed up - and a populated one is refused out
//! loud.
//!
//! upstream `generator.c:2464-2485` `atomic_create()`:
//!
//! ```text
//! int skip_atomic, dir_in_the_way = del_for_flag && S_ISDIR(sxp->st.st_mode);
//! if (!del_for_flag || dir_in_the_way || tmpdir || !get_tmpname(tmpname, fname, True))
//!         skip_atomic = 1;
//! ...
//! if (make_backups > 0 && !dir_in_the_way) {
//!         if (!make_backup(fname, skip_atomic))
//!                 return 0;
//! } else if (skip_atomic) {
//!         int del_opts = delete_mode || force_delete ? DEL_RECURSE : 0;
//!         if (delete_item(fname, sxp->st.st_mode, del_opts | del_for_flag) != 0)
//!                 return 0;
//! }
//! ```
//!
//! `dir_in_the_way` forces `skip_atomic`, so a directory obstacle reaches
//! `delete_item()` in BOTH modes: the `make_backup` arm is unreachable for it
//! whether or not `--backup` is set. `delete_item()` then splits the same way
//! one level down - the `rmdir` arm at `delete.c:222-226`, the
//! `make_backup`/`unlink` arm in the `else` at `delete.c:227-238`.
//!
//! That is why the removal and the reporting are one change and not two. oc
//! had no directory arm at all, so:
//!
//! | src kind | obstacle | rsync 3.5.0 | oc before |
//! |---|---|---|---|
//! | symlink | empty dir     | 0, symlink placed | 12, dir kept, sibling not transferred |
//! | symlink | non-empty dir | 23 + two diagnostics | 12, dir kept, sibling not transferred |
//! | fifo    | empty dir     | 0, fifo placed | **0 in silence**, dir kept |
//! | fifo    | non-empty dir | 23 + two diagnostics | **0 in silence**, dir kept |
//!
//! The regular-file source reaches the same `delete_item()` from its own call
//! site rather than through `atomic_create()`:
//!
//! ```text
//! generator.c:2148-2153
//! if (statret == 0 && !(stype == FT_REG || (write_devices && stype == FT_DEVICE))) {
//!         if (delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_FILE) != 0)
//!                 goto cleanup;
//!         statret = -1;
//!         stat_errno = ENOENT;
//! }
//! ```
//!
//! oc had no analogue there, so the destination directory survived into the
//! commit and the rename failed against it:
//!
//! | src kind | obstacle | rsync 3.5.0 | oc before |
//! |---|---|---|---|
//! | regular | empty dir     | 0, regular file placed | 12 `server error: Is a directory (21)`, dir kept |
//! | regular | non-empty dir | 23 + two diagnostics | 12, dir kept |
//!
//! and under `--backup` with a sibling backup dir, oc RENAMED the directory
//! into the backup area - a wrong-data outcome at exit 0, where upstream
//! removes it.
//!
//! Measured against rsync 3.5.0 (protocol 32) over a real `--server` child on
//! both ends. Every cell below was run against that binary first; the
//! expectations are its output, not a reading of the source.
//!
//! ⚠ These must run over a REAL `--server` process. A local transfer takes the
//! engine's local-copy executor, which refuses a directory obstacle in its own
//! error type (`local_copy/error.rs:427`, "cannot replace existing directory
//! with symbolic link") and exercises none of the receiver path under test.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// `rsync` invokes `$RSYNC_RSH <host> <command...>`; drop the host and exec the
/// command locally, so the receiver really is a separate `--server` process.
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

const SIBLING: &str = "sibling payload\n";

/// The bytes a regular-file source carries, distinct from anything the
/// destination could already hold.
const PAYLOAD: &str = "regular payload\n";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

/// What the source names `obstacle`.
#[derive(Clone, Copy)]
enum Source {
    Symlink,
    Fifo,
    Regular,
}

/// Builds `src/{obstacle,sibling}` and `dst/{obstacle/,sibling}`, where
/// `dst/obstacle` is a directory and `src/obstacle` is the non-regular entry
/// that has to replace it.
///
/// `sibling` is load-bearing: upstream keeps transferring it when the obstacle
/// is refused, so it separates "the obstacle entry was skipped" from "the whole
/// transfer stopped". oc did the latter.
fn fixture(source: Source, populate_obstacle: bool) -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst/obstacle")).expect("create dst obstacle");
    fs::create_dir_all(root.join("outside")).expect("create outside");

    match source {
        Source::Symlink => {
            std::os::unix::fs::symlink("../outside", root.join("src/obstacle"))
                .expect("plant source symlink");
        }
        Source::Fifo => {
            // Same `mkfifo(1)` shell-out as tests/drop_devices.rs, rather than
            // a new unsafe libc call in the test tree.
            let status = std::process::Command::new("mkfifo")
                .arg(root.join("src/obstacle"))
                .status()
                .expect("spawn mkfifo");
            assert!(status.success(), "mkfifo failed: {status}");
        }
        Source::Regular => {
            fs::write(root.join("src/obstacle"), PAYLOAD).expect("write source payload");
        }
    }
    fs::write(root.join("src/sibling"), SIBLING).expect("write src sibling");
    if populate_obstacle {
        fs::write(root.join("dst/obstacle/occupant"), "occupant\n").expect("populate obstacle");
    }

    let shim = write_rsh_shim(&root);
    Fixture {
        _temp: temp,
        root,
        shim,
    }
}

/// Pushes `src/` to `dst/` through the shim against an external `--server`
/// receiver. `extra` carries the `--backup` flags where a cell needs them.
fn push(fx: &Fixture, extra: &[&str]) -> (Option<i32>, String, String) {
    let binary = oc_rsync_binary();
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        // -l keeps symlinks as symlinks, -D materialises the fifo, -I defeats
        // the quick check so the obstacle entry is always reconsidered.
        .args(["-rlDI"])
        .args(extra)
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/src/", fx.root.display()))
        .arg(format!("obshost:{}/dst/", fx.root.display()))
        .run()
        .expect("push did not finish");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn obstacle(fx: &Fixture) -> PathBuf {
    fx.root.join("dst/obstacle")
}

/// The headline for `--backup` off: an EMPTY directory is `rmdir`'d and the
/// symlink takes its place, at exit 0.
///
/// upstream: `delete.c:222-226` - the `rmdir` arm of `delete_item()`.
#[test]
fn an_empty_directory_obstacle_is_removed_for_a_symlink() {
    let fx = fixture(Source::Symlink, false);

    let (status, _stdout, stderr) = push(&fx, &[]);

    assert_eq!(
        status,
        Some(0),
        "rsync 3.5.0 rmdir's the empty directory and creates the symlink at \
         exit 0; oc reported `server error: File exists` and stopped. \
         stderr was: {stderr}"
    );
    assert_eq!(
        fs::read_link(obstacle(&fx)).expect("obstacle must now be a symlink"),
        Path::new("../outside"),
        "the symlink has to actually replace the directory - a still-standing \
         directory means the removal never ran"
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/sibling")).expect("read sibling"),
        SIBLING,
        "the rest of the transfer must still land"
    );
}

/// Same for a FIFO, which is the shape oc failed SILENTLY on: it reported exit
/// 0 while leaving the directory in place and never creating the node.
///
/// This cell is the one that distinguishes "the removal works" from "the
/// removal now fails loudly instead of silently" - every other cell here would
/// pass under a change that only added diagnostics.
///
/// upstream: `generator.c:2091` `atomic_create(file, fname, NULL, NULL, rdev,
/// &sx, dest_existed ? del_for_flag : 0)`.
#[test]
fn an_empty_directory_obstacle_is_removed_for_a_fifo() {
    use std::os::unix::fs::FileTypeExt;

    let fx = fixture(Source::Fifo, false);

    let (status, _stdout, stderr) = push(&fx, &[]);

    assert_eq!(status, Some(0), "stderr was: {stderr}");
    let meta = fs::symlink_metadata(obstacle(&fx)).expect("obstacle must still exist");
    assert!(
        meta.file_type().is_fifo(),
        "oc reported exit 0 here while leaving the directory standing and \
         never creating the fifo - silent data loss, not a loud failure. \
         The obstacle is now: {meta:?}"
    );
}

/// A POPULATED directory is refused, and the refusal is audible: upstream's
/// two lines plus `RERR_PARTIAL`.
///
/// This is the reporting half. Before the change the same shape exited 0 (fifo)
/// or 12 (symlink) and printed nothing an operator could act on, because the
/// failure went to a verbosity-gated `debug_log!`.
///
/// upstream: `delete.c:178-181` `cannot delete non-empty directory: %s` at
/// `FINFO`, then `delete.c:283-285` `could not make way for %s %s: %s` at
/// `FERROR_XFER`, whose `got_xfer_error` lifts the exit to 23
/// (`log.c:310-311`, `cleanup.c:217-218`).
#[test]
fn a_populated_directory_obstacle_is_refused_at_23() {
    let fx = fixture(Source::Symlink, true);

    let (status, stdout, stderr) = push(&fx, &[]);

    assert_eq!(
        status,
        Some(23),
        "rsync 3.5.0 exits 23 for this shape. stdout: {stdout} stderr: {stderr}"
    );
    let output = format!("{stdout}{stderr}");
    assert!(
        output.contains("cannot delete non-empty directory: obstacle"),
        "upstream reports the emptiness probe at FINFO before naming the item \
         it blocked. output was: {output}"
    );
    assert!(
        output.contains("could not make way for new symlink: obstacle"),
        "upstream's FERROR_XFER line is what makes the run exit non-zero; \
         without it the obstacle is skipped in silence. output was: {output}"
    );
    assert!(
        obstacle(&fx).is_dir(),
        "the contents are never removed to make room - `del_opts` carries no \
         DEL_RECURSE here"
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/sibling")).expect("read sibling"),
        SIBLING,
        "a refused obstacle skips its own entry only; upstream keeps going, \
         and oc used to abandon the rest of the batch"
    );
}

/// The noun in the refusal comes from the NEW entry's type, not the obstacle's.
///
/// upstream: `generator.c:2041-2047` picks `DEL_FOR_DEVICE` or
/// `DEL_FOR_SPECIAL`; `delete.c:275-282` turns it into the printed noun.
#[test]
fn the_refusal_names_the_new_entrys_kind() {
    let fx = fixture(Source::Fifo, true);

    let (status, stdout, stderr) = push(&fx, &[]);

    assert_eq!(status, Some(23), "stdout: {stdout} stderr: {stderr}");
    let output = format!("{stdout}{stderr}");
    assert!(
        output.contains("could not make way for new special file: obstacle"),
        "a FIFO is DEL_FOR_SPECIAL, so upstream says `special file` here and \
         `symlink` in the cell above. output was: {output}"
    );
}

/// The non-vacuity companion, and the reason the two halves ship together.
///
/// `dir_in_the_way` excludes a directory from `make_backup` in BOTH modes, so
/// under `--backup` the empty directory is still `rmdir`'d and the symlink
/// still lands at exit 0. oc instead RENAMED the directory into the backup
/// area - a wrong-data outcome that exited 0, so no exit-code assertion could
/// have caught it.
///
/// Reporting the obstacle failure loudly WITHOUT this removal would turn this
/// cell from a wrong-data pass into a hard failure on a shape upstream
/// completes at 0. That is what makes the halves inseparable.
///
/// upstream: `generator.c:2476` - `if (make_backups > 0 && !dir_in_the_way)`.
#[test]
fn a_directory_obstacle_is_removed_not_backed_up_under_backup() {
    let fx = fixture(Source::Symlink, false);
    fs::create_dir_all(fx.root.join("dst/bak")).expect("create backup dir");

    let (status, _stdout, stderr) = push(&fx, &["-b", "--backup-dir=bak"]);

    assert_eq!(
        status,
        Some(0),
        "upstream completes this shape at exit 0. stderr was: {stderr}"
    );
    assert_eq!(
        fs::read_link(obstacle(&fx)).expect("obstacle must now be a symlink"),
        Path::new("../outside"),
        "the symlink must land under --backup exactly as it does without it"
    );
    assert!(
        !fx.root.join("dst/bak/obstacle").exists(),
        "upstream never backs a directory obstacle up - dir_in_the_way keeps \
         it out of make_backup in both modes. oc renamed it into the backup \
         area, which is a data outcome no exit code reports"
    );
}

/// The same `--backup` run, with the obstacle populated: still refused, still
/// audible, and still not copied into the backup area.
#[test]
fn a_populated_directory_obstacle_under_backup_is_refused_not_backed_up() {
    let fx = fixture(Source::Symlink, true);
    fs::create_dir_all(fx.root.join("dst/bak")).expect("create backup dir");

    let (status, stdout, stderr) = push(&fx, &["-b", "--backup-dir=bak"]);

    assert_eq!(status, Some(23), "stdout: {stdout} stderr: {stderr}");
    assert!(
        obstacle(&fx).join("occupant").exists(),
        "the populated directory and its contents stay put"
    );
    assert!(
        !fx.root.join("dst/bak/obstacle").exists(),
        "and no part of it is moved into the backup area"
    );
}

/// The regular-file headline for `--backup` off: an EMPTY directory is
/// `rmdir`'d and the file's data lands in its place, at exit 0.
///
/// Nothing masks this one. A rename over a symlink, FIFO, or socket replaces
/// the node by accident, which is why the plain-mode cells for those shapes
/// looked correct; `rename()` onto a directory is `EISDIR`, so oc surfaced
/// `server error: Is a directory (21)` and stopped the whole run at 12 with
/// the sibling unsent.
///
/// The `-i` row is part of the expectation: `statret = -1` is what makes the
/// entry `ITEM_IS_NEW`, so 3.5.0 prints `<f+++++++++ a` for a cleared
/// obstacle. Measured against 3.5.0, not read off the source.
///
/// upstream: `generator.c:2149` - `delete_item(fname, sx.st.st_mode,
/// del_opts | DEL_FOR_FILE)`, then `statret = -1`.
#[test]
fn an_empty_directory_obstacle_is_removed_for_a_regular_file() {
    let fx = fixture(Source::Regular, false);

    let (status, stdout, stderr) = push(&fx, &["-i"]);

    assert_eq!(
        status,
        Some(0),
        "rsync 3.5.0 rmdir's the empty directory and writes the file at exit \
         0; oc reported `server error: Is a directory (21)` and stopped. \
         stderr was: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(obstacle(&fx)).expect("obstacle must now be a regular file"),
        PAYLOAD,
        "the file has to actually replace the directory, with the sender's \
         bytes - a still-standing directory means the removal never ran"
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/sibling")).expect("read sibling"),
        SIBLING,
        "the rest of the transfer must still land"
    );
    assert!(
        stdout.contains("+++++++++ obstacle"),
        "upstream prints `<f+++++++++ a` for a cleared obstacle: the removal \
         sets `statret = -1`, so the entry is ITEM_IS_NEW rather than a \
         replacement of the directory it displaced. stdout was: {stdout}"
    );
}

/// A POPULATED directory is refused for a regular file too, with upstream's
/// two lines and `RERR_PARTIAL` - and the noun is `regular file`, which is the
/// `DEL_FOR_FILE` arm of the same switch that says `symlink` and `special
/// file` above.
///
/// upstream: `delete.c:276` - `case DEL_FOR_FILE: desc = "regular file";`.
#[test]
fn a_populated_directory_obstacle_is_refused_for_a_regular_file() {
    let fx = fixture(Source::Regular, true);

    let (status, stdout, stderr) = push(&fx, &[]);

    assert_eq!(
        status,
        Some(23),
        "rsync 3.5.0 exits 23 for this shape; oc exited 12. stdout: {stdout} \
         stderr: {stderr}"
    );
    let output = format!("{stdout}{stderr}");
    assert!(
        output.contains("cannot delete non-empty directory: obstacle"),
        "output was: {output}"
    );
    assert!(
        output.contains("could not make way for new regular file: obstacle"),
        "the noun comes from the NEW entry's type, so a regular-file source \
         says `regular file` where a symlink says `symlink`. output was: \
         {output}"
    );
    assert!(
        obstacle(&fx).is_dir(),
        "`del_opts` carries no DEL_RECURSE here, so the contents stay put"
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/sibling")).expect("read sibling"),
        SIBLING,
        "a refused obstacle skips its own entry only; oc used to abandon the \
         rest of the batch at 12"
    );
}

/// The non-vacuity companion: with NO obstacle, an ordinary regular-file
/// replacement still lands byte-for-byte at exit 0 AND is still reported as a
/// replacement, not as a creation.
///
/// The removal above sits directly in front of every regular-file transfer, so
/// a gate one type too wide would unlink the existing destination first. The
/// bytes alone do not catch that - `-I` re-sends the whole file either way, so
/// a delete-then-create still produces the right content at exit 0. The `-i`
/// row does: an unlinked destination itemizes `+++++++++` where 3.5.0 prints
/// `<f..T......` for a file it kept and wrote over. That glyph is exactly what
/// `statret = -1` controls, so this cell is the mirror of the assertion above.
#[test]
fn a_regular_file_destination_with_no_obstacle_still_transfers() {
    let fx = fixture(Source::Regular, false);
    // Replace the directory obstacle with an ordinary stale file, which is
    // upstream's `stype == FT_REG` leg: kept in place and written over.
    fs::remove_dir_all(obstacle(&fx)).expect("clear the obstacle");
    fs::write(obstacle(&fx), "stale destination bytes\n").expect("plant stale destination");

    let (status, stdout, stderr) = push(&fx, &["-i"]);

    assert_eq!(status, Some(0), "stderr was: {stderr}");
    assert_eq!(
        fs::read_to_string(obstacle(&fx)).expect("read replaced file"),
        PAYLOAD,
        "an ordinary replace must still carry the sender's bytes"
    );
    assert!(
        !stdout.contains("+++++++++ obstacle"),
        "`stype == FT_REG` is upstream's keep-it leg: the destination is \
         written over, never removed, so its row is a change and not a \
         creation. A `+++++++++` here means the obstacle removal fired one \
         type too wide. stdout was: {stdout}"
    );
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/sibling")).expect("read sibling"),
        SIBLING
    );
}
