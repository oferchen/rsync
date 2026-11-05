//! Oracle cells for a local copy's `Total bytes sent/received`.
//!
//! Upstream prints a local copy's totals from the socketpair between its two
//! forked halves (main.c:334-335 handle_stats), and oc-rsync replays that
//! stream through a wire model. Each cell builds a single-directory fixture
//! below one 64 KiB buffer, where 3.5.1's totals are stable from run to run,
//! and asserts the totals exactly.
//!
//! Every fixture pins names, sizes, contents, modes and mtimes, and the cells
//! run with `-rt` so no owner names or ids reach the file list: the pinned
//! totals recorded from rsync 3.5.1 then hold on any machine and filesystem.
//! When an rsync 3.5.1 oracle resolves (`OC_RSYNC_UPSTREAM_RSYNC`, the interop
//! install, the upstream build tree, then `/usr/bin/rsync`, each banner
//! checked), every cell also runs it on the same fixture and compares live.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::{FileTime, set_file_mtime};

const ORACLE_VERSION: &str = "3.5.1";
/// Every fixture entry's mtime: 2023-11-14 22:13:20 UTC, nanoseconds zero.
const MTIME: i64 = 1_700_000_000;

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Resolves an rsync whose `--version` banner names [`ORACLE_VERSION`].
fn upstream_rsync() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    if let Some(installed) = test_support::upstream_install_bin(ORACLE_VERSION) {
        candidates.push(installed);
    }
    if let Some(root) = test_support::workspace_root() {
        candidates.push(root.join(format!(
            "target/interop/upstream-src/rsync-{ORACLE_VERSION}/rsync"
        )));
    }
    candidates.push(PathBuf::from("/usr/bin/rsync"));
    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|banner| banner.lines().next().map(str::to_owned))
            // The release banner reads `version 3.5.1  protocol`; a git
            // build (`3.5.1-g<hash>`) is a different program.
            .is_some_and(|first| {
                first.starts_with("rsync") && first.contains(&format!(" version {ORACLE_VERSION} "))
            })
    })
}

/// Deterministic, incompressible-looking content.
fn content(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn write_pinned(path: &Path, data: &[u8]) {
    fs::write(path, data).expect("write fixture file");
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).expect("chmod file");
    set_file_mtime(path, FileTime::from_unix_time(MTIME, 0)).expect("pin file mtime");
}

fn pin_dir(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod dir");
    set_file_mtime(path, FileTime::from_unix_time(MTIME, 0)).expect("pin dir mtime");
}

/// One cell's fixture: a source tree and an optional destination state.
#[derive(Clone, Copy)]
enum Fixture {
    /// One 5000-byte file into a fresh destination.
    WholeFile,
    /// Twenty files whose names share no prefix, so the file list costs the
    /// same in any `readdir()` order.
    ManyFiles,
    /// A destination already identical to the source.
    UpToDate,
    /// A 50000-byte file whose destination differs in ten bytes.
    Delta,
    /// An up-to-date destination with an extraneous file and an extraneous
    /// empty directory.
    Extraneous,
    /// Names sharing long prefixes, so the file list's size depends on the
    /// order `readdir()` returns them in.
    SharedPrefixes,
    /// Two files with user xattrs: a short value sent whole in the file list,
    /// and a 64-byte value sent abbreviated and then requested in full.
    Xattrs,
}

impl Fixture {
    fn build(self, root: &Path) -> (PathBuf, PathBuf) {
        let src = root.join("src");
        let dst = root.join("dst");
        fs::create_dir(&src).expect("mkdir src");
        match self {
            Self::WholeFile | Self::UpToDate | Self::Extraneous => {
                write_pinned(&src.join("alpha"), &content(1, 5000));
            }
            Self::ManyFiles => {
                for (i, name) in ('a'..='t').enumerate() {
                    write_pinned(&src.join(name.to_string()), &content(i as u64, 50 + i));
                }
            }
            Self::Delta => {
                write_pinned(&src.join("alpha"), &content(1, 50_000));
            }
            Self::Xattrs => {
                write_pinned(&src.join("alpha"), &content(1, 100));
                write_pinned(&src.join("beta"), &content(2, 100));
            }
            Self::SharedPrefixes => {
                for i in 0..40 {
                    write_pinned(&src.join(format!("shared_prefix_{i:02}")), &content(i, 10));
                }
            }
        }
        pin_dir(&src);
        match self {
            Self::WholeFile | Self::ManyFiles | Self::SharedPrefixes | Self::Xattrs => {}
            Self::UpToDate | Self::Extraneous => {
                fs::create_dir(&dst).expect("mkdir dst");
                write_pinned(&dst.join("alpha"), &content(1, 5000));
                if matches!(self, Self::Extraneous) {
                    write_pinned(&dst.join("x1"), b"stale one");
                    fs::create_dir(dst.join("x2")).expect("mkdir stale dir");
                    pin_dir(&dst.join("x2"));
                }
                pin_dir(&dst);
            }
            Self::Delta => {
                fs::create_dir(&dst).expect("mkdir dst");
                let mut basis = content(1, 50_000);
                basis[20_000..20_010].copy_from_slice(b"XXXXXXXXXX");
                fs::write(dst.join("alpha"), &basis).expect("write basis");
                set_file_mtime(dst.join("alpha"), FileTime::from_unix_time(MTIME - 3600, 0))
                    .expect("backdate basis");
                pin_dir(&dst);
            }
        }
        (src, dst)
    }
}

/// Asserts a cell whose sent total 3.5.1 itself splits across a varying
/// number of frames: the received total is exact, and the sent total must be
/// one of the values 3.5.1 printed for the same fixture.
fn check_sent_within(fixture: Fixture, args: &[&str], sent: &[u64], received: u64) {
    let (oc_sent, oc_received) = totals(&oc_rsync(), fixture, args);
    assert_eq!(
        oc_received, received,
        "received vs the rsync {ORACLE_VERSION} recording for {args:?}"
    );
    assert!(
        sent.contains(&oc_sent),
        "sent {oc_sent} is not one of rsync {ORACLE_VERSION}'s {sent:?} for {args:?}"
    );
    if let Some(upstream) = upstream_rsync() {
        let (up_sent, up_received) = totals(&upstream, fixture, args);
        assert_eq!(
            oc_received, up_received,
            "received vs live rsync {ORACLE_VERSION} for {args:?}"
        );
        assert!(
            sent.contains(&up_sent),
            "live rsync {ORACLE_VERSION} sent {up_sent}, outside {sent:?}"
        );
    }
}

/// `(sent, received)` from a `--stats` run, and the destination it used.
fn totals(binary: &Path, fixture: Fixture, args: &[&str]) -> (u64, u64) {
    totals_with_destination(binary, fixture, args).0
}

fn totals_with_destination(
    binary: &Path,
    fixture: Fixture,
    args: &[&str],
) -> ((u64, u64), PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (src, dst) = fixture.build(dir.path());
    (run_totals(binary, &src, &dst, args), dst)
}

/// `(sent, received)` from a `--stats` run of `binary` from `src/` to `dst`.
fn run_totals(binary: &Path, src: &Path, dst: &Path, args: &[&str]) -> (u64, u64) {
    let mut src_arg = src.as_os_str().to_owned();
    src_arg.push("/");
    let output = Command::new(binary)
        .args(args)
        .arg("--stats")
        .arg(&src_arg)
        .arg(dst)
        .output()
        .expect("run rsync");
    assert!(
        output.status.success(),
        "{} failed: {}",
        binary.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stats");
    let field = |label: &str| -> u64 {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(label))
            .map(|value| value.trim().replace(',', ""))
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no {label:?} line in:\n{stdout}"))
    };
    (field("Total bytes sent:"), field("Total bytes received:"))
}

/// Asserts oc's totals for a cell against the 3.5.1 recording and, when an
/// oracle is installed, against a live 3.5.1 run of the same fixture.
fn check(fixture: Fixture, args: &[&str], pinned: (u64, u64)) {
    let oc = totals(&oc_rsync(), fixture, args);
    if let Some(upstream) = upstream_rsync() {
        let live = totals(&upstream, fixture, args);
        assert_eq!(oc, live, "oc vs live rsync {ORACLE_VERSION} for {args:?}");
    }
    assert_eq!(
        oc, pinned,
        "oc vs the rsync {ORACLE_VERSION} recording for {args:?}"
    );
}

/// Whole-file copy: the file list, the `.` item and its echo, one transfer
/// (sum head, one literal chunk, end token, xxh128 whole-file sum) and the
/// NDX_DONE tail.
#[test]
fn whole_file_copy_totals_match_upstream() {
    check(Fixture::WholeFile, &["-rt"], (5_098, 38));
}

/// Twenty transfers in one round each way: per-file NDX, iflags and sum head
/// in both directions.
#[test]
fn many_file_copy_totals_match_upstream() {
    check(Fixture::ManyFiles, &["-rt"], (2_234, 399));
}

/// Nothing to transfer: the file list and the NDX_DONE tail alone.
#[test]
fn up_to_date_totals_match_upstream() {
    check(Fixture::UpToDate, &["-rt"], (48, 12));
}

/// A delta transfer: 72 block sums of 4 + 2 bytes in the request, 71
/// block-match tokens and one 700-byte literal in the reply.
#[test]
fn delta_transfer_totals_match_upstream() {
    check(
        Fixture::Delta,
        &["-rt", "--no-whole-file", "-I"],
        (1_079, 467),
    );
}

/// `--dry-run`: requests and replies shrink to NDX and iflags.
#[test]
fn dry_run_totals_match_upstream() {
    check(Fixture::WholeFile, &["-rtn"], (58, 22));
}

/// `--delete`: each deleted name arrives as `MSG_DELETED` (the directory's
/// with a trailing NUL, log.c:913-916), and the delete statistics ride in
/// the phase tail.
///
/// Recorded from rsync 3.5.1 (the release tarball build, protocol 33) on this
/// fixture, five runs, all identical:
/// `rsync -rt --delete --stats src/ dst` printed
/// `Total bytes sent: 48` and `Total bytes received: 32`.
#[test]
fn delete_totals_match_upstream() {
    check(Fixture::Extraneous, &["-rt", "--delete"], (48, 32));
}

/// `--delete-after` holds the last NDX_DONE back until after the delete
/// pass, so the deletions land outside the counted window.
///
/// Recorded from rsync 3.5.1 (the release tarball build, protocol 33) on this
/// fixture, five runs, all identical:
/// `rsync -rt --delete-after --stats src/ dst` printed
/// `Total bytes sent: 46` and `Total bytes received: 11`.
#[test]
fn delete_after_totals_match_upstream() {
    check(Fixture::Extraneous, &["-rt", "--delete-after"], (46, 11));
}

/// `-v` makes the receiver announce the destination root it creates, one
/// `MSG_INFO` frame (main.c:828-829): the whole-file totals plus the notice.
#[test]
fn created_directory_notice_is_counted_as_received() {
    let ((sent, received), dst) =
        totals_with_destination(&oc_rsync(), Fixture::WholeFile, &["-rtv"]);
    let notice = format!("created directory {}\n", dst.display());
    assert_eq!((sent, received), (5_098, 38 + 4 + notice.len() as u64));
    if let Some(upstream) = upstream_rsync() {
        let ((up_sent, up_received), up_dst) =
            totals_with_destination(&upstream, Fixture::WholeFile, &["-rtv"]);
        let up_notice = format!("created directory {}\n", up_dst.display());
        assert_eq!(
            (up_sent, up_received),
            (5_098, 38 + 4 + up_notice.len() as u64)
        );
    }
}

/// The file list goes out in `readdir()` order (flist.c:send_directory()),
/// and with shared prefixes its size depends on that order, which is a
/// property of the filesystem: this cell has no pinned value and runs only
/// against a live oracle on the same filesystem.
#[test]
fn file_list_follows_readdir_order_like_upstream() {
    let Some(upstream) = upstream_rsync() else {
        eprintln!("no rsync {ORACLE_VERSION} oracle; readdir-order cell skipped");
        return;
    };
    assert_eq!(
        totals(&oc_rsync(), Fixture::SharedPrefixes, &["-rt"]),
        totals(&upstream, Fixture::SharedPrefixes, &["-rt"]),
    );
}

/// Without incremental recursion the file list is one flat list and its
/// NDX_DONE rides in the generator's last request (generator.c:2832).
///
/// Recorded from rsync 3.5.1 (the release tarball build, protocol 33), ten runs, all
/// identical: `rsync -rt --no-i-r --stats src/ dst` printed sent 46,
/// received 12.
#[test]
fn flat_list_up_to_date_totals_match_upstream() {
    check(Fixture::UpToDate, &["-rt", "--no-i-r"], (46, 12));
}

/// A flat list with deletions: the delete statistics and `MSG_DELETED`
/// frames ride in the phase tail as with incremental lists.
///
/// Recorded from rsync 3.5.1, ten runs, all identical:
/// `rsync -rt --no-i-r --delete --stats src/ dst` printed sent 46,
/// received 32.
#[test]
fn flat_list_delete_totals_match_upstream() {
    check(
        Fixture::Extraneous,
        &["-rt", "--no-i-r", "--delete"],
        (46, 32),
    );
}

/// A flat-list transfer: the generator itemizes `.` in a frame of its own
/// before the request (generator.c:2832 flushes per file).
///
/// Recorded from rsync 3.5.1, ten runs: `rsync -rt --no-i-r --stats src/ dst`
/// always printed received 38; sent was 5092 (seven runs) or 5096 (three),
/// as the sender's reply was or was not split from its NDX_DONE.
#[test]
fn flat_list_transfer_totals_match_upstream() {
    check_sent_within(
        Fixture::WholeFile,
        &["-rt", "--no-i-r"],
        &[5_092, 5_096],
        38,
    );
}

/// Owner names travel inline under incremental recursion and in trailing id
/// lists without it (uidlist.c:send_id_lists(), with CF_ID0_NAMES naming id
/// 0). The names are this machine's account names, so these cells have no
/// pinned value and run only against a live oracle.
#[test]
fn owner_names_cost_what_upstream_sends() {
    let Some(upstream) = upstream_rsync() else {
        eprintln!("no rsync {ORACLE_VERSION} oracle; owner-name cells skipped");
        return;
    };
    for args in [["-rtog", "--no-i-r"].as_slice(), ["-rtog"].as_slice()] {
        assert_eq!(
            totals(&oc_rsync(), Fixture::UpToDate, args),
            totals(&upstream, Fixture::UpToDate, args),
            "oc vs live rsync {ORACLE_VERSION} for {args:?}"
        );
    }
}

/// Sets the xattrs of the [`Fixture::Xattrs`] tree, or reports why the
/// filesystem cannot hold user xattrs.
fn set_fixture_xattrs(src: &Path) -> std::io::Result<()> {
    xattr::set(src.join("alpha"), "user.short", b"v")?;
    xattr::set(src.join("beta"), "user.long", &[b'x'; 64])
}

/// `-X`: each entry's xattrs ride in the file list, a value over 32 bytes
/// abbreviated to its checksum (xattrs.c:send_xattr()), and each new file
/// carries ITEM_REPORT_XATTR, after whose iflags the generator names the
/// long value it lacks and the sender returns it whole
/// (generator.c:598-605, sender.c:482-484, xattrs.c:651-703).
///
/// Recorded from rsync 3.5.1 (the release tarball build, protocol 33),
/// ten runs, all identical: `rsync -rtX --stats src/ dst` printed
/// sent 467, received 60.
#[test]
fn xattr_totals_match_upstream() {
    let dir = tempfile::tempdir().expect("tempdir");
    let probe = dir.path().join("probe");
    fs::write(&probe, b"").expect("write probe");
    if let Err(error) = xattr::set(&probe, "user.probe", b"1") {
        eprintln!("filesystem holds no user xattrs ({error}); xattr cell skipped");
        return;
    }
    let run = |binary: &Path| {
        let dir = tempfile::tempdir().expect("tempdir");
        let (src, dst) = Fixture::Xattrs.build(dir.path());
        set_fixture_xattrs(&src).expect("set fixture xattrs");
        run_totals(binary, &src, &dst, &["-rtX"])
    };
    let oc = run(&oc_rsync());
    if let Some(upstream) = upstream_rsync() {
        assert_eq!(
            oc,
            run(&upstream),
            "oc vs live rsync {ORACLE_VERSION} for -rtX"
        );
    }
    assert_eq!(
        oc,
        (467, 60),
        "oc vs the rsync {ORACLE_VERSION} recording for -rtX"
    );
}
