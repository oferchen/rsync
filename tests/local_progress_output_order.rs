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
/// stdout, as `2>&1 | cat` does, and returns the combined stream. `flags` is
/// split on whitespace into separate arguments.
fn run_merged(binary: &Path, flags: &str, workdir: &Path) -> String {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exec \"$0\" \"$@\" 2>&1")
        .arg(binary)
        .args(flags.split_whitespace())
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

/// How upstream names each entry of the fixture.
#[derive(Clone, Copy)]
enum Listing {
    /// `-v` / `--info=name`: the bare name.
    Names,
    /// `-i`: the itemized line of a newly created entry.
    Itemized,
}

/// Which parts of upstream's output a command produces for the fixture.
#[derive(Clone, Copy)]
struct Expected {
    listing: Listing,
    /// A progress block after every regular file.
    progress: bool,
    /// The `sent` / `total size` trailer, which needs `-v` or `--stats`.
    trailer: bool,
}

/// upstream 3.5.0's normalized output for the fixture.
fn expected_sequence(expected: Expected) -> Vec<String> {
    let entry = |name: &str, dir: bool| match expected.listing {
        Listing::Names => name.to_owned(),
        Listing::Itemized if dir => format!("cd+++++++++ {name}"),
        Listing::Itemized => format!(">f+++++++++ {name}"),
    };
    let file = |name: &str| {
        let mut lines = vec![entry(name, false)];
        if expected.progress {
            lines.push(PROGRESS.to_owned());
        }
        lines
    };
    let mut lines: Vec<String> = [
        "sending incremental file list".to_owned(),
        "created directory qqq".to_owned(),
        entry("./", true),
    ]
    .to_vec();
    lines.extend(file("top.txt"));
    lines.extend([entry("bad/", true), entry("bad/Album A/", true)]);
    lines.extend(file("bad/Album A/01 - t.opus"));
    lines.extend(file("bad/Album A/02 - u.opus"));
    lines.push(entry("bad/Album B/", true));
    lines.extend(file("bad/Album B/01 - v.opus"));
    lines.push(entry("good/", true));
    lines.extend(file("good/x.txt"));
    if expected.trailer {
        lines.extend(["", "sent <stats>", "total size is 19"].map(str::to_owned));
    }
    lines
}

/// upstream's `-v` listing, with a progress block per file when `progress`.
fn upstream_sequence(progress: bool) -> Vec<String> {
    expected_sequence(Expected {
        listing: Listing::Names,
        progress,
        trailer: true,
    })
}

/// upstream's `-vP` output for the fixture: without `-r` the source directory
/// is skipped (flist.c:1484) and reported once.
fn skipped_directory_sequence() -> Vec<String> {
    [
        "skipping directory .",
        "",
        "sent <stats>",
        "total size is 0",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn assert_sequence(flags: &str, expected: &[String]) {
    let temp = setup();
    let output = run_merged(&oc_rsync_binary(), flags, temp.path());
    assert_eq!(
        normalize(&output),
        expected,
        "{flags}: upstream prints the banner, then `created directory`, then every \
         entry line - directories included - in the order the generator reaches it, \
         each file's progress right after its line\nraw output:\n{output}",
    );
}

fn assert_upstream_order(flags: &str, progress: bool) {
    assert_sequence(flags, &upstream_sequence(progress));
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

/// `-P` without `-v` still prints the banner and `created directory`: upstream
/// raises FLIST to 2 and an unset NAME to 1 for `--progress` (options.c:2511-2515),
/// and both lines are gated on those levels, not on `-v`.
#[test]
fn progress_without_verbose_prints_banner_and_created_directory() {
    let expected = expected_sequence(Expected {
        listing: Listing::Names,
        progress: true,
        trailer: false,
    });
    assert_sequence("-aP", &expected);
}

/// Under `-i` upstream logs each entry before its transfer
/// (options.c:2507 `log_before_transfer`), so every itemized line is written as
/// the entry is reached, with a file's progress right after it - not the whole
/// itemized list after the run.
#[test]
fn itemized_progress_interleaves_itemized_lines_with_progress() {
    let expected = expected_sequence(Expected {
        listing: Listing::Itemized,
        progress: true,
        trailer: false,
    });
    assert_sequence("-aiP", &expected);
}

/// `--info=progress2` is not `--progress`, but `-v` still sets NAME to 1, so
/// upstream names every entry and ends each file with the overall progress line.
#[test]
fn verbose_overall_progress_names_every_entry() {
    assert_upstream_order("-av --info=progress2", true);
}

/// A dry run moves no data, so upstream prints no progress block: under
/// `!do_xfers` the sender logs the entry and skips the transfer
/// (sender.c:638-642).
#[test]
fn dry_run_progress_prints_names_without_progress() {
    assert_upstream_order("-avPn", false);
}

/// The `--progress` forwarder previews the copy with a dry run before the real
/// one; a notice the preview produces must not reach the output a second time.
#[test]
fn progress_without_recursion_reports_skipped_directory_once() {
    assert_sequence("-vP", &skipped_directory_sequence());
}

/// Builds the fixture for the `is uptodate` ordering: `zzz/` holds `d1/f1`
/// and `same`, already copied to `qqq/`, plus a new symlink `link -> d1/f1`
/// and an `extra` file only in `qqq/` for `--delete`.
fn setup_uptodate(binary: &Path) -> tempfile::TempDir {
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let src = temp.path().join("zzz");
    fs::create_dir_all(src.join("d1")).unwrap();
    fs::write(src.join("d1/f1"), b"f1\n").unwrap();
    fs::write(src.join("same"), b"same\n").unwrap();
    run_merged(binary, "-a", temp.path());
    std::os::unix::fs::symlink("d1/f1", src.join("link")).unwrap();
    fs::write(temp.path().join("qqq/extra"), b"x\n").unwrap();
    temp
}

/// The generator writes `is uptodate` itself (generator.c:1305, rsync.c:828),
/// while a new symlink's line is itemized to the sender, which logs it when it
/// gets there. upstream therefore prints `same is uptodate` ahead of `link ->
/// d1/f1` although `link` sorts first. Where `d1/f1 is uptodate` lands against
/// the symlink line is a race in upstream; this is its usual order.
#[test]
fn uptodate_notice_precedes_the_symlink_entry_under_progress() {
    let binary = oc_rsync_binary();
    let temp = setup_uptodate(&binary);
    let output = run_merged(&binary, "-avvP --delete", temp.path());
    let expected: Vec<String> = [
        "sending incremental file list",
        "delta-transmission disabled for local transfer or --whole-file",
        "deleting extra",
        "same is uptodate",
        "d1/f1 is uptodate",
        "link -> d1/f1",
        "total: matches=0  hash_hits=0  false_alarms=0 data=0",
        "",
        "sent <stats>",
        "total size is 13",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(normalize(&output), expected, "raw output:\n{output}");
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
    let names = |progress, trailer| {
        expected_sequence(Expected {
            listing: Listing::Names,
            progress,
            trailer,
        })
    };
    let itemized = expected_sequence(Expected {
        listing: Listing::Itemized,
        progress: true,
        trailer: false,
    });
    let cells = [
        ("-avPc", upstream_sequence(true)),
        ("-avP", upstream_sequence(true)),
        ("-av", upstream_sequence(false)),
        ("-aP", names(true, false)),
        ("-aiP", itemized),
        ("-av --info=progress2", upstream_sequence(true)),
        ("-avPn", upstream_sequence(false)),
        ("-vP", skipped_directory_sequence()),
    ];
    for (flags, expected) in cells {
        let upstream_temp = setup();
        let upstream_output = run_merged(&upstream, flags, upstream_temp.path());
        assert_eq!(
            normalize(&upstream_output),
            expected,
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
    let temp = setup_uptodate(&upstream);
    let output = normalize(&run_merged(&upstream, "-avvP --delete", temp.path()));
    let position = |line: &str| {
        output
            .iter()
            .position(|l| l == line)
            .unwrap_or_else(|| panic!("upstream printed no `{line}`: {output:?}"))
    };
    assert!(
        position("same is uptodate") < position("link -> d1/f1"),
        "upstream writes `is uptodate` ahead of the itemized symlink: {output:?}",
    );
}
