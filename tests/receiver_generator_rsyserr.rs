//! Failures in the generator half of a network receiver are reported the way
//! upstream's `rsyserr(FERROR_XFER, ...)` renders them, tagged `[generator]`,
//! and end the run with exit 23 on both the push and the pull side.
//!
//! ```c
//! /* hlink.c:478-488 - hard_link_one() */
//!         rsyserr(code, errno, "link %s => %s failed", full_fname(fname), oldname);
//! /* delete.c:265 - delete_item(), run by the generator for an obstacle */
//!         rsyserr(FERROR_XFER, errno, "delete_file: %s(%s) failed", what, fbuf);
//! /* rsync.c:682-684, 781, 811-813 - set_file_attrs() */
//!         rsyserr(FERROR_XFER, errno, "%s %s failed", change_uid ? "chown" : "chgrp", ...);
//!         rsyserr(FERROR_XFER, errno, "failed to set times on %s", full_fname(fname));
//!         rsyserr(FERROR_XFER, errno, "failed to set permissions on %s", full_fname(fname));
//! /* rsync.c:987-995 - who_am_i() is "generator" in the generator process */
//! ```
//!
//! Measured against rsync 3.5.1 over a real `--server` process, push and pull:
//!
//! | cell                          | rsync 3.5.1                                     | oc before                    |
//! |-------------------------------|-------------------------------------------------|------------------------------|
//! | hard link into `chmod 555`    | 23, `link "<dst>/b" => a failed: Permission ...` | **0, silent**                |
//! | empty dir in the way of fifo  | 23, `[generator] delete_file: rmdir(x) ...`      | 23, **`[receiver]`** tag     |
//! | root-owned fifo, `-D -t`      | 23, `failed to set times on "<dst>/p": ...`     | **0, silent**                |
//! | root-owned dir, `-rg`         | 23, `chgrp "<dst>/d" failed: ...`               | push **0**, pull 23, silent  |
//! | root-owned file, `-p`         | 23, `failed to set permissions on "<dst>/f": ...` | push **0**, pull 23, silent |
//! | root-owned symlink, `-lt`     | 23, `failed to set times on "<dst>/l": ...`     | **0, silent**                |
//! | hard link under a file `sub`  | 23, `link "<dst>/sub/b" => a failed: Not a ...` | **11, fatal abort**          |
//! | empty dir in the way of link  | 0, the link replaces the directory              | **3, fatal abort**           |
//! | root-owned file, `--inplace`  | 23, `[receiver] chgrp "<dst>/f" failed: ...`    | 23, oc-only wording          |
//! | sender I/O error, `--delete`  | `IO error encountered -- skipping ...` notice   | **silent at `--info=nonreg0`** |
//!
//! Skipped (with a printed reason) as root, which ignores the denials. The
//! attribute cells also need `sudo -n` to plant the root-owned entry.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn running_as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

/// Runs `argv` through `sudo -n`, reporting whether it succeeded.
fn sudo(argv: &[&str]) -> bool {
    Command::new("sudo")
        .arg("-n")
        .args(argv)
        .status()
        .is_ok_and(|status| status.success())
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
    let mut perms = fs::symlink_metadata(path).expect("stat").permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms).expect("chmod");
}

fn backdate(path: &Path) {
    let status = Command::new("touch")
        .args(["-h", "-t", "202001010000"])
        .arg(path)
        .status()
        .expect("spawn touch");
    assert!(status.success(), "touch failed: {status}");
}

fn mkfifo(path: &Path) {
    let status = Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo failed: {status}");
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    Push,
    Pull,
    /// A local copy, for the cells whose generator code it shares.
    Local,
}

const DIRECTIONS: [Direction; 2] = [Direction::Push, Direction::Pull];

/// `src/` and `dst/` under a canonical temp root, so the rendered name matches
/// upstream's `getcwd()`-based `curr_dir` even behind a symlinked temp dir.
struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
        fs::create_dir_all(root.join("src")).expect("create src");
        fs::create_dir_all(root.join("dst")).expect("create dst");
        let shim = write_rsh_shim(&root);
        Self {
            _temp: temp,
            root,
            shim,
        }
    }

    fn src(&self, name: &str) -> PathBuf {
        self.root.join("src").join(name)
    }

    fn dst(&self, name: &str) -> PathBuf {
        self.root.join("dst").join(name)
    }

    /// Upstream's `full_fname()` rendering of a destination entry.
    fn full_fname(&self, name: &str) -> String {
        format!("\"{}\"", self.dst(name).display())
    }

    /// Transfers the named source entries into `dst/`. Naming the entries
    /// keeps `dst/` itself out of the file list, so neither implementation
    /// re-grants itself write permission on it.
    fn run(&self, flags: &[&str], names: &[&str], direction: Direction) -> (Option<i32>, String) {
        let (status, _stdout, stderr) = self.run_output(flags, names, direction);
        (status, stderr)
    }

    /// [`Self::run`], also returning the client's stdout, where a client
    /// prints `FINFO` notices.
    fn run_output(
        &self,
        flags: &[&str],
        names: &[&str],
        direction: Direction,
    ) -> (Option<i32>, String, String) {
        let binary = oc_rsync_binary();
        let dest = format!("{}/dst/", self.root.display());
        let mut runner = test_support::OcRsyncCliRunner::new()
            .binary(&binary)
            .args(flags)
            .arg("--rsh")
            .arg(&self.shim)
            .arg("--rsync-path")
            .arg(&binary);
        for name in names {
            let source = self.src(name).display().to_string();
            runner = runner.arg(match direction {
                Direction::Push | Direction::Local => source,
                Direction::Pull => format!("h:{source}"),
            });
        }
        runner = runner.arg(match direction {
            Direction::Push => format!("h:{dest}"),
            Direction::Pull | Direction::Local => dest,
        });
        let out = runner.run().expect("transfer did not finish");
        (
            out.status,
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let dst = self.root.join("dst");
        if fs::metadata(&dst).is_ok() {
            set_mode(&dst, 0o755);
        }
    }
}

fn assert_reported(
    direction: Direction,
    status: Option<i32>,
    stderr: &str,
    expected: &str,
    why: &str,
) {
    assert!(
        stderr.lines().any(|line| line == expected),
        "{direction:?}: {why}\nexpected line: {expected}\nstderr:\n{stderr}"
    );
    assert_eq!(
        status,
        Some(23),
        "{direction:?}: FERROR_XFER sets got_xfer_error, which lifts the exit to \
         RERR_PARTIAL (23)\nstderr:\n{stderr}"
    );
}

/// A hard link the receiver cannot create is reported by `hard_link_one()`,
/// naming the follower through `full_fname()` and the leader by its
/// transfer-relative name.
#[test]
fn failed_hard_link_is_reported_and_exits_23() {
    if running_as_root() {
        eprintln!("skip: root ignores the write-denied destination");
        return;
    }
    for direction in DIRECTIONS {
        let fx = Fixture::new();
        fs::write(fx.src("a"), b"data\n").expect("write src/a");
        fs::hard_link(fx.src("a"), fx.src("b")).expect("link src/b");
        backdate(&fx.src("a"));
        // The leader is already up to date, so only the follower's link runs.
        fs::copy(fx.src("a"), fx.dst("a")).expect("seed dst/a");
        backdate(&fx.dst("a"));
        set_mode(&fx.root.join("dst"), 0o555);

        let (status, stderr) = fx.run(&["-aH"], &["a", "b"], direction);

        assert!(
            fs::symlink_metadata(fx.dst("b")).is_err(),
            "the denied link must not land"
        );
        let expected = format!(
            "rsync: [generator] link {} => a failed: Permission denied (13)",
            fx.full_fname("b")
        );
        assert_reported(
            direction,
            status,
            &stderr,
            &expected,
            "a failed hard link must not be skipped silently",
        );
    }
}

/// `hard_link_one()` reports every failed `link()` the same way, whatever the
/// errno: `rsyserr(FERROR_XFER, ...)`, then on to the next file. No errno
/// makes the failure fatal.
///
/// `--no-implied-dirs` keeps `sub` out of the file list, so the regular file
/// planted at `dst/sub` stays and the follower's `link()` fails with `ENOTDIR`.
#[test]
fn hard_link_under_a_file_parent_is_reported_and_exits_23() {
    for direction in DIRECTIONS {
        let fx = Fixture::new();
        fs::create_dir(fx.src("sub")).expect("create src/sub");
        fs::write(fx.src("a"), b"data\n").expect("write src/a");
        fs::hard_link(fx.src("a"), fx.src("sub/b")).expect("link src/sub/b");
        backdate(&fx.src("a"));
        fs::copy(fx.src("a"), fx.dst("a")).expect("seed dst/a");
        backdate(&fx.dst("a"));
        fs::write(fx.dst("sub"), b"not a directory\n").expect("plant dst/sub");

        let (status, stderr) = fx.run(
            &["-aRH", "--no-implied-dirs"],
            &["./a", "./sub/b"],
            direction,
        );

        let expected = format!(
            "rsync: [generator] link {} => a failed: Not a directory (20)",
            fx.full_fname("sub/b")
        );
        assert_reported(
            direction,
            status,
            &stderr,
            &expected,
            "a link() failure other than EACCES is still a per-file error",
        );
    }
}

/// A directory where a hard-link follower goes is `dir_in_the_way`:
/// `atomic_create()` hands it to `delete_item()`, which removes an empty one
/// without `--delete` or `--force`, and the link is made.
///
/// upstream: generator.c:2469-2483 atomic_create(), delete.c:222-226
#[test]
fn empty_directory_in_the_way_of_a_hard_link_is_replaced() {
    use std::os::unix::fs::MetadataExt;
    for direction in DIRECTIONS {
        let fx = Fixture::new();
        fs::write(fx.src("a"), b"data\n").expect("write src/a");
        fs::hard_link(fx.src("a"), fx.src("b")).expect("link src/b");
        fs::create_dir(fx.dst("b")).expect("plant dst/b directory");

        let (status, stderr) = fx.run(&["-aH"], &["a", "b"], direction);

        assert_eq!(
            status,
            Some(0),
            "{direction:?}: the empty directory must give way\nstderr:\n{stderr}"
        );
        let leader = fs::metadata(fx.dst("a")).expect("stat dst/a");
        let follower = fs::symlink_metadata(fx.dst("b")).expect("stat dst/b");
        assert_eq!(
            (follower.dev(), follower.ino()),
            (leader.dev(), leader.ino()),
            "{direction:?}: dst/b must be a hard link to dst/a"
        );
    }
}

/// A populated directory where a hard-link follower goes is refused by
/// `delete_item()` without `--delete` or `--force`, and the follower is
/// skipped with exit 23.
///
/// DEVIATION: rsync 3.5.1 prints the same two lines and then hangs until it
/// is killed (measured on push and pull), so this cell has no upstream oracle.
/// The runner's wall-time limit is the hang guard: a hang fails the test.
///
/// upstream: delete.c:178-181 (FINFO), delete.c:273-286 (FERROR_XFER)
#[test]
fn populated_directory_in_the_way_of_a_hard_link_is_refused_without_hanging() {
    for direction in DIRECTIONS {
        let fx = Fixture::new();
        fs::write(fx.src("a"), b"data\n").expect("write src/a");
        fs::hard_link(fx.src("a"), fx.src("b")).expect("link src/b");
        fs::create_dir(fx.dst("b")).expect("plant dst/b directory");
        fs::write(fx.dst("b").join("x"), b"x\n").expect("populate dst/b");

        let (status, stdout, stderr) = fx.run_output(&["-aH"], &["a", "b"], direction);

        let output = format!("{stdout}{stderr}");
        for expected in [
            "cannot delete non-empty directory: b",
            "could not make way for new regular file: b",
        ] {
            assert!(
                output.lines().any(|line| line == expected),
                "{direction:?}: expected line: {expected}\noutput:\n{output}"
            );
        }
        assert_eq!(
            status,
            Some(23),
            "{direction:?}: the refused follower is a per-file error\noutput:\n{output}"
        );
        assert!(
            fx.dst("b").join("x").exists(),
            "{direction:?}: the populated directory must be left alone"
        );
    }
}

/// The obstacle deletion runs in the generator (`delete_item()` from
/// `recv_generator()`), so upstream tags it `[generator]`, never `[receiver]`.
#[test]
fn failed_obstacle_delete_is_tagged_generator() {
    if running_as_root() {
        eprintln!("skip: root ignores the write-denied destination");
        return;
    }
    for direction in DIRECTIONS {
        for (obstacle, what) in [("dir", "rmdir"), ("file", "unlink")] {
            let fx = Fixture::new();
            mkfifo(&fx.src("x"));
            if obstacle == "dir" {
                fs::create_dir(fx.dst("x")).expect("plant dir obstacle");
            } else {
                fs::write(fx.dst("x"), b"z\n").expect("plant file obstacle");
            }
            set_mode(&fx.root.join("dst"), 0o555);

            let (status, stderr) = fx.run(&["-aD"], &["x"], direction);

            let expected =
                format!("rsync: [generator] delete_file: {what}(x) failed: Permission denied (13)");
            assert_reported(
                direction,
                status,
                &stderr,
                &expected,
                "who_am_i() is the generator for an obstacle delete",
            );
        }
    }
}

/// Which destination entry a `set_file_attrs()` cell plants root-owned.
#[derive(Clone, Copy)]
enum Planted {
    Fifo,
    Dir,
    File,
    Symlink,
}

/// Plants `dst/<name>` owned by root:0 so the unprivileged receiver's chown,
/// utimes, or chmod on it fails with `EPERM`. Returns `false` without `sudo -n`.
fn plant_root_owned(fx: &Fixture, name: &str, kind: Planted) -> bool {
    let path = fx.dst(name);
    let path = path.to_str().expect("utf-8 temp path");
    let created = match kind {
        Planted::Fifo => sudo(&["mkfifo", "-m", "600", path]),
        Planted::Dir => sudo(&["mkdir", "-m", "755", path]),
        Planted::File => {
            let src = fx.src(name);
            sudo(&["cp", src.to_str().expect("utf-8 temp path"), path])
                && sudo(&["chmod", "600", path])
        }
        Planted::Symlink => sudo(&["ln", "-s", "tgt", path]),
    };
    created && sudo(&["chown", "-h", "0:0", path])
}

/// Every `set_file_attrs()` arm the generator runs on an existing entry reports
/// its failure in upstream's wording and exits 23, on push and pull alike.
#[test]
fn failed_set_file_attrs_is_reported_and_exits_23() {
    if running_as_root() {
        eprintln!("skip: root can change a root-owned entry's attributes");
        return;
    }
    if !sudo(&["true"]) {
        eprintln!("skip: planting a root-owned destination entry needs `sudo -n`");
        return;
    }
    let cells: [(Planted, &str, &[&str], &str); 4] = [
        (Planted::Fifo, "p", &["-Dt"], "failed to set times on {}"),
        (Planted::Dir, "d", &["-dg"], "chgrp {} failed"),
        (
            Planted::File,
            "f",
            &["-p", "--size-only"],
            "failed to set permissions on {}",
        ),
        (Planted::Symlink, "l", &["-lt"], "failed to set times on {}"),
    ];
    for direction in DIRECTIONS {
        for (kind, name, flags, template) in cells {
            let fx = Fixture::new();
            match kind {
                Planted::Fifo => mkfifo(&fx.src(name)),
                Planted::Dir => fs::create_dir(fx.src(name)).expect("create src dir"),
                Planted::File => {
                    fs::write(fx.src(name), b"hi\n").expect("write src file");
                    set_mode(&fx.src(name), 0o644);
                }
                Planted::Symlink => {
                    std::os::unix::fs::symlink("tgt", fx.src(name)).expect("plant src symlink");
                }
            }
            backdate(&fx.src(name));
            assert!(
                plant_root_owned(&fx, name, kind),
                "sudo could not plant dst/{name}"
            );

            let (status, stderr) = fx.run(flags, &[name], direction);

            let expected = format!(
                "rsync: [generator] {}: Operation not permitted (1)",
                template.replace("{}", &fx.full_fname(name))
            );
            assert_reported(
                direction,
                status,
                &stderr,
                &expected,
                "a failed set_file_attrs() step is rsyserr(FERROR_XFER), not a debug trace",
            );
        }
    }
}

/// The receiver applies attributes to the file it wrote through the same
/// `set_file_attrs()`, so a failure there has the same wording, tagged
/// `[receiver]`. `--inplace` writes through a root-owned 0666 file: the data
/// lands, then the `chgrp` fails with `EPERM`.
///
/// upstream: receiver.c:1340 finish_transfer() -> rsync.c:685-687
#[test]
fn failed_receiver_set_file_attrs_uses_upstream_wording() {
    if running_as_root() {
        eprintln!("skip: root can change a root-owned file's group");
        return;
    }
    if !sudo(&["true"]) {
        eprintln!("skip: planting a root-owned destination file needs `sudo -n`");
        return;
    }
    for direction in DIRECTIONS {
        let fx = Fixture::new();
        fs::write(fx.src("f"), b"newdata\n").expect("write src/f");
        set_mode(&fx.src("f"), 0o644);
        backdate(&fx.src("f"));
        let seed = fx.root.join("seed");
        fs::write(&seed, b"olddata\n").expect("write seed");
        let dst = fx.dst("f");
        let dst = dst.to_str().expect("utf-8 temp path");
        assert!(
            sudo(&["cp", seed.to_str().expect("utf-8 temp path"), dst])
                && sudo(&["chmod", "666", dst])
                && sudo(&["chown", "0:0", dst]),
            "sudo could not plant dst/f"
        );

        let (status, stderr) = fx.run(&["-a", "--inplace"], &["f"], direction);

        assert_eq!(
            fs::read(fx.dst("f")).expect("read dst/f"),
            b"newdata\n",
            "{direction:?}: the in-place data must land before the attribute step"
        );
        let expected = format!(
            "rsync: [receiver] chgrp {} failed: Operation not permitted (1)",
            fx.full_fname("f")
        );
        assert_reported(
            direction,
            status,
            &stderr,
            &expected,
            "the receiver's set_file_attrs() failure uses upstream's rsyserr() wording",
        );
    }
}

/// `delete_in_dir()` prints its notice with a plain `rprintf(FINFO, ...)` and
/// no `INFO_GTE()` gate, so `--info=nonreg0` keeps it and only `--quiet`
/// drops it (log.c:344-345). Measured on 3.5.1 for push, pull and a local
/// copy.
///
/// upstream: generator.c:304-311 delete_in_dir()
#[test]
fn io_error_delete_notice_is_a_plain_finfo() {
    if running_as_root() {
        eprintln!("skip: root reads a mode-000 directory, so no I/O error occurs");
        return;
    }
    const NOTICE: &str = "IO error encountered -- skipping file deletion";
    for direction in [Direction::Push, Direction::Pull, Direction::Local] {
        for (extra, printed) in [
            (None, true),
            (Some("--info=nonreg0"), true),
            (Some("-q"), false),
        ] {
            let fx = Fixture::new();
            fs::create_dir(fx.src("u")).expect("create src/u");
            fs::write(fx.src("k"), b"k\n").expect("write src/k");
            fs::write(fx.dst("extra"), b"e\n").expect("write dst/extra");
            set_mode(&fx.src("u"), 0o000);
            let mut flags = vec!["-a", "--delete"];
            flags.extend(extra);

            let (status, stdout, stderr) = fx.run_output(&flags, &[""], direction);
            set_mode(&fx.src("u"), 0o755);

            assert_eq!(
                stdout.lines().any(|line| line == NOTICE),
                printed,
                "{direction:?} {extra:?}: the notice is FINFO, gated only by --quiet\n\
                 stdout:\n{stdout}\nstderr:\n{stderr}"
            );
            assert_eq!(
                status,
                Some(23),
                "{direction:?} {extra:?}: the sender's I/O error exits 23\nstderr:\n{stderr}"
            );
        }
    }
}

/// A nested socket the platform cannot create race-safely is skipped with
/// upstream's `FWARNING`, which a server receiver frames as `MSG_WARNING`, so
/// the pushing client prints it too; the run still exits 0.
///
/// upstream: generator.c:2516-2519, log.c:rwrite() `am_server` framing
#[cfg(any(
    target_os = "ios",
    target_os = "macos",
    target_os = "tvos",
    target_os = "watchos"
))]
#[test]
fn nested_socket_skip_warning_reaches_the_pushing_client() {
    let fx = Fixture::new();
    let nest = fx.src("nest");
    fs::create_dir_all(&nest).expect("create src/nest");
    let _listener =
        std::os::unix::net::UnixListener::bind(nest.join("sock")).expect("bind source socket");

    let (status, stderr) = fx.run(&["-rD"], &["nest"], Direction::Push);

    let expected = format!(
        "skipping socket (creation unsupported here): {}",
        fx.full_fname("nest/sock")
    );
    assert!(
        stderr.lines().any(|line| line == expected),
        "the server receiver's FWARNING must reach the client\nexpected line: \
         {expected}\nstderr:\n{stderr}"
    );
    assert_eq!(
        status,
        Some(0),
        "a skipped socket is a warning and must not lift the exit\nstderr:\n{stderr}"
    );
}
