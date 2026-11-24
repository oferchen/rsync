//! `--read-batch` replays each recorded delta against the basis it was made
//! from.
//!
//! A batch transfer row carries the record-time generator's basis choice as
//! its `fnamecmp_type`: the destination, a `--partial-dir` file, the
//! `--inplace --backup` copy, a fuzzy match or a `--copy-dest` /
//! `--compare-dest` / `--link-dest` file. The recorded COPY tokens index that
//! file's blocks, so the replay must open the same one (receiver.c:1009-1053).
//! Each row below records with rsync 3.5.2 and replays the same batch with
//! 3.5.2 and with oc, so the expectation is 3.5.2's own behaviour.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use filetime::FileTime;

const SRC_MTIME: i64 = 1_600_000_000;
const OLD_MTIME: i64 = 1_500_000_000;
const HALF: usize = 150_000;

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Deterministic incompressible bytes, so every recorded match is a real
/// block reference into the basis.
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

/// One basis type: how the destination looks before the transfer, and the
/// options that make the generator pick that basis.
struct Cell {
    root: tempfile::TempDir,
    src: Vec<u8>,
    seed_dest: fn(&Path, &[u8]),
    batch: PathBuf,
}

impl Cell {
    /// Records `src` (two halves `a`, `b`) into a destination laid out by
    /// `seed_dest`, with `record_flags`.
    fn recorded_by(
        recorder: &Path,
        src: Vec<u8>,
        seed_dest: fn(&Path, &[u8]),
        record_flags: &[&str],
    ) -> Self {
        let root = tempfile::TempDir::new().expect("temp dir");
        let src_dir = root.path().join("src");
        fs::create_dir_all(&src_dir).expect("create src");
        fs::write(src_dir.join("f"), &src).expect("write src");
        stamp(&src_dir.join("f"), SRC_MTIME);
        let cell = Self {
            batch: root.path().join("batch"),
            root,
            src,
            seed_dest,
        };
        let recorded = cell.dest("recorded");
        let out = Command::new(recorder)
            .args(["-a", "--no-whole-file"])
            .args(record_flags)
            .arg(format!("--write-batch={}", cell.batch.display()))
            .arg(format!("{}/", src_dir.display()))
            .arg(format!("{}/", recorded.display()))
            .output()
            .expect("spawn recorder");
        assert!(
            out.status.success(),
            "record failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        cell
    }

    /// A fresh destination in the pre-transfer state, beside the shared
    /// reference directory `ref`.
    fn dest(&self, name: &str) -> PathBuf {
        let dest = self.root.path().join(name);
        fs::create_dir_all(&dest).expect("create dest");
        (self.seed_dest)(&dest, &self.src);
        dest
    }

    fn replay(&self, replayer: &Path, dest: &Path, flags: &[&str]) -> Output {
        Command::new(replayer)
            .arg("-a")
            .args(flags)
            .arg(format!("--read-batch={}", self.batch.display()))
            .arg(format!("{}/", dest.display()))
            .output()
            .expect("spawn replayer")
    }

    /// Replays into `<root>/<name>`, named relative to the root as the working
    /// directory, so a reported path shows how the replay resolves it.
    fn replay_relative(&self, replayer: &Path, name: &str, flags: &[&str]) -> Output {
        Command::new(replayer)
            .current_dir(self.root.path())
            .arg("-a")
            .args(flags)
            .arg(format!("--read-batch={}", self.batch.display()))
            .arg(format!("{name}/"))
            .output()
            .expect("spawn replayer")
    }

    /// Replays with `upstream` and with oc into separate destinations and
    /// asserts both rebuild the source file exactly.
    fn assert_both_replay(&self, upstream: &Path, flags: &[&str]) -> [PathBuf; 2] {
        [("upstream", upstream.to_path_buf()), ("oc", oc_rsync())].map(|(name, bin)| {
            let dest = self.dest(name);
            let out = self.replay(&bin, &dest, flags);
            assert!(
                out.status.success(),
                "{name} replay failed ({:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                fs::read(dest.join("f")).expect("read f") == self.src,
                "{name} replay did not rebuild f"
            );
            dest
        })
    }
}

/// The source's first two thirds, as the partial or alternate basis.
fn source_prefix(src: &[u8]) -> &[u8] {
    &src[..HALF * 4 / 3]
}

fn upstream_or_skip() -> Option<test_support::UpstreamRsync> {
    test_support::require_upstream_rsync(test_support::UpstreamVersion::V3_5_2)
}

/// FNAMECMP_PARTIAL_DIR: the generator resumes from `.p/f`, the destination
/// has no `f`, and the replay opens `partial_dir_fname(f)`
/// (receiver.c:1009, :1015-1017).
#[test]
fn partial_dir_row_replays_against_the_partial_file() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };
    fn seed(dest: &Path, src: &[u8]) {
        fs::create_dir_all(dest.join(".p")).expect("create .p");
        fs::write(dest.join(".p/f"), source_prefix(src)).expect("write partial");
    }
    let cell = Cell::recorded_by(
        upstream.binary(),
        noise(1, 2 * HALF),
        seed,
        &["--partial-dir=.p"],
    );
    cell.assert_both_replay(upstream.binary(), &["--partial-dir=.p"]);
}

/// FNAMECMP_BASIS_DIR_LOW: the generator found `f` in `--copy-dest=../ref`,
/// a directory relative to the destination, and the replay opens
/// `basis_dir[0]/f` (receiver.c:1050-1052).
#[test]
fn basis_dir_row_replays_against_the_alternate_basis() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };
    fn seed(dest: &Path, src: &[u8]) {
        let reference = dest.parent().expect("parent").join("ref");
        fs::create_dir_all(&reference).expect("create ref");
        fs::write(reference.join("f"), source_prefix(src)).expect("write ref");
    }
    let cell = Cell::recorded_by(
        upstream.binary(),
        noise(2, 2 * HALF),
        seed,
        &["--copy-dest=../ref"],
    );
    cell.assert_both_replay(upstream.binary(), &["--copy-dest=../ref"]);
}

/// A basis-dir row replayed without the alternate-basis option names an
/// index past `basis_dir_cnt`: 3.5.2 refuses it as a protocol error
/// (receiver.c:1045-1049, exit 2) instead of applying the delta to the
/// destination.
#[test]
fn basis_dir_row_without_the_option_is_a_protocol_error() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };
    fn seed(dest: &Path, src: &[u8]) {
        let reference = dest.parent().expect("parent").join("ref");
        fs::create_dir_all(&reference).expect("create ref");
        fs::write(reference.join("f"), source_prefix(src)).expect("write ref");
    }
    let cell = Cell::recorded_by(
        upstream.binary(),
        noise(3, 2 * HALF),
        seed,
        &["--copy-dest=../ref"],
    );
    for (name, bin) in [
        ("upstream", upstream.binary().to_path_buf()),
        ("oc", oc_rsync()),
    ] {
        let dest = cell.dest(name);
        let out = cell.replay(&bin, &dest, &[]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{name}: {stderr}");
        assert!(
            stderr.contains("invalid basis_dir index: 0."),
            "{name}: {stderr}"
        );
        assert!(!dest.join("f").exists(), "{name} wrote f");
    }
}

/// FNAMECMP_BACKUP: under `--inplace --backup` the generator copies `f` to
/// `f~` and the sender matches against that copy freely, so the recorded
/// delta moves blocks backwards. The replay must read `get_backup_name(f)`;
/// reading the destination it is rewriting in place would copy blocks it has
/// already overwritten (receiver.c:1018-1020, generator.c:2328).
#[test]
fn backup_row_replays_against_the_inplace_backup() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };
    fn seed(dest: &Path, src: &[u8]) {
        let (front, back) = src.split_at(HALF);
        fs::write(dest.join("f"), [back, front].concat()).expect("write old f");
        stamp(&dest.join("f"), OLD_MTIME);
    }
    let cell = Cell::recorded_by(
        upstream.binary(),
        noise(4, 2 * HALF),
        seed,
        &["--inplace", "-b"],
    );
    let (front, back) = cell.src.split_at(HALF);
    let old = [back, front].concat();
    for dest in cell.assert_both_replay(upstream.binary(), &["--inplace", "-b"]) {
        assert!(
            fs::read(dest.join("f~")).expect("read f~") == old,
            "backup lost"
        );
    }
}

/// FNAMECMP_FUZZY: the generator picked the similarly named `f.old` and the
/// replay opens the recorded xname in the entry's directory
/// (receiver.c:1021-1030). This arm already replayed correctly; the row keeps
/// it covered now that every arm goes through one resolver.
#[test]
fn fuzzy_row_replays_against_the_fuzzy_match() {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };
    fn seed(dest: &Path, src: &[u8]) {
        fs::write(dest.join("f.old"), source_prefix(src)).expect("write f.old");
    }
    let cell = Cell::recorded_by(upstream.binary(), noise(5, 2 * HALF), seed, &["--fuzzy"]);
    cell.assert_both_replay(upstream.binary(), &["--fuzzy"]);
}

/// The receiver's refusal line for a block match with no basis, as
/// receiver.c:618-620 prints it: `got a block match with no basis file for
/// "<curr_dir>/<name>" [receiver]`.
fn no_basis_line(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let start = stderr
        .find("got a block match with no basis file for ")
        .unwrap_or_else(|| panic!("no refusal line: {stderr}"));
    let rest = &stderr[start..];
    let end = rest.find("[receiver]").expect("role tag") + "[receiver]".len();
    rest[..end].to_owned()
}

/// Replays a partial-dir row with `upstream` and with oc into destinations
/// laid out by `seed_replay`, and asserts both refuse it the same way: exit 2
/// (RERR_PROTOCOL), the same receiver line naming the file under the absolute
/// working directory, and no file written.
fn assert_both_refuse_partial_row(seed_replay: fn(&Path, &str), flags: &[&str]) {
    let Some(upstream) = upstream_or_skip() else {
        return;
    };
    fn seed(dest: &Path, src: &[u8]) {
        fs::create_dir_all(dest.join(".p")).expect("create .p");
        fs::write(dest.join(".p/f"), source_prefix(src)).expect("write partial");
    }
    let cell = Cell::recorded_by(
        upstream.binary(),
        noise(6, 2 * HALF),
        seed,
        &["--partial-dir=.p"],
    );
    let mut lines = Vec::new();
    for (name, bin) in [
        ("upstream", upstream.binary().to_path_buf()),
        ("oc", oc_rsync()),
    ] {
        let dest = cell.root.path().join(name);
        fs::create_dir_all(&dest).expect("create dest");
        seed_replay(cell.root.path(), name);
        let out = cell.replay_relative(&bin, name, flags);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!dest.join("f").exists(), "{name} wrote f");
        lines.push(no_basis_line(&out).replace(&format!("/{name}/"), "/DEST/"));
    }
    assert_eq!(lines[0], lines[1], "oc must print 3.5.2's receiver line");
}

/// receiver.c:1009 - without `--partial-dir` on the replay, `partialptr` is
/// the destination name; the destination has no `f`, so the row has no basis
/// and its first match token is a protocol error (receiver.c:616-620).
#[test]
fn partial_dir_row_without_the_option_has_no_basis() {
    fn seed(root: &Path, name: &str) {
        let dest = root.join(name);
        fs::create_dir_all(dest.join(".p")).expect("create .p");
        fs::write(dest.join(".p/f"), b"not consulted").expect("write partial");
    }
    assert_both_refuse_partial_row(seed, &[]);
}

/// receiver.c:1083-1087, :1110-1122 - a relative partial-dir basis opens
/// through `secure_relative_open()`, which refuses a `.p` symlink leading out
/// of the destination. The refused basis is dropped, not replaced by the
/// destination file: the row has no basis, so its match token ends the run
/// instead of reading the file outside the destination.
#[test]
fn refused_partial_dir_basis_is_a_no_basis_update() {
    fn seed(root: &Path, name: &str) {
        let outside = root.join(format!("outside-{name}"));
        fs::create_dir_all(&outside).expect("create outside");
        fs::write(outside.join("f"), b"outside the destination").expect("write outside");
        std::os::unix::fs::symlink(format!("../outside-{name}"), root.join(name).join(".p"))
            .expect("plant .p symlink");
    }
    assert_both_refuse_partial_row(seed, &["--partial-dir=.p"]);
}
