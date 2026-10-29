//! A directory the receiver cannot make is reported and its contents skipped.
//!
//! upstream: generator.c:1873-1887 treats every mkdir failure but EEXIST
//! alike: `rsyserr(FERROR_XFER, errno, "recv_generator: mkdir %s failed")`,
//! the `*** Skipping any contents from this failed directory ***` line, and
//! `skip_dir` so nothing below it is attempted (generator.c:1648-1658). The
//! run carries on and ends `RERR_PARTIAL`. Measured on 3.5.1 for a local
//! copy, a pull and a push, with and without incremental recursion: the
//! same two lines, exit 23, and only the unrelated entry copied.
#![cfg(unix)]
use std::fs;
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

/// The two recursion modes: incremental (the default) and `--no-i-r`, whose
/// receiver creates every directory in one pass before any file.
const RECURSION: [&[&str]; 2] = [&[], &["--no-inc-recursive"]];

/// `src/afile/sub/f` and `src/other`; the destination holds a regular file
/// `afile`, so `afile/sub` fails ENOTDIR. `--no-implied-dirs` leaves the
/// implied `afile` alone, as upstream's generator.c:1811 does.
fn fixture(root: &Path) {
    let src = root.join("src");
    fs::create_dir_all(src.join("afile/sub")).expect("mkdir src");
    fs::write(src.join("afile/sub/f"), b"below").expect("write f");
    fs::write(src.join("other"), b"other").expect("write other");
    fs::create_dir_all(root.join("dest")).expect("mkdir dest");
    fs::write(root.join("dest/afile"), b"file").expect("plant obstruction");
}

fn run(root: &Path, mode: Mode, extra: &[&str]) -> CliOutput {
    let dest = root.join("dest");
    let src = root.join("src");
    let runner = OcRsyncCliRunner::new()
        .cwd(&src)
        .arg("-rR")
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
    let runner = match mode {
        Mode::Local => runner
            .arg("afile/sub")
            .arg("other")
            .arg(format!("{}/", dest.display())),
        Mode::Pull => rsh(runner)
            .arg(format!("localhost:{}/./afile/sub", src.display()))
            .arg(format!("localhost:{}/./other", src.display()))
            .arg(format!("{}/", dest.display())),
        Mode::Push => rsh(runner)
            .arg("afile/sub")
            .arg("other")
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

#[test]
fn a_failed_mkdir_is_reported_once_with_its_errno_and_its_contents_skipped() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    for mode in MODES {
        for extra in RECURSION {
            let tmp = create_tempdir();
            fixture(tmp.path());

            let out = run(tmp.path(), mode, extra);
            let cell = format!("{mode:?} {extra:?}");
            out.assert_exit(23);
            for (needle, lines) in [
                ("/dest/afile/sub\" failed: Not a directory (20)", 1),
                ("recv_generator: mkdir \"", 1),
                (
                    "*** Skipping any contents from this failed directory ***",
                    1,
                ),
                // oc's own wording for the same failure, which upstream never
                // prints.
                ("failed to create directory", 0),
                ("failed to inspect destination directory", 0),
            ] {
                assert_eq!(
                    count(&out, needle),
                    lines,
                    "{cell}: {needle}, stderr:\n{}",
                    out.stderr_str()
                );
            }
            assert_eq!(
                fs::read(tmp.path().join("dest/other")).expect("other copied"),
                b"other",
                "{cell}: the unrelated entry is still transferred"
            );
            assert_eq!(
                fs::read(tmp.path().join("dest/afile")).expect("obstruction"),
                b"file",
                "{cell}: the implied parent is left alone"
            );
        }
    }
}

/// EACCES from a read-only destination: the mkdir of `src` is reported once,
/// not once per child, and its three children are never attempted.
#[test]
fn a_permission_denied_mkdir_is_reported_once_for_all_its_children() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    if rustix::process::geteuid().is_root() {
        eprintln!("SKIP: root bypasses the read-only destination");
        return;
    }
    for mode in MODES {
        for extra in RECURSION {
            let tmp = create_tempdir();
            let src = tmp.path().join("src");
            fs::create_dir_all(src.join("d")).expect("mkdir src");
            for (name, data) in [("f1", "1"), ("f2", "2"), ("d/g", "3")] {
                fs::write(src.join(name), data).expect("write child");
            }
            let dest = tmp.path().join("dest");
            fs::create_dir_all(&dest).expect("mkdir dest");
            set_mode(&dest, 0o555);

            let stub = LshRunnerStub::locate().expect("lsh-stub located");
            let runner = OcRsyncCliRunner::new().arg("-r").args(extra);
            let runner = match mode {
                Mode::Local => runner.arg(&src).arg(format!("{}/", dest.display())),
                Mode::Pull | Mode::Push => {
                    let runner =
                        runner
                            .arg(format!("--rsh={}", stub.path().display()))
                            .arg(format!(
                                "--rsync-path={}",
                                test_support::oc_rsync_bin().display()
                            ));
                    if matches!(mode, Mode::Pull) {
                        runner
                            .arg(format!("localhost:{}", src.display()))
                            .arg(format!("{}/", dest.display()))
                    } else {
                        runner
                            .arg(&src)
                            .arg(format!("localhost:{}/", dest.display()))
                    }
                }
            };
            let out = runner.run().expect("transfer run");
            set_mode(&dest, 0o755);

            let cell = format!("{mode:?} {extra:?}");
            out.assert_exit(23);
            for (needle, lines) in [
                ("/dest/src\" failed: Permission denied (13)", 1),
                ("recv_generator: mkdir \"", 1),
                (
                    "*** Skipping any contents from this failed directory ***",
                    1,
                ),
                ("failed to create directory", 0),
            ] {
                assert_eq!(
                    count(&out, needle),
                    lines,
                    "{cell}: {needle}, stderr:\n{}",
                    out.stderr_str()
                );
            }
            assert!(!dest.join("src").exists(), "{cell}");
        }
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
}
