//! `--relative --no-implied-dirs` leaves the destination's own parents alone.
//!
//! upstream: at protocol 30+ the sender lists every implied parent anyway,
//! flagged `FLAG_IMPLIED_DIR` (flist.c:2769-2770, flist.c:2456), and the
//! generator under `!implied_dirs` neither creates, lists, names nor re-times
//! one (generator.c:1640, generator.c:1811, generator.c:2586). A destination
//! parent that is a symlink leading out of the destination is therefore never
//! replaced: the entry's stat through the confined walk fails and is reported
//! (generator.c:2213-2223), a deeper parent's `make_path()` fails the same way
//! (generator.c:1718-1725), and the run ends `RERR_PARTIAL`. Measured on
//! 3.5.1 for a local copy, a pull and a push alike.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

#[derive(Clone, Copy, Debug)]
enum Mode {
    Local,
    Pull,
    Push,
}

const MODES: [Mode; 3] = [Mode::Local, Mode::Pull, Mode::Push];

/// How 3.5.1 prints a confinement refusal.
const ELOOP_TEXT: &str = ": Too many levels of symbolic links (40)";

/// How the same refusal printed before oc mapped its internal escape signal.
const EXDEV_TEXT: &str = ": Invalid cross-device link (18)";

/// `src/sub/f` and `src/sub/deep/g`, the parents dated in the past so a
/// re-timed destination parent is visible.
fn source(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub/deep")).expect("mkdir src");
    fs::write(src.join("sub/f"), b"data").expect("write f");
    fs::write(src.join("sub/deep/g"), b"d2").expect("write g");
    for dir in ["sub", "sub/deep"] {
        filetime::set_file_mtime(
            src.join(dir),
            filetime::FileTime::from_unix_time(1_600_000_000, 0),
        )
        .expect("date source parent");
    }
}

fn run(root: &Path, mode: Mode, extra: &[&str]) -> CliOutput {
    let dest = root.join("dest");
    let mut runner = OcRsyncCliRunner::new()
        .cwd(root.join("src"))
        .arg("-R")
        .arg("--no-implied-dirs")
        .args(extra);
    let rsh = |runner: OcRsyncCliRunner| {
        let stub = LshRunnerStub::locate().expect("lsh-stub located");
        runner
            .arg(format!("--rsh={}", stub.path().display()))
            .arg(format!(
                "--rsync-path={}",
                test_support::oc_rsync_bin().display()
            ))
    };
    runner = match mode {
        Mode::Local => runner
            .arg("sub/f")
            .arg("sub/deep/g")
            .arg(format!("{}/", dest.display())),
        Mode::Pull => {
            let src = root.join("src");
            rsh(runner)
                .arg(format!("localhost:{}/./sub/f", src.display()))
                .arg(format!("localhost:{}/./sub/deep/g", src.display()))
                .arg(format!("{}/", dest.display()))
        }
        Mode::Push => rsh(runner)
            .arg("sub/f")
            .arg("sub/deep/g")
            .arg(format!("localhost:{}/", dest.display())),
    };
    runner.run().expect("transfer run")
}

fn count(out: &CliOutput, needle: &str) -> usize {
    out.stderr_str()
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

/// The destructive case: `dest/sub` is a symlink to a tree outside the
/// destination. It must survive, nothing may be written through it, and the
/// two entries are reported as upstream reports them.
#[test]
fn a_symlinked_parent_leading_out_is_kept_and_its_entries_are_reported() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in MODES {
        let tmp = create_tempdir();
        source(tmp.path());
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).expect("mkdir outside");
        fs::write(outside.join("f"), b"bait").expect("write bait");
        // upstream's make_path() runs when the confined stat of `sub/deep`
        // fails, so it is reported even though the symlink's target holds a
        // `deep` a following stat would find.
        fs::create_dir_all(outside.join("deep")).expect("mkdir outside/deep");
        fs::write(outside.join("deep/g"), b"deep bait").expect("write deep bait");
        fs::create_dir_all(tmp.path().join("dest")).expect("mkdir dest");
        std::os::unix::fs::symlink(&outside, tmp.path().join("dest/sub")).expect("plant symlink");

        let out = run(tmp.path(), mode, &["-t"]);
        out.assert_exit(23);
        let sub = fs::symlink_metadata(tmp.path().join("dest/sub")).expect("dest/sub");
        assert!(
            sub.file_type().is_symlink(),
            "{mode:?}: the symlinked parent must not be replaced, stderr:\n{}",
            out.stderr_str()
        );
        assert_eq!(
            fs::read(outside.join("f")).expect("bait"),
            b"bait",
            "{mode:?}"
        );
        assert_eq!(
            fs::read(outside.join("deep/g")).expect("deep bait"),
            b"deep bait",
            "{mode:?}: nothing written through the symlink"
        );
        for (needle, lines) in [
            ("recv_generator: failed to stat \"", 2),
            ("recv_generator: mkdir \"", 1),
            // upstream: syscall.c:2920-2931 - the portable walk refuses an
            // escape with ELOOP; oc's EXDEV escape signal must not leak out.
            (ELOOP_TEXT, 3),
            (EXDEV_TEXT, 0),
        ] {
            assert_eq!(
                count(&out, needle),
                lines,
                "{mode:?}: {needle}, stderr:\n{}",
                out.stderr_str()
            );
        }
        assert!(
            out.stderr_contains("/dest/sub/deep\" failed: "),
            "{mode:?}: the make_path() line names the entry's directory, stderr:\n{}",
            out.stderr_str()
        );
    }
}

/// An existing implied parent is neither itemized nor re-timed: it keeps the
/// time the file writes gave it rather than taking the source's.
#[test]
fn existing_implied_parents_are_not_itemized_or_retimed() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in MODES {
        let tmp = create_tempdir();
        source(tmp.path());
        fs::create_dir_all(tmp.path().join("dest/sub/deep")).expect("mkdir dest parents");

        let out = run(tmp.path(), mode, &["-t", "-i"]);
        out.assert_exit(0);
        let rows = format!("{}{}", out.stdout_str(), out.stderr_str());
        assert!(
            !rows.lines().any(|l| l.contains(".d") && l.contains("sub")),
            "{mode:?}: implied parents must not be itemized:\n{rows}"
        );
        for dir in ["sub", "sub/deep"] {
            let mtime = fs::metadata(tmp.path().join("dest").join(dir))
                .expect("dest parent")
                .mtime();
            assert_ne!(
                mtime, 1_600_000_000,
                "{mode:?}: {dir} must not take the source parent's time"
            );
        }
    }
}

/// `-v` names only the files: the implied parents created on the way are
/// not listed.
#[test]
fn verbose_names_no_implied_parent() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in MODES {
        let tmp = create_tempdir();
        source(tmp.path());
        fs::create_dir_all(tmp.path().join("dest")).expect("mkdir dest");

        let out = run(tmp.path(), mode, &["-v"]);
        out.assert_exit(0);
        let stdout = out.stdout_str();
        let names: Vec<&str> = stdout.lines().filter(|l| l.starts_with("sub")).collect();
        assert_eq!(names, ["sub/f", "sub/deep/g"], "{mode:?}");
    }
}

/// `--list-only` lists only the named files, never their implied parents.
/// A push is not listed here: an oc push prints no listing at all, which is
/// a separate gap.
#[test]
fn list_only_omits_implied_parents() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in [Mode::Local, Mode::Pull] {
        let tmp = create_tempdir();
        source(tmp.path());
        fs::create_dir_all(tmp.path().join("dest")).expect("mkdir dest");

        let out = run(tmp.path(), mode, &["--list-only"]);
        out.assert_exit(0);
        let stdout = out.stdout_str();
        let names: Vec<&str> = stdout
            .lines()
            .filter_map(|l| l.split_whitespace().last())
            .collect();
        assert_eq!(names, ["sub/f", "sub/deep/g"], "{mode:?}:\n{stdout}");
    }
}

/// Recursion takes the incremental receiver: the implied `sub` above the
/// operand is left alone and is not counted as a failure, so the run
/// succeeds. A recursed directory operand (`sub/deep`) streams its sub-lists;
/// a file operand (`sub/f`) has nothing to recurse into, so the whole list
/// arrives at once and the receiver's batch body walks it.
#[test]
fn a_recursed_operand_leaves_its_implied_parent_alone() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for (mode, operand) in MODES
        .into_iter()
        .flat_map(|mode| [(mode, "sub/deep"), (mode, "sub/f")])
    {
        let tmp = create_tempdir();
        source(tmp.path());
        let src = tmp.path().join("src");
        let dest = tmp.path().join("dest");
        let dest_sub = dest.join("sub");
        fs::create_dir_all(&dest_sub).expect("mkdir dest/sub");
        filetime::set_file_mtime(
            &dest_sub,
            filetime::FileTime::from_unix_time(1_500_000_000, 0),
        )
        .expect("date dest/sub");

        let mut runner =
            OcRsyncCliRunner::new()
                .cwd(&src)
                .args(["-rR", "--no-implied-dirs", "-t", "-i"]);
        if !matches!(mode, Mode::Local) {
            let stub = LshRunnerStub::locate().expect("lsh-stub located");
            runner = runner
                .arg(format!("--rsh={}", stub.path().display()))
                .arg(format!(
                    "--rsync-path={}",
                    test_support::oc_rsync_bin().display()
                ));
        }
        runner = match mode {
            Mode::Local => runner.arg(operand).arg(format!("{}/", dest.display())),
            Mode::Pull => runner
                .arg(format!("localhost:{}/./{operand}", src.display()))
                .arg(format!("{}/", dest.display())),
            Mode::Push => runner
                .arg(operand)
                .arg(format!("localhost:{}/", dest.display())),
        };
        let out = runner.run().expect("transfer run");
        let rows = format!("{}{}", out.stdout_str(), out.stderr_str());
        assert_eq!(out.status, Some(0), "{mode:?} {operand}:\n{rows}");
        assert!(
            !rows.lines().any(|l| l.ends_with(" sub/")),
            "{mode:?} {operand}: the implied parent must not be itemized:\n{rows}"
        );
        if operand == "sub/deep" {
            assert!(rows.contains("cd+++++++++ sub/deep/"), "{mode:?}:\n{rows}");
            let deep = fs::metadata(dest.join("sub/deep"))
                .expect("dest/sub/deep")
                .mtime();
            assert_eq!(
                deep, 1_600_000_000,
                "{mode:?}: sub/deep takes the source time"
            );
        } else {
            assert!(dest.join("sub/f").is_file(), "{mode:?}:\n{rows}");
        }
        let sub = fs::metadata(&dest_sub).expect("dest/sub").mtime();
        assert_ne!(
            sub, 1_600_000_000,
            "{mode:?} {operand}: sub must not take the source time"
        );
    }
}

/// A recursed directory operand below a symlinked parent leading out of the
/// destination: the directory's own mkdir fails through the confined walk,
/// is reported once with the skip line, and nothing below it is written,
/// on every transport and both recursion modes. Measured on 3.5.1.
///
/// upstream: generator.c:1745-1746 stats the directory through the confined
/// walk, generator.c:1873-1887 reports the failed mkdir and skips its
/// contents.
#[test]
fn a_recursed_directory_below_a_symlinked_parent_is_reported_and_skipped() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in MODES {
        for extra in [&[][..], &["--no-inc-recursive"][..]] {
            let tmp = create_tempdir();
            source(tmp.path());
            let src = tmp.path().join("src");
            let dest = tmp.path().join("dest");
            let outside = tmp.path().join("outside");
            fs::create_dir_all(&outside).expect("mkdir outside");
            fs::create_dir_all(&dest).expect("mkdir dest");
            std::os::unix::fs::symlink(&outside, dest.join("sub")).expect("plant symlink");

            let mut runner = OcRsyncCliRunner::new()
                .cwd(&src)
                .args(["-rR", "--no-implied-dirs"])
                .args(extra);
            if !matches!(mode, Mode::Local) {
                let stub = LshRunnerStub::locate().expect("lsh-stub located");
                runner = runner
                    .arg(format!("--rsh={}", stub.path().display()))
                    .arg(format!(
                        "--rsync-path={}",
                        test_support::oc_rsync_bin().display()
                    ));
            }
            runner = match mode {
                Mode::Local => runner.arg("sub/deep").arg(format!("{}/", dest.display())),
                Mode::Pull => runner
                    .arg(format!("localhost:{}/./sub/deep", src.display()))
                    .arg(format!("{}/", dest.display())),
                Mode::Push => runner
                    .arg("sub/deep")
                    .arg(format!("localhost:{}/", dest.display())),
            };
            let out = runner.run().expect("transfer run");
            let cell = format!("{mode:?} {extra:?}");
            out.assert_exit(23);
            for (needle, lines) in [
                ("recv_generator: mkdir \"", 1),
                ("/dest/sub/deep\" failed: ", 1),
                (
                    "*** Skipping any contents from this failed directory ***",
                    1,
                ),
                ("cannot replace non-directory destination", 0),
                (ELOOP_TEXT, 1),
                (EXDEV_TEXT, 0),
            ] {
                assert_eq!(
                    count(&out, needle),
                    lines,
                    "{cell}: {needle}, stderr:\n{}",
                    out.stderr_str()
                );
            }
            assert!(
                fs::symlink_metadata(dest.join("sub"))
                    .expect("dest/sub")
                    .file_type()
                    .is_symlink(),
                "{cell}: the symlinked parent must be kept"
            );
            assert_eq!(
                fs::read_dir(&outside).expect("outside").count(),
                0,
                "{cell}: nothing may be written through the symlink"
            );
        }
    }
}
