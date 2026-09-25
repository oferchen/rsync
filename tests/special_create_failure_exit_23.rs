//! A special file or symlink the receiver cannot create is an `FERROR_XFER`
//! and ends the run with exit 23, never a silent exit 0.
//!
//! ```c
//! /* generator.c:2490-2493 - atomic_create(), symlink arm */
//!         if (gen_entry_symlink(slnk, create_name, file) < 0) {
//!                 rsyserr(FERROR_XFER, errno, "symlink %s -> \"%s\" failed",
//!                         full_fname(create_name), slnk);
//! /* generator.c:2506-2522 - atomic_create(), mknod arm */
//!         if (S_ISSOCK(file->mode) && (e == EOPNOTSUPP || e == ENOSYS)) {
//!                 rprintf(FWARNING, "skipping socket (creation unsupported here): %s\n", ...);
//!                 return 0;
//!         }
//!         rsyserr(FERROR_XFER, e, "mknod %s failed", full_fname(create_name));
//! /* log.c:337-338 - rwrite() */
//!         case FERROR_XFER:
//!                 got_xfer_error = 1;
//! /* cleanup.c:217-218 - exit_cleanup() lifts a zero exit to RERR_PARTIAL */
//! ```
//!
//! Measured against rsync 3.5.0 with a single-entry operand pushed or pulled
//! into a `chmod 555` destination:
//!
//! | entry   | rsync 3.5.0                                 | oc before     |
//! |---------|---------------------------------------------|---------------|
//! | fifo    | 23, `mknod "<dst>/p" failed: Permission ...` | **0, silent** |
//! | symlink | 23, `symlink "<dst>/l" -> "../outside" ...`  | **0, silent** |
//!
//! The destination is the transfer root, not an entry in the file list, so
//! neither implementation re-grants itself write permission on it first.
//!
//! These run over a REAL `--server` process: a local transfer takes the
//! engine's local-copy executor and exercises none of the receiver path.
//!
//! Skipped (with a printed reason) as root, which ignores the mode bits.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Root ignores the mode bits, so the denied destination would accept the
/// create and the cells would say nothing about the failure path.
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
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

fn set_mode(path: &Path, mode: u32) {
    let mut perms = fs::metadata(path).expect("stat").permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).expect("chmod");
}

/// Which entry the source offers.
#[derive(Clone, Copy)]
enum Entry {
    Fifo,
    Symlink,
}

impl Entry {
    const fn name(self) -> &'static str {
        match self {
            Self::Fifo => "p",
            Self::Symlink => "lnk",
        }
    }

    const fn flag(self) -> &'static str {
        match self {
            Self::Fifo => "-D",
            Self::Symlink => "-l",
        }
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Push,
    Pull,
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

impl Fixture {
    /// `src/<entry>` plus an empty, write-denied `dst/`. The root is
    /// canonicalized so the rendered name matches upstream's `getcwd()`-based
    /// `curr_dir` even where the temp dir sits behind a symlink.
    fn new(entry: Entry) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("dst")).expect("create dst");
        let source = root.join("src").join(entry.name());
        match entry {
            Entry::Fifo => {
                let status = std::process::Command::new("mkfifo")
                    .arg(&source)
                    .status()
                    .expect("spawn mkfifo");
                assert!(status.success(), "mkfifo failed: {status}");
            }
            Entry::Symlink => {
                std::os::unix::fs::symlink("../outside", &source).expect("plant symlink");
            }
        }
        let shim = write_rsh_shim(&root);
        set_mode(&root.join("dst"), 0o555);
        Self {
            _temp: temp,
            root,
            shim,
        }
    }

    fn run(&self, entry: Entry, direction: Direction) -> (Option<i32>, String) {
        let binary = oc_rsync_binary();
        let source = format!("{}/src/{}", self.root.display(), entry.name());
        let dest = format!("{}/dst/", self.root.display());
        let (from, to) = match direction {
            Direction::Push => (source, format!("h:{dest}")),
            Direction::Pull => (format!("h:{source}"), dest),
        };
        let out = test_support::OcRsyncCliRunner::new()
            .binary(&binary)
            .arg(entry.flag())
            .arg("--rsh")
            .arg(&self.shim)
            .arg("--rsync-path")
            .arg(&binary)
            .arg(from)
            .arg(to)
            .run()
            .expect("transfer did not finish");
        (
            out.status,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn created(&self, entry: Entry) -> bool {
        fs::symlink_metadata(self.root.join("dst").join(entry.name())).is_ok()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let dst = self.root.join("dst");
        if let Ok(meta) = fs::metadata(&dst) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&dst, perms);
        }
    }
}

fn expected_line(fx: &Fixture, entry: Entry) -> String {
    let name = format!("\"{}/dst/{}\"", fx.root.display(), entry.name());
    match entry {
        Entry::Fifo => {
            format!("rsync: [generator] mknod {name} failed: Permission denied (13)")
        }
        Entry::Symlink => format!(
            "rsync: [generator] symlink {name} -> \"../outside\" failed: Permission denied (13)"
        ),
    }
}

fn assert_cell(entry: Entry, direction: Direction) {
    if running_as_root() {
        eprintln!("skip: root ignores the write-denied destination");
        return;
    }
    let fx = Fixture::new(entry);
    let (status, stderr) = fx.run(entry, direction);
    assert!(!fx.created(entry), "the denied create must not land");
    let expected = expected_line(&fx, entry);
    assert!(
        stderr.lines().any(|line| line == expected),
        "the failed create must be reported the way upstream's rsyserr(FERROR_XFER) \
         renders it\nexpected line: {expected}\nstderr:\n{stderr}"
    );
    assert_eq!(
        status,
        Some(23),
        "FERROR_XFER sets got_xfer_error, which lifts the exit to RERR_PARTIAL (23); \
         exit 0 hides a node that never reached the destination\nstderr:\n{stderr}"
    );
}

#[test]
fn fifo_create_failure_on_push_exits_23() {
    assert_cell(Entry::Fifo, Direction::Push);
}

#[test]
fn fifo_create_failure_on_pull_exits_23() {
    assert_cell(Entry::Fifo, Direction::Pull);
}

#[test]
fn symlink_create_failure_on_push_exits_23() {
    assert_cell(Entry::Symlink, Direction::Push);
}

#[test]
fn symlink_create_failure_on_pull_exits_23() {
    assert_cell(Entry::Symlink, Direction::Pull);
}

/// Control: a nested socket that cannot be created race-safely (no `bindat()`)
/// stays a WARNING with exit 0. Upstream skips it before the `rsyserr` arm
/// (generator.c:2506-2519), so the exit-23 fix must not reach it.
#[cfg(any(
    target_os = "ios",
    target_os = "macos",
    target_os = "tvos",
    target_os = "watchos"
))]
#[test]
fn nested_socket_skip_still_warns_with_exit_0() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
    let nest = root.join("src/nest");
    fs::create_dir_all(&nest).expect("create src/nest");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    let _listener =
        std::os::unix::net::UnixListener::bind(nest.join("sock")).expect("bind source socket");
    let shim = write_rsh_shim(&root);
    let binary = oc_rsync_binary();
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .arg("-rD")
        .arg("--rsh")
        .arg(&shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(format!("{}/src/", root.display()))
        .arg(format!("h:{}/dst/", root.display()))
        .run()
        .expect("transfer did not finish");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("skipping socket (creation unsupported here):"),
        "the nested socket must still be skipped with upstream's warning\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("mknod"),
        "the socket skip is a warning, never the mknod FERROR_XFER\nstderr:\n{stderr}"
    );
    assert_eq!(
        out.status,
        Some(0),
        "a skipped socket is a warning and must not lift the exit to 23\nstderr:\n{stderr}"
    );
    assert!(
        root.join("dst/nest").is_dir(),
        "the rest of the tree transfers"
    );
}
