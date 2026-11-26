//! `--write-batch` records each delta with the basis it was built against.
//!
//! A batch row names its basis through `ITEM_BASIS_TYPE_FOLLOWS`, the
//! `fnamecmp_type` byte and, for a fuzzy match, the basis name
//! (generator.c:2424-2429, sender.c:477-488), and carries a token for every
//! block the sender matched (match.c:128-140). A reader opens the named basis
//! and copies those blocks from it, so a row that omits either replays the
//! delta against the wrong file. Each cell records with oc - as a local copy,
//! or as the client of a remote-shell or daemon push or pull - checks the
//! recorded row, then replays the batch with rsync 3.5.2 and with oc. Every
//! child runs under the harness timeout, so a replay that hangs fails the cell
//! instead of the run.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use filetime::FileTime;
use test_support::oc_daemon::OcDaemon;
use test_support::{OcRsyncCliRunner, UpstreamRsync, UpstreamVersion, require_upstream_rsync};

const SRC_MTIME: i64 = 1_600_000_000;
const OLD_MTIME: i64 = 1_500_000_000;
const SRC_LEN: usize = 300_000;
const BASIS_LEN: usize = 200_000;
/// main.c:381-386 ends a batch with the run's stats; oc's `total_read`
/// differs there on every local batch, so byte comparisons stop before it.
const STATS_TRAILER_MAX: usize = 16;

/// rsync.h ITEM_* bits a transfer row carries.
const ITEM_BASIS_TYPE_FOLLOWS: u16 = 1 << 11;
const ITEM_XNAME_FOLLOWS: u16 = 1 << 12;
const ITEM_TRANSFER: u16 = 1 << 15;

/// The sum head every cell's single row carries: a 200,000-byte basis in
/// 700-byte blocks (286 of them, the last 500 bytes) with 2-byte strong sums.
const SUM_HEAD: [i32; 4] = [286, 700, 2, 500];

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Deterministic incompressible bytes, so every match is a real block
/// reference into the basis.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 56) as u8
        })
        .collect()
}

fn stamp(path: &Path, secs: i64) {
    test_support::file_times::set_file_mtime(path, FileTime::from_unix_time(secs, 0))
        .expect("stamp");
}

/// The basis a shape's row must name.
#[derive(Clone, Copy)]
enum Row {
    /// FNAMECMP_FNAME: no basis type follows the iflags word.
    Destination,
    /// A non-fuzzy `fnamecmp_type` byte.
    Basis(u8),
    /// FNAMECMP_FUZZY with the fuzzy file's basename as the xname.
    Fuzzy(&'static [u8]),
}

/// One basis shape: how the destination looks before the transfer, the
/// options that make the generator choose that basis, and the row it yields.
struct Shape {
    seed: u64,
    lay_out: fn(&Path, &[u8]),
    flags: &'static [&'static str],
    row: Row,
}

/// Where the recording client's peer runs.
#[derive(Clone, Copy, Debug)]
enum Link {
    Local,
    RemoteShellPush,
    RemoteShellPull,
    DaemonPush,
    DaemonPull,
}

struct Recording {
    root: tempfile::TempDir,
    src: Vec<u8>,
    batch: PathBuf,
}

/// A remote shell that runs the server command on this host.
fn write_rsh(root: &Path) -> PathBuf {
    let rsh = root.join("rsh");
    fs::write(&rsh, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").expect("write rsh");
    fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    rsh
}

impl Shape {
    /// Records the shape with `recorder` over `link` into a fresh tree. The
    /// source is `<root>/src` and the destination `<root>/recorded`, whichever
    /// side of the link each is on.
    fn record(&self, recorder: &Path, link: Link) -> Recording {
        let root = tempfile::TempDir::new().expect("temp dir");
        let src = noise(self.seed, SRC_LEN);
        let src_dir = root.path().join("src");
        fs::create_dir_all(&src_dir).expect("create src");
        fs::write(src_dir.join("f"), &src).expect("write src");
        stamp(&src_dir.join("f"), SRC_MTIME);
        stamp(&src_dir, SRC_MTIME);
        let recording = Recording {
            batch: root.path().join("batch"),
            root,
            src,
        };
        let dest = recording.dest("recorded", self);
        let root = recording.root.path();
        let mut runner = OcRsyncCliRunner::new()
            .binary(recorder)
            .env("OC_RSYNC_FALLBACK", "0")
            .args(["-a", "--no-whole-file", "--checksum-seed=1"])
            .args(self.flags)
            .arg(format!("--write-batch={}", recording.batch.display()));
        let mut _daemon = None;
        let (from, to) = match link {
            Link::Local => (
                format!("{}/", src_dir.display()),
                format!("{}/", dest.display()),
            ),
            Link::RemoteShellPush | Link::RemoteShellPull => {
                runner = runner
                    .arg(format!("--rsh={}", write_rsh(root).display()))
                    .arg(format!("--rsync-path={}", recorder.display()));
                let src = format!("{}/", src_dir.display());
                let dest = format!("{}/", dest.display());
                match link {
                    Link::RemoteShellPush => (src, format!("localhost:{dest}")),
                    _ => (format!("localhost:{src}"), dest),
                }
            }
            Link::DaemonPush | Link::DaemonPull => {
                let config = root.join("rsyncd.conf");
                fs::write(
                    &config,
                    format!(
                        "[m]\n    path = {}\n    use chroot = false\n    read only = false\n",
                        root.display()
                    ),
                )
                .expect("write rsyncd.conf");
                let daemon = OcDaemon::builder(&config).spawn().expect("spawn daemon");
                let module = format!("rsync://127.0.0.1:{}/m", daemon.port());
                _daemon = Some(daemon);
                match link {
                    Link::DaemonPush => (
                        format!("{}/", src_dir.display()),
                        format!("{module}/recorded/"),
                    ),
                    _ => (format!("{module}/src/"), format!("{}/", dest.display())),
                }
            }
        };
        let out = runner.arg(from).arg(to).run().expect("recorder completes");
        assert_eq!(
            out.status,
            Some(0),
            "record over {link:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        recording
    }
}

impl Recording {
    /// A fresh destination in the shape's pre-transfer state.
    fn dest(&self, name: &str, shape: &Shape) -> PathBuf {
        let dest = self.root.path().join(name);
        fs::create_dir_all(&dest).expect("create dest");
        (shape.lay_out)(&dest, &self.src);
        stamp(&dest, OLD_MTIME);
        dest
    }

    /// Replays the batch with `replayer` and asserts it rebuilds the source.
    fn assert_replays(&self, who: &str, replayer: &Path, shape: &Shape) {
        let dest = self.dest(&format!("replay-{who}"), shape);
        let out = OcRsyncCliRunner::new()
            .binary(replayer)
            .arg("-a")
            .args(shape.flags)
            .arg(format!("--read-batch={}", self.batch.display()))
            .arg(format!("{}/", dest.display()))
            .run()
            .unwrap_or_else(|e| panic!("{who} replay did not complete: {e}"));
        assert_eq!(
            out.status,
            Some(0),
            "{who} replay failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            fs::read(dest.join("f")).expect("read f") == self.src,
            "{who} replay did not rebuild f"
        );
    }

    /// Checks the iflags word, the basis type byte and the xname that precede
    /// the row's sum head (sender.c:477-488 writes them in that order).
    fn assert_row(&self, row: Row, link: Link) {
        let batch = fs::read(&self.batch).expect("read batch");
        let head: Vec<u8> = SUM_HEAD.iter().flat_map(|v| v.to_le_bytes()).collect();
        let at = batch
            .windows(head.len())
            .position(|w| w == head.as_slice())
            .unwrap_or_else(|| panic!("{link:?}: no row with sum head {SUM_HEAD:?}"));
        let attrs = &batch[..at];
        let iflags_at = |end: usize| u16::from_le_bytes([attrs[end - 2], attrs[end - 1]]);
        let iflags = match row {
            Row::Destination => iflags_at(attrs.len()),
            Row::Basis(kind) => {
                assert_eq!(attrs[attrs.len() - 1], kind, "{link:?}: basis type byte");
                iflags_at(attrs.len() - 1)
            }
            Row::Fuzzy(name) => {
                let start = attrs.len() - name.len();
                assert_eq!(&attrs[start..], name, "{link:?}: xname");
                assert_eq!(
                    usize::from(attrs[start - 1]),
                    name.len(),
                    "{link:?}: xname length"
                );
                assert_eq!(attrs[start - 2], 0x83, "{link:?}: basis type byte");
                iflags_at(start - 2)
            }
        };
        let expected = match row {
            Row::Destination => 0,
            Row::Basis(_) => ITEM_BASIS_TYPE_FOLLOWS,
            Row::Fuzzy(_) => ITEM_BASIS_TYPE_FOLLOWS | ITEM_XNAME_FOLLOWS,
        };
        assert_eq!(
            iflags & (ITEM_TRANSFER | ITEM_BASIS_TYPE_FOLLOWS | ITEM_XNAME_FOLLOWS),
            ITEM_TRANSFER | expected,
            "{link:?}: iflags {iflags:#06x}"
        );
    }
}

/// Records a local copy with oc, checks its row, replays it with 3.5.2 and
/// with oc, and checks the batch matches the one 3.5.2 records for the same
/// tree up to the stats trailer.
fn check(shape: &Shape) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let ours = shape.record(&oc_rsync(), Link::Local);
    ours.assert_row(shape.row, Link::Local);
    ours.assert_replays("upstream", upstream.binary(), shape);
    ours.assert_replays("oc", &oc_rsync(), shape);
    assert_same_rows(
        &ours,
        &shape.record(upstream.binary(), Link::Local),
        &upstream,
    );
}

/// Records over a remote shell or a daemon with oc as both ends, checks the
/// row, and replays the batch with 3.5.2 and with oc. The network sender's
/// literal chunking differs from 3.5.2's (#8816), so the batch bytes are not
/// compared here.
fn check_over(shape: &Shape, link: Link) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let ours = shape.record(&oc_rsync(), link);
    ours.assert_row(shape.row, link);
    ours.assert_replays("upstream", upstream.binary(), shape);
    ours.assert_replays("oc", &oc_rsync(), shape);
}

fn assert_same_rows(ours: &Recording, theirs: &Recording, upstream: &UpstreamRsync) {
    let a = fs::read(&ours.batch).expect("read oc batch");
    let b = fs::read(&theirs.batch).expect("read upstream batch");
    assert_eq!(a.len(), b.len(), "batch length vs {:?}", upstream.version());
    let first = a.iter().zip(&b).position(|(x, y)| x != y);
    if let Some(at) = first {
        assert!(
            at + STATS_TRAILER_MAX >= a.len(),
            "batch differs from {:?}'s at byte {at} of {}",
            upstream.version(),
            a.len()
        );
    }
}

fn basis_prefix(src: &[u8]) -> &[u8] {
    &src[..BASIS_LEN]
}

fn partial_dir_layout(dest: &Path, src: &[u8]) {
    fs::create_dir_all(dest.join(".p")).expect("create .p");
    fs::write(dest.join(".p/f"), basis_prefix(src)).expect("write partial");
    stamp(&dest.join(".p/f"), OLD_MTIME);
}

fn fuzzy_layout(dest: &Path, src: &[u8]) {
    fs::write(dest.join("f.old"), basis_prefix(src)).expect("write fuzzy basis");
    stamp(&dest.join("f.old"), OLD_MTIME);
}

fn inplace_layout(dest: &Path, src: &[u8]) {
    fs::write(dest.join("f"), basis_prefix(src)).expect("write dest");
    stamp(&dest.join("f"), OLD_MTIME);
}

/// The destination holds the source's bytes from offset 100 on, so every
/// block sits earlier in the basis than in the source: a scan restricted to
/// blocks the writer has not yet overwritten matches none of them.
fn shifted_basis(src: &[u8]) -> &[u8] {
    &src[100..100 + BASIS_LEN]
}

fn inplace_backup_layout(dest: &Path, src: &[u8]) {
    fs::write(dest.join("f"), shifted_basis(src)).expect("write dest");
    stamp(&dest.join("f"), OLD_MTIME);
}

/// FNAMECMP_PARTIAL_DIR: the delta resumes from `.p/f`; the row must name
/// that basis and carry the matches the in-place rewrite of `.p/f` skips.
const PARTIAL_DIR: Shape = Shape {
    seed: 1,
    lay_out: partial_dir_layout,
    flags: &["--partial-dir=.p"],
    row: Row::Basis(0x81),
};

/// FNAMECMP_FUZZY: no `f` exists, the delta is built against `f.old`; the
/// row must name the fuzzy type and carry `f.old` as its xname.
const FUZZY: Shape = Shape {
    seed: 2,
    lay_out: fuzzy_layout,
    flags: &["--fuzzy"],
    row: Row::Fuzzy(b"f.old"),
};

/// FNAMECMP_FNAME under `--inplace`: blocks already at their own offset are
/// skipped by the writer but still sent, so the row must carry them.
const INPLACE: Shape = Shape {
    seed: 3,
    lay_out: inplace_layout,
    flags: &["--inplace"],
    row: Row::Destination,
};

/// FNAMECMP_BACKUP under `--inplace --backup`: the delta is built against
/// the backup copy (generator.c:2371-2399), so blocks that moved to a lower
/// offset still match (sender.c:637-638 leaves `updating_basis_file` off).
const INPLACE_BACKUP: Shape = Shape {
    seed: 4,
    lay_out: inplace_backup_layout,
    flags: &["--inplace", "-b"],
    row: Row::Basis(0x82),
};

#[test]
fn partial_dir_row_names_its_basis() {
    check(&PARTIAL_DIR);
}

#[test]
fn fuzzy_row_names_its_basis() {
    check(&FUZZY);
}

#[test]
fn inplace_row_carries_same_offset_matches() {
    check(&INPLACE);
}

#[test]
fn inplace_backup_row_names_its_basis() {
    check(&INPLACE_BACKUP);
}

/// The backup the recording run leaves behind holds the destination's
/// pre-image, and the destination itself is the source.
#[test]
fn inplace_backup_keeps_the_pre_image() {
    let recording = INPLACE_BACKUP.record(&oc_rsync(), Link::Local);
    let recorded = recording.root.path().join("recorded");
    assert!(fs::read(recorded.join("f")).expect("read f") == recording.src);
    assert!(
        fs::read(recorded.join("f~")).expect("read f~") == shifted_basis(&recording.src),
        "f~ must hold the destination as it was before the update"
    );
}

#[test]
fn partial_dir_row_over_a_remote_shell_push() {
    check_over(&PARTIAL_DIR, Link::RemoteShellPush);
}

#[test]
fn partial_dir_row_over_a_remote_shell_pull() {
    check_over(&PARTIAL_DIR, Link::RemoteShellPull);
}

#[test]
fn partial_dir_row_over_a_daemon_push() {
    check_over(&PARTIAL_DIR, Link::DaemonPush);
}

#[test]
fn partial_dir_row_over_a_daemon_pull() {
    check_over(&PARTIAL_DIR, Link::DaemonPull);
}

#[test]
fn fuzzy_row_over_a_remote_shell_push() {
    check_over(&FUZZY, Link::RemoteShellPush);
}

#[test]
fn fuzzy_row_over_a_remote_shell_pull() {
    check_over(&FUZZY, Link::RemoteShellPull);
}

#[test]
fn fuzzy_row_over_a_daemon_push() {
    check_over(&FUZZY, Link::DaemonPush);
}

#[test]
fn fuzzy_row_over_a_daemon_pull() {
    check_over(&FUZZY, Link::DaemonPull);
}

#[test]
fn inplace_row_over_a_remote_shell_push() {
    check_over(&INPLACE, Link::RemoteShellPush);
}

#[test]
fn inplace_row_over_a_remote_shell_pull() {
    check_over(&INPLACE, Link::RemoteShellPull);
}

#[test]
fn inplace_row_over_a_daemon_push() {
    check_over(&INPLACE, Link::DaemonPush);
}

#[test]
fn inplace_row_over_a_daemon_pull() {
    check_over(&INPLACE, Link::DaemonPull);
}
