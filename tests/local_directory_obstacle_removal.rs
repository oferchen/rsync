//! The LOCAL-COPY path answers upstream's make-way decision the way the
//! receiver path already does: a directory standing where a regular file,
//! symlink, or special has to be written is `rmdir`'d when empty and refused
//! OUT LOUD when populated - never gated on `--force` or `--delete`.
//!
//! `tests/directory_obstacle_removal.rs` pins the same decision on the
//! RECEIVER, and its own header says why a second file is needed: a local
//! transfer "takes the engine's local-copy executor ... and exercises none of
//! the receiver path under test". The two paths are siblings, the receiver one
//! was fixed first, and the local one kept the old behaviour - which is the
//! failure mode this file exists to stop recurring. Every cell below therefore
//! runs a PLAIN LOCAL invocation with no `--rsh`.
//!
//! # What upstream does
//!
//! Both upstream call sites reach `delete_item()` unconditionally:
//!
//! ```text
//! generator.c:2148-2153                      (a regular file is arriving)
//! if (statret == 0 && !(stype == FT_REG || (write_devices && stype == FT_DEVICE))) {
//!         if (delete_item(fname, sx.st.st_mode, del_opts | DEL_FOR_FILE) != 0)
//!                 goto cleanup;
//!
//! generator.c:2477-2483                      (atomic_create(), symlink/special)
//! } else if (skip_atomic) {
//!         int del_opts = delete_mode || force_delete ? DEL_RECURSE : 0;
//!         if (delete_item(fname, sxp->st.st_mode, del_opts | del_for_flag) != 0)
//!                 return 0;
//! ```
//!
//! `del_opts` selects the RECURSION, not the removal - `delete.c:207-209`,
//! "If DEL_RECURSE is not set, this just reports emptiness". oc gated the whole
//! removal on `--force` / `--delete` instead, so on the local path:
//!
//! | src kind | obstacle | rsync 3.5.0 | oc before |
//! |---|---|---|---|
//! | regular | empty dir     | 0, file placed | **0 in silence**, dir kept, file never written |
//! | regular | non-empty dir | 23 + two diagnostics | **0 in silence**, dir kept |
//! | symlink | empty dir     | 0, symlink placed | 23, oc-only `cannot replace existing directory with symbolic link` |
//! | symlink | non-empty dir | 23 + two diagnostics | 23, that same oc-only line, neither upstream line |
//! | fifo    | empty dir     | 0, fifo placed | 23, oc-only `cannot replace existing directory with special file` |
//! | fifo    | non-empty dir | 23 + two diagnostics | 23, that same oc-only line |
//! | symlink | non-empty dir, `--delete` | 0, symlink placed | 23, refused |
//!
//! The two regular-file rows are the reason this shipped as a fix rather than a
//! diagnostic tidy-up: exit 0 having not written the file the operator asked
//! for is data loss from where they stand.
//!
//! Measured against `rsync 3.5.0` (protocol 32) as an unprivileged user on
//! APFS, over a 120-cell sweep of `{regular, fifo, symlink, directory}` sources
//! crossed with `{regular, fifo, socket, symlink, empty dir, populated dir}`
//! obstacles and `{plain, --backup, --force, --delete, --backup --force}`. The
//! expectations below are that binary's output, not a reading of its source.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

const SIBLING: &str = "sibling payload\n";
const PAYLOAD: &str = "regular payload\n";

/// What the source names `obstacle`.
#[derive(Clone, Copy)]
enum Source {
    Symlink,
    Fifo,
    Regular,
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

/// Builds `src/{obstacle,sibling}` and `dst/obstacle/`, where `dst/obstacle` is
/// a directory and `src/obstacle` is the entry that has to replace it.
///
/// `sibling` is load-bearing: upstream keeps transferring it when the obstacle
/// is refused, so it separates "the obstacle entry was skipped" from "the whole
/// transfer stopped".
fn fixture(source: Source, populate_obstacle: bool) -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst/obstacle")).expect("create dst obstacle");

    match source {
        Source::Symlink => {
            std::os::unix::fs::symlink("target", root.join("src/obstacle"))
                .expect("plant source symlink");
        }
        Source::Fifo => {
            // Same `mkfifo(1)` shell-out as tests/directory_obstacle_removal.rs,
            // rather than a new unsafe libc call in the test tree.
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

    Fixture { _temp: temp, root }
}

/// Copies `src/` to `dst/` with a PLAIN LOCAL invocation - no `--rsh`, no
/// `--server` child - so the engine's local-copy executor is the code under
/// test.
fn copy(fx: &Fixture, extra: &[&str]) -> (Option<i32>, String, String) {
    let out = test_support::OcRsyncCliRunner::new()
        // -l keeps symlinks as symlinks, -D materialises the fifo, -I defeats
        // the quick check so the obstacle entry is always reconsidered.
        .args(["-rlDI"])
        .args(extra)
        .arg(format!("{}/src/", fx.root.display()))
        .arg(format!("{}/dst/", fx.root.display()))
        .run()
        .expect("copy did not finish");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn obstacle(fx: &Fixture) -> PathBuf {
    fx.root.join("dst/obstacle")
}

fn sibling_landed(fx: &Fixture) -> bool {
    fs::read_to_string(fx.root.join("dst/sibling")).is_ok_and(|body| body == SIBLING)
}

/// The headline. A regular file arriving over an EMPTY destination directory:
/// upstream `rmdir`s it and writes the file at exit 0. oc exited 0 having
/// written nothing at all.
///
/// upstream: `generator.c:2148-2153` -> `delete.c:221-226`, the `rmdir` arm.
#[test]
fn an_empty_directory_obstacle_is_removed_for_a_regular_file() {
    let fx = fixture(Source::Regular, false);

    let (status, stdout, stderr) = copy(&fx, &[]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert_eq!(
        fs::read_to_string(obstacle(&fx)).unwrap_or_default(),
        PAYLOAD,
        "oc exited 0 here while leaving the directory standing and never \
         writing the file - silent data loss, not a loud failure"
    );
}

/// The other silent row, and the one no exit-code assertion alone would catch:
/// a POPULATED directory must be refused AUDIBLY at 23, with the rest of the
/// batch still landing.
///
/// upstream: `delete.c:260-262` `cannot delete non-empty directory: %s` at
/// `FINFO` (stdout), then `delete.c:283-285` `could not make way for %s %s: %s`
/// at `FERROR_XFER` (stderr), whose `got_xfer_error` lifts the exit to 23
/// (`log.c:310-311`, `cleanup.c:217-218`).
#[test]
fn a_populated_directory_obstacle_is_refused_at_23_for_a_regular_file() {
    let fx = fixture(Source::Regular, true);

    let (status, stdout, stderr) = copy(&fx, &[]);

    assert_eq!(
        status,
        Some(23),
        "oc exited 0 in silence here. stdout: {stdout} stderr: {stderr}"
    );
    assert!(
        stdout.contains("cannot delete non-empty directory: obstacle"),
        "upstream reports the emptiness probe at FINFO, which is STDOUT. \
         stdout was: {stdout}"
    );
    assert!(
        stderr.contains("could not make way for new regular file: obstacle"),
        "upstream's FERROR_XFER line is STDERR and is what makes the run exit \
         non-zero; without it the obstacle is skipped in silence. stderr was: \
         {stderr}"
    );
    assert!(
        obstacle(&fx).is_dir(),
        "the contents are never removed to make room - `del_opts` carries no \
         DEL_RECURSE here"
    );
    assert!(
        sibling_landed(&fx),
        "a refused obstacle skips its own entry only; upstream keeps going"
    );
}

/// A FIFO over an empty directory. oc aborted the whole run with an oc-only
/// argument error (`cannot replace existing directory with special file`) that
/// has no upstream analogue - upstream never aborts a run for an obstacle.
///
/// upstream: `generator.c:2469-2483` `atomic_create()`.
#[test]
fn an_empty_directory_obstacle_is_removed_for_a_fifo() {
    let fx = fixture(Source::Fifo, false);

    let (status, stdout, stderr) = copy(&fx, &[]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    let meta = fs::symlink_metadata(obstacle(&fx)).expect("obstacle must still exist");
    assert!(
        meta.file_type().is_fifo(),
        "the fifo has to actually replace the directory. The obstacle is now: \
         {meta:?}"
    );
    assert!(
        !stderr.contains("cannot replace existing directory"),
        "the oc-only argument error has no upstream analogue. stderr: {stderr}"
    );
}

/// A symlink over an empty directory, the third source kind through the same
/// decision.
#[test]
fn an_empty_directory_obstacle_is_removed_for_a_symlink() {
    let fx = fixture(Source::Symlink, false);

    let (status, stdout, stderr) = copy(&fx, &[]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert_eq!(
        fs::read_link(obstacle(&fx)).expect("obstacle must now be a symlink"),
        Path::new("target"),
    );
}

/// The noun in the refusal comes from the NEW entry's type, not the obstacle's.
///
/// upstream: `generator.c:2041-2047` picks `DEL_FOR_DEVICE` or
/// `DEL_FOR_SPECIAL`; `delete.c:275-282` turns it into the printed noun.
#[test]
fn the_refusal_names_the_new_entrys_kind() {
    let fifo = fixture(Source::Fifo, true);
    let (fifo_status, _out, fifo_err) = copy(&fifo, &[]);
    assert_eq!(fifo_status, Some(23), "stderr: {fifo_err}");
    assert!(
        fifo_err.contains("could not make way for new special file: obstacle"),
        "a FIFO is DEL_FOR_SPECIAL. stderr was: {fifo_err}"
    );

    let link = fixture(Source::Symlink, true);
    let (link_status, _out, link_err) = copy(&link, &[]);
    assert_eq!(link_status, Some(23), "stderr: {link_err}");
    assert!(
        link_err.contains("could not make way for new symlink: obstacle"),
        "a symlink is DEL_FOR_SYMLINK, so the same shape says `symlink` here \
         and `special file` above. stderr was: {link_err}"
    );
}

/// `dir_in_the_way` excludes a directory from `make_backup` in BOTH modes
/// (`generator.c:2469-2477`), so under `--backup` the empty directory is still
/// `rmdir`'d, the entry still lands at exit 0, and NO `obstacle~` appears.
///
/// This is the non-vacuity companion: a change that only added diagnostics
/// would leave this cell as it was, and a change that routed the directory into
/// the backup area would pass every exit-code assertion above.
#[test]
fn a_directory_obstacle_is_never_backed_up() {
    let fx = fixture(Source::Symlink, false);

    let (status, stdout, stderr) = copy(&fx, &["--backup"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert_eq!(
        fs::read_link(obstacle(&fx)).expect("obstacle must now be a symlink"),
        Path::new("target"),
    );
    assert!(
        fs::symlink_metadata(fx.root.join("dst/obstacle~")).is_err(),
        "upstream never backs up the directory NODE - the make_backup arm is \
         unreachable for it whether or not --backup is set"
    );
}

/// `--delete` supplies `DEL_RECURSE` just as `--force` does
/// (`generator.c:2481` `delete_mode || force_delete`). oc honoured only
/// `--force` at the special/symlink sites, so this shape was refused at 23.
#[test]
fn delete_recurses_a_populated_obstacle_like_force() {
    for flag in ["--delete", "--force"] {
        let fx = fixture(Source::Symlink, true);

        let (status, stdout, stderr) = copy(&fx, &[flag]);

        assert_eq!(status, Some(0), "{flag}: stdout: {stdout} stderr: {stderr}");
        assert_eq!(
            fs::read_link(obstacle(&fx))
                .unwrap_or_else(|error| panic!("{flag}: obstacle must now be a symlink: {error}")),
            Path::new("target"),
        );
    }
}

/// The `--backup --force` shape, where upstream refuses even WITH `--force`.
///
/// `DEL_RECURSE` peels the directory by calling `delete_item()` per child, and
/// a child takes the `make_backup` arm under `--backup` (`delete.c:227-232`).
/// With a plain `~` suffix the child is renamed IN PLACE, so the directory
/// never empties and the caller's `rmdir` fails: upstream leaves `occupant~`
/// inside the surviving directory and exits 23. A peel implemented as a plain
/// recursive delete would exit 0 with the directory gone.
#[test]
fn backup_leaves_the_peeled_child_in_place_and_refuses() {
    let fx = fixture(Source::Symlink, true);

    let (status, stdout, stderr) = copy(&fx, &["--backup", "--force"]);

    assert_eq!(status, Some(23), "stdout: {stdout} stderr: {stderr}");
    assert!(
        obstacle(&fx).is_dir(),
        "the directory survives its own rmdir because the in-place backup \
         refilled it"
    );
    assert!(
        fs::symlink_metadata(fx.root.join("dst/obstacle/occupant~")).is_ok(),
        "the peel must back the child up in place, not unlink it; \
         dst/obstacle now holds: {:?}",
        fs::read_dir(obstacle(&fx))
            .map(|entries| entries.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
            .unwrap_or_default()
    );
    assert!(
        stderr.contains("could not make way for new symlink: obstacle"),
        "stderr was: {stderr}"
    );
}

/// `--ignore-existing` short-circuits BEFORE the make-way removal, so the
/// directory is left standing and nothing is written.
///
/// The removal and the skip decisions are ORDERED, not independent: reading the
/// destination state AFTER the removal makes `--ignore-existing` clear a
/// directory it was asked to leave alone.
///
/// upstream: `generator.c:1780-1804` - the `ignore_existing` test is at
/// `statret == 0` and `goto cleanup`s ahead of `generator.c:2149` /
/// `generator.c:2477-2483`.
#[test]
fn ignore_existing_leaves_the_directory_obstacle_standing() {
    for source in [Source::Regular, Source::Fifo, Source::Symlink] {
        let fx = fixture(source, false);

        let (status, stdout, stderr) = copy(&fx, &["--ignore-existing"]);

        assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
        assert!(
            obstacle(&fx).is_dir(),
            "--ignore-existing must not clear the obstacle it was asked to skip"
        );
    }
}

/// `--existing` asks whether the destination existed BEFORE the make-way
/// removal. A directory obstacle IS an existing destination, so the entry is
/// transferred and the directory removed.
///
/// The failure this pins is the exact mirror of the cell above: reading the
/// post-removal `None` skips the very entry the removal cleared the way for,
/// leaving NEITHER the directory NOR the file - a strictly worse outcome than
/// the silent skip this whole change set replaced.
///
/// upstream: `generator.c:1758-1766` - `ignore_non_existing` is tested at
/// `statret == -1 && stat_errno == ENOENT`.
#[test]
fn existing_only_still_replaces_a_directory_obstacle() {
    let fx = fixture(Source::Regular, false);

    let (status, stdout, stderr) = copy(&fx, &["--existing"]);

    assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
    assert_eq!(
        fs::read_to_string(obstacle(&fx)).unwrap_or_default(),
        PAYLOAD,
        "the destination existed - as a directory - so --existing does not skip \
         it, and the file must land where the directory was"
    );
}

/// Under `--dry-run` the refusal still happens, because upstream's emptiness
/// probe is a `get_dirlist()` READDIR, not the `rmdir` errno - and `rmdir`
/// itself returns 0 without touching anything under `--dry-run`
/// (`syscall.c do_rmdir_at()`).
///
/// A make-way implemented purely on the `rmdir` errno reports success here and
/// itemizes a transfer that the real run refuses.
///
/// upstream: `delete.c:110-118` and `delete.c:204-209`.
#[test]
fn a_dry_run_refuses_a_populated_obstacle_too() {
    let fx = fixture(Source::Regular, true);

    let (status, stdout, stderr) = copy(&fx, &["--dry-run"]);

    assert_eq!(status, Some(23), "stdout: {stdout} stderr: {stderr}");
    assert!(
        stdout.contains("cannot delete non-empty directory: obstacle"),
        "stdout was: {stdout}"
    );
    assert!(
        stderr.contains("could not make way for new regular file: obstacle"),
        "stderr was: {stderr}"
    );
}

/// NEGATIVE CONTROL. No obstacle at all: every source kind lands over a plain
/// regular-file destination at exit 0 with nothing printed.
///
/// This cell must stay GREEN under every mutation of the make-way decision. A
/// mutation that reddens it has broken the ordinary path rather than the
/// obstacle path, and a suite where it reddens alongside the cells above cannot
/// tell the two apart.
#[test]
fn a_non_directory_destination_is_untouched_by_the_make_way_decision() {
    for source in [Source::Regular, Source::Fifo, Source::Symlink] {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().to_path_buf();
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("dst")).expect("create dst");
        match source {
            Source::Symlink => {
                std::os::unix::fs::symlink("target", root.join("src/obstacle")).expect("symlink");
            }
            Source::Fifo => {
                let status = std::process::Command::new("mkfifo")
                    .arg(root.join("src/obstacle"))
                    .status()
                    .expect("spawn mkfifo");
                assert!(status.success());
            }
            Source::Regular => {
                fs::write(root.join("src/obstacle"), PAYLOAD).expect("write payload");
            }
        }
        fs::write(root.join("src/sibling"), SIBLING).expect("write sibling");
        fs::write(root.join("dst/obstacle"), "pre-existing\n").expect("plant dest file");

        let fx = Fixture { _temp: temp, root };
        let (status, stdout, stderr) = copy(&fx, &[]);

        assert_eq!(status, Some(0), "stdout: {stdout} stderr: {stderr}");
        assert!(
            !stdout.contains("cannot delete non-empty directory")
                && !stderr.contains("could not make way"),
            "the make-way diagnostics belong to a directory obstacle only. \
             stdout: {stdout} stderr: {stderr}"
        );
        assert!(sibling_landed(&fx));
    }
}
