//! `--write-batch` records each delta with the basis it was built against.
//!
//! A batch row names its basis through `ITEM_BASIS_TYPE_FOLLOWS`, the
//! `fnamecmp_type` byte and, for a fuzzy match, the basis name
//! (generator.c:2424-2429, sender.c:477-488), and carries a token for every
//! block the sender matched (match.c:128-140). A reader opens the named basis
//! and copies those blocks from it, so a row that omits either replays the
//! delta against the wrong file. Each cell records with oc, then replays the
//! batch with rsync 3.5.2 and with oc; every child runs under the harness
//! timeout, so a replay that hangs fails the cell instead of the run.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use filetime::FileTime;
use test_support::{OcRsyncCliRunner, UpstreamRsync, UpstreamVersion, require_upstream_rsync};

const SRC_MTIME: i64 = 1_600_000_000;
const OLD_MTIME: i64 = 1_500_000_000;
const SRC_LEN: usize = 300_000;
const BASIS_LEN: usize = 200_000;
/// main.c:381-386 ends a batch with the run's stats; oc's `total_read`
/// differs there on every local batch, so byte comparisons stop before it.
const STATS_TRAILER_MAX: usize = 16;

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

/// One basis shape: how the destination looks before the transfer and the
/// options that make the generator choose that basis.
struct Shape {
    seed: u64,
    lay_out: fn(&Path, &[u8]),
    flags: &'static [&'static str],
}

struct Recording {
    root: tempfile::TempDir,
    src: Vec<u8>,
    batch: PathBuf,
}

impl Shape {
    /// Records the shape with `recorder` into a fresh tree.
    fn record(&self, recorder: &Path) -> Recording {
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
        let out = OcRsyncCliRunner::new()
            .binary(recorder)
            .args(["-a", "--no-whole-file", "--checksum-seed=1"])
            .args(self.flags)
            .arg(format!("--write-batch={}", recording.batch.display()))
            .arg(format!("{}/", src_dir.display()))
            .arg(format!("{}/", dest.display()))
            .run()
            .expect("recorder completes");
        assert_eq!(
            out.status,
            Some(0),
            "record failed: {}",
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
}

/// Records with oc, replays with 3.5.2 and with oc, and checks the batch
/// matches the one 3.5.2 records for the same tree up to the stats trailer.
fn check(shape: &Shape) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let ours = shape.record(&oc_rsync());
    ours.assert_replays("upstream", upstream.binary(), shape);
    ours.assert_replays("oc", &oc_rsync(), shape);
    assert_same_rows(&ours, &shape.record(upstream.binary()), &upstream);
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

/// FNAMECMP_PARTIAL_DIR: the delta resumes from `.p/f`; the row must name
/// that basis and carry the matches the in-place rewrite of `.p/f` skips.
#[test]
fn partial_dir_row_names_its_basis() {
    fn lay_out(dest: &Path, src: &[u8]) {
        fs::create_dir_all(dest.join(".p")).expect("create .p");
        fs::write(dest.join(".p/f"), basis_prefix(src)).expect("write partial");
        stamp(&dest.join(".p/f"), OLD_MTIME);
    }
    check(&Shape {
        seed: 1,
        lay_out,
        flags: &["--partial-dir=.p"],
    });
}

/// FNAMECMP_FUZZY: no `f` exists, the delta is built against `f.old`; the
/// row must name the fuzzy type and carry `f.old` as its xname.
#[test]
fn fuzzy_row_names_its_basis() {
    fn lay_out(dest: &Path, src: &[u8]) {
        fs::write(dest.join("f.old"), basis_prefix(src)).expect("write fuzzy basis");
        stamp(&dest.join("f.old"), OLD_MTIME);
    }
    check(&Shape {
        seed: 2,
        lay_out,
        flags: &["--fuzzy"],
    });
}

/// FNAMECMP_FNAME under `--inplace`: blocks already at their own offset are
/// skipped by the writer but still sent, so the row must carry them.
#[test]
fn inplace_row_carries_same_offset_matches() {
    fn lay_out(dest: &Path, src: &[u8]) {
        fs::write(dest.join("f"), basis_prefix(src)).expect("write dest");
        stamp(&dest.join("f"), OLD_MTIME);
    }
    check(&Shape {
        seed: 3,
        lay_out,
        flags: &["--inplace"],
    });
}
