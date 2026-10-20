//! `--max-delete` caps the CONTENTS of a directory cleared to make room for an
//! incoming entry, while the make-room removal of the obstacle node itself stays
//! exempt.
//!
//! upstream `delete.c:delete_item()` exempts only `DEL_MAKE_ROOM` deletions
//! from the cap:
//!
//! ```text
//! delete.c:217
//! if (!(flags & DEL_MAKE_ROOM) && max_delete >= 0 && stats.deleted_files >= max_delete) {
//!         skipped_deletes++;
//!         return DR_AT_LIMIT;
//! }
//! ```
//!
//! but `delete_dir_contents()` strips that flag before it recurses
//! (`delete.c:126`, `flags & ~(DEL_RECURSE|DEL_MAKE_ROOM|DEL_NO_UID_WRITE)`), so
//! every child of a populated obstacle is an ordinary, counted, capped deletion.
//! A child the cap refuses leaves the directory non-empty, and the make-room
//! then fails out loud at `RERR_PARTIAL` (23), with the generator's
//! `Deletions stopped due to --max-delete limit (%d skipped)` warning
//! (`generator.c:2904-2908`). `cleanup.c:213-218` ranks `got_xfer_error` above
//! `IOERR_DEL_LIMIT`, so the run exits 23 and not 25.
//!
//! Measured against rsync 3.5.1 (protocol 33), local and over a `--server`
//! child, with `-rlI -v --delete` (or `--force`) and `dst/obstacle/{a,b}`
//! standing where `src/obstacle` (a regular file) has to land:
//!
//! | `--max-delete` | rsync 3.5.1 | oc before |
//! |---|---|---|
//! | 0 | 23, obstacle keeps `a`,`b`, `(2 skipped)` | 0, obstacle replaced, both children deleted |
//! | 1 | 23, `deleting obstacle/b`, keeps `a`, `(1 skipped)` | 0, obstacle replaced |
//! | 2 | 0, both children deleted, file placed | 0, same |
//! | 0, symlink obstacle | 0, symlink replaced (the node is exempt) | 0, same |
//!
//! The cap is ONE run-wide budget (`stats.deleted_files`). With an extraneous
//! `dst/zextra` beside the obstacle, `--delete --max-delete=1` spends it on
//! `zextra` first and refuses both children (23, `(2 skipped)`), and
//! `--delete-after --max-delete=2` spends it on the children first and keeps
//! `zextra` (25, `(1 skipped)`).
//!
//! Over the remote shell oc failed earlier still: for `--max-delete=0` the
//! client forwards `--max-delete=-1` (`options.c:3002-3006`), and the oc server
//! rejected the negative value outright with exit 12 instead of clamping it to
//! 0 the way `options.c:2308-2311` does.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const PAYLOAD: &str = "regular payload\n";
const SIBLING: &str = "sibling payload\n";

/// How the transfer reaches the destination.
#[derive(Clone, Copy, Debug)]
enum Mode {
    /// The engine's local-copy executor.
    Local,
    /// A push into an external `--server` receiver through a shell shim.
    RemoteShell,
}

/// What stands at `dst/obstacle`.
#[derive(Clone, Copy)]
enum Obstacle {
    /// A directory holding `a` and `b`.
    PopulatedDirectory,
    /// A dangling symlink: a single-entry make-room, exempt from the cap.
    Symlink,
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// `rsync` invokes `$RSYNC_RSH <host> <command...>`; drop the host and exec the
/// command locally, so the receiver is a real separate `--server` process.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(&script, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
    let mut perms = fs::metadata(&script).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod shim");
    script
}

/// Builds `src/{obstacle,sibling}` and the chosen `dst/obstacle`.
///
/// `sibling` separates "the obstacle entry was skipped" from "the whole
/// transfer stopped": upstream keeps transferring it when the make-room fails.
fn fixture(obstacle: Obstacle) -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    fs::write(root.join("src/obstacle"), PAYLOAD).expect("write source payload");
    fs::write(root.join("src/sibling"), SIBLING).expect("write source sibling");
    match obstacle {
        Obstacle::PopulatedDirectory => {
            fs::create_dir(root.join("dst/obstacle")).expect("create obstacle");
            fs::write(root.join("dst/obstacle/a"), "a\n").expect("write child a");
            fs::write(root.join("dst/obstacle/b"), "b\n").expect("write child b");
        }
        Obstacle::Symlink => {
            std::os::unix::fs::symlink("nowhere", root.join("dst/obstacle"))
                .expect("plant symlink obstacle");
        }
    }
    Fixture { _temp: temp, root }
}

struct Outcome {
    status: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(fx: &Fixture, mode: Mode, extra: &[&str]) -> Outcome {
    let binary = oc_rsync_binary();
    let destination = format!("{}/dst/", fx.root.display());
    let mut runner = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        // -l keeps the symlink obstacle a symlink, -I defeats the quick check.
        .args(["-rlI", "-v"])
        .args(extra);
    runner = match mode {
        Mode::Local => runner
            .arg(format!("{}/src/", fx.root.display()))
            .arg(destination),
        Mode::RemoteShell => runner
            .arg("--rsh")
            .arg(write_rsh_shim(&fx.root))
            .arg("--rsync-path")
            .arg(&binary)
            .arg(format!("{}/src/", fx.root.display()))
            .arg(format!("mdhost:{destination}")),
    };
    let out = runner.run().expect("transfer did not finish");
    Outcome {
        status: out.status,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn obstacle_children(fx: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(fx.root.join("dst/obstacle"))
        .expect("the obstacle directory must survive")
        .map(|entry| {
            entry
                .expect("read obstacle entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

fn sibling_landed(fx: &Fixture) -> bool {
    fs::read_to_string(fx.root.join("dst/sibling")).is_ok_and(|body| body == SIBLING)
}

/// The refusal shape every capped cell shares: exit 23, the obstacle kept with
/// `survivors`, both upstream diagnostics, the skipped-count warning, and the
/// sibling still transferred.
fn assert_refused(fx: &Fixture, mode: Mode, out: &Outcome, survivors: &[&str], skipped: u64) {
    let context = format!("{mode:?}\nstdout: {}\nstderr: {}", out.stdout, out.stderr);
    assert_eq!(out.status, Some(23), "{context}");
    assert_eq!(obstacle_children(fx), survivors, "{context}");
    assert!(
        out.stderr
            .contains("could not make way for new regular file: obstacle"),
        "delete.c:283-285 names the make-room failure. {context}"
    );
    assert!(
        out.stderr.contains(&format!(
            "Deletions stopped due to --max-delete limit ({skipped} skipped)"
        )),
        "generator.c:2904-2907 reports the capped children. {context}"
    );
    assert!(
        out.stdout
            .contains("cannot delete non-empty directory: obstacle"),
        "delete.c:178-181 explains why the obstacle survived. {context}"
    );
    assert!(
        sibling_landed(fx),
        "the rest of the transfer must finish. {context}"
    );
}

fn assert_replaced(fx: &Fixture, mode: Mode, out: &Outcome) {
    let context = format!("{mode:?}\nstdout: {}\nstderr: {}", out.stdout, out.stderr);
    assert_eq!(out.status, Some(0), "{context}");
    assert_eq!(
        fs::read_to_string(fx.root.join("dst/obstacle")).expect("obstacle must now be a file"),
        PAYLOAD,
        "{context}"
    );
    assert!(sibling_landed(fx), "{context}");
}

const MODES: [Mode; 2] = [Mode::Local, Mode::RemoteShell];

#[test]
fn a_zero_cap_refuses_to_clear_a_populated_obstacle_under_delete() {
    for mode in MODES {
        let fx = fixture(Obstacle::PopulatedDirectory);
        let out = run(&fx, mode, &["--delete", "--max-delete=0"]);
        assert_refused(&fx, mode, &out, &["a", "b"], 2);
    }
}

/// `--force` reaches the same `DEL_RECURSE` peel as `--delete`
/// (`generator.c:2477`, `del_opts = delete_mode || force_delete`).
#[test]
fn a_zero_cap_refuses_to_clear_a_populated_obstacle_under_force() {
    for mode in MODES {
        let fx = fixture(Obstacle::PopulatedDirectory);
        let out = run(&fx, mode, &["--force", "--max-delete=0"]);
        assert_refused(&fx, mode, &out, &["a", "b"], 2);
    }
}

/// The cap is checked per child in upstream's reverse dirlist order
/// (`delete.c:141`), so `b` is deleted and `a` is the one skipped.
#[test]
fn a_partial_cap_deletes_children_until_it_trips() {
    for mode in MODES {
        let fx = fixture(Obstacle::PopulatedDirectory);
        let out = run(&fx, mode, &["--delete", "--max-delete=1"]);
        assert_refused(&fx, mode, &out, &["a"], 1);
        // The local executor withholds every `-v` row of a run that ends
        // RERR_PARTIAL, a separate divergence from upstream, so the row of the
        // child deleted before the cap is pinned over the remote shell.
        if matches!(mode, Mode::RemoteShell) {
            assert!(
                out.stdout.contains("deleting obstacle/b"),
                "the child deleted before the cap is logged. stdout: {}",
                out.stdout
            );
        }
    }
}

/// Control: a cap that covers every child clears the obstacle. The node itself
/// is never counted (`DEL_MAKE_ROOM`), so a cap equal to the child count is
/// enough - an implementation that also charged the node would refuse here.
#[test]
fn a_cap_covering_every_child_clears_the_obstacle() {
    for mode in MODES {
        let fx = fixture(Obstacle::PopulatedDirectory);
        let out = run(&fx, mode, &["--delete", "--max-delete=2"]);
        assert_replaced(&fx, mode, &out);
    }
}

/// Control: a single-entry make-room is exempt from the cap even at zero
/// (`delete.c:217`'s `!(flags & DEL_MAKE_ROOM)`).
#[test]
fn a_zero_cap_still_replaces_a_single_entry_obstacle() {
    for mode in MODES {
        let fx = fixture(Obstacle::Symlink);
        let out = run(&fx, mode, &["--delete", "--max-delete=0"]);
        assert_replaced(&fx, mode, &out);
    }
}

fn plant_extraneous(fx: &Fixture) {
    fs::write(fx.root.join("dst/zextra"), "extraneous\n").expect("plant extraneous entry");
}

fn extraneous_kept(fx: &Fixture) -> bool {
    fx.root.join("dst/zextra").exists()
}

/// `--delete-during` sweeps the directory before its files are generated, so
/// the extraneous entry takes the whole budget and the make-room that follows
/// has none left for either child.
#[test]
fn a_sweep_deletion_spends_the_budget_a_later_make_room_needs() {
    for mode in MODES {
        let fx = fixture(Obstacle::PopulatedDirectory);
        plant_extraneous(&fx);
        let out = run(&fx, mode, &["--delete", "--max-delete=1"]);
        assert_refused(&fx, mode, &out, &["a", "b"], 2);
        assert!(
            !extraneous_kept(&fx),
            "{mode:?}: the sweep spent the budget first"
        );
    }
}

/// `--delete-after` runs once every file is placed, so the make-room children
/// take the budget and the sweep that follows is the one refused.
#[test]
fn make_room_deletions_spend_the_budget_a_later_sweep_needs() {
    for mode in MODES {
        let fx = fixture(Obstacle::PopulatedDirectory);
        plant_extraneous(&fx);
        let out = run(&fx, mode, &["--delete-after", "--max-delete=2"]);
        let context = format!("{mode:?}\nstdout: {}\nstderr: {}", out.stdout, out.stderr);
        assert_eq!(out.status, Some(25), "{context}");
        assert_eq!(
            fs::read_to_string(fx.root.join("dst/obstacle")).expect("obstacle replaced"),
            PAYLOAD,
            "{context}"
        );
        assert!(
            extraneous_kept(&fx),
            "the make-room spent the budget. {context}"
        );
        assert!(
            out.stderr
                .contains("Deletions stopped due to --max-delete limit (1 skipped)"),
            "{context}"
        );
    }
}
