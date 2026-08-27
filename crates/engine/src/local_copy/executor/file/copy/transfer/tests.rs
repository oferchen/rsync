//! Transfer-layer tests that need `TransferFlags`, which is `pub(super)` and
//! so cannot be named from the crate-level test modules.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Instant;

use tempfile::TempDir;

use ::metadata::MetadataOptions;

use super::TransferFlags;
use super::execute::execute_transfer_once;
use crate::local_copy::{
    CopyContext, LocalCopyExecution, LocalCopyOptions, LocalCopyProgress, LocalCopyRecord,
    LocalCopyRecordHandler,
};

/// Length the file list recorded, and the length the copy must NOT stop at.
const RECORDED_LEN: usize = 4096;
/// Bytes appended after the length was recorded.
const APPENDED_LEN: usize = 256 * 1024;

fn plain_flags() -> TransferFlags {
    TransferFlags {
        append_allowed: false,
        append_verify: false,
        whole_file_enabled: true,
        inplace_enabled: false,
        partial_enabled: false,
        use_sparse_writes: false,
        compress_enabled: false,
        size_only_enabled: false,
        ignore_times_enabled: false,
        checksum_enabled: false,
        #[cfg(all(any(unix, windows), feature = "xattr"))]
        preserve_xattrs: false,
        xattrs_changed: false,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        preserve_acls: false,
    }
}

fn write_bytes(path: &Path, len: usize) {
    let mut file = File::create(path).expect("create source");
    let block: Vec<u8> = (0..len).map(|i| (i.wrapping_mul(31) % 251) as u8).collect();
    file.write_all(&block).expect("write source");
}

fn append_bytes(path: &Path, len: usize) {
    let mut file = OpenOptions::new().append(true).open(path).expect("reopen");
    let block: Vec<u8> = (0..len).map(|i| (i % 253) as u8).collect();
    file.write_all(&block).expect("append");
}

/// A copy must move exactly the length it was sized from, even when the file
/// on disk is longer.
///
/// The per-iteration loop guard (`total_bytes >= expected_remaining`) only
/// stops the loop *between* chunks, so the final read has to be clamped too -
/// otherwise a source that grew after it was sized contributes up to a whole
/// buffer beyond the bound and the destination outruns what the transfer
/// accounted for.
///
/// upstream: sender.c sizes `map_file` / `match_sums` from the `do_fstat`
/// length and walks exactly that many bytes, so data appended after the stat is
/// never sent. This drives the copy loop directly with a `total_size` smaller
/// than the file, which is the same disagreement without a race.
///
/// The dense loop and `copy_file_contents_sparse` each own a separate copy of
/// this loop, so each is asserted by its own `#[test]`. A single test looping
/// over both would stop at the first panic and report nothing about the
/// second - which silently halves the mutation kill set.
fn assert_copy_stops_at_the_declared_length(sparse: bool) {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");

    write_bytes(&source, RECORDED_LEN + APPENDED_LEN);

    let mut reader = File::open(&source).expect("open source");
    let mut writer = File::create(&destination).expect("create destination");
    // Larger than the bound, so an unclamped read overshoots it.
    let mut buffer = vec![0u8; 128 * 1024];

    // A limiter keeps the dense case off the `copy_file_range` fast path (which
    // takes `expected_remaining` as an explicit length and is already bounded);
    // the sparse case skips that path by construction.
    const NO_PACING: u64 = 1 << 30;
    let options =
        LocalCopyOptions::default().bandwidth_limit(Some(NonZeroU64::new(NO_PACING).unwrap()));
    let mut context = CopyContext::new(
        LocalCopyExecution::Apply,
        options,
        None,
        temp.path().to_path_buf(),
    );

    context
        .copy_file_contents(
            &mut reader,
            &mut writer,
            &mut buffer,
            sparse,
            false,
            true,
            &source,
            &destination,
            Path::new("src.bin"),
            None,
            RECORDED_LEN as u64,
            0,
            0,
            Instant::now(),
            false,
        )
        .expect("copy");
    drop(writer);

    assert_eq!(
        fs::metadata(&destination).expect("stat destination").len(),
        RECORDED_LEN as u64,
        "sparse={sparse}: copy overshot the length it was sized from"
    );
}

#[test]
fn dense_copy_moves_exactly_the_length_it_was_sized_from() {
    assert_copy_stops_at_the_declared_length(false);
}

#[test]
fn sparse_copy_moves_exactly_the_length_it_was_sized_from() {
    assert_copy_stops_at_the_declared_length(true);
}

/// The copy must be bounded by the size of the file it actually opened, not by
/// the length the file-list scan recorded.
///
/// upstream: sender.c re-stats the OPENED handle (`do_fstat`) and sizes
/// `map_file` / `match_sums` from `st.st_size`. The recorded length drives the
/// skip / delta / sparse decisions and is never a ceiling on the bytes moved,
/// so a file appended to after it was scanned is still sent whole.
///
/// This drives the executor directly with a deliberately stale `metadata`
/// rather than racing a real transfer: the production defect is exactly "the
/// recorded length disagrees with the opened file", and passing a stale
/// `Metadata` reproduces that with no threads and no timing window. A
/// growth-during-transfer scenario at the CLI layer cannot pin this - its
/// window has two edges and neither is guaranteed across platforms.
#[test]
fn copy_is_bounded_by_the_opened_file_not_the_recorded_length() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");

    write_bytes(&source, RECORDED_LEN);
    let recorded = fs::metadata(&source).expect("stat source");
    append_bytes(&source, APPENDED_LEN);

    // Non-vacuity: the fixture is only meaningful while the recorded length
    // still disagrees with the file on disk. If this ever reads the grown
    // size, the test below would pass without exercising anything.
    assert_eq!(
        recorded.len(),
        RECORDED_LEN as u64,
        "fixture is not stale: the recorded length already reflects the append"
    );
    let grown = fs::metadata(&source).expect("re-stat source").len();
    assert_eq!(grown, (RECORDED_LEN + APPENDED_LEN) as u64);

    // Select the buffered copy, which is the path that carries the bound. The
    // whole-file reflink tiers (macOS `clonefile`, Linux `FICLONE`) and the
    // `copy_file_range` fast path each clone or splice the entire file and so
    // cannot express this defect; all three are vetoed by the presence of a
    // bandwidth limiter (`clonefile::eligible` checks
    // `!context.has_bandwidth_limiter()`, and the `copy_file_range` arm checks
    // `self.limiter.is_none()`). The limit is set far above the fixture size so
    // it selects the path without pacing it - without this the test runs the
    // reflink tier, copies the file wholesale, and passes no matter what the
    // bound says.
    const NO_PACING: u64 = 1 << 30;
    let options =
        LocalCopyOptions::default().bandwidth_limit(Some(NonZeroU64::new(NO_PACING).unwrap()));

    let mode = LocalCopyExecution::Apply;
    let mut context = CopyContext::new(mode, options, None, temp.path().to_path_buf());

    execute_transfer_once(
        &mut context,
        &source,
        &destination,
        &recorded,
        MetadataOptions::default(),
        Path::new("src.bin"),
        None,
        false,
        recorded.file_type(),
        None,
        plain_flags(),
        mode,
        None,
        None,
    )
    .expect("transfer");

    assert_eq!(
        fs::metadata(&destination).expect("stat destination").len(),
        grown,
        "destination was bounded by the recorded length instead of the opened file's size"
    );
}

/// Length the file list recorded for a source that then shrank.
const SHRUNK_LEN: usize = 1024;

/// A source that ends before the length the transfer was sized from must be
/// diagnosed, not silently reported as a complete copy.
///
/// upstream: `map_ptr()` (fileio.c:359-371) records `ENODATA` when a read
/// returns 0 before the mapped window is filled, `unmap_file()` (fileio.c:385)
/// returns that status, and the sender turns it into
/// `io_error |= IOERR_GENERAL` plus one `read errors mapping %s` line
/// (sender.c:787-795) - which main.c reports as `RERR_PARTIAL` (23). Without
/// this the destination is silently short and the run exits 0, so data loss is
/// indistinguishable from success.
///
/// Driving the loop with a `total_size` larger than the file reproduces
/// "the source shrank after it was sized" without racing a real truncation.
/// Each copy path owns its own loop, so each gets its own `#[test]`: a single
/// test looping over them would stop at the first panic and report nothing
/// about the rest, silently halving the mutation kill set.
fn assert_short_source_is_recorded(sparse: bool, paced: bool) {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");

    write_bytes(&source, SHRUNK_LEN);

    let mut reader = File::open(&source).expect("open source");
    let mut writer = File::create(&destination).expect("create destination");
    let mut buffer = vec![0u8; 128 * 1024];

    // A limiter keeps the dense case off the `copy_file_range` fast path, so
    // the two are exercised separately rather than one masking the other.
    const NO_PACING: u64 = 1 << 30;
    let options = if paced {
        LocalCopyOptions::default().bandwidth_limit(Some(NonZeroU64::new(NO_PACING).unwrap()))
    } else {
        LocalCopyOptions::default()
    };
    let mut context = CopyContext::new(
        LocalCopyExecution::Apply,
        options,
        None,
        temp.path().to_path_buf(),
    );

    context
        .copy_file_contents(
            &mut reader,
            &mut writer,
            &mut buffer,
            sparse,
            false,
            true,
            &source,
            &destination,
            Path::new("src.bin"),
            None,
            // Declared longer than the file: the source shrank after sizing.
            (SHRUNK_LEN + APPENDED_LEN) as u64,
            0,
            0,
            Instant::now(),
            false,
        )
        .expect("a short source must not abort the copy");
    drop(writer);

    assert!(
        context.source_read_error_occurred(),
        "sparse={sparse} paced={paced}: a source that ended early was not recorded, \
         so the run would exit 0 on a short destination"
    );
}

#[test]
fn dense_copy_records_a_source_that_ended_early() {
    assert_short_source_is_recorded(false, true);
}

#[test]
fn sparse_copy_records_a_source_that_ended_early() {
    assert_short_source_is_recorded(true, false);
}

#[test]
fn copy_file_range_records_a_source_that_ended_early() {
    assert_short_source_is_recorded(false, false);
}

/// Non-vacuity companion: the same fixture with an honest length must leave
/// the flag clear. Without this, a predicate that fired unconditionally would
/// satisfy every assertion above.
#[test]
fn a_complete_source_records_no_read_error() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");

    write_bytes(&source, SHRUNK_LEN);

    let mut reader = File::open(&source).expect("open source");
    let mut writer = File::create(&destination).expect("create destination");
    let mut buffer = vec![0u8; 128 * 1024];

    let mut context = CopyContext::new(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default(),
        None,
        temp.path().to_path_buf(),
    );

    context
        .copy_file_contents(
            &mut reader,
            &mut writer,
            &mut buffer,
            false,
            false,
            true,
            &source,
            &destination,
            Path::new("src.bin"),
            None,
            SHRUNK_LEN as u64,
            0,
            0,
            Instant::now(),
            false,
        )
        .expect("copy");
    drop(writer);

    assert!(
        !context.source_read_error_occurred(),
        "a source that matched its recorded length was reported as short"
    );
}

/// Buffer the mover-selection fixture drives the copy with.
const MOVER_BUFFER_LEN: usize = 64 * 1024;

/// Source length for that fixture: several buffers, so a chunked userspace
/// read loop reports one progress update per chunk while a single handoff to
/// the kernel reports exactly one for the whole file.
const MOVER_SOURCE_LEN: usize = 3 * MOVER_BUFFER_LEN;

/// Counts the in-flight progress updates a copy produces, which is how this
/// module tells the two movers apart from outside the executor.
#[derive(Default)]
struct ProgressCounter {
    updates: usize,
    transferred: u64,
}

impl LocalCopyRecordHandler for ProgressCounter {
    fn handle(&mut self, _record: LocalCopyRecord) {}

    fn handle_progress(&mut self, progress: LocalCopyProgress<'_>) {
        self.updates += 1;
        self.transferred = progress.bytes_transferred();
    }
}

/// Runs one whole-file copy and reports how many progress updates it produced
/// and how many bytes the last update accounted for.
fn mover_progress_updates(whole_file_enabled: bool) -> (usize, u64) {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");

    write_bytes(&source, MOVER_SOURCE_LEN);

    let mut reader = File::open(&source).expect("open source");
    let mut writer = File::create(&destination).expect("create destination");
    let mut buffer = vec![0u8; MOVER_BUFFER_LEN];

    let mut counter = ProgressCounter::default();
    {
        let mut context = CopyContext::new(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default(),
            Some(&mut counter),
            temp.path().to_path_buf(),
        );

        context
            .copy_file_contents(
                &mut reader,
                &mut writer,
                &mut buffer,
                false,
                false,
                whole_file_enabled,
                &source,
                &destination,
                Path::new("src.bin"),
                None,
                MOVER_SOURCE_LEN as u64,
                0,
                0,
                Instant::now(),
                false,
            )
            .expect("copy");
    }
    drop(writer);

    assert_eq!(
        fs::metadata(&destination).expect("stat destination").len(),
        MOVER_SOURCE_LEN as u64,
        "whole_file_enabled={whole_file_enabled}: the destination is incomplete, so the \
         update count describes a copy that did not happen"
    );

    (counter.updates, counter.transferred)
}

/// `--no-whole-file` must reach the userspace read loop, not a kernel handoff.
///
/// The kernel content tier (io_uring, then `copy_file_range`) moves the file
/// without the process ever reading it. That is the same "send the whole file
/// as-is" that `--whole-file` names, and the executor's three other tiers -
/// `clonefile::eligible`, `ficlone::eligible`, `wincopy::eligible` - already
/// decline it when the operator asked for the delta algorithm. This tier did
/// not, so a reflink of a file was refused while an identical io_uring handoff
/// of the same file was not, and nothing that observes the source being read
/// could see the transfer at all.
///
/// upstream keeps the default local copy on the fast tier: `main.c:653-657`
/// forces `whole_file = 1` for a local transfer that did not ask otherwise
/// (`if (whole_file < 0 && !write_batch) whole_file = 1;`), so only an explicit
/// `--no-whole-file`, `--append` or `--write-batch` gets here.
///
/// Both legs are asserted together because either alone is satisfiable by a
/// blanket answer: pinning only the `false` leg passes for a build that always
/// reads in userspace, and pinning only the `true` leg passes for a build that
/// always hands off.
#[test]
fn no_whole_file_reads_the_source_in_userspace() {
    let (chunked_updates, chunked_bytes) = mover_progress_updates(false);
    let (handoff_updates, handoff_bytes) = mover_progress_updates(true);

    assert!(
        chunked_updates > 1,
        "--no-whole-file produced {chunked_updates} progress update(s) for a \
         {MOVER_SOURCE_LEN}-byte source read through a {MOVER_BUFFER_LEN}-byte buffer: \
         the copy was handed to the kernel instead of being read in userspace"
    );
    assert_eq!(
        handoff_updates, 1,
        "the default whole-file copy produced {handoff_updates} progress updates, so it \
         no longer takes the kernel content tier and this test can no longer tell the \
         two movers apart"
    );
    assert_eq!(
        chunked_bytes, MOVER_SOURCE_LEN as u64,
        "the --no-whole-file copy accounted for {chunked_bytes} of {MOVER_SOURCE_LEN} bytes"
    );
    assert_eq!(
        handoff_bytes, MOVER_SOURCE_LEN as u64,
        "the whole-file copy accounted for {handoff_bytes} of {MOVER_SOURCE_LEN} bytes"
    );
}

/// Non-vacuity companion for the cell above: a `--no-whole-file` copy of a
/// source that matches its recorded length must still reproduce it byte for
/// byte and must leave the short-read flag clear. Without this, routing every
/// copy through a loop that reported an error unconditionally would satisfy
/// the mover assertion.
#[test]
fn no_whole_file_copy_of_a_complete_source_records_no_read_error() {
    let temp = TempDir::new().expect("tempdir");
    let source = temp.path().join("src.bin");
    let destination = temp.path().join("dst.bin");

    write_bytes(&source, MOVER_SOURCE_LEN);

    let mut reader = File::open(&source).expect("open source");
    let mut writer = File::create(&destination).expect("create destination");
    let mut buffer = vec![0u8; MOVER_BUFFER_LEN];

    let mut context = CopyContext::new(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default(),
        None,
        temp.path().to_path_buf(),
    );

    context
        .copy_file_contents(
            &mut reader,
            &mut writer,
            &mut buffer,
            false,
            false,
            false,
            &source,
            &destination,
            Path::new("src.bin"),
            None,
            MOVER_SOURCE_LEN as u64,
            0,
            0,
            Instant::now(),
            false,
        )
        .expect("copy");
    drop(writer);

    assert!(
        !context.source_read_error_occurred(),
        "a --no-whole-file copy of a source that matched its recorded length was \
         reported as short"
    );
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        fs::read(&source).expect("read source"),
        "the --no-whole-file copy did not reproduce the source"
    );
}
