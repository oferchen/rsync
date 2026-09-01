//! A `--relative` transfer must report the same implied parent directories
//! whether or not it is allowed to create them.
//!
//! Upstream builds a `-R` implied parent exactly once, in the file list's own
//! directory pass, and reports it there:
//!
//! ```c
//! /* generator.c:1869-1873 - itemize() then the mkdir, in that order */
//! if (itemizing && f_out != -1)
//!     itemize(fnamecmp, file, ndx, statret, &sx,
//!             statret ? ITEM_LOCAL_CHANGE : 0, 0, NULL);
//! if (real_ret != 0 && gen_entry_mkdir(fname, file, file->mode|added_perms) < 0 ...)
//!
//! /* generator.c:1718-1725 - and a *second* creator that fires only when the
//!  * parent is STILL absent by the time an entry under it is reached */
//! if (relative_paths && !implied_dirs && file->mode != 0
//!  && do_stat_at(dn, &sx.st) < 0) {
//!         if (dry_run)
//!                 goto parent_is_dry_missing;
//!         if (make_path(fname, MKP_DROP_NAME | MKP_SKIP_SLASH) < 0) { ... }
//! }
//! ```
//!
//! The ordering is the whole point. `make_path()` is a *fallback*: by the time
//! it can fire, the directory pass has already classified and reported every
//! directory the file list carries. oc ran its equivalent
//! (`ensure_relative_parents`) as a *pre-pass* instead, so the directories
//! existed before anything classified them and a real run reported them as
//! pre-existing:
//!
//! ```text
//! $ oc-rsync -a -i -R -n src/./a/b/c/file.txt dest/     # dry
//! cd+++++++++ a/
//! cd+++++++++ a/b/
//! cd+++++++++ a/b/c/
//! $ oc-rsync -a -i -R    src/./a/b/c/file.txt dest/     # the SAME transfer, wet
//! .d..t...... a/
//! .d..t...... a/b/
//! .d..t...... a/b/c/
//! ```
//!
//! Both runs describe one transfer into an empty destination, so they cannot
//! both be right. rsync 3.5.0 prints `cd+++++++++` for all three in both runs,
//! measured on the same tree.
//!
//! The dry/wet comparison is the discriminating assertion, not the row text:
//! a fix that made the wet run wrong in a *new* way would still be caught, and
//! it cannot be satisfied by letting `-n` create the destination (that is
//! pinned separately by `dry_run_itemize_stats_remote.rs`).
//!
//! The "remote" is the oc-rsync binary itself behind a `--rsh` shim, because
//! `ensure_relative_parents` lives on the receiver, which a local copy never
//! reaches. Running both directions exercises the server receiver (push) and
//! the client receiver (pull).
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the shim uses `/bin/sh`).
//! - The `oc-rsync` binary is not built.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The rows rsync 3.5.0 prints for [`setup`]'s tree, in flist order, for both
/// `-i` and `-ni`. `%c` is `<` when the client sends, `>` when it receives.
fn expected_rows(direction: char) -> Vec<String> {
    vec![
        "cd+++++++++ a/".to_owned(),
        "cd+++++++++ a/b/".to_owned(),
        "cd+++++++++ a/b/c/".to_owned(),
        format!("{direction}f+++++++++ a/b/c/file.txt"),
    ]
}

fn oc_rsync_binary() -> PathBuf {
    test_support::oc_rsync_bin()
}

/// Writes the `--rsh` shim: drops the SSH-style options and the host, then
/// execs the server command locally.
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
    let mut perms = fs::metadata(&script).expect("stat rsh shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod rsh shim");
    script
}

/// `src/a/b/c/file.txt`, transferred as `./a/b/c/file.txt` so `-R` has three
/// implied parents to build and none of them exists at the destination.
fn setup(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("a/b/c")).expect("create source tree");
    fs::write(src.join("a/b/c/file.txt"), b"payload\n").expect("seed source file");
    src
}

/// Runs one `-a -i -R` transfer into a freshly created `dest`, optionally with
/// `-n`, and returns the process output.
fn run(shim: &Path, binary: &Path, src: &Path, dest: &Path, push: bool, dry: bool) -> Output {
    fs::create_dir_all(dest).expect("create destination");
    let operand = "./a/b/c/file.txt";
    let (from, to) = if push {
        (operand.to_owned(), format!("relhost:{}/", dest.display()))
    } else {
        (format!("relhost:{operand}"), format!("{}/", dest.display()))
    };
    let mut cmd = Command::new(binary);
    cmd.current_dir(src)
        .arg("-a")
        .arg("-i")
        .arg("-R")
        .arg("--rsh")
        .arg(shim)
        .arg("--rsync-path")
        .arg(binary)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if dry {
        cmd.arg("-n");
    }
    cmd.arg(from).arg(to);
    cmd.output().expect("run oc-rsync")
}

/// The itemize rows naming the implied parents and the transferred file.
///
/// The transfer root's own `./` row is dropped: it reports the mtime
/// difference between two directories the fixture created seconds apart, which
/// is not what this file is about.
fn parent_rows(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| line.len() > 12 && line.as_bytes()[11] == b' ' && !line.ends_with(" ./"))
        .map(str::to_owned)
        .collect()
}

fn assert_dry_wet_parity(push: bool) {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let label = if push { "push" } else { "pull" };
    let temp = test_support::create_tempdir();
    let root = temp.path();
    let src = setup(root);
    let shim = write_rsh_shim(root);

    let dry_dest = root.join("dest-dry");
    let wet_dest = root.join("dest-wet");
    let dry = run(&shim, &binary, &src, &dry_dest, push, true);
    let wet = run(&shim, &binary, &src, &wet_dest, push, false);

    for (what, output) in [("dry", &dry), ("wet", &wet)] {
        assert!(
            output.status.success(),
            "{label} {what} run must exit 0, got {:?}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let dry_stdout = String::from_utf8_lossy(&dry.stdout);
    let wet_stdout = String::from_utf8_lossy(&wet.stdout);
    let dry_rows = parent_rows(&dry_stdout);
    let wet_rows = parent_rows(&wet_stdout);

    // Non-vacuity first: an empty row set satisfies the equality below.
    assert_eq!(
        dry_rows.len(),
        4,
        "{label}: the fixture produced no implied-parent rows, so the parity \
         assertion would hold vacuously\nstdout:\n{dry_stdout}"
    );

    assert_eq!(
        dry_rows, wet_rows,
        "{label}: -n and the real run disagree about the same transfer. The \
         real run classified the implied parents against a tree an unconfined \
         pre-pass had already built.\ndry stdout:\n{dry_stdout}\nwet stdout:\n{wet_stdout}"
    );

    let direction = if push { '<' } else { '>' };
    assert_eq!(
        wet_rows,
        expected_rows(direction),
        "{label}: rows must match rsync 3.5.0\nstdout:\n{wet_stdout}"
    );

    // The rows must describe a transfer that actually happened, or "they agree"
    // would be a statement about two runs that both did nothing.
    assert_eq!(
        fs::read_to_string(wet_dest.join("a/b/c/file.txt")).expect("destination file"),
        "payload\n",
        "{label}: the real run must still deliver the file"
    );
    assert!(
        !dry_dest.join("a").exists(),
        "{label}: -n must leave the destination untouched"
    );
}

#[test]
fn relative_implied_parents_report_identically_dry_and_wet_on_push() {
    assert_dry_wet_parity(true);
}

#[test]
fn relative_implied_parents_report_identically_dry_and_wet_on_pull() {
    assert_dry_wet_parity(false);
}
