//! Under `--inplace` the sender picks the same basis block for every match as
//! rsync 3.5.2 does.
//!
//! upstream: match.c:290-334 `hash_search()` - when updating the basis file in
//! place, a match whose source offset is a block boundary is redirected to the
//! basis block at that same offset (`aligned_i`) and flagged
//! `SUMFLG_SAME_OFFSET`; an all-zero window that matches a zero block at the
//! next boundary is realigned onto that boundary, sending the bytes before it
//! as literal data. match.c:243-247 keeps a flagged block usable after the scan
//! passes its offset, and match.c:338-345 lets the `want_i` hint override only
//! a non-aligned choice.
//!
//! Each cell runs one `--inplace --no-whole-file -B700` transfer with rsync
//! 3.5.2 and with oc-rsync, as a local copy and as a push over `lsh-stub`, and
//! compares the `--debug=deltasum2` `match at` lines (offset and basis block
//! `j` of every match) and the `--stats` byte counts. The destination must
//! equal the source either way. Cells self-skip unless
//! `OC_RSYNC_UPSTREAM_COMPAT=1` and a 3.5.2 binary resolves.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use filetime::{FileTime, set_file_mtime};
use test_support::{
    LSH_STUB_BIN, LshRunnerStub, UpstreamVersion, create_tempdir, oc_rsync_bin, require_binaries,
    require_upstream_rsync,
};

const BLOCK: usize = 700;

/// A 700-byte block of pseudo-random bytes, distinct per `seed`.
fn random_block(seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..BLOCK)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// `base` with bytes `pos..pos + 3` shifted by `+1, -2, +1`: the weak checksum
/// is unchanged and the content differs, so the two collide.
fn weak_twin(base: &[u8], pos: usize) -> Vec<u8> {
    let mut twin = base.to_vec();
    twin[pos] = twin[pos].wrapping_add(1);
    twin[pos + 1] = twin[pos + 1].wrapping_sub(2);
    twin[pos + 2] = twin[pos + 2].wrapping_add(1);
    twin
}

/// A block of mid-range bytes, so a `weak_twin` never wraps a byte.
fn mid_block(seed: u64) -> Vec<u8> {
    random_block(seed).iter().map(|b| 64 + b / 2).collect()
}

fn zeros(len: usize) -> Vec<u8> {
    vec![0; len]
}

fn join(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// Writes `src/f` and an older `dst/f` basis under `root`.
fn fixture(root: &Path, basis: &[u8], source: &[u8]) {
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::create_dir_all(root.join("dst")).expect("mkdir dst");
    fs::write(root.join("dst/f"), basis).expect("write basis");
    fs::write(root.join("src/f"), source).expect("write source");
    set_file_mtime(
        root.join("dst/f"),
        FileTime::from_unix_time(1_000_000_000, 0),
    )
    .expect("backdate basis");
}

/// Runs `client` and returns its `match at` lines and `--stats` byte lines,
/// after checking the destination now equals the source.
fn match_trace(client: &Path, root: &Path, remote: Option<&Path>) -> Vec<String> {
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .args(["-a", "--inplace", "--no-whole-file", "-B700", "--stats"])
        .arg("--debug=deltasum2");
    let dest = match remote {
        Some(stub) => {
            command
                .arg(format!("--rsh={}", stub.display()))
                .arg(format!("--rsync-path={}", client.display()));
            format!("localhost:{}/", root.join("dst").display())
        }
        None => "dst/".to_owned(),
    };
    let output = command.arg("src/").arg(dest).output().expect("spawn rsync");
    assert!(
        output.status.success(),
        "{client:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(root.join("dst/f")).expect("read copy"),
        fs::read(root.join("src/f")).expect("read source"),
        "{client:?} must reproduce the source"
    );
    let mut lines: Vec<String> = [output.stdout, output.stderr]
        .iter()
        .flat_map(|stream| {
            String::from_utf8_lossy(stream)
                .into_owned()
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter_map(|line| {
            let line = line.strip_prefix("[sender] ").unwrap_or(&line).to_owned();
            (line.starts_with("match at ")
                || line.starts_with("Literal data:")
                || line.starts_with("Matched data:"))
            .then_some(line)
        })
        .collect();
    lines.sort();
    lines
}

/// Compares the match trace of rsync 3.5.2 and oc-rsync for one fixture on
/// both the local-copy and the push sender.
fn assert_matches_upstream(basis: &[u8], source: &[u8]) {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let oc = oc_rsync_bin();
    for remote in [None, Some(stub.path())] {
        let tmp = create_tempdir();
        let (up_root, oc_root) = (tmp.path().join("up"), tmp.path().join("oc"));
        fixture(&up_root, basis, source);
        fixture(&oc_root, basis, source);
        let expected = match_trace(upstream.binary(), &up_root, remote);
        let actual = match_trace(&oc, &oc_root, remote);
        assert!(
            expected.iter().any(|line| line.starts_with("match at ")),
            "the fixture must produce at least one match"
        );
        assert_eq!(actual, expected, "push={}", remote.is_some());
    }
}

/// A source block at a boundary whose content also sits at a later basis
/// block: the chain offers the later block first, upstream takes the one at
/// the same offset (match.c:293-308, 331).
#[test]
fn a_boundary_match_prefers_the_block_at_the_same_offset() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let (a, x) = (random_block(1), random_block(2));
    let basis = join(&[&a, &x, &x]);
    let source = join(&[&random_block(3), &x, &x]);
    assert_matches_upstream(&basis, &source);
}

/// An all-zero window off a boundary is realigned onto the next boundary's
/// zero block, sending the bytes before it as literals (match.c:300, 309-329).
#[test]
fn an_unaligned_zero_run_is_realigned_onto_the_next_boundary() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let (r1, r4) = (random_block(1), random_block(4));
    let basis = join(&[&r1, &zeros(BLOCK), &zeros(BLOCK), &r4]);
    let source = join(&[&random_block(3)[..300], &zeros(2 * BLOCK), &r4]);
    assert_matches_upstream(&basis, &source);
}

/// A block matched at its own offset stays usable after the scan passes that
/// offset (match.c:243-247, 331).
#[test]
fn a_block_matched_in_place_is_reused_after_the_scan_passes_it() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let (r1, r5) = (random_block(1), random_block(5));
    let basis = join(&[&r1, &random_block(2), &r5]);
    let source = join(&[&r1, &random_block(6), &r1, &r5]);
    assert_matches_upstream(&basis, &source);
}

/// A strong-sum miss unlinks every later chain entry with the same strong sum
/// for the rest of the file (match.c:254-260), and a passed entry stays
/// unlinked (match.c:240-247): once matched through the aligned rule, the
/// duplicate block is never offered by the chain again.
#[test]
fn a_chain_entry_unlinked_once_stays_out_for_the_rest_of_the_file() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let base = mid_block(7);
    let (v1, v2, v3) = (
        weak_twin(&base, 100),
        weak_twin(&base, 300),
        weak_twin(&base, 500),
    );
    let basis = join(&[&v1, &random_block(8), &v2, &v2]);
    let source = join(&[&v3, &v1, &v2, &random_block(9)[..15], &v2]);
    assert_matches_upstream(&basis, &source);
}

/// The short final block obeys the same passed-offset rule as every other
/// block (match.c:240-247): behind the cursor it stays literal.
#[test]
fn a_passed_short_final_block_is_not_matched_in_place() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tail = &random_block(10)[..589];
    let basis = join(&[&random_block(1), &random_block(2), tail]);
    let source = join(&[&random_block(1), &random_block(4), &random_block(5), tail]);
    assert_matches_upstream(&basis, &source);
}
