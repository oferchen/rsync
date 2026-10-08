//! `--delete-missing-args` must not empty a populated directory unless
//! `--delete` or `--force` is in effect.
//!
//! upstream `generator.c:1749-1753` handles the mode-0 entry a missing operand
//! becomes:
//!
//! ```text
//! if (missing_args == 2 && file->mode == 0) {
//!         ...
//!         if (statret == 0)
//!                 delete_item(fname, sx.st.st_mode, del_opts);
//!         return;
//! }
//! ```
//!
//! with `int del_opts = delete_mode || force_delete ? DEL_RECURSE : 0;`
//! (`generator.c:1629`). Without `DEL_RECURSE`, `delete_dir_contents()` only
//! probes (`delete.c:115-118`) and a populated directory comes back
//! `DR_NOT_EMPTY` with `cannot delete non-empty directory: %s` at FINFO
//! (`delete.c:178-180`); the directory and everything in it is kept and the
//! run still exits 0. An empty directory is removed either way.
//!
//! Ground truth, rsync 3.5.1 with `--delete-missing-args src/nope src/empty dst/`
//! and a populated `dst/nope/`, in all three of local, push and pull:
//!
//! | flags | `dst/nope` | `dst/empty` | output |
//! |---|---|---|---|
//! | `-r` | kept | removed | `cannot delete non-empty directory: nope` |
//! | `-r --delete` | removed | removed | - |
//! | `-r --force` | removed | removed | - |
//!
//! oc removed `dst/nope` and its contents in every cell.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NOTICE: &str = "cannot delete non-empty directory: nope";

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// `rsync` invokes `$RSYNC_RSH <host> <command...>`; drop the host and exec the
/// command locally, so the remote side really is a separate `--server` process.
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

#[derive(Clone, Copy, Debug)]
enum Mode {
    Local,
    Push,
    Pull,
}

const MODES: [Mode; 3] = [Mode::Local, Mode::Push, Mode::Pull];

struct Run {
    output: Output,
    nope_kept: bool,
    nope_file_kept: bool,
    empty_kept: bool,
}

/// Builds `src/` with neither operand present, and `dst/` holding a populated
/// `nope/` (a file one level down, so a recursive delete has real work to do)
/// and an empty `empty/`, then runs `oc-rsync <flags> --delete-missing-args
/// src/nope src/empty dst/` in `mode`.
fn run(mode: Mode, flags: &[&str]) -> Run {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    fs::create_dir_all(root.join("src")).expect("create src");
    fs::create_dir_all(root.join("dst/nope/sub")).expect("create dst/nope");
    fs::write(root.join("dst/nope/sub/f"), b"keep me\n").expect("write nested file");
    fs::write(root.join("dst/nope/g"), b"keep me too\n").expect("write file");
    fs::create_dir_all(root.join("dst/empty")).expect("create dst/empty");
    let shim = write_rsh_shim(root);

    let binary = oc_rsync_binary();
    let src = |name: &str| root.join("src").join(name).display().to_string();
    let dst = format!("{}/", root.join("dst").display());
    let mut command = Command::new(&binary);
    command.args(flags).arg("--delete-missing-args");
    match mode {
        Mode::Local => {
            command.arg(src("nope")).arg(src("empty")).arg(&dst);
        }
        Mode::Push => {
            command
                .arg("-e")
                .arg(&shim)
                .arg(format!("--rsync-path={}", binary.display()))
                .arg(src("nope"))
                .arg(src("empty"))
                .arg(format!("host:{dst}"));
        }
        Mode::Pull => {
            command
                .arg("-e")
                .arg(&shim)
                .arg(format!("--rsync-path={}", binary.display()))
                .arg(format!("host:{}", src("nope")))
                .arg(format!("host:{}", src("empty")))
                .arg(&dst);
        }
    }
    let output = command.output().expect("run oc-rsync");
    Run {
        output,
        nope_kept: root.join("dst/nope").is_dir(),
        nope_file_kept: root.join("dst/nope/sub/f").is_file(),
        empty_kept: root.join("dst/empty").exists(),
    }
}

fn describe(mode: Mode, run: &Run) -> String {
    format!(
        "{mode:?}: exit {:?}\nstdout:\n{}\nstderr:\n{}",
        run.output.status.code(),
        String::from_utf8_lossy(&run.output.stdout),
        String::from_utf8_lossy(&run.output.stderr),
    )
}

/// The data-loss cell: no `--delete`, no `--force`. The populated directory and
/// its contents survive, the notice is printed, and the run still succeeds.
#[test]
fn populated_directory_is_kept_without_delete_or_force() {
    for mode in MODES {
        let run = run(mode, &["-r"]);
        let what = describe(mode, &run);
        assert_eq!(run.output.status.code(), Some(0), "{what}");
        assert!(run.nope_kept, "dst/nope must survive: {what}");
        assert!(run.nope_file_kept, "dst/nope contents must survive: {what}");
        let stdout = String::from_utf8_lossy(&run.output.stdout);
        assert!(stdout.contains(NOTICE), "missing FINFO notice: {what}");
        assert!(
            !run.empty_kept,
            "control: dst/empty must still be removed: {what}"
        );
    }
}

/// `--delete` and `--force` set `DEL_RECURSE`, so the populated directory is
/// emptied and removed. This is the half of `del_opts` that must not regress.
#[test]
fn populated_directory_is_removed_under_delete_or_force() {
    for mode in MODES {
        for flag in ["--delete", "--force"] {
            let run = run(mode, &["-r", flag]);
            let what = describe(mode, &run);
            assert_eq!(run.output.status.code(), Some(0), "{flag} {what}");
            assert!(!run.nope_kept, "{flag}: dst/nope must be removed: {what}");
            assert!(!run.empty_kept, "{flag}: dst/empty must be removed: {what}");
        }
    }
}
