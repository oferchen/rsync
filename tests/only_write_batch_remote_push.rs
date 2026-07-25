//! Regression tests for `--only-write-batch` on a remote-shell PUSH.
//!
//! `--only-write-batch` is documented as "like --write-batch but w/o updating
//! destination". Upstream honours that on a remote push through two coupled
//! mechanisms:
//!
//! ```c
//! /* options.c:2850-2851, inside the am_sender block */
//! if (write_batch < 0)
//!     args[ac++] = "--only-write-batch=X";
//!
//! /* main.c:1839 - on the remote, the placeholder means write_batch < 0 */
//! if (write_batch < 0)
//!     dry_run = 1;
//!
//! /* sender.c:217 - the local sender records instead of sending */
//! int f_xfer = write_batch < 0 ? batch_fd : f_out;
//! ```
//!
//! The receiver therefore makes no destination updates (`receiver.c:811-817`
//! logs the item and continues) and reads no payload, which is only safe
//! because the sender diverted that payload into the batch file. Get one half
//! without the other and the push either writes the destination or stalls on an
//! unread token stream.
//!
//! The "remote" here is the oc-rsync binary itself, reached through a `--rsh`
//! shim, so the whole push path is exercised: server-argument construction, the
//! `--server` receiver's flag parsing, and the sender's stream routing.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the shim uses `/bin/sh`).

#![cfg(unix)]

use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

fn oc_rsync_binary() -> PathBuf {
    if let Some(env_path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return path;
        }
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
/// execs the rest locally, giving a real two-process push over a pipe pair
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

/// Counts regular files anywhere under `dir` (absent dir counts as zero).
fn count_files(dir: &Path) -> usize {
    let mut count = 0usize;
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else {
                count += 1;
            }
        }
    }
    count
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
    fs::write(src.join("sub/beta.txt"), vec![b'b'; 40_000]).unwrap();
    let batch = root.join(format!("{name}.batch"));
    (temp, src, dest, batch)
}

/// Runs a remote-shell push of `src/` into `dest/` with `batch_flag`.
fn run_push(shim: &Path, binary: &Path, batch_flag: &str, src: &Path, dest: &Path) -> Output {
    let mut cmd = Command::new(binary);
    cmd.arg("-a")
        .arg(batch_flag)
        .arg("--rsh")
        .arg(shim)
        .arg("--rsync-path")
        .arg(binary)
        .arg(format!("{}/", src.display()))
        .arg(format!("batchhost:{}/", dest.display()));
    spawn_with_timeout(cmd, RUN_TIMEOUT).expect("push did not finish within the timeout")
}

/// The headline bug: a `--only-write-batch` push updated the remote
/// destination. Upstream writes nothing there - `--only-write-batch=X` puts the
/// remote receiver into `dry_run` (options.c:2850, main.c:1839) and the sender
/// records the payload instead of transmitting it (sender.c:217).
///
/// Exit 0 is asserted separately from the file count because the two halves fail
/// differently: forwarding the placeholder without diverting the stream leaves
/// the remote refusing to read the tokens, which surfaces as exit 12.
#[test]
fn only_write_batch_push_leaves_the_destination_untouched() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let (temp, src, dest, batch) = setup("only");
    let shim = write_rsh_shim(temp.path());

    let output = run_push(
        &shim,
        &binary,
        &format!("--only-write-batch={}", batch.display()),
        &src,
        &dest,
    );

    assert!(
        output.status.success(),
        "--only-write-batch push must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        count_files(&dest),
        0,
        "--only-write-batch must not update the destination; found: {:?}",
        fs::read_dir(&dest)
            .map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
            .unwrap_or_default()
    );
    let recorded = fs::metadata(&batch).map(|m| m.len()).unwrap_or(0);
    assert!(
        recorded > 0,
        "a non-empty batch file must still be produced at {}",
        batch.display()
    );
}

/// The divert is scoped to `--only-write-batch`. Plain `--write-batch`
/// (upstream `write_batch > 0`) performs the transfer AND records it, so the
/// shared recorder path must keep teeing onto the wire.
#[test]
fn write_batch_push_still_transfers_and_records() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let (temp, src, dest, batch) = setup("plain");
    let shim = write_rsh_shim(temp.path());

    let output = run_push(
        &shim,
        &binary,
        &format!("--write-batch={}", batch.display()),
        &src,
        &dest,
    );

    assert!(
        output.status.success(),
        "--write-batch push must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        count_files(&dest),
        count_files(&src),
        "--write-batch must still transfer every source file"
    );
    assert_eq!(
        fs::read(dest.join("alpha.txt")).unwrap(),
        fs::read(src.join("alpha.txt")).unwrap(),
        "--write-batch destination content must match the source"
    );
    let recorded = fs::metadata(&batch).map(|m| m.len()).unwrap_or(0);
    assert!(
        recorded > 0,
        "--write-batch must record a non-empty batch at {}",
        batch.display()
    );
}
