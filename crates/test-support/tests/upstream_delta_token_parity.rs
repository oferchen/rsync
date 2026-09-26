//! oc's sender delta stream must be byte-identical to upstream rsync's.
//!
//! Two fixtures where the matcher's block choice is visible on the wire:
//!
//! 1. A basis holding block C once and a source holding it five times.
//!    upstream `match.c:hash_search()` never retires a chain entry outside
//!    `--inplace`, so every repeat of C goes out as a copy token.
//! 2. A sparse, VM-image-like file: a few zero blocks in the basis and long
//!    zero runs in the source. With duplicate-content blocks the chain walk
//!    order (`match.c:98-110`, highest block index first) and the `want_i`
//!    hint (`match.c:321-334`) decide which block index each zero window
//!    names.
//!
//! Each fixture is PULLED by an upstream client (the receiver, pinned to
//! protocol 32) from two senders in turn - upstream itself and oc - through
//! the `capture-rsh` trampoline. From each server->client capture the test
//! cuts the file's delta section - the echoed sum header, every token and the
//! whole-file checksum (upstream: match.c:match_sums(), token.c
//! simple_send_token()) - and requires the two sections to be byte-identical.
//! The rest of the stream legitimately differs (the server advertises its own
//! protocol version, and the goodbye framing is not what this test is about).
//! The client's `--stats` "Matched data" line is also pinned to the value the
//! fixture implies, so the comparison cannot pass with both senders sending
//! literals.
//!
//! `match.c` is byte-identical between upstream 3.5.0 and 3.5.1, so the test
//! runs against every installed release of the two and requires at least one.
//! Gated on `OC_RSYNC_UPSTREAM_COMPAT=1` like the other upstream oracles.
#![cfg(unix)]

use std::fs;
use std::path::Path;

use filetime::FileTime;
use test_support::transcript::{FIXED_CHECKSUM_SEED, TranscriptRecorder};
use test_support::{UpstreamVersion, require_upstream_rsync, upstream_compat_enabled};

const BLOCK_LEN: usize = 1024;

/// Fixed source mtime; the basis gets an older one so quick-check never
/// skips the file.
const FIXED_MTIME_SECS: i64 = 1_700_000_000;

fn filler(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Basis `A B C D`; source `lit C C lit C C C`.
fn repeated_block_fixture() -> (Vec<u8>, Vec<u8>, u64) {
    let blocks: Vec<Vec<u8>> = (0..4u64)
        .map(|k| filler(0xC0FF_EE00 + k, BLOCK_LEN))
        .collect();
    let c = &blocks[2];
    let mut source = filler(0x0BAD_5EED, 300);
    source.extend_from_slice(c);
    source.extend_from_slice(c);
    source.extend_from_slice(&filler(0x0BAD_5EEE, 17));
    for _ in 0..3 {
        source.extend_from_slice(c);
    }
    (blocks.concat(), source, 5 * BLOCK_LEN as u64)
}

/// A 64-block basis with zero blocks at 5, 6, 20 and 40; the source keeps the
/// basis layout but widens the zero regions and appends a zero tail.
fn sparse_image_fixture() -> (Vec<u8>, Vec<u8>, u64) {
    let mut basis = filler(0x5A55_0001, 64 * BLOCK_LEN);
    for k in [5usize, 6, 20, 40] {
        basis[k * BLOCK_LEN..(k + 1) * BLOCK_LEN].fill(0);
    }
    let zeros = vec![0u8; BLOCK_LEN];
    let mut source = Vec::new();
    for k in 0..64 {
        let block = &basis[k * BLOCK_LEN..(k + 1) * BLOCK_LEN];
        source.extend_from_slice(block);
        if matches!(k, 6 | 20 | 33) {
            for _ in 0..12 {
                source.extend_from_slice(&zeros);
            }
        }
    }
    for _ in 0..40 {
        source.extend_from_slice(&zeros);
    }
    let matched = source.len() as u64;
    (basis, source, matched)
}

/// `MSG_DATA` multiplex tag (upstream: rsync.h `MPLEX_BASE`).
const MPLEX_DATA: u8 = 7;

/// Length of the MD5 whole-file checksum that ends the delta section; both
/// arms pin `RSYNC_CHECKSUM_LIST=md5` through the recorder.
const FILE_SUM_LEN: usize = 16;

fn le32(data: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Concatenates the `MSG_DATA` payloads that follow the unmultiplexed
/// handshake, whose last write is the fixed checksum seed.
fn demux(stream: &[u8]) -> Vec<u8> {
    let seed = FIXED_CHECKSUM_SEED.to_le_bytes();
    let mut pos = stream
        .windows(seed.len())
        .position(|w| w == seed)
        .expect("fixed checksum seed in the server stream")
        + seed.len();
    let mut data = Vec::new();
    while pos < stream.len() {
        let header = le32(stream, pos).expect("multiplex header") as u32;
        let len = (header & 0x00ff_ffff) as usize;
        pos += 4;
        let payload = stream.get(pos..pos + len).expect("multiplex payload");
        if (header >> 24) as u8 == MPLEX_DATA {
            data.extend_from_slice(payload);
        }
        pos += len;
    }
    data
}

/// Cuts the delta section for a basis of `count` blocks out of the sender's
/// data stream: the echoed sum header (`count`, `blength`, `s2length`,
/// `remainder`), the tokens up to the zero terminator, and the file checksum.
fn delta_section(data: &[u8], count: usize, remainder: usize) -> Vec<u8> {
    let start = (0..data.len())
        .find(|&p| {
            le32(data, p) == Some(count as i32)
                && le32(data, p + 4) == Some(BLOCK_LEN as i32)
                && matches!(le32(data, p + 8), Some(2..=16))
                && le32(data, p + 12) == Some(remainder as i32)
        })
        .expect("echoed sum header in the sender stream");
    let mut pos = start + 16;
    loop {
        let token = le32(data, pos).expect("token");
        pos += 4;
        match token {
            0 => break,
            n if n > 0 => pos += n as usize,
            _ => {}
        }
    }
    data.get(start..pos + FILE_SUM_LEN)
        .expect("file checksum after the tokens")
        .to_vec()
}

/// Pulls `source` over `basis` with `client` as receiver and `server` as
/// sender. Returns the sender's delta section and the client's `--stats`
/// output.
fn pull(client: &Path, server: &Path, basis: &[u8], source: &[u8]) -> (Vec<u8>, String) {
    let work = tempfile::tempdir().expect("workdir");
    let src = work.path().join("src.bin");
    let dest = work.path().join("dest.bin");
    fs::write(&src, source).expect("write source");
    fs::write(&dest, basis).expect("write basis");
    filetime::set_file_mtime(&src, FileTime::from_unix_time(FIXED_MTIME_SECS, 0))
        .expect("pin source mtime");
    filetime::set_file_mtime(
        &dest,
        FileTime::from_unix_time(FIXED_MTIME_SECS - 86_400, 0),
    )
    .expect("pin basis mtime");

    let recorder = TranscriptRecorder::new(work.path());
    let capture_rsh = Path::new(env!("CARGO_BIN_EXE_capture-rsh"));
    let output = recorder
        .command(client, server, capture_rsh)
        .arg("--protocol=32")
        .arg("-t")
        .arg("--no-whole-file")
        .arg(format!("--block-size={BLOCK_LEN}"))
        .arg("--stats")
        .arg(format!("transcripthost:{}", src.display()))
        .arg(&dest)
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "pull via {} failed: {}\nstderr: {}",
        server.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(&dest).expect("read dest"),
        source,
        "the pulled file must equal the source"
    );
    let transcript = recorder.finish().expect("read captures");
    let section = delta_section(
        &demux(&transcript.server_to_client),
        basis.len().div_ceil(BLOCK_LEN),
        basis.len() % BLOCK_LEN,
    );
    (
        section,
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

/// Extracts the byte count from the `Matched data: N bytes` stats line.
fn matched_data(stats: &str) -> u64 {
    let line = stats
        .lines()
        .find(|l| l.starts_with("Matched data:"))
        .unwrap_or_else(|| panic!("no Matched data line in:\n{stats}"));
    line.trim_start_matches("Matched data:")
        .trim()
        .trim_end_matches("bytes")
        .trim()
        .replace(',', "")
        .parse()
        .unwrap_or_else(|e| panic!("unparsable {line:?}: {e}"))
}

fn assert_oc_matches_upstream(name: &str, fixture: (Vec<u8>, Vec<u8>, u64)) {
    if !upstream_compat_enabled() {
        return;
    }
    let (basis, source, expected_matched) = fixture;
    let upstreams: Vec<_> = [UpstreamVersion::V3_5_1, UpstreamVersion::V3_5_0]
        .into_iter()
        .filter_map(require_upstream_rsync)
        .collect();
    assert!(
        !upstreams.is_empty(),
        "OC_RSYNC_UPSTREAM_COMPAT=1 but neither upstream 3.5.1 nor 3.5.0 is installed"
    );
    let oc = test_support::oc_rsync_bin();
    for upstream in upstreams {
        let up = upstream.binary();
        let (reference, up_stats) = pull(up, up, &basis, &source);
        let (candidate, oc_stats) = pull(up, &oc, &basis, &source);
        let version = upstream.version().directory();
        assert_eq!(
            matched_data(&up_stats),
            expected_matched,
            "{name}: upstream {version} Matched data"
        );
        assert_eq!(
            matched_data(&oc_stats),
            expected_matched,
            "{name}: oc Matched data against upstream {version} client"
        );
        assert!(
            candidate == reference,
            "{name}: oc delta section differs from upstream {version} ({} vs {} bytes)",
            candidate.len(),
            reference.len()
        );
    }
}

#[test]
fn repeated_basis_block_stream_matches_upstream() {
    assert_oc_matches_upstream("repeated block", repeated_block_fixture());
}

#[test]
fn sparse_image_stream_matches_upstream() {
    assert_oc_matches_upstream("sparse image", sparse_image_fixture());
}
