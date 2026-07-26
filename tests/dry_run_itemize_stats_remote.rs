//! `--dry-run` reporting fidelity over a remote shell, in both directions.
//!
//! Upstream suppresses only the *mutations* under `-n`, never the reporting:
//!
//! ```c
//! /* syscall.c:1010-1016 */
//! int do_mkdir(const char *path, mode_t mode) { if (dry_run) return 0; ... }
//! /* rsync.c:498-499 */
//! if (dry_run) return 1;                 /* set_file_attrs() */
//!
//! /* generator.c:1480-1483 - runs whether or not the mkdir happened */
//! if (itemizing && f_out != -1)
//!     itemize(fnamecmp, file, ndx, statret, &sx, statret ? ITEM_LOCAL_CHANGE : 0, 0, NULL);
//!
//! /* receiver.c:732-746 - and so does the created-file tally */
//! if (iflags & ITEM_IS_NEW) {
//!     stats.created_files++;
//!     ... else if (S_ISDIR(file->mode)) stats.created_dirs++; ...
//! }
//! ```
//!
//! So `-ni --stats` must print exactly the rows and counts a real run would
//! produce, while leaving the destination byte-for-byte untouched. Verified
//! against rsync 3.4.4 (protocol 32) on the same tree:
//!
//! ```text
//! $ rsync -a -n -i --stats src/ host:dest/
//! <f+++++++++ alpha.txt
//! <f+++++++++ beta.txt
//! cL+++++++++ link -> alpha.txt
//! cd+++++++++ sub/
//! <f+++++++++ sub/gamma.txt
//! Number of created files: 5 (reg: 3, dir: 1, link: 1)
//! Number of regular files transferred: 3
//! Total transferred file size: 40 bytes
//! ```
//!
//! WHY this lives at the binary level rather than in a receiver unit test: the
//! receiver has two drivers, `run_pipelined` and `run_pipelined_incremental`,
//! picked by the `incremental-flist` feature. A unit test compiles one of them;
//! this file compiles whichever the build selected, so the default build and
//! CI's `--all-features` build each assert the same expectations against a
//! different driver. Three separate bugs have already come from those two
//! ladders drifting apart, and every one of them was invisible to a test that
//! exercised only the shared helper.
//!
//! The "remote" is the oc-rsync binary itself behind a `--rsh` shim, so both
//! roles run for real: on a push the rows are computed by the *server*
//! receiver, put on the wire as iflags, and rendered by the client sender; on a
//! pull the client receiver renders them itself.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the shim uses `/bin/sh`).

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// The itemize rows rsync 3.4.4 prints for [`setup`]'s tree, in flist order.
///
/// `%c` is the direction glyph: `<` when the client is the sender (push), `>`
/// when it is the receiver (pull). The symlink and directory rows carry `c`
/// (locally created) in both directions.
fn expected_rows(direction: char) -> Vec<String> {
    vec![
        format!("{direction}f+++++++++ alpha.txt"),
        format!("{direction}f+++++++++ beta.txt"),
        "cL+++++++++ link -> alpha.txt".to_owned(),
        "cd+++++++++ sub/".to_owned(),
        format!("{direction}f+++++++++ sub/gamma.txt"),
    ]
}

/// Locates the binary under test.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read with
/// `env!`, not `env::var_os`: at run time it is unset and the lookup would fall
/// through to whatever stale `target/debug/oc-rsync` happens to be on disk -
/// silently testing a different build than the one just compiled.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    for profile in ["debug", "release", "dist"] {
        let candidate = PathBuf::from(manifest_dir)
            .join("target")
            .join(profile)
            .join("oc-rsync");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("oc-rsync")
}

/// Writes the `--rsh` shim.
///
/// oc-rsync invokes it with an SSH-style argv - `[<opts>..., <host>, <rsync
/// path>, "--server", ...]`. The shim drops the options and the host, then
/// execs the rest locally, giving a real two-process transfer over a pipe pair
/// without needing an SSH server.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    let body = "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         # $1 is the host placeholder; the server command follows it.\n\
         shift || true\n\
         exec \"$@\"\n";
    fs::write(&script, body).expect("write rsh shim");
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

fn spawn_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Drain both pipes on their own threads. Polling try_wait() while the pipes
    // fill would deadlock: the child blocks writing into a full OS pipe buffer
    // and therefore never exits. Draining also preserves the child's output so a
    // real failure is reported instead of being swallowed by the timeout.
    let mut child_stdout = child.stdout.take()?;
    let mut child_stderr = child.stderr.take()?;
    let stdout_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                return Some(Output {
                    status,
                    stdout: stdout_reader.join().unwrap_or_default(),
                    stderr: stderr_reader.join().unwrap_or_default(),
                });
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Every path under `dir`, relative and sorted - the destination fingerprint
/// that must stay empty across a dry run.
fn tree_paths(dir: &Path) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            }
            if let Ok(relative) = entry.path().strip_prefix(dir).map(Path::to_path_buf) {
                paths.insert(relative);
            }
        }
    }
    paths
}

/// Lays out the source tree and an empty destination: two new regular files, a
/// new subdirectory holding a third, and a new symlink - one of every entry
/// kind the "Number of created files" breakdown reports separately.
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let root = temp.path();
    let src = root.join("src");
    let dest = root.join("dest");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::write(src.join("alpha.txt"), b"alpha contents\n").unwrap();
    fs::write(src.join("beta.txt"), b"beta contents here\n").unwrap();
    fs::write(src.join("sub/gamma.txt"), b"gamma\n").unwrap();
    std::os::unix::fs::symlink("alpha.txt", src.join("link")).unwrap();
    (temp, src, dest)
}

/// Runs `-a -n -i --stats` over the shim in the requested direction.
fn run_dry_run(shim: &Path, binary: &Path, src: &Path, dest: &Path, push: bool) -> Output {
    let (from, to) = if push {
        (
            format!("{}/", src.display()),
            format!("dryhost:{}/", dest.display()),
        )
    } else {
        (
            format!("dryhost:{}/", src.display()),
            format!("{}/", dest.display()),
        )
    };
    let mut cmd = Command::new(binary);
    cmd.arg("-a")
        .arg("-n")
        .arg("-i")
        .arg("--stats")
        .arg("--rsh")
        .arg(shim)
        .arg("--rsync-path")
        .arg(binary)
        .arg(from)
        .arg(to);
    spawn_with_timeout(cmd, RUN_TIMEOUT).expect("dry run did not finish within the timeout")
}

/// Extracts the itemize rows: every line before the blank line that opens the
/// `--stats` block.
fn itemize_rows(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .take_while(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Returns the value part of a `--stats` line, e.g. `5 (reg: 3, dir: 1, link: 1)`.
fn stat_line<'a>(stdout: &'a str, prefix: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("missing {prefix:?} in --stats output:\n{stdout}"))
        .trim()
}

/// Asserts one direction end to end: the rows, the itemized counts, and an
/// untouched destination.
///
/// All three are asserted together on purpose. Rows without counts is the state
/// the non-incremental driver shipped in (`Number of created files: 0` beside a
/// full row set); counts without rows is what the incremental driver shipped
/// (directory rows only, no regular-file or symlink row); and either fix is
/// trivially "achievable" by letting the dry run create the destination, which
/// is the bug the previous fix removed. Only the three together pin the
/// behaviour to upstream's.
fn assert_dry_run_fidelity(push: bool) {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let (temp, src, dest) = setup();
    let shim = write_rsh_shim(temp.path());

    let output = run_dry_run(&shim, &binary, &src, &dest, push);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let label = if push { "push" } else { "pull" };

    assert!(
        output.status.success(),
        "{label} dry run must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        itemize_rows(&stdout),
        expected_rows(if push { '<' } else { '>' }),
        "{label} -ni rows must match rsync 3.4.4\nfull stdout:\n{stdout}"
    );

    // upstream: main.c:431 output_itemized_counts("Number of created files").
    // The dir sub-count is 1, not 2: the transfer root `.` already exists, so
    // only `sub/` is new.
    assert_eq!(
        stat_line(&stdout, "Number of created files:"),
        "5 (reg: 3, dir: 1, link: 1)",
        "{label} created-file breakdown\nfull stdout:\n{stdout}"
    );
    // upstream: receiver.c:781-784 / sender.c:341-343 - xferred_files and
    // total_transferred_size are summed before the `!do_xfers` continue.
    assert_eq!(
        stat_line(&stdout, "Number of regular files transferred:"),
        "3",
        "{label} transferred-file count\nfull stdout:\n{stdout}"
    );
    assert_eq!(
        stat_line(&stdout, "Total transferred file size:"),
        "40 bytes",
        "{label} transferred size\nfull stdout:\n{stdout}"
    );

    assert!(
        tree_paths(&dest).is_empty(),
        "{label} dry run must not touch the destination; found: {:?}",
        tree_paths(&dest)
    );
}

/// PUSH: the rows are produced by the remote *server* receiver and travel as
/// wire iflags, so a wrong iflags word is the only thing that can break them.
/// Sending a bare `ITEM_TRANSFER` rendered every new file as `<f.........` and
/// dropped the directory and symlink rows entirely, because the receiver only
/// ever requested regular files.
#[test]
fn dry_run_push_reports_rows_and_counts_like_upstream() {
    assert_dry_run_fidelity(true);
}

/// PULL: the rows are rendered locally by the client receiver, so this pins the
/// receiver's own reporting pass - the half that was silent on the incremental
/// driver and uncounted on the non-incremental one.
#[test]
fn dry_run_pull_reports_rows_and_counts_like_upstream() {
    assert_dry_run_fidelity(false);
}
