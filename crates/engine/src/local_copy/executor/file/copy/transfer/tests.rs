//! Transfer-layer tests that need `TransferFlags`, which is `pub(super)` and
//! so cannot be named from the crate-level test modules.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::num::NonZeroU64;
use std::path::Path;

use tempfile::TempDir;

use ::metadata::MetadataOptions;

use super::TransferFlags;
use super::execute::execute_transfer_once;
use crate::local_copy::{CopyContext, LocalCopyExecution, LocalCopyOptions};

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
