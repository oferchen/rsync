//! UTS-NEXTEST-EDGE.n: nextest port of the upstream
//! `compress-zlib-insert_test.py` scenario, locking in the BufferedMap
//! window-length clamp from UTS-18.g.1/.g.2/.g.3.
//!
//! # Background
//!
//! Upstream rsync's `compress-zlib-insert_test.py` (testsuite issue #951)
//! exercises `-z` / `-zz` against an incompressible basis whose tail sits
//! near the basis-window edge. The transfer routes the basis through
//! `map_file`/`map_ptr` (`fileio.c`), and a defect in our buffered mapper's
//! window accounting surfaced as a slice-out-of-bounds when:
//!
//! - `file_size < MAX_MAP_SIZE` (256 KiB), and
//! - the caller requested a `map_ptr(offset, len)` that legitimately covered
//!   the whole file.
//!
//! `load_window` had set `window_len` from `min(max_window, remaining)`
//! before the clamp landed, but the subsequent slice index used
//! `window_len` against a buffer whose backing `Vec` had been resized to
//! `target_len = window_size.max(buffer.len())`. When a follow-up call
//! requested a range that walked past the file end, the existing
//! `offset + len > size` guard correctly rejected it; but a legitimate
//! request for the full file (`offset=0, len=file_size`) crashed with
//! `"exceeds buffer length"` because `window_len` was claiming `max_window`
//! bytes instead of the file's actual remaining bytes from `window_start`.
//!
//! # Fix
//!
//! UTS-18.g re-derived `window_len` at the single assignment site against
//! `file_size - window_start`, making the invariant locally provable:
//!
//! ```ignore
//! self.window_len = window_size.min(file_size - window_start);
//! ```
//!
//! # What this test pins
//!
//! Inline unit tests under `crates/transfer/src/map_file/buffered.rs` cover
//! the clamp invariant against fabricated state. This integration test pins
//! the *public API contract* the four production `map_ptr` call sites
//! depend on (`token_loop.rs`, `response.rs`, `sync.rs`, `applicator.rs`):
//!
//! - Full-file `map_ptr` on a basis smaller than `MAX_MAP_SIZE` returns
//!   `Ok` with byte-identical contents.
//! - Near-EOF first read + cached re-read both return `Ok` - the clamp
//!   must hold on the initial load AND on cached re-entry.
//! - A `map_ptr` whose `offset+len` walks past EOF returns
//!   `Err(UnexpectedEof)`, never panics on a buffer-index out-of-range.
//! - A sliding-window walk that crosses multiple window boundaries and
//!   terminates in a near-EOF window returns correct bytes throughout.
//! - The exact-fit boundary at `file_size == MAX_MAP_SIZE` also round-trips.
//!
//! # Upstream references
//!
//! - Upstream test: `testsuite/compress-zlib-insert_test.py` (issue #951).
//! - Upstream code path: `fileio.c::map_ptr()` lines 236-279.
//! - In-tree lineage: UTS-18.f (panic-to-Err guard), UTS-18.g.1/.g.2/.g.3
//!   (window_len clamp), EDG-PANIC.3 (negative-bounds coverage).

use std::io;
use std::io::Write;

use tempfile::NamedTempFile;

use transfer::constants::MAX_MAP_SIZE;
use transfer::map_file::{BufferedMap, MapStrategy};

/// Builds a temp file of `size` bytes with a deterministic byte pattern
/// (each byte is its offset mod 256). Returns the temp file handle so the
/// caller can stash it for cleanup.
fn make_basis(size: usize) -> (NamedTempFile, Vec<u8>) {
    let payload: Vec<u8> = (0..size).map(|i| (i & 0xff) as u8).collect();
    let mut tmp = NamedTempFile::new().expect("tempfile create");
    tmp.write_all(&payload).expect("write basis");
    tmp.flush().expect("flush basis");
    (tmp, payload)
}

/// Production crash math: file = 48128 bytes, `MAX_MAP_SIZE` = 262144. A
/// full-file `map_ptr` must succeed - the historical defect surfaced as
/// `"exceeds buffer length"` because `window_len` claimed `max_window`
/// instead of `file_size - window_start`.
///
/// Strictly smaller than `MAX_MAP_SIZE` so the clamp branch is the
/// only path that produces a correct `window_len`.
#[test]
fn full_file_map_ptr_succeeds_when_basis_smaller_than_max_map_size() {
    const FILE_SIZE: usize = 48128;
    assert!(FILE_SIZE < MAX_MAP_SIZE);

    let (tmp, payload) = make_basis(FILE_SIZE);
    let mut map = BufferedMap::open(tmp.path()).expect("open basis");

    let slice = map
        .map_ptr(0, FILE_SIZE)
        .expect("full-file map_ptr on sub-MAX_MAP_SIZE basis must Ok");
    assert_eq!(slice.len(), FILE_SIZE);
    assert_eq!(slice, &payload[..]);
}

/// Round-trip across a near-EOF read then a re-read of the same tail: a
/// second `map_ptr` for the same region must hit the cached window and
/// return identical bytes. If the clamp regresses, the cached `window_len`
/// would over-claim and the bounded slice index on the re-entry would
/// fail-loud as `Err("exceeds buffer length")` instead of returning the
/// payload tail. Pins the cached-state half of the invariant that the
/// inline `load_window_clamps_window_len_to_remaining_file_bytes` test
/// covers from the first-read side.
#[test]
fn cached_tail_reread_after_near_eof_load_returns_payload() {
    const FILE_SIZE: usize = 48128;
    assert!(FILE_SIZE < MAX_MAP_SIZE);

    let (tmp, payload) = make_basis(FILE_SIZE);
    let mut map = BufferedMap::open(tmp.path()).expect("open basis");

    // First read: a tail slice that forces `load_window` to clamp
    // `window_len` to the file's remaining bytes from `window_start`.
    let tail_start = FILE_SIZE - 1024;
    let first = map
        .map_ptr(tail_start as u64, 1024)
        .expect("tail read must Ok");
    assert_eq!(first, &payload[tail_start..]);

    // Second read: same tail region, served from the cached window. If the
    // clamp regresses, the inner slice index trips the bounds guard.
    let second = map
        .map_ptr(tail_start as u64, 1024)
        .expect("cached tail re-read must Ok");
    assert_eq!(second, &payload[tail_start..]);
}

/// Negative control: a request that legitimately walks past the basis tail
/// must return `Err(UnexpectedEof)`, never panic. Mirrors the upstream
/// `compress-zlib-insert` failure shape (offset near file end, short tail
/// read whose sum exceeds file size) so the public-API contract pins both
/// halves: clamp succeeds on legitimate full-file reads (test above) AND
/// the entry guard rejects bogus over-reads.
#[test]
fn map_ptr_past_basis_end_returns_unexpected_eof_not_panic() {
    const FILE_SIZE: usize = 32 * 1024;
    assert!(FILE_SIZE < MAX_MAP_SIZE);

    let (tmp, _) = make_basis(FILE_SIZE);
    let mut map = BufferedMap::open(tmp.path()).expect("open basis");

    let offset = (FILE_SIZE - 32) as u64;
    let err = map
        .map_ptr(offset, 96)
        .expect_err("read past EOF must fail-loud as Err, not panic");
    assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
}

/// Sliding-window walk across a sub-`MAX_MAP_SIZE` basis: chunk-by-chunk
/// reads that cross window boundaries multiple times and terminate at a
/// final window whose `window_len` must clamp to `file_size - window_start`
/// (8 KiB at the tail, against a 32 KiB max window). Every chunk must
/// return correct bytes; a tail re-read must hit the cached state with the
/// clamp still holding.
#[test]
fn sliding_window_walk_near_eof_matches_payload_and_caches_tail() {
    const FILE_SIZE: usize = 200 * 1024;
    assert!(FILE_SIZE < MAX_MAP_SIZE);
    const WINDOW: usize = 32 * 1024;

    let (tmp, payload) = make_basis(FILE_SIZE);
    let mut map = BufferedMap::open_with_window(tmp.path(), WINDOW).expect("open basis");

    const CHUNK: usize = 4 * 1024;
    let mut offset = 0usize;
    while offset < FILE_SIZE {
        let len = CHUNK.min(FILE_SIZE - offset);
        let slice = map
            .map_ptr(offset as u64, len)
            .expect("sequential map_ptr must Ok");
        assert_eq!(slice.len(), len, "len mismatch at offset {offset}");
        assert_eq!(
            slice,
            &payload[offset..offset + len],
            "byte mismatch at offset {offset}",
        );
        offset += len;
    }

    let tail_offset = FILE_SIZE - CHUNK;
    let tail = map
        .map_ptr(tail_offset as u64, CHUNK)
        .expect("tail re-read must Ok");
    assert_eq!(tail, &payload[tail_offset..]);
}

/// Boundary control: a basis whose size is exactly `MAX_MAP_SIZE` must also
/// round-trip through the same public API. The clamp's `min` operation
/// degenerates to identity here; the assertion guards against a future
/// refactor that off-by-one-flips the comparison to `<` and silently rejects
/// the exact-fit case.
#[test]
fn full_file_map_ptr_succeeds_when_basis_equals_max_map_size() {
    let (tmp, payload) = make_basis(MAX_MAP_SIZE);
    let mut map = BufferedMap::open(tmp.path()).expect("open basis");

    let slice = map
        .map_ptr(0, MAX_MAP_SIZE)
        .expect("full-file map_ptr at MAX_MAP_SIZE must Ok");
    assert_eq!(slice.len(), MAX_MAP_SIZE);
    assert_eq!(slice, &payload[..]);
}
