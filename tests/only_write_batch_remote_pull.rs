//! Regression tests for `--only-write-batch` on a remote-shell PULL.
//!
//! A pull is not the mirror image of the push. Upstream never forwards the
//! option to the remote side on a pull - the placeholder is emitted inside the
//! `am_sender` block:
//!
//! ```c
//! /* options.c:2850-2851, inside `if (am_sender)` */
//! if (write_batch < 0)
//!     args[ac++] = "--only-write-batch=X";
//! ```
//!
//! so the remote sender is an ordinary one that streams sum head, delta tokens
//! and the whole-file checksum onto the wire. What makes the destination stay
//! untouched is the LOCAL receiver:
//!
//! ```c
//! /* main.c:1839 - do_xfers was already computed, so it stays 1 */
//! if (write_batch < 0)
//!     dry_run = 1;
//!
//! /* receiver.c:811-817 */
//! if (write_batch < 0) {
//!     log_item(FCLIENT, file, iflags, NULL);
//!     if (!am_server)
//!         discard_receive_data(f_in, file);
//!     ...
//!     continue;
//! }
//! ```
//!
//! `discard_receive_data()` (receiver.c:524-527) drains the delta the sender is
//! still writing. Going dry WITHOUT draining would desync the connection: the
//! next NDX read would parse delta bytes as a frame header. Writing the
//! destination anyway - what oc did before this fix - contradicts the
//! documented "like --write-batch but w/o updating destination".
//!
//! The "remote" here is the oc-rsync binary itself, reached through a `--rsh`
//! shim, so the whole pull path is exercised: server-argument construction, the
//! `--server --sender` process, and the local receiver's discard loop.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the shim uses `/bin/sh`).

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Locates the binary under test.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read with
/// `env!`, not `env::var_os`: at run time it is unset and the lookup would fall
/// through to whatever stale `target/debug/oc-rsync` happens to be on disk -
/// silently testing a different build than the one just compiled.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    let mode = fs::metadata(&built)
        .expect("stat oc-rsync binary")
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "oc-rsync at {} is not executable (mode {mode:o})",
        built.display()
    );
    built
}

/// Writes the `--rsh` shim.
///
/// oc-rsync invokes it with an SSH-style argv - `[<opts>..., <host>, <rsync
/// path>, "--server", "--sender", ...]`. The shim drops the options and the
/// host, then execs the rest locally, giving a real two-process pull over a
/// pipe pair without needing an SSH server.
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

/// Maps every regular file under `dir` to its bytes, keyed by relative path.
fn file_tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut tree = BTreeMap::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file()
                && let Ok(relative) = entry.path().strip_prefix(dir).map(Path::to_path_buf)
                && let Ok(bytes) = fs::read(entry.path())
            {
                tree.insert(relative, bytes);
            }
        }
    }
    tree
}

/// Lays out a source tree and returns `(tempdir, src, dest, batch)`.
fn setup(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let root = temp.path();
    let src = root.join("src");
    let dest = root.join("dest");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::write(src.join("alpha.txt"), b"alpha payload for the batch\n").unwrap();
    // Larger than one token chunk so the discard loop spans several literal
    // tokens rather than a single short read.
    fs::write(src.join("sub/beta.txt"), vec![b'b'; 40_000]).unwrap();
    let batch = root.join(format!("{name}.batch"));
    (temp, src, dest, batch)
}

/// Runs a remote-shell pull of `src/` into `dest/` with `batch_flag`.
fn run_pull(shim: &Path, binary: &Path, batch_flag: &str, src: &Path, dest: &Path) -> Output {
    let mut cmd = Command::new(binary);
    cmd.arg("-a")
        .arg(batch_flag)
        .arg("--rsh")
        .arg(shim)
        .arg("--rsync-path")
        .arg(binary)
        .arg(format!("batchhost:{}/", src.display()))
        .arg(format!("{}/", dest.display()));
    spawn_with_timeout(cmd, RUN_TIMEOUT).expect("pull did not finish within the timeout")
}

/// The headline bug: a `--only-write-batch` PULL updated the local destination.
/// Upstream writes nothing there - `main.c:1839` puts the local receiver into
/// `dry_run` and `receiver.c:813-814` drains the sender's stream with
/// `discard_receive_data()` instead of applying it.
///
/// Exit 0 is asserted separately from the file count because the two halves fail
/// differently: going dry without draining leaves the sender streaming tokens
/// nobody reads, which surfaces as a protocol desync (exit 12) or a hang, not as
/// a stray destination file.
#[test]
fn only_write_batch_pull_leaves_the_destination_untouched() {
    let binary = oc_rsync_binary();
    let (temp, src, dest, batch) = setup("only");
    let shim = write_rsh_shim(temp.path());

    let output = run_pull(
        &shim,
        &binary,
        &format!("--only-write-batch={}", batch.display()),
        &src,
        &dest,
    );

    assert!(
        output.status.success(),
        "--only-write-batch pull must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        file_tree(&dest).is_empty(),
        "--only-write-batch must not update the destination; found: {:?}",
        file_tree(&dest).keys().collect::<Vec<_>>()
    );
    let recorded = fs::metadata(&batch).map(|m| m.len()).unwrap_or(0);
    assert!(
        recorded > 0,
        "a non-empty batch file must still be produced at {}",
        batch.display()
    );

    // The point of recording is replay: the drained stream still had to reach
    // the batch file in full, so `--read-batch` must reconstruct the source.
    let replay_dest = temp.path().join("replay");
    fs::create_dir_all(&replay_dest).unwrap();
    let mut replay = Command::new(&binary);
    replay
        .arg("-a")
        .arg(format!("--read-batch={}", batch.display()))
        .arg(format!("{}/", replay_dest.display()));
    let replay_output =
        spawn_with_timeout(replay, RUN_TIMEOUT).expect("read-batch did not finish within timeout");
    assert!(
        replay_output.status.success(),
        "--read-batch replay must exit 0, got {:?}\nstderr: {}",
        replay_output.status.code(),
        String::from_utf8_lossy(&replay_output.stderr)
    );
    assert_eq!(
        file_tree(&replay_dest),
        file_tree(&src),
        "the batch recorded by a --only-write-batch pull must replay to the source tree"
    );
}

/// The dry receive path is scoped to `--only-write-batch`. Plain
/// `--write-batch` (upstream `write_batch > 0`) performs the transfer AND
/// records it, so the pull receiver must keep applying every delta it drains.
#[test]
fn write_batch_pull_still_transfers_and_records() {
    let binary = oc_rsync_binary();
    let (temp, src, dest, batch) = setup("plain");
    let shim = write_rsh_shim(temp.path());

    let output = run_pull(
        &shim,
        &binary,
        &format!("--write-batch={}", batch.display()),
        &src,
        &dest,
    );

    assert!(
        output.status.success(),
        "--write-batch pull must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        file_tree(&dest),
        file_tree(&src),
        "--write-batch must still transfer every source file"
    );
    let recorded = fs::metadata(&batch).map(|m| m.len()).unwrap_or(0);
    assert!(
        recorded > 0,
        "--write-batch must record a non-empty batch at {}",
        batch.display()
    );
}
