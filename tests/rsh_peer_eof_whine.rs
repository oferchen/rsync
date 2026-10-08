//! A remote shell whose command exits before the protocol handshake ends the
//! stream with no bytes. Upstream reports that on two lines: the end-of-stream
//! whine, then the exit trailer naming the winning code.
//!
//! upstream: io.c:298-303 whine_about_eof() - `rprintf(FERROR, RSYNC_NAME ":
//! connection unexpectedly closed (%s bytes received so far) [%s]\n", ...)`
//! then `exit_cleanup(RERR_STREAMIO)`, whose log_exit() line (log.c:937-963)
//! names the worse of RERR_STREAMIO and the remote shell's exit status
//! (cleanup.c:150-152).
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A remote shell that drops the host and runs the joined remote command
/// through `sh -c`, as ssh does.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("rsh.sh");
    fs::write(&script, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

/// Runs a transfer against a remote command that reads the client greeting,
/// runs `then` and exits without speaking the protocol. Reading the greeting
/// first means the client always meets EOF on a read, never a failed write.
/// Returns (exit code, stderr lines).
fn run_against_silent_peer(then: &str, push: bool) -> (Option<i32>, Vec<String>) {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("f"), b"x").expect("write file");
    let rsh = write_rsh_shim(temp.path());
    let local = format!("{}/", src.display());
    let remote = format!("peer:{}/", temp.path().join("dst").display());
    let (from, to) = if push {
        (local.as_str(), remote.as_str())
    } else {
        (remote.as_str(), local.as_str())
    };
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("-a")
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path=head -c 4 >/dev/null; {then}"))
        .args([from, to])
        .output()
        .expect("run oc-rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| !line.starts_with("sh: "))
        .map(str::to_owned)
        .collect();
    (output.status.code(), lines)
}

/// The whine is its own line and the trailer carries only the exit code's
/// name; merging them hides the whine from tools that grep for it.
#[test]
fn push_to_a_peer_that_exits_prints_the_whine_then_the_trailer() {
    let (code, lines) = run_against_silent_peer("true", true);
    assert_eq!(code, Some(12), "{lines:#?}");
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(
        lines[0], "oc-rsync: connection unexpectedly closed (0 bytes received so far) [sender]",
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("oc-rsync error: error in rsync protocol data stream (code 12) at ")
            && lines[1].contains(" [sender="),
        "{lines:#?}"
    );
}

#[test]
fn pull_from_a_peer_that_exits_prints_the_whine_then_the_trailer() {
    let (code, lines) = run_against_silent_peer("true", false);
    assert_eq!(code, Some(12), "{lines:#?}");
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[0]
            .starts_with("oc-rsync: connection unexpectedly closed (0 bytes received so far) ["),
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("oc-rsync error: error in rsync protocol data stream (code 12) at "),
        "{lines:#?}"
    );
}

/// A remote shell status worse than RERR_STREAMIO wins the exit code, but the
/// whine is still printed first: upstream whines before exit_cleanup() picks
/// the code.
#[test]
fn a_worse_remote_shell_status_keeps_the_whine_and_names_its_own_code() {
    let (code, lines) = run_against_silent_peer("oc-rsync-no-such-command", true);
    assert_eq!(code, Some(127), "{lines:#?}");
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(
        lines[0], "oc-rsync: connection unexpectedly closed (0 bytes received so far) [sender]",
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("oc-rsync error: remote command not found (code 127) at ")
            && lines[1].contains(" [sender="),
        "{lines:#?}"
    );
}

/// A remote shell that runs the remote command but forwards only the first
/// `cut` bytes of its output, then ends the stream: the peer dies mid-transfer.
/// `dd bs=1` passes each byte straight through, so the cut lands exactly.
fn write_truncating_rsh_shim(dir: &Path, cut: usize) -> PathBuf {
    let script = dir.join(format!("rsh-cut-{cut}.sh"));
    let fifo = dir.join(format!("out-{cut}.fifo"));
    let body = format!(
        "#!/bin/sh\nshift\nmkfifo '{fifo}'\nsh -c \"$*\" <&0 > '{fifo}' &\nexec dd bs=1 count={cut} < '{fifo}' 2>/dev/null\n",
        fifo = fifo.display(),
    );
    fs::write(&script, body).expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

/// Pulls a tree whose server output is cut after `cut` bytes.
/// Returns (exit code, stderr lines).
fn pull_cut_after(cut: usize) -> (Option<i32>, Vec<String>) {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    let data: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    fs::write(src.join("big"), &data).expect("write file");
    let rsh = write_truncating_rsh_shim(temp.path(), cut);
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("-a")
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path={}", env!("CARGO_BIN_EXE_oc-rsync")))
        .arg(format!("peer:{}/", src.display()))
        .arg(format!("{}/", temp.path().join("dst").display()))
        .output()
        .expect("run oc-rsync");
    // The killed server's own stderr reaches the terminal too; only the
    // client's lines are under test.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| !line.contains("[server="))
        .map(str::to_owned)
        .collect();
    (output.status.code(), lines)
}

/// Parses N out of the whine's "(N bytes received so far)".
fn whine_count(line: &str) -> u64 {
    let prefix = "oc-rsync: connection unexpectedly closed (";
    let rest = line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("not a whine: {line}"));
    let end = rest
        .find(" bytes received so far) [receiver]")
        .expect("whine suffix");
    rest[..end].parse().expect("byte count")
}

/// A peer that dies mid-transfer gets the same two lines as one that never
/// spoke, and N counts every byte read after setup (io.c:938 stats.total_read).
/// Cutting the stream 40000 bytes later must report exactly 40000 more bytes;
/// a constant or a frame-local count cannot pass that.
#[test]
fn pull_cut_mid_transfer_whines_with_the_bytes_read_since_setup() {
    let mut counts = Vec::new();
    for cut in [10_000usize, 50_000] {
        let (code, lines) = pull_cut_after(cut);
        assert_eq!(code, Some(12), "{lines:#?}");
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(
            lines[1]
                .starts_with("oc-rsync error: error in rsync protocol data stream (code 12) at ")
                && lines[1].contains(" [receiver="),
            "{lines:#?}"
        );
        let count = whine_count(&lines[0]);
        assert!(
            count > 0 && count < cut as u64,
            "{count} vs cut {cut}: {lines:#?}"
        );
        counts.push(count);
    }
    assert_eq!(counts[1] - counts[0], 40_000, "{counts:?}");
}
