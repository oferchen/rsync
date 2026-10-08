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

/// Parses N out of the whine's "(N bytes received so far) [role]".
fn whine_count(line: &str, role: &str) -> u64 {
    let prefix = "oc-rsync: connection unexpectedly closed (";
    let rest = line
        .strip_prefix(prefix)
        .unwrap_or_else(|| panic!("not a whine: {line}"));
    let end = rest
        .find(&format!(" bytes received so far) [{role}]"))
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
        let count = whine_count(&lines[0], "receiver");
        assert!(
            count > 0 && count < cut as u64,
            "{count} vs cut {cut}: {lines:#?}"
        );
        counts.push(count);
    }
    assert_eq!(counts[1] - counts[0], 40_000, "{counts:?}");
}

/// Pushes a small tree to a server whose output is cut after `cut` bytes.
/// Returns (exit code, client stderr lines, server bytes actually forwarded).
fn push_cut_after(cut: usize) -> (Option<i32>, Vec<String>, u64) {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    for i in 0..3 {
        fs::write(src.join(format!("f{i}")), vec![b'x'; 1000 + i]).expect("write file");
    }
    let script = temp.path().join("rsh-count.sh");
    let fifo = temp.path().join("out.fifo");
    let tally = temp.path().join("dd.tally");
    let body = format!(
        "#!/bin/sh\nshift\nmkfifo '{fifo}'\nsh -c \"$*\" <&0 > '{fifo}' &\nexec dd bs=1 count={cut} < '{fifo}' 2>'{tally}'\n",
        fifo = fifo.display(),
        tally = tally.display(),
    );
    fs::write(&script, body).expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("-a")
        .arg("-e")
        .arg(&script)
        .arg(format!("--rsync-path={}", env!("CARGO_BIN_EXE_oc-rsync")))
        .arg(format!("{}/", src.display()))
        .arg(format!("peer:{}/", temp.path().join("dst").display()))
        .output()
        .expect("run oc-rsync");
    // dd reports "<N> bytes ... copied" on stderr once it stops.
    let tally = fs::read_to_string(&tally).expect("read dd tally");
    let forwarded = tally
        .lines()
        .find_map(|line| line.split_once(" bytes").map(|(n, _)| n.trim().to_owned()))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no byte count in dd tally: {tally}"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| {
            !line.contains("[server=")
                && !line.contains("[receiver=")
                && !line.contains("[generator=")
        })
        .map(str::to_owned)
        .collect();
    (output.status.code(), lines, forwarded)
}

/// A push whose server dies inside the goodbye exchange is not a clean run:
/// upstream's sender reads the closing NDX_DONEs through read_ndx_and_attrs()
/// and whines on EOF (main.c:921-948 read_final_goodbye, io.c:282-304), so
/// every cut short of the full server stream exits 12 with the whine, and the
/// count tracks the cut byte for byte.
#[test]
fn push_cut_inside_the_goodbye_whines_instead_of_exiting_clean() {
    let (code, lines, total) = push_cut_after(1 << 30);
    assert_eq!(code, Some(0), "uncut push must succeed: {lines:#?}");
    let mut counts = Vec::new();
    for cut in (total as usize).saturating_sub(16)..total as usize {
        let (code, lines, forwarded) = push_cut_after(cut);
        assert_eq!(
            forwarded, cut as u64,
            "shim forwarded {forwarded}, wanted {cut}"
        );
        assert_eq!(code, Some(12), "cut {cut} of {total}: {lines:#?}");
        assert_eq!(lines.len(), 2, "cut {cut} of {total}: {lines:#?}");
        assert!(
            lines[1]
                .starts_with("oc-rsync error: error in rsync protocol data stream (code 12) at ")
                && lines[1].contains(" [sender="),
            "cut {cut} of {total}: {lines:#?}"
        );
        counts.push((cut as u64, whine_count(&lines[0], "sender")));
    }
    let (first_cut, first_count) = counts[0];
    for (cut, count) in &counts {
        assert_eq!(count - first_count, cut - first_cut, "{counts:?}");
    }
}
