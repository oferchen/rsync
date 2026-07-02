//! Sans-io decode seam shared by the blocking and async file-list readers.
//!
//! The file-list entry decoder in [`super::FileListReader::read_entry_with_flist`]
//! is deeply read/decode-interleaved: it reads a field, decodes it, updates the
//! cross-entry compression state, and only then knows the shape (and byte count)
//! of the next field. Every wire read on that path is a `read_exact` of a count
//! that is known at its own decision point (a flag byte, a varint tag plus its
//! table-driven extra bytes, a fixed-width field, or a length-prefixed blob);
//! no branch depends on partial-read or EOF-vs-data semantics. That property is
//! what makes a sans-io split possible without forking the decode logic.
//!
//! Rather than flatten the entry decode into a `Need(n)` state machine (which
//! would fork the byte-critical decode across `flags`/`name`/`metadata`/`extras`
//! plus the varint/codec/acl/xattr leaves and risk wire-byte divergence), this
//! seam runs the *existing* sync decode verbatim over an in-memory
//! [`Cursor`](std::io::Cursor). It does this speculatively:
//!
//! 1. Snapshot the fields the decode mutates across a single entry
//!    ([`EntrySnapshot`]: compression state, ACL/xattr caches, io-error, stats).
//! 2. Run [`FileListReader::read_entry_with_flist`] over `Cursor::new(buf)`.
//! 3. If the cursor is exhausted mid-entry, the `read_exact` inside the decode
//!    surfaces [`io::ErrorKind::UnexpectedEof`]; restore the snapshot and report
//!    [`EntryStep::NeedMore`] so the driver reads more bytes and retries.
//! 4. On success, keep the mutations and report the exact bytes consumed
//!    (`cursor.position()`), which the driver drains from its buffer.
//!
//! Because the sync decode is the single source of truth, the async twin can
//! never diverge from it on wire interpretation - it differs only in how bytes
//! reach the buffer (`.await` vs a blocking read). This mirrors the shared-seam
//! discipline of the async multiplex read leaf (`recv_msg_into_async`) and the
//! sans-io token decoder (`wire/compressed_token/step.rs`); see the 2026-07-02
//! amendment in `docs/design/asy-2-tokio-runtime-feature.md`.

use std::io::{self, Cursor};

use super::FileListReader;
use crate::acl::AclCache;
use crate::flist::entry::FileEntry;
use crate::flist::state::{FileListCompressionState, FileListStats};
use crate::xattr::XattrCache;

/// Outcome of one speculative decode step over the caller's buffer.
pub enum EntryStep {
    /// The buffer does not yet hold a complete entry; the driver must read more
    /// bytes and call [`FileListReader::read_entry_step`] again with the
    /// extended buffer. No compression state was mutated (the snapshot was
    /// restored), so retrying is exact.
    NeedMore,
    /// A complete entry was decoded (or the end-of-list marker was reached).
    ///
    /// `entry` is `None` at end-of-list (mirroring
    /// [`FileListReader::read_entry_with_flist`]). `consumed` is the exact number
    /// of leading bytes of the buffer the decode read; the driver must drain
    /// them before the next step.
    Emit {
        /// The decoded entry, or `None` at end-of-list.
        entry: Option<FileEntry>,
        /// Exact number of leading buffer bytes consumed by this entry.
        consumed: usize,
    },
}

/// Snapshot of the reader fields that [`FileListReader::read_entry_with_flist`]
/// mutates while decoding a single entry.
///
/// A speculative decode that runs out of buffered bytes leaves these partially
/// updated; restoring the snapshot makes the retry byte-identical to a decode
/// that saw the whole entry at once. The `dirname_interner`, `iconv`, and codec
/// are intentionally excluded: interning is idempotent (re-interning the same
/// path on retry is harmless) and the others are immutable during a decode.
struct EntrySnapshot {
    state: FileListCompressionState,
    acl_cache: AclCache,
    xattr_cache: XattrCache,
    io_error: i32,
    stats: FileListStats,
}

impl FileListReader {
    /// Captures the per-entry mutable state before a speculative decode.
    fn snapshot(&self) -> EntrySnapshot {
        EntrySnapshot {
            state: self.state.clone(),
            acl_cache: self.acl_cache.clone(),
            xattr_cache: self.xattr_cache.clone(),
            io_error: self.io_error,
            stats: self.stats.clone(),
        }
    }

    /// Restores the per-entry mutable state after a decode that ran out of bytes.
    fn restore(&mut self, snap: EntrySnapshot) {
        self.state = snap.state;
        self.acl_cache = snap.acl_cache;
        self.xattr_cache = snap.xattr_cache;
        self.io_error = snap.io_error;
        self.stats = snap.stats;
    }

    /// Attempts to decode one file-list entry from an in-memory buffer.
    ///
    /// This is the sans-io core that both the blocking and async file-list
    /// readers drive. It runs the identical sync decode
    /// ([`Self::read_entry_with_flist`]) over `Cursor::new(buf)`:
    ///
    /// - If `buf` holds a complete entry, returns
    ///   [`EntryStep::Emit`] with the decoded entry (or `None` at end-of-list)
    ///   and the exact byte count consumed.
    /// - If the entry is truncated (the decode's inner `read_exact` hit end of
    ///   buffer), the compression/cache/stats state is restored to its
    ///   pre-call value and [`EntryStep::NeedMore`] is returned. The driver must
    ///   append more wire bytes and call again.
    /// - Any other decode error (malformed data, oversize name, ...) propagates
    ///   unchanged, exactly as the blocking reader would surface it.
    ///
    /// `segment_entries` has the same meaning as in
    /// [`Self::read_entry_with_flist`]: the entries already decoded in the
    /// current flist segment, used to resolve abbreviated hardlink followers.
    ///
    /// Because the mutation set is snapshotted and restored on the truncated
    /// path, a `NeedMore`/retry cycle is indistinguishable from a single decode
    /// that saw all the bytes at once - which is what guarantees byte-identical
    /// [`FileEntry`] output regardless of how the wire bytes were chunked.
    pub fn read_entry_step(
        &mut self,
        buf: &[u8],
        segment_entries: &[FileEntry],
    ) -> io::Result<EntryStep> {
        // An empty buffer can never satisfy even the 1-byte flag read; asking
        // the decode would just surface UnexpectedEof, so short-circuit.
        if buf.is_empty() {
            return Ok(EntryStep::NeedMore);
        }

        let snap = self.snapshot();
        let mut cursor = Cursor::new(buf);
        match self.read_entry_with_flist(&mut cursor, segment_entries) {
            Ok(entry) => {
                let consumed = cursor.position() as usize;
                Ok(EntryStep::Emit { entry, consumed })
            }
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                // Truncated entry: the decode consumed part of the buffer and
                // mutated compression/cache state. Roll everything back so the
                // retry (with more bytes) is byte-for-byte identical.
                self.restore(snap);
                Ok(EntryStep::NeedMore)
            }
            Err(err) => Err(err),
        }
    }
}
