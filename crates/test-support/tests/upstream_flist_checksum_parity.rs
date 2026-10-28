//! oc's `--checksum` file list must be byte-identical to upstream rsync's.
//!
//! Under `-c` the sender stores each regular file's whole-file digest in its
//! flist entry (upstream: flist.c:1815 make_file() calling checksum.c:419
//! file_checksum(), written by flist.c:1228 send_file_entry()), and the
//! receiver skips a file only when its own digest of the basis equals that
//! sum (generator.c:639 quick_check_ok()). oc hashes the files of each list
//! and sub-list in one parallel batch before encoding them, so this test pins
//! that batching to the exact upstream bytes.
//!
//! An upstream client (the receiver) PULLS one fixture from two senders in
//! turn - upstream itself and oc - through the `capture-rsh` trampoline, and
//! the demultiplexed `MSG_DATA` payloads of the two server->client streams
//! must be identical once the run-dependent stats fields are masked
//! ([`mask_stats`]). Upstream sends entries in the order it meets them
//! while oc sends its sorted list, so the operands are named in flist sort
//! order (files before directories, upstream: flist.c f_name_cmp()) and each
//! directory holds one file: the entry order on the wire is then the same
//! for both senders. Each directory's file travels in its own INC_RECURSE
//! sub-list, so both encode paths - the initial list and a sub-list - are
//! covered.
//!
//! The comparison cannot pass vacuously: the fixture holds a file whose basis
//! differs only in content (same size and mtime), which `-c` must transfer,
//! and a content-identical file with an older basis mtime, which `-c` must
//! skip. Both outcomes are pinned on the client's itemize output, and an
//! all-zero or missing sum flips the second one.
//!
//! Gated on `OC_RSYNC_UPSTREAM_COMPAT=1` like the other upstream oracles.
#![cfg(unix)]
use std::fs;
use std::path::Path;

use filetime::FileTime;
use test_support::transcript::{FIXED_CHECKSUM_SEED, TranscriptRecorder};
use test_support::{UpstreamVersion, require_upstream_rsync, upstream_compat_enabled};

const FIXED_MTIME_SECS: i64 = 1_700_000_000;

/// `MSG_DATA` multiplex tag (upstream: rsync.h:210 `MPLEX_BASE`).
const MPLEX_DATA: u8 = 7;

/// Source content, basis content, and whether the basis is backdated.
const FIXTURE: [(&str, &[u8], &[u8], bool); 5] = [
    (
        "d1/g",
        b"directory file, identical",
        b"directory file, identical",
        false,
    ),
    (
        "d2/h",
        b"directory file, edited",
        b"directory file, EDITED",
        false,
    ),
    (
        "f1",
        b"identical content, older basis",
        b"identical content, older basis",
        true,
    ),
    (
        "f2",
        b"same size, different bytes",
        b"same size, DIFFERENT bytes",
        false,
    ),
    ("f3", b"", b"", false),
];

/// Content-identical files repeat their text past one 256 KiB read window,
/// so their sums exercise the multi-read hash loop. Edited files stay below
/// one block so their delta is a single literal token: this test is about
/// the file list, not the literal chunking of the delta stream.
fn contents(source: &[u8], dest: &[u8], index: usize, basis: bool) -> Vec<u8> {
    let text = if basis { dest } else { source };
    if source != dest || text.is_empty() {
        return text.to_vec();
    }
    text.iter().copied().cycle().take(300_000 + index).collect()
}

fn write_tree(root: &Path, basis: bool) {
    for (index, (name, source, dest, backdate)) in FIXTURE.iter().enumerate() {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, contents(source, dest, index, basis)).expect("write");
        let secs = if basis && *backdate {
            FIXED_MTIME_SECS - 86_400
        } else {
            FIXED_MTIME_SECS
        };
        filetime::set_file_mtime(&path, FileTime::from_unix_time(secs, 0)).expect("mtime");
    }
    for dir in ["d1", "d2"] {
        filetime::set_file_mtime(
            root.join(dir),
            FileTime::from_unix_time(FIXED_MTIME_SECS, 0),
        )
        .expect("dir mtime");
    }
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
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
        let header = le32(stream, pos).expect("multiplex header");
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

/// Width of each `varlong30(x, 3)` stats field below 2^23
/// (upstream: io.c write_varlong()).
const STATS_FIELD_WIDTH: usize = 3;

/// Zeroes the run-dependent fields of the sender's stats block, which ends
/// the stream just before the one-byte `NDX_DONE` goodbye (upstream:
/// main.c:350-358 handle_stats(), main.c:933-935 read_final_goodbye()):
/// `total_read` and `total_written` count multiplex headers, whose number
/// follows flush timing (upstream against itself varies here), and
/// `flist_buildtime` and `flist_xfertime` are wall-clock milliseconds.
/// `total_size` stays compared. Refuses any other tail.
fn mask_stats(data: &mut [u8]) {
    const FIELDS: usize = 5;
    const TOTAL_SIZE: usize = 2;
    let end = data.len().checked_sub(1).expect("non-empty data stream");
    assert_eq!(
        data[end], 0,
        "data stream must end with the NDX_DONE goodbye"
    );
    let start = end - FIELDS * STATS_FIELD_WIDTH;
    for (index, field) in data[start..end]
        .chunks_exact_mut(STATS_FIELD_WIDTH)
        .enumerate()
    {
        assert!(field[0] < 0x80, "stats field is not a 3-byte varlong30");
        if index != TOTAL_SIZE {
            field.fill(0);
        }
    }
}

/// Pulls the fixture with `client` as receiver and `server` as sender.
/// Returns the sender's masked data stream and the client's itemize output.
fn pull(client: &Path, server: &Path) -> (Vec<u8>, String) {
    let work = test_support::create_tempdir();
    let src = work.path().join("src");
    let dest = work.path().join("dest");
    write_tree(&src, false);
    write_tree(&dest, true);
    let recorder = TranscriptRecorder::new(work.path());
    let capture_rsh = Path::new(env!("CARGO_BIN_EXE_capture-rsh"));
    let mut cmd = recorder.command(client, server, capture_rsh);
    cmd.args(["--protocol=32", "-rtc", "-i"]);
    for (index, operand) in ["f1", "f2", "f3", "d1", "d2"].iter().enumerate() {
        let path = src.join(operand);
        cmd.arg(if index == 0 {
            format!("transcripthost:{}", path.display())
        } else {
            format!(":{}", path.display())
        });
    }
    let output = cmd
        .arg(format!("{}/", dest.display()))
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "pull via {} failed: {}\nstderr: {}",
        server.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    for (name, source, _, _) in FIXTURE {
        assert_eq!(
            fs::read(dest.join(name)).expect("read dest"),
            fs::read(src.join(name)).expect("read src"),
            "{name}: the pulled file must equal the source ({})",
            String::from_utf8_lossy(source)
        );
    }
    let mut data = demux(&recorder.finish().expect("read captures").server_to_client);
    mask_stats(&mut data);
    (data, String::from_utf8_lossy(&output.stdout).into_owned())
}

fn itemize_line<'a>(itemize: &'a str, name: &str) -> &'a str {
    itemize
        .lines()
        .find(|line| line.ends_with(&format!(" {name}")))
        .unwrap_or_else(|| panic!("no itemize line for {name} in:\n{itemize}"))
}

#[test]
fn checksum_flist_stream_matches_upstream() {
    if !upstream_compat_enabled() {
        return;
    }
    let upstream = require_upstream_rsync(UpstreamVersion::V3_5_1)
        .expect("OC_RSYNC_UPSTREAM_COMPAT=1 but upstream 3.5.1 is not installed");
    let up = upstream.binary();
    let (reference, up_itemize) = pull(up, up);
    let (candidate, oc_itemize) = pull(up, &test_support::oc_rsync_bin());

    // `-c` saw the content change behind an unchanged size and mtime ...
    assert!(
        itemize_line(&up_itemize, "f2").starts_with(">fc"),
        "{up_itemize}"
    );
    // ... and matched the identical file whose basis mtime is older.
    assert!(
        itemize_line(&up_itemize, "f1").starts_with(".f..t"),
        "{up_itemize}"
    );
    assert_eq!(oc_itemize, up_itemize, "client itemize output");
    assert!(
        candidate == reference,
        "oc sender data stream differs from upstream 3.5.1 ({} vs {} bytes)",
        candidate.len(),
        reference.len()
    );
}
