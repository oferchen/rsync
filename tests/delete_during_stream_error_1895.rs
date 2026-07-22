//! Interop test for `--delete-during` when the sender stream errors mid-transfer.
//!
//! Issue #1895. The receiver must:
//! - exit with a documented non-zero code (RERR_STREAMIO=12 or RERR_PROTOCOL=2),
//! - leave no `.oc-rsync-tmp` or other partial-write artefacts in the destination,
//! - not panic, hang, or self-terminate from a signal,
//! - not mis-report success when the upstream stream is truncated.
//!
//! Mechanism: a tiny POSIX shell shim acts as `--rsh`. It runs an upstream
//! `rsync --server --sender ...`, but passes that sender's stdout through
//! `dd bs=1 count=$BYTES` so the receiver sees a truncated wire stream. Once
//! the byte cap is hit the inner `dd` closes, the upstream sender gets SIGPIPE
//! on its next write, and the shim exits non-zero. The oc-rsync client must
//! then surface a clean failure rather than crashing or partially deleting.
//!
//! Upstream reference: `delete.c:delete_in_dir()` (deletion runs on the
//! generator side per directory) and `io.c:read_buf()` (raises
//! `error in rsync protocol data stream (code 12)` on premature EOF). The
//! generator's own `io_error` accumulator covers `RERR_PARTIAL` (23) when
//! file-level errors precede the stream death; we accept either family of
//! exit codes provided the receiver shuts down cleanly.
//!
//! Skip conditions (test exits cleanly with a printed reason):
//! - Not Unix (the shim uses `/bin/sh`, `dd`).
//! - No upstream `rsync` available (env `OC_RSYNC_UPSTREAM`, then
//!   `target/interop/upstream-install/3.4.1/bin/rsync`, then `which rsync`).
//! - `dd` not on `PATH`.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(60);
/// Byte cap for the inner `dd`. Large enough that the receiver can parse the
/// greeting and (often) start ingesting the file list, small enough that the
/// stream dies well before clean completion.
const STREAM_BYTE_CAP: u64 = 256;

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

/// Locate an upstream rsync binary, returning `None` if the test should skip.
fn locate_upstream_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("OC_RSYNC_UPSTREAM") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for version in ["3.4.2", "3.4.1", "3.1.3", "3.0.9"] {
        let in_tree = manifest_dir
            .join("target/interop/upstream-install")
            .join(version)
            .join("bin/rsync");
        if in_tree.is_file() {
            return Some(in_tree);
        }
    }
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v rsync 2>/dev/null")
        .output()
        .ok()?;
    if !which.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8(which.stdout).ok()?.trim());
    if path.is_file() { Some(path) } else { None }
}

fn dd_available() -> bool {
    Command::new("sh")
        .arg("-c")
        .arg("command -v dd >/dev/null 2>&1")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Write the `--rsh` shim. Receives the SSH-style argv
/// `[<host>, "rsync", "--server", "--sender", ...]` from oc-rsync, drops the
/// host, replaces the literal `rsync` token with the real upstream binary, and
/// runs the sender with its stdout piped through `dd bs=1 count=$BYTES`.
fn write_rsh_shim(dir: &Path, real_rsync: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    let body = format!(
        "#!/bin/sh\n\
         # Shim invoked by oc-rsync as the remote shell. Strip leading SSH-style\n\
         # options up to the host argument, then drop the literal 'rsync' token,\n\
         # then exec the real sender with a byte-capped stdout.\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         # $1 is the host placeholder; discard.\n\
         shift || true\n\
         # $1 is the literal 'rsync' command name; discard.\n\
         case \"${{1:-}}\" in\n\
         rsync|*/rsync) shift ;;\n\
         esac\n\
         '{rsync}' \"$@\" 2>/dev/null | dd bs=1 count={cap} 2>/dev/null\n\
         # SIGPIPE on the inner sender, or `dd` exit, surfaces as a non-zero\n\
         # pipeline status. Use 12 (RERR_STREAMIO) when neither side reported.\n\
         status=$?\n\
         if [ $status -eq 0 ]; then status=12; fi\n\
         exit $status\n",
        rsync = real_rsync.display(),
        cap = STREAM_BYTE_CAP,
    );
    fs::write(&script, body).expect("write fake rsh shim");
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
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().ok()? {
            Some(_) => return child.wait_with_output().ok(),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn count_dst_tmp_files(dst: &Path) -> usize {
    let mut count = 0usize;
    let mut stack: Vec<_> = match fs::read_dir(dst) {
        Ok(w) => w.flatten().collect(),
        Err(_) => return 0,
    };
    while let Some(entry) = stack.pop() {
        let name = entry.file_name();
        let s = name.to_string_lossy();
        // upstream get_tmpname() stages the in-flight temp as a hidden
        // `.<name>.XXXXXX` with a six-character mkstemp-style suffix.
        let is_get_tmpname = s.starts_with('.')
            && s.rsplit_once('.').is_some_and(|(stem, suffix)| {
                !stem.is_empty()
                    && suffix.len() == 6
                    && suffix.chars().all(|c| c.is_ascii_alphanumeric())
            });
        if s.starts_with(".oc-rsync-tmp")
            || s.starts_with(".~tmp~")
            || s.contains(".oc-rsync.")
            || is_get_tmpname
        {
            count += 1;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if let Ok(sub) = fs::read_dir(entry.path()) {
                stack.extend(sub.flatten());
            }
        }
    }
    count
}

#[test]
fn delete_during_survives_sender_stream_truncation() {
    let upstream = match locate_upstream_rsync() {
        Some(p) => p,
        None => {
            eprintln!(
                "skipping #1895 interop: no upstream rsync found \
                 (set OC_RSYNC_UPSTREAM or install rsync)"
            );
            return;
        }
    };
    if !dd_available() {
        eprintln!("skipping #1895 interop: `dd` not on PATH");
        return;
    }
    let oc_rsync = oc_rsync_binary();
    if !oc_rsync.is_file() {
        eprintln!("skipping #1895 interop: oc-rsync binary not built");
        return;
    }

    let tmp = tempfile::tempdir().expect("create tempdir");
    let root = tmp.path();
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();

    // Source: a handful of small files. Keep the file list bounded so a single
    // 256-byte sender window is guaranteed to hit truncation.
    for name in ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"] {
        fs::write(src.join(name), vec![b'x'; 4096]).unwrap();
    }
    // Destination: pre-seed extraneous files that --delete-during would target.
    let extras = [
        "extraneous-1.dat",
        "extraneous-2.dat",
        "extraneous-3.dat",
        "extraneous-4.dat",
    ];
    for name in extras {
        fs::write(dst.join(name), b"extra").unwrap();
    }

    let shim = write_rsh_shim(root, &upstream);

    // Pull: oc-rsync is the local client/receiver; the "remote" is the shim,
    // which truncates the sender's wire output mid-stream.
    let host_spec = format!("phantom-host:{}/", src.display());
    let mut cmd = Command::new(&oc_rsync);
    cmd.arg(format!("--rsh={}", shim.display()))
        .arg("-r")
        .arg("--delete-during")
        .arg("--timeout=20")
        .arg(host_spec)
        .arg(format!("{}/", dst.display()));

    let output = match spawn_with_timeout(cmd, RUN_TIMEOUT) {
        Some(o) => o,
        None => panic!(
            "#1895: oc-rsync did not exit within {RUN_TIMEOUT:?} - \
             a hung receiver is a regression"
        ),
    };

    // Receiver must exit non-zero. We accept the documented stream / partial
    // / startup family. A zero exit is a regression (false success). A signal
    // death is a regression (crash). Other codes are surfaced for triage.
    let code = output.status.code();
    let ok_codes = [
        2,  // RERR_PROTOCOL
        5,  // RERR_STARTCLIENT
        10, // RERR_SOCKETIO
        12, // RERR_STREAMIO - the expected primary path
        14, // RERR_IPC
        23, // RERR_PARTIAL
        24, // RERR_VANISHED
        30, // RERR_TIMEOUT
    ];
    match code {
        Some(0) => panic!(
            "#1895: oc-rsync exited 0 despite truncated sender stream. \
             stderr=\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
        Some(c) if ok_codes.contains(&c) => {}
        Some(c) => panic!(
            "#1895: oc-rsync exited with unexpected code {c}. \
             stderr=\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
        None => panic!(
            "#1895: oc-rsync was killed by a signal (no exit code). \
             stderr=\n{}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }

    // No partial-write artefacts must remain.
    let leftover = count_dst_tmp_files(&dst);
    assert_eq!(
        leftover, 0,
        "#1895: oc-rsync left {leftover} partial temp files in dst after \
         sender stream error - violates the no-half-state contract"
    );

    // Whatever subset of `extras` is still on disk must be intact (not
    // truncated to zero bytes). We do not require that the set is unchanged
    // because upstream rsync's contract is unspecified when the stream dies
    // partway through delete-during; we only require that surviving files
    // are not corrupted.
    for name in extras {
        let p = dst.join(name);
        if let Ok(meta) = fs::metadata(&p) {
            assert!(
                meta.len() > 0,
                "#1895: surviving extraneous file {name} was truncated to 0 bytes"
            );
        }
    }
}
