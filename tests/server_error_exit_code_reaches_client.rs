//! A server-side fatal error must reach the client as the server's own
//! `RERR_*` code, carried by `MSG_ERROR_EXIT`.
//!
//! Upstream's `_exit_cleanup()` writes the code it is about to exit with onto
//! the multiplexed stream before the process ends (cleanup.c:242-258), and the
//! peer's `read_a_msg()` turns receipt of that frame into the NORETURN
//! `_exit_cleanup(val, __FILE__, 0 - __LINE__)` (io.c:1854-1892). The client
//! therefore exits with the *server's* code, not with whatever the connection
//! dropping happened to look like locally.
//!
//! oc had neither half on the remote-shell path: the `--server` entry point
//! collapsed every transfer failure to a flat `1` and sent no frame, so an
//! oc client saw only EOF and reported `RERR_STREAMIO` (12).
//!
//! Measured against the pinned upstream rsync 3.5.0 over a remote shell, for a
//! push whose server receiver cannot create the destination root:
//!
//! | client | server | protocol | client-observed exit |
//! |--------|--------|----------|----------------------|
//! | rsync  | rsync  | 32       | 3                    |
//! | rsync  | rsync  | 30       | 12                   |
//!
//! The protocol split is upstream's gate. `cleanup.c:244` reads
//! `protocol_version >= 31 || am_receiver`, but `am_receiver` selects the forked
//! receiver child's *sibling* message channel, not the socket; a single-process
//! port must gate on the protocol version alone, which is exactly what the two
//! rows above show on the wire.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Locates the binary under test. `CARGO_BIN_EXE_oc-rsync` is a compile-time
/// variable, so it must be read with `env!`, never `env::var_os`: at run time it
/// is unset and the lookup would fall through to a stale `target/debug` build.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Effective-UID probe used as a skip gate. Shells out to `id -u` rather than
/// linking `libc::geteuid` so the test stays free of FFI. Root ignores the
/// directory mode the fixture relies on, so the fatal path would never fire.
fn is_root() -> bool {
    match Command::new("id").arg("-u").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u32>()
            .map(|uid| uid == 0)
            .unwrap_or(false),
        _ => false,
    }
}

/// Writes an executable `--rsh` shim that drops the host operand and runs
/// oc-rsync as the remote `--server`, giving a real two-process transfer with no
/// sshd.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let path = dir.join("rsh.sh");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nshift\nexec {} \"$@\"\n",
            oc_rsync_binary().display()
        ),
    )
    .expect("write rsh shim");
    let mut perms = fs::metadata(&path).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod shim");
    path
}

fn run_with_timeout(mut command: Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync");
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("oc-rsync did not exit within {RUN_TIMEOUT:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    child.wait_with_output().expect("collect output")
}

/// Restores the fixture's directory mode however the test leaves.
struct ModeGuard(PathBuf);

impl Drop for ModeGuard {
    fn drop(&mut self) {
        if let Ok(meta) = fs::metadata(&self.0) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&self.0, perms);
        }
    }
}

/// Pushes into `<dst>/sub/` where `<dst>` is not writable, so the *server*
/// receiver fails to create the destination root. Returns the client's exit
/// code and its combined output.
fn push_into_unwritable_parent(extra: &[&str]) -> (i32, String) {
    let scratch = TempDir::new().expect("tempdir");
    let src = scratch.path().join("src");
    let dst = scratch.path().join("dst");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    fs::write(src.join("f.txt"), b"payload").expect("write source file");

    let shim = write_rsh_shim(scratch.path());

    let guard = ModeGuard(dst.clone());
    let mut perms = fs::metadata(&dst).expect("stat dst").permissions();
    perms.set_mode(0o555);
    fs::set_permissions(&dst, perms).expect("chmod dst");

    let mut command = Command::new(oc_rsync_binary());
    command.arg("-e").arg(&shim);
    command.args(extra);
    command.arg("-r");
    command.arg(format!("{}/", src.display()));
    command.arg(format!("host:{}/sub/", dst.display()));

    let output = run_with_timeout(command);
    drop(guard);

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), text)
}

/// The regression. At the negotiated default protocol the server receiver's
/// `RERR_FILESELECT` must arrive as the client's own exit code.
///
/// Before the fix the server exited a flat `1` with no `MSG_ERROR_EXIT`, so the
/// client only saw the stream end and reported `RERR_STREAMIO` (12).
///
/// upstream: cleanup.c:250 `send_msg_int(MSG_ERROR_EXIT, exit_code)`;
/// io.c:1892 `_exit_cleanup(val, __FILE__, 0 - __LINE__)`.
#[test]
fn server_fatal_exit_code_reaches_the_client() {
    if is_root() {
        eprintln!("skip: running as root, the unwritable-destination path cannot fire");
        return;
    }

    let (code, text) = push_into_unwritable_parent(&[]);
    assert_eq!(
        code, 3,
        "the client must exit with the server's RERR_FILESELECT, not a \
         locally-inferred code; output was:\n{text}"
    );
}

/// The gate's companion: below protocol 31 upstream sends nothing, and the
/// client is left inferring `RERR_STREAMIO` (12) from the closed stream. This
/// row is green both before and after the fix, so a blanket "always propagate"
/// change would redden it.
///
/// upstream: cleanup.c:244 `protocol_version >= 31`.
#[test]
fn protocol_30_keeps_the_stream_io_code() {
    if is_root() {
        eprintln!("skip: running as root, the unwritable-destination path cannot fire");
        return;
    }

    let (code, text) = push_into_unwritable_parent(&["--protocol=30"]);
    assert_eq!(
        code, 12,
        "protocol 30 is below upstream's MSG_ERROR_EXIT gate, so the client \
         must still report RERR_STREAMIO; output was:\n{text}"
    );
}

/// A transfer that succeeds keeps exiting 0 over the same remote-shell path, so
/// the new producer cannot be firing on the happy path.
#[test]
fn a_successful_remote_shell_push_still_exits_zero() {
    let scratch = TempDir::new().expect("tempdir");
    let src = scratch.path().join("src");
    let dst = scratch.path().join("dst");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    fs::write(src.join("f.txt"), b"payload").expect("write source file");

    let shim = write_rsh_shim(scratch.path());

    let mut command = Command::new(oc_rsync_binary());
    command.arg("-e").arg(&shim);
    command.arg("-r");
    command.arg(format!("{}/", src.display()));
    command.arg(format!("host:{}/", dst.display()));

    let output = run_with_timeout(command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a clean push must still exit 0; stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dst.join("f.txt")).expect("destination file"),
        b"payload",
        "the payload must land"
    );
}
