//! A local `--write-batch` records a new file's data from the copy's own
//! read: the batch holds each byte once, whichever copy strategy the run
//! would otherwise pick.
//!
//! The copy used to finish and then reopen the source to read it again for the
//! batch tokens, so every new file was read twice and its bytes were counted
//! twice in "Total bytes sent". With whole-file on, the kernel copy moved the
//! data unread and the capture read it a second time.
//!
//! # Upstream Reference
//!
//! - `main.c:666-670` - a local transfer forces `whole_file = 1` only without
//!   `--write-batch`.
//! - `match.c:480-486` `match_sums()` - with no block sums the sender sends the
//!   file as literal data from its one read.
//! - `token.c:317-327` `simple_send_token()` - literal runs go out in
//!   `CHUNK_SIZE` (32 KiB) tokens.

use std::fs;
use std::sync::{Arc, Mutex};

use batch::{BatchConfig, BatchFlags, BatchMode, BatchWriter};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan, WireTotals};
use protocol::{CompatibilityFlags, ProtocolVersion};
use tempfile::tempdir;

/// Three full literal tokens and a short one.
const FILE_LEN: usize = 3 * 32 * 1024 + 1000;

fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

const COMPAT_FLAGS: CompatibilityFlags = CompatibilityFlags::SAFE_FILE_LIST
    .union(CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
    .union(CompatibilityFlags::CHECKSUM_SEED_FIX)
    .union(CompatibilityFlags::INPLACE_PARTIAL_DIR)
    .union(CompatibilityFlags::VARINT_FLIST_FLAGS);

fn make_writer(path: &std::path::Path) -> Arc<Mutex<BatchWriter>> {
    let compat_flags = COMPAT_FLAGS;
    let config = BatchConfig::new(BatchMode::Write, path.to_string_lossy().into_owned(), 32)
        .with_compat_flags(compat_flags.bits() as i32)
        .with_checksum_seed(1);
    let mut writer = BatchWriter::new(config).expect("create batch writer");
    writer
        .write_header(BatchFlags {
            recurse: true,
            ..Default::default()
        })
        .expect("write batch header");
    Arc::new(Mutex::new(writer))
}

/// Records a batch of a tree holding one new file and returns its bytes.
fn record(whole_file: bool, content: &[u8]) -> Vec<u8> {
    record_with(whole_file, None, content).0
}

/// Records a batch of a tree holding one new file and returns its bytes and
/// the run's "Total bytes sent".
fn record_with(whole_file: bool, bwlimit: Option<u64>, content: &[u8]) -> (Vec<u8>, u64) {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    let batch_path = temp.path().join("batch.bin");
    fs::create_dir_all(&source).expect("create source dir");
    fs::write(source.join("f"), content).expect("write source");

    let writer = make_writer(&batch_path);
    let options = LocalCopyOptions::default()
        .recursive(true)
        .whole_file(whole_file)
        .bandwidth_limit(bwlimit.and_then(std::num::NonZeroU64::new))
        .batch_writer(Some(Arc::clone(&writer)))
        .wire_totals(Some(WireTotals::new(ProtocolVersion::NEWEST, COMPAT_FLAGS)));
    let mut src_os = source.into_os_string();
    src_os.push("/");
    let operands = vec![src_os, dest.clone().into_os_string()];
    let summary = LocalCopyPlan::from_operands(&operands)
        .expect("plan")
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("write-batch copy succeeds");
    assert_eq!(fs::read(dest.join("f")).expect("read copy"), content);

    Arc::try_unwrap(writer)
        .expect("writer uniquely owned")
        .into_inner()
        .expect("writer mutex not poisoned")
        .finalize()
        .expect("finalize batch writer");
    (
        fs::read(&batch_path).expect("read batch bytes"),
        summary.bytes_sent(),
    )
}

/// Counts the 32 KiB windows of `content` that occur in `batch`: each one
/// must be recorded exactly once.
fn recorded_copies(batch: &[u8], content: &[u8]) -> Vec<usize> {
    content
        .chunks(32 * 1024)
        .map(|chunk| batch.windows(chunk.len()).filter(|w| *w == chunk).count())
        .collect()
}

#[test]
fn write_batch_records_each_byte_of_a_new_file_once() {
    let content = noise(FILE_LEN);
    for whole_file in [false, true] {
        let batch = record(whole_file, &content);
        assert_eq!(
            recorded_copies(&batch, &content),
            vec![1; 4],
            "whole_file={whole_file}: every chunk of the file is recorded once"
        );
        // Four literal tokens add a 4-byte length each; the file list, header
        // and trailer stay well under a kilobyte for one file.
        assert!(
            batch.len() < FILE_LEN + 1024,
            "whole_file={whole_file}: batch is {} bytes for a {FILE_LEN}-byte file",
            batch.len()
        );
    }
}

/// Reads the literal token lengths of the batch's one file: each token is a
/// 4-byte length followed by that many bytes of `content`.
fn token_lengths(batch: &[u8], content: &[u8]) -> Vec<u32> {
    let mut at = batch
        .windows(64)
        .position(|w| w == &content[..64])
        .expect("first literal in the batch");
    let mut lengths = Vec::new();
    let mut done = 0;
    while done < content.len() {
        let len = u32::from_le_bytes(batch[at - 4..at].try_into().unwrap());
        assert_eq!(
            &batch[at..at + len as usize],
            &content[done..done + len as usize]
        );
        lengths.push(len);
        done += len as usize;
        at += len as usize + 4;
    }
    lengths
}

/// The literal tokens are `CHUNK_SIZE` long except the last, like rsync
/// 3.5.2's, whatever size the copy buffer has: small files are copied with a
/// buffer smaller than a token, large ones with a larger one.
#[test]
fn write_batch_literal_tokens_are_chunk_sized() {
    for len in [50_000usize, FILE_LEN, 3 * 1024 * 1024 + 7] {
        let content = noise(len);
        let lengths = token_lengths(&record(false, &content), &content);
        let full = len / (32 * 1024);
        let mut expected = vec![32 * 1024u32; full];
        if len % (32 * 1024) != 0 {
            expected.push((len % (32 * 1024)) as u32);
        }
        assert_eq!(lengths, expected, "{len}-byte file");
    }
}

/// `--bwlimit` makes the copy read in pieces smaller than a token (200 KiB/s
/// reads 25,600 bytes at a time); the tokens still follow `CHUNK_SIZE`, as
/// the limit only paces rsync 3.5.2's writes (io.c:2330-2364 `sleep_for_bwlimit()`).
#[test]
fn write_batch_literal_tokens_ignore_the_read_size() {
    let content = noise(70_000);
    let lengths = token_lengths(&record_with(false, Some(200 * 1024), &content).0, &content);
    assert_eq!(lengths, vec![32 * 1024, 32 * 1024, 70_000 - 64 * 1024]);
}

/// "Total bytes sent" counts the file's literal data once, as rsync 3.5.2's
/// sender does: the copy and the batch tokens are one read, not two tallies.
#[test]
fn write_batch_counts_the_file_once_in_bytes_sent() {
    let content = noise(FILE_LEN);
    for whole_file in [false, true] {
        let (_, sent) = record_with(whole_file, None, &content);
        let sent = usize::try_from(sent).expect("sent fits usize");
        assert!(
            (FILE_LEN..FILE_LEN + 1024).contains(&sent),
            "whole_file={whole_file}: {sent} bytes sent for a {FILE_LEN}-byte file"
        );
    }
}
