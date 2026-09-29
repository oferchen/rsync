//! A create the generator cannot perform is reported in upstream's words, and
//! a run that owes `RERR_PARTIAL` closes with upstream's summary line.
//!
//! ```c
//! /* generator.c:2490-2522 - atomic_create() */
//!         rsyserr(FERROR_XFER, errno, "symlink %s -> \"%s\" failed", ...);
//!         rsyserr(FERROR_XFER, e, "mknod %s failed", full_fname(create_name));
//! /* hlink.c:486-487 - hard_link_one() */
//!         rsyserr(code, errno, "link %s => %s failed", full_fname(fname), oldname);
//! /* delete.c:265 - delete_item(), reached from the generator */
//!         rsyserr(FERROR_XFER, errno, "delete_file: %s(%s) failed", what, fbuf);
//! /* log.c:959-960 - log_exit() */
//!         rprintf(FERROR, "rsync error: %s (code %d) at %s(%d) [%s=%s]\n", ...);
//! ```
//!
//! Measured against rsync 3.5.1 on Linux, non-root:
//!
//! | cell                         | rsync 3.5.1                                               |
//! |------------------------------|-----------------------------------------------------------|
//! | local fifo                   | `[generator] mknod "<abs>" failed: ...`, summary `[sender]` |
//! | local symlink                | `[generator] symlink "<abs>" -> "target" failed: ...`      |
//! | local hard link              | `[generator] link "<abs>" => <leader> failed: ...`         |
//! | local dir obstacle           | `[generator] delete_file: rmdir(x) failed: ...`            |
//! | push, got_xfer_error only    | summary `(code 23) at main.c(1412) [sender=3.5.1]`         |
//! | pull, got_xfer_error only    | summary `(code 23) at main.c(1983) [generator=3.5.1]`      |
//!
//! Every cell exits 23. Before this fix oc's local copy printed its own
//! `failed to create fifo 'dst/p': ...` in place of both lines, and a push or
//! pull exited 23 with no summary line at all.
//!
//! Skipped (with a printed reason) as root, which ignores the mode bits.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const SUMMARY: &str =
    "error: some files/attrs were not transferred (see previous errors) (code 23) at ";

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).expect("stat").permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).expect("chmod");
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
    set_mode(&script, 0o755);
    script
}

/// A canonical scratch root holding `src/` and `dst/`. Every directory is made
/// writable again on drop so the temp dir can be removed.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("dst")).expect("create dst");
        Self { _temp: temp, root }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn mkfifo(&self, rel: &str) {
        let status = std::process::Command::new("mkfifo")
            .arg(self.path(rel))
            .status()
            .expect("spawn mkfifo");
        assert!(status.success(), "mkfifo failed: {status}");
    }

    /// Runs oc-rsync from the fixture root, so relative operands resolve there
    /// and upstream's `full_fname()` prefix is the root itself.
    fn run(&self, args: &[&str]) -> (Option<i32>, String) {
        let out = test_support::OcRsyncCliRunner::new()
            .binary(oc_rsync_binary())
            .cwd(&self.root)
            .args(args)
            .run()
            .expect("transfer did not finish");
        (
            out.status,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for dir in ["dst", "dst/d6"] {
            let path = self.root.join(dir);
            if path.is_dir() {
                set_mode(&path, 0o755);
            }
        }
    }
}

/// The closing line `log_exit()` prints, tagged with the process role.
fn assert_summary_last(stderr: &str, role: &str) {
    let last = stderr
        .lines()
        .rev()
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let trailer = format!(" [{role}={}]", env!("CARGO_PKG_VERSION"));
    assert!(
        last.contains(SUMMARY) && last.ends_with(&trailer),
        "the run must close with upstream's log_exit() line tagged {trailer}\n\
         last line: {last}\nstderr:\n{stderr}"
    );
}

fn assert_line(stderr: &str, expected: &str) {
    assert!(
        stderr.lines().any(|line| line == expected),
        "the failure must be reported the way upstream's rsyserr(FERROR_XFER) renders it\n\
         expected line: {expected}\nstderr:\n{stderr}"
    );
}

fn assert_partial(status: Option<i32>, stderr: &str) {
    assert_eq!(
        status,
        Some(23),
        "FERROR_XFER sets got_xfer_error, which lifts the exit to RERR_PARTIAL (23)\n\
         stderr:\n{stderr}"
    );
}

fn skip_as_root() -> bool {
    if running_as_root() {
        eprintln!("skip: root ignores the write-denied destination");
        return true;
    }
    false
}

#[test]
fn local_fifo_create_failure_prints_upstream_mknod_line() {
    if skip_as_root() {
        return;
    }
    let fx = Fixture::new();
    fx.mkfifo("src/p");
    set_mode(&fx.path("dst"), 0o555);
    let (status, stderr) = fx.run(&["-D", "src/p", "dst/"]);
    assert_line(
        &stderr,
        &format!(
            "rsync: [generator] mknod \"{}/dst/p\" failed: Permission denied (13)",
            fx.root.display()
        ),
    );
    assert_summary_last(&stderr, "sender");
    assert_partial(status, &stderr);
}

#[test]
fn local_symlink_create_failure_prints_upstream_symlink_line() {
    if skip_as_root() {
        return;
    }
    let fx = Fixture::new();
    std::os::unix::fs::symlink("target", fx.path("src/l")).expect("plant symlink");
    set_mode(&fx.path("dst"), 0o555);
    let (status, stderr) = fx.run(&["-l", "src/l", "dst/"]);
    assert_line(
        &stderr,
        &format!(
            "rsync: [generator] symlink \"{}/dst/l\" -> \"target\" failed: Permission denied (13)",
            fx.root.display()
        ),
    );
    assert_summary_last(&stderr, "sender");
    assert_partial(status, &stderr);
}

/// The leader `a` lands; the follower's directory is write-denied and, being
/// outside the file list (`--no-implied-dirs`), is never re-granted write
/// permission, so only the `link()` fails.
#[test]
fn local_hard_link_failure_prints_upstream_link_line() {
    if skip_as_root() {
        return;
    }
    let fx = Fixture::new();
    fs::create_dir_all(fx.path("src/d6")).expect("create src/d6");
    fs::create_dir_all(fx.path("dst/d6")).expect("create dst/d6");
    fs::write(fx.path("src/a"), b"hi\n").expect("write leader");
    fs::hard_link(fx.path("src/a"), fx.path("src/d6/b")).expect("link follower");
    set_mode(&fx.path("dst/d6"), 0o555);
    let (status, stderr) = fx.run(&[
        "-rH",
        "-R",
        "--no-implied-dirs",
        "src/./a",
        "src/./d6/b",
        "dst/",
    ]);
    assert_line(
        &stderr,
        &format!(
            "rsync: [generator] link \"{}/dst/d6/b\" => a failed: Permission denied (13)",
            fx.root.display()
        ),
    );
    assert_summary_last(&stderr, "sender");
    assert_partial(status, &stderr);
}

/// The obstacle is cleared by the generator, so `who_am_i()` is `generator`.
#[test]
fn local_obstacle_rmdir_failure_is_tagged_generator() {
    if skip_as_root() {
        return;
    }
    let fx = Fixture::new();
    fx.mkfifo("src/x");
    fs::create_dir_all(fx.path("dst/x")).expect("create obstacle");
    set_mode(&fx.path("dst"), 0o555);
    let (status, stderr) = fx.run(&["-D", "src/x", "dst/"]);
    assert_line(
        &stderr,
        "rsync: [generator] delete_file: rmdir(x) failed: Permission denied (13)",
    );
    assert_summary_last(&stderr, "sender");
    assert_partial(status, &stderr);
}

/// Over a real `--server`: only `got_xfer_error` is set, and the client still
/// owes the `log_exit()` line - `[sender]` when it pushes, `[generator]` when
/// it pulls (its main process runs the generator).
fn remote_summary_cell(pull: bool) {
    if skip_as_root() {
        return;
    }
    let fx = Fixture::new();
    fx.mkfifo("src/p");
    let shim = write_rsh_shim(&fx.root);
    set_mode(&fx.path("dst"), 0o555);
    let binary = oc_rsync_binary();
    let source = format!("{}/src/p", fx.root.display());
    let dest = format!("{}/dst/", fx.root.display());
    let (from, to) = if pull {
        (format!("h:{source}"), dest)
    } else {
        (source, format!("h:{dest}"))
    };
    let (status, stderr) = fx.run(&[
        "-D",
        "--rsh",
        shim.to_str().expect("utf-8 shim path"),
        "--rsync-path",
        binary.to_str().expect("utf-8 binary path"),
        &from,
        &to,
    ]);
    assert_summary_last(&stderr, if pull { "generator" } else { "sender" });
    assert_partial(status, &stderr);
}

#[test]
fn push_with_only_xfer_error_prints_sender_summary() {
    remote_summary_cell(false);
}

#[test]
fn pull_with_only_xfer_error_prints_generator_summary() {
    remote_summary_cell(true);
}
