//! A `--progress` local copy must print its output in upstream's event order.
//!
//! upstream writes every line of a local copy at the moment it happens, on
//! stdout: the sender's `sending incremental file list` banner at the top of
//! `send_file_list()` (flist.c:2521-2524), the receiver's `created directory
//! <dest>` from `get_local_name()` (main.c:807-808), then each entry's name as
//! the generator reaches it (generator.c:recv_generator() -> itemize() ->
//! log_item()), with a regular file's progress block right after its name.
//! Captured from rsync 3.5.0 with stdout and stderr merged into one pipe:
//!
//! ```text
//! $ rsync -avPc zzz/ qqq 2>&1 | cat
//! sending incremental file list
//! created directory qqq
//! ./
//! top.txt
//!               2 100%    0.00kB/s    0:00:00 (xfr#1, to-chk=8/10)
//! bad/
//! bad/Album A/
//! bad/Album A/01 - t.opus
//! ...
//! ```
//!
//! oc renders a local copy's name list after the run, while `--progress`
//! lines are written live. Under `-P` the names and progress came first, the
//! banner and `created directory` trailed them, and the directory lines were
//! dropped entirely, because the post-run listing is suppressed once progress
//! has named the files.
//!
//! WHY this lives at the binary level: the defect is an interleaving between
//! the live progress writer and the post-run renderer. Only a real run with
//! both streams merged into one pipe observes the byte order a user sees.

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Normalized placeholder for one per-file progress block.
const PROGRESS: &str = "<progress>";

/// Reads `CARGO_BIN_EXE_oc-rsync` at compile time so the test always runs the
/// binary this build produced, never a stale one left on disk.
fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Lays out a nested source tree whose listing interleaves directory lines
/// with file lines: `zzz/top.txt`, two files in `bad/Album A/`, one in
/// `bad/Album B/`, and one in `good/`.
fn setup() -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let src = temp.path().join("zzz");
    fs::create_dir_all(src.join("bad/Album A")).unwrap();
    fs::create_dir_all(src.join("bad/Album B")).unwrap();
    fs::create_dir_all(src.join("good")).unwrap();
    fs::write(src.join("bad/Album A/01 - t.opus"), b"aaaa\n").unwrap();
    fs::write(src.join("bad/Album A/02 - u.opus"), b"bbbbbb\n").unwrap();
    fs::write(src.join("bad/Album B/01 - v.opus"), b"cc\n").unwrap();
    fs::write(src.join("good/x.txt"), b"d\n").unwrap();
    fs::write(src.join("top.txt"), b"e\n").unwrap();
    temp
}

/// Runs `<binary> <flags> zzz/ qqq` inside `workdir` with stderr merged into
/// stdout, as `2>&1 | cat` does, and returns the combined stream.
fn run_merged(binary: &Path, flags: &str, workdir: &Path) -> String {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exec \"$0\" \"$@\" 2>&1")
        .arg(binary)
        .arg(flags)
        .arg("zzz/")
        .arg("qqq")
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn transfer");
    // Drain the pipe on its own thread: polling try_wait() while the pipe
    // fills would deadlock once the child blocks on a full OS buffer.
    let mut stdout = child.stdout.take().expect("child stdout");
    let reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + RUN_TIMEOUT;
    let status = loop {
        match child.try_wait().expect("wait for transfer") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{flags}: transfer did not finish within {RUN_TIMEOUT:?}");
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };
    let output = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    assert!(
        status.success(),
        "{flags}: transfer failed: {status:?}\n{output}"
    );
    output
}

/// Whether `line` is a progress tick: `<size> <pct>% <rate> <time> ...`.
fn is_progress_tick(line: &str) -> bool {
    let mut fields = line.split_whitespace();
    let size_is_number = fields
        .next()
        .is_some_and(|f| f.chars().all(|c| c.is_ascii_digit() || c == ','));
    size_is_number && fields.next().is_some_and(|f| f.ends_with('%'))
}

/// Reduces a captured stream to its implementation-neutral line sequence.
///
/// Rates, byte counts and `to-chk` counters legitimately differ between the
/// two implementations, so each per-file progress block collapses to
/// [`PROGRESS`] (in-flight ticks without the `(xfr#...)` trailer are dropped),
/// and the numeric tails of the `sent` / `total size` trailer are cut.
fn normalize(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let last_tick = line.rsplit('\r').next().unwrap_or(line);
            if is_progress_tick(last_tick) {
                return last_tick.contains("(xfr#").then(|| PROGRESS.to_owned());
            }
            if line.starts_with("sent ") {
                return Some("sent <stats>".to_owned());
            }
            if let Some(rest) = line.strip_prefix("total size is ") {
                let size = rest.split_whitespace().next().unwrap_or_default();
                return Some(format!("total size is {size}"));
            }
            Some(line.to_owned())
        })
        .collect()
}

/// upstream 3.5.0's normalized output for the fixture, with a progress block
/// after every regular file when `progress` is set.
fn upstream_sequence(progress: bool) -> Vec<String> {
    let file = |name: &str| {
        let mut lines = vec![name.to_owned()];
        if progress {
            lines.push(PROGRESS.to_owned());
        }
        lines
    };
    let mut expected: Vec<String> = [
        "sending incremental file list",
        "created directory qqq",
        "./",
    ]
    .map(str::to_owned)
    .to_vec();
    expected.extend(file("top.txt"));
    expected.extend(["bad/".to_owned(), "bad/Album A/".to_owned()]);
    expected.extend(file("bad/Album A/01 - t.opus"));
    expected.extend(file("bad/Album A/02 - u.opus"));
    expected.push("bad/Album B/".to_owned());
    expected.extend(file("bad/Album B/01 - v.opus"));
    expected.push("good/".to_owned());
    expected.extend(file("good/x.txt"));
    expected.extend(["", "sent <stats>", "total size is 19"].map(str::to_owned));
    expected
}

fn assert_upstream_order(flags: &str, progress: bool) {
    let temp = setup();
    let output = run_merged(&oc_rsync_binary(), flags, temp.path());
    assert_eq!(
        normalize(&output),
        upstream_sequence(progress),
        "{flags}: upstream prints the banner, then `created directory`, then every \
         entry name - directories included - in the order the generator reaches it, \
         each file's progress right after its name\nraw output:\n{output}",
    );
}

/// The reporter's command: `-P` plus `-c`.
#[test]
fn progress_checksum_local_copy_prints_header_and_dirs_in_upstream_order() {
    assert_upstream_order("-avPc", true);
}

/// `-P` alone triggers both defects; `-c` plays no part.
#[test]
fn progress_local_copy_prints_header_and_dirs_in_upstream_order() {
    assert_upstream_order("-avP", true);
}

/// Without `-P` the post-run renderer owns the whole listing; pin it too so a
/// fix for the live path cannot regress the plain verbose one.
#[test]
fn verbose_local_copy_prints_header_and_dirs_in_upstream_order() {
    assert_upstream_order("-av", false);
    assert_upstream_order("-avc", false);
}

/// Locates an upstream rsync 3.5.0 to use as a live oracle: the
/// `OC_RSYNC_UPSTREAM_RSYNC` override, then the interop install and build
/// trees. Returns it only when its `--version` banner names 3.5.0.
fn locate_upstream_rsync() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Some(installed) = test_support::upstream_install_bin("3.5.0") {
        candidates.push(installed);
    }
    if let Some(root) = test_support::workspace_root() {
        candidates.push(root.join("target/interop/upstream-src/rsync-3.5.0/rsync"));
    }
    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|banner| banner.lines().next().map(str::to_owned))
            .is_some_and(|first| first.starts_with("rsync") && first.contains(" version 3.5.0"))
    })
}

/// When an upstream 3.5.0 binary is available, its own output for the same
/// fixture must match both the pinned sequence and oc's output.
#[test]
fn local_copy_output_order_matches_live_upstream_oracle() {
    let Some(upstream) = locate_upstream_rsync() else {
        eprintln!("skipping: no upstream rsync 3.5.0 found (set OC_RSYNC_UPSTREAM_RSYNC)");
        return;
    };
    for (flags, progress) in [("-avPc", true), ("-avP", true), ("-av", false)] {
        let upstream_temp = setup();
        let upstream_output = run_merged(&upstream, flags, upstream_temp.path());
        assert_eq!(
            normalize(&upstream_output),
            upstream_sequence(progress),
            "{flags}: the pinned sequence must be upstream's own output\n{upstream_output}",
        );
        let oc_temp = setup();
        let oc_output = run_merged(&oc_rsync_binary(), flags, oc_temp.path());
        assert_eq!(
            normalize(&oc_output),
            normalize(&upstream_output),
            "{flags}: oc must print the same line sequence as upstream\n\
             oc:\n{oc_output}\nupstream:\n{upstream_output}",
        );
    }
}
