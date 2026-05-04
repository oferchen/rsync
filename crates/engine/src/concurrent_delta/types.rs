//! Work-item and result types for the concurrent delta pipeline.
//!
//! [`DeltaWork`] describes one file's delta computation request - carrying the
//! file index, destination path, optional basis path, and expected file size.
//! [`DeltaResult`] captures the outcome of that computation, including transfer
//! statistics and checksum data needed for post-commit verification.
//!
//! Both types are `Send` so they can safely cross thread boundaries in the
//! pipelined architecture.

use std::path::PathBuf;

/// File index in the transfer file list.
///
/// Wraps a `u32` NDX value to prevent accidental mixing with sequence
/// numbers or other integer types in the concurrent delta pipeline.
///
/// # Upstream Reference
///
/// Corresponds to the NDX (file index) values that upstream rsync uses
/// throughout `receiver.c` and `generator.c` to identify files in the
/// sorted file list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(transparent)]
pub struct FileNdx(u32);

impl FileNdx {
    /// Creates a new file index.
    #[must_use]
    pub const fn new(ndx: u32) -> Self {
        Self(ndx)
    }

    /// Returns the raw `u32` value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for FileNdx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u32> for FileNdx {
    fn from(ndx: u32) -> Self {
        Self(ndx)
    }
}

/// A unit of work for the concurrent delta pipeline.
///
/// Represents a single file that requires delta computation. Created by the
/// generator/receiver when deciding how to transfer a file and dispatched to
/// a worker for signature generation, delta application, or whole-file write.
///
/// # Upstream Reference
///
/// Mirrors the per-file work item that upstream rsync's `recv_files()` loop
/// processes sequentially in `receiver.c`. The concurrent pipeline dispatches
/// these across threads for parallel I/O.
#[derive(Debug, Clone)]
pub struct DeltaWork {
    /// File list index (NDX) - stable position in the sorted file list.
    ndx: FileNdx,
    /// Pipeline sequence number for reordering after parallel dispatch.
    ///
    /// Assigned by the producer before sending into the work queue so that
    /// the consumer can reconstruct the original submission order via
    /// [`ReorderBuffer`](super::reorder::ReorderBuffer). Defaults to 0;
    /// the producer stamps monotonically increasing values.
    sequence: u64,
    /// Destination path where the reconstructed file will be placed.
    dest_path: PathBuf,
    /// Path to the basis file for delta transfer, `None` for whole-file transfer.
    basis_path: Option<PathBuf>,
    /// Optional source path for self-contained delta computation.
    ///
    /// When `Some` together with `basis_path`, [`DeltaTransferStrategy::process`]
    /// runs the full `DeltaGenerator` pipeline (signature build, block matching,
    /// script application) and reports actual stats. When `None`, the strategy
    /// reports the pre-computed `literal_bytes`/`matched_bytes` set by the
    /// receiver pipeline that already applied the wire delta.
    ///
    /// [`DeltaTransferStrategy::process`]: super::strategy::DeltaTransferStrategy::process
    source_path: Option<PathBuf>,
    /// Expected target file size from the file list entry.
    target_size: u64,
    /// Literal bytes received over the wire during delta token processing.
    /// Only meaningful for delta transfers; zero for whole-file transfers.
    literal_bytes: u64,
    /// Bytes matched from the basis file during delta token processing.
    /// Only meaningful for delta transfers; zero for whole-file transfers.
    matched_bytes: u64,
    /// Transfer kind (whole-file vs delta).
    kind: DeltaWorkKind,
}

/// Distinguishes whole-file transfers from delta-based transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaWorkKind {
    /// Whole-file transfer - no basis file exists or delta is not beneficial.
    WholeFile,
    /// Delta transfer - a basis file exists and will be used for block matching.
    Delta,
}

impl DeltaWork {
    /// Creates a new whole-file transfer work item.
    #[must_use]
    pub fn whole_file(ndx: impl Into<FileNdx>, dest_path: PathBuf, target_size: u64) -> Self {
        Self {
            ndx: ndx.into(),
            sequence: 0,
            dest_path,
            basis_path: None,
            source_path: None,
            target_size,
            literal_bytes: 0,
            matched_bytes: 0,
            kind: DeltaWorkKind::WholeFile,
        }
    }

    /// Creates a new delta transfer work item with actual literal/matched byte counts
    /// accumulated during delta token stream processing.
    ///
    /// # Arguments
    ///
    /// * `ndx` - File list index
    /// * `dest_path` - Destination path for the reconstructed file
    /// * `basis_path` - Path to the basis file used for block matching
    /// * `target_size` - Expected target file size
    /// * `literal_bytes` - Bytes received as literal data over the wire
    /// * `matched_bytes` - Bytes copied from the basis file via block references
    #[must_use]
    pub fn delta(
        ndx: impl Into<FileNdx>,
        dest_path: PathBuf,
        basis_path: PathBuf,
        target_size: u64,
        literal_bytes: u64,
        matched_bytes: u64,
    ) -> Self {
        Self {
            ndx: ndx.into(),
            sequence: 0,
            dest_path,
            basis_path: Some(basis_path),
            source_path: None,
            target_size,
            literal_bytes,
            matched_bytes,
            kind: DeltaWorkKind::Delta,
        }
    }

    /// Creates a delta transfer work item that triggers self-contained
    /// block matching against a local source file.
    ///
    /// Unlike [`DeltaWork::delta`], this constructor stores both the basis and
    /// the source paths, enabling
    /// [`DeltaTransferStrategy::process`](super::strategy::DeltaTransferStrategy::process)
    /// to run the full [`DeltaGenerator`](matching::DeltaGenerator) pipeline:
    /// signature generation from the basis, rolling+strong checksum block
    /// matching against the source, literal/COPY token emission, and applied
    /// script written to the destination. Returned stats reflect the actual
    /// matching outcome.
    ///
    /// # Arguments
    ///
    /// * `ndx` - File list index
    /// * `dest_path` - Destination path where the reconstructed file is written
    /// * `basis_path` - Path to the basis file used for block matching
    /// * `source_path` - Path to the source file consumed by the matcher
    /// * `target_size` - Expected target file size (typically equals source size)
    #[must_use]
    pub fn delta_with_source(
        ndx: impl Into<FileNdx>,
        dest_path: PathBuf,
        basis_path: PathBuf,
        source_path: PathBuf,
        target_size: u64,
    ) -> Self {
        Self {
            ndx: ndx.into(),
            sequence: 0,
            dest_path,
            basis_path: Some(basis_path),
            source_path: Some(source_path),
            target_size,
            literal_bytes: 0,
            matched_bytes: 0,
            kind: DeltaWorkKind::Delta,
        }
    }

    /// Returns the file list index (NDX).
    #[must_use]
    pub const fn ndx(&self) -> FileNdx {
        self.ndx
    }

    /// Returns the destination path.
    #[must_use]
    pub fn dest_path(&self) -> &std::path::Path {
        &self.dest_path
    }

    /// Returns the basis path, if this is a delta transfer.
    #[must_use]
    pub fn basis_path(&self) -> Option<&std::path::Path> {
        self.basis_path.as_deref()
    }

    /// Returns the source path used for self-contained delta computation.
    ///
    /// Present only when the work item was constructed via
    /// [`DeltaWork::delta_with_source`]. When `Some`, the strategy runs the
    /// full block-matching pipeline rather than relying on pre-computed stats.
    #[must_use]
    pub fn source_path(&self) -> Option<&std::path::Path> {
        self.source_path.as_deref()
    }

    /// Returns the expected target file size.
    #[must_use]
    pub const fn target_size(&self) -> u64 {
        self.target_size
    }

    /// Returns the literal bytes accumulated during delta token processing.
    #[must_use]
    pub const fn literal_bytes(&self) -> u64 {
        self.literal_bytes
    }

    /// Returns the matched bytes accumulated during delta token processing.
    #[must_use]
    pub const fn matched_bytes(&self) -> u64 {
        self.matched_bytes
    }

    /// Returns the transfer kind.
    #[must_use]
    pub const fn kind(&self) -> DeltaWorkKind {
        self.kind
    }

    /// Returns `true` if this is a delta transfer (has a basis file).
    #[must_use]
    pub const fn is_delta(&self) -> bool {
        matches!(self.kind, DeltaWorkKind::Delta)
    }

    /// Returns the pipeline sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Sets the pipeline sequence number.
    ///
    /// Called by the producer before dispatching to the work queue so that
    /// the consumer can reconstruct the original submission order via
    /// [`ReorderBuffer`](super::reorder::ReorderBuffer).
    pub fn set_sequence(&mut self, seq: u64) {
        self.sequence = seq;
    }

    /// Sets the pipeline sequence number (builder-style).
    ///
    /// Returns `self` for chaining. Equivalent to calling
    /// [`set_sequence`](Self::set_sequence) but consumes and returns the item.
    #[must_use]
    pub const fn with_sequence(mut self, seq: u64) -> Self {
        self.sequence = seq;
        self
    }

    /// Consumes the work item and returns its components.
    ///
    /// Returns `(ndx, dest_path, basis_path, target_size, literal_bytes, matched_bytes)`.
    #[must_use]
    pub fn into_parts(self) -> (FileNdx, PathBuf, Option<PathBuf>, u64, u64, u64) {
        (
            self.ndx,
            self.dest_path,
            self.basis_path,
            self.target_size,
            self.literal_bytes,
            self.matched_bytes,
        )
    }
}

/// Outcome of a concurrent delta computation.
///
/// Produced by a worker thread after processing a [`DeltaWork`] item.
/// Contains transfer statistics and the file index for correlation with
/// the original request.
///
/// # Upstream Reference
///
/// Aggregates the per-file statistics that upstream rsync tracks across
/// `receive_data()` and `recv_files()` in `receiver.c`.
#[derive(Debug, Clone, Default)]
pub struct DeltaResult {
    /// File list index (NDX) - correlates with the originating [`DeltaWork`].
    ndx: FileNdx,
    /// Pipeline sequence number for reordering results from concurrent workers.
    ///
    /// Assigned by the producer before dispatching to the work queue so that
    /// the consumer can reconstruct the original submission order even when
    /// workers complete out of order.
    sequence: u64,
    /// Total bytes written to the output file.
    bytes_written: u64,
    /// Literal bytes received over the wire.
    literal_bytes: u64,
    /// Bytes copied from the basis file via block references.
    matched_bytes: u64,
    /// Outcome status of the delta operation.
    status: DeltaResultStatus,
}

/// Status of a completed delta operation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeltaResultStatus {
    /// Delta application completed successfully.
    #[default]
    Success,
    /// Delta failed and the file should be retried in phase 2.
    ///
    /// Mirrors upstream `receiver.c:960-968` where checksum mismatch triggers
    /// `MSG_REDO`.
    NeedsRedo {
        /// Human-readable reason for the redo.
        reason: String,
    },
    /// Delta failed with a non-recoverable error.
    Failed {
        /// Human-readable error description.
        reason: String,
    },
}

impl DeltaResult {
    /// Creates a successful result.
    #[must_use]
    pub fn success(
        ndx: impl Into<FileNdx>,
        bytes_written: u64,
        literal_bytes: u64,
        matched_bytes: u64,
    ) -> Self {
        Self {
            ndx: ndx.into(),
            sequence: 0,
            bytes_written,
            literal_bytes,
            matched_bytes,
            status: DeltaResultStatus::Success,
        }
    }

    /// Creates a redo result (checksum mismatch in phase 1).
    #[must_use]
    pub fn needs_redo(ndx: impl Into<FileNdx>, reason: String) -> Self {
        Self {
            ndx: ndx.into(),
            status: DeltaResultStatus::NeedsRedo { reason },
            ..Default::default()
        }
    }

    /// Creates a failed result (non-recoverable error).
    #[must_use]
    pub fn failed(ndx: impl Into<FileNdx>, reason: String) -> Self {
        Self {
            ndx: ndx.into(),
            status: DeltaResultStatus::Failed { reason },
            ..Default::default()
        }
    }

    /// Sets the pipeline sequence number.
    ///
    /// Typically called by the producer before dispatching the work item so
    /// the consumer can reorder results via [`ReorderBuffer`].
    ///
    /// [`ReorderBuffer`]: super::reorder::ReorderBuffer
    #[must_use]
    pub const fn with_sequence(mut self, sequence: u64) -> Self {
        self.sequence = sequence;
        self
    }

    /// Returns the file list index (NDX).
    #[must_use]
    pub const fn ndx(&self) -> FileNdx {
        self.ndx
    }

    /// Returns the pipeline sequence number.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the total bytes written.
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Returns the literal bytes received.
    #[must_use]
    pub const fn literal_bytes(&self) -> u64 {
        self.literal_bytes
    }

    /// Returns the matched bytes copied from basis.
    #[must_use]
    pub const fn matched_bytes(&self) -> u64 {
        self.matched_bytes
    }

    /// Returns the result status.
    #[must_use]
    pub const fn status(&self) -> &DeltaResultStatus {
        &self.status
    }

    /// Returns `true` if the delta completed successfully.
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self.status, DeltaResultStatus::Success)
    }

    /// Returns `true` if the file needs to be retried.
    #[must_use]
    pub fn needs_retry(&self) -> bool {
        matches!(self.status, DeltaResultStatus::NeedsRedo { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_file_work_has_no_basis() {
        let work = DeltaWork::whole_file(0u32, PathBuf::from("/dest/file.txt"), 1024);
        assert_eq!(work.ndx(), FileNdx::new(0));
        assert_eq!(work.dest_path(), std::path::Path::new("/dest/file.txt"));
        assert!(work.basis_path().is_none());
        assert_eq!(work.target_size(), 1024);
        assert_eq!(work.kind(), DeltaWorkKind::WholeFile);
        assert!(!work.is_delta());
    }

    #[test]
    fn delta_work_has_basis() {
        let work = DeltaWork::delta(
            5u32,
            PathBuf::from("/dest/file.txt"),
            PathBuf::from("/basis/file.txt"),
            2048,
            800,
            1248,
        );
        assert_eq!(work.ndx(), FileNdx::new(5));
        assert_eq!(work.dest_path(), std::path::Path::new("/dest/file.txt"));
        assert_eq!(
            work.basis_path(),
            Some(std::path::Path::new("/basis/file.txt"))
        );
        assert_eq!(work.target_size(), 2048);
        assert_eq!(work.literal_bytes(), 800);
        assert_eq!(work.matched_bytes(), 1248);
        assert_eq!(work.kind(), DeltaWorkKind::Delta);
        assert!(work.is_delta());
    }

    #[test]
    fn work_into_parts_returns_components() {
        let work = DeltaWork::delta(
            3u32,
            PathBuf::from("/dest"),
            PathBuf::from("/basis"),
            500,
            200,
            300,
        );
        let (ndx, dest, basis, size, literal, matched) = work.into_parts();
        assert_eq!(ndx, FileNdx::new(3));
        assert_eq!(dest, PathBuf::from("/dest"));
        assert_eq!(basis, Some(PathBuf::from("/basis")));
        assert_eq!(size, 500);
        assert_eq!(literal, 200);
        assert_eq!(matched, 300);
    }

    #[test]
    fn whole_file_into_parts_has_no_basis() {
        let work = DeltaWork::whole_file(1, PathBuf::from("/dest"), 100);
        let (_, _, basis, _, _, _) = work.into_parts();
        assert!(basis.is_none());
    }

    #[test]
    fn work_clone() {
        let work = DeltaWork::delta(
            7,
            PathBuf::from("/dest/clone.txt"),
            PathBuf::from("/basis/clone.txt"),
            4096,
            1500,
            2596,
        );
        let cloned = work.clone();
        assert_eq!(cloned.ndx(), FileNdx::new(7));
        assert_eq!(cloned.dest_path(), std::path::Path::new("/dest/clone.txt"));
        assert!(cloned.is_delta());
    }

    #[test]
    fn work_kind_equality() {
        assert_eq!(DeltaWorkKind::WholeFile, DeltaWorkKind::WholeFile);
        assert_eq!(DeltaWorkKind::Delta, DeltaWorkKind::Delta);
        assert_ne!(DeltaWorkKind::WholeFile, DeltaWorkKind::Delta);
    }

    #[test]
    fn success_result() {
        let result = DeltaResult::success(42u32, 1000, 300, 700);
        assert_eq!(result.ndx(), FileNdx::new(42));
        assert_eq!(result.bytes_written(), 1000);
        assert_eq!(result.literal_bytes(), 300);
        assert_eq!(result.matched_bytes(), 700);
        assert!(result.is_success());
        assert!(!result.needs_retry());
    }

    #[test]
    fn needs_redo_result() {
        let result = DeltaResult::needs_redo(10u32, "checksum mismatch".to_string());
        assert_eq!(result.ndx(), FileNdx::new(10));
        assert_eq!(result.bytes_written(), 0);
        assert!(!result.is_success());
        assert!(result.needs_retry());
    }

    #[test]
    fn failed_result() {
        let result = DeltaResult::failed(99u32, "I/O error".to_string());
        assert_eq!(result.ndx(), FileNdx::new(99));
        assert!(!result.is_success());
        assert!(!result.needs_retry());
    }

    #[test]
    fn default_result_is_success_with_zeroes() {
        let result = DeltaResult::default();
        assert_eq!(result.ndx(), FileNdx::new(0));
        assert_eq!(result.bytes_written(), 0);
        assert_eq!(result.literal_bytes(), 0);
        assert_eq!(result.matched_bytes(), 0);
        assert!(result.is_success());
    }

    #[test]
    fn result_clone() {
        let result = DeltaResult::success(5u32, 500, 200, 300);
        let cloned = result.clone();
        assert_eq!(cloned.ndx(), FileNdx::new(5));
        assert_eq!(cloned.bytes_written(), 500);
        assert!(cloned.is_success());
    }

    #[test]
    fn result_status_equality() {
        assert_eq!(DeltaResultStatus::Success, DeltaResultStatus::Success);
        assert_ne!(
            DeltaResultStatus::Success,
            DeltaResultStatus::NeedsRedo {
                reason: String::new()
            }
        );
    }

    #[test]
    fn result_status_default_is_success() {
        assert_eq!(DeltaResultStatus::default(), DeltaResultStatus::Success);
    }

    #[test]
    fn work_default_sequence_is_zero() {
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest"), 100);
        assert_eq!(work.sequence(), 0);
    }

    #[test]
    fn work_set_sequence() {
        let mut work = DeltaWork::whole_file(0, PathBuf::from("/dest"), 100);
        work.set_sequence(42);
        assert_eq!(work.sequence(), 42);
    }

    #[test]
    fn work_with_sequence_builder() {
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest"), 100).with_sequence(7);
        assert_eq!(work.sequence(), 7);
    }

    #[test]
    fn work_sequence_preserved_on_clone() {
        let work = DeltaWork::delta(
            1,
            PathBuf::from("/dest"),
            PathBuf::from("/basis"),
            200,
            80,
            120,
        )
        .with_sequence(99);
        let cloned = work.clone();
        assert_eq!(cloned.sequence(), 99);
    }

    #[test]
    fn delta_work_default_sequence_is_zero() {
        let work = DeltaWork::delta(
            5,
            PathBuf::from("/dest"),
            PathBuf::from("/basis"),
            1024,
            400,
            624,
        );
        assert_eq!(work.sequence(), 0);
    }

    #[test]
    fn result_default_sequence_is_zero() {
        let result = DeltaResult::default();
        assert_eq!(result.sequence(), 0);
    }

    #[test]
    fn result_with_sequence() {
        let result = DeltaResult::success(1, 100, 50, 50).with_sequence(10);
        assert_eq!(result.sequence(), 10);
        assert_eq!(result.ndx(), FileNdx::new(1));
    }

    #[test]
    fn result_sequence_preserved_on_clone() {
        let result = DeltaResult::success(0, 0, 0, 0).with_sequence(55);
        let cloned = result.clone();
        assert_eq!(cloned.sequence(), 55);
    }

    #[test]
    fn result_needs_redo_default_sequence() {
        let result = DeltaResult::needs_redo(3, "mismatch".to_string());
        assert_eq!(result.sequence(), 0);
    }

    #[test]
    fn result_failed_with_sequence() {
        let result = DeltaResult::failed(7u32, "I/O error".to_string()).with_sequence(42);
        assert_eq!(result.sequence(), 42);
        assert!(!result.is_success());
    }

    #[test]
    fn file_ndx_new_and_get_roundtrip() {
        for val in [0u32, 1, 42, u32::MAX] {
            let ndx = FileNdx::new(val);
            assert_eq!(ndx.get(), val);
        }
    }

    #[test]
    fn file_ndx_ordering_matches_u32() {
        let a = FileNdx::new(1);
        let b = FileNdx::new(2);
        let c = FileNdx::new(2);
        assert!(a < b);
        assert!(b > a);
        assert_eq!(b, c);
        assert!(a <= b);
        assert!(b >= a);
    }

    #[test]
    fn file_ndx_display() {
        assert_eq!(FileNdx::new(0).to_string(), "0");
        assert_eq!(FileNdx::new(42).to_string(), "42");
        assert_eq!(FileNdx::new(u32::MAX).to_string(), u32::MAX.to_string());
    }

    #[test]
    fn file_ndx_from_u32() {
        let ndx: FileNdx = 7u32.into();
        assert_eq!(ndx, FileNdx::new(7));
    }

    #[test]
    fn file_ndx_btreemap_key() {
        use std::collections::BTreeMap;
        let mut map = BTreeMap::new();
        map.insert(FileNdx::new(3), "third");
        map.insert(FileNdx::new(1), "first");
        map.insert(FileNdx::new(2), "second");

        let keys: Vec<FileNdx> = map.keys().copied().collect();
        assert_eq!(
            keys,
            vec![FileNdx::new(1), FileNdx::new(2), FileNdx::new(3)]
        );
        assert_eq!(map[&FileNdx::new(2)], "second");
    }

    #[test]
    fn file_ndx_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(FileNdx::new(5));
        set.insert(FileNdx::new(5));
        set.insert(FileNdx::new(10));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn file_ndx_default_is_zero() {
        assert_eq!(FileNdx::default(), FileNdx::new(0));
    }

    #[test]
    fn file_ndx_copy_semantics() {
        let a = FileNdx::new(42);
        let b = a;
        assert_eq!(a, b); // `a` still usable after copy
    }

    #[test]
    fn delta_with_source_records_source_path() {
        let work = DeltaWork::delta_with_source(
            4u32,
            PathBuf::from("/dest/d.txt"),
            PathBuf::from("/basis/d.txt"),
            PathBuf::from("/source/d.txt"),
            8192,
        );
        assert_eq!(
            work.source_path(),
            Some(std::path::Path::new("/source/d.txt"))
        );
        assert_eq!(
            work.basis_path(),
            Some(std::path::Path::new("/basis/d.txt"))
        );
        assert_eq!(work.target_size(), 8192);
        assert_eq!(work.literal_bytes(), 0);
        assert_eq!(work.matched_bytes(), 0);
        assert!(work.is_delta());
    }

    #[test]
    fn delta_without_source_returns_none_for_source_path() {
        let work = DeltaWork::delta(
            1u32,
            PathBuf::from("/dest"),
            PathBuf::from("/basis"),
            128,
            32,
            96,
        );
        assert!(work.source_path().is_none());
    }

    #[test]
    fn whole_file_has_no_source_path() {
        let work = DeltaWork::whole_file(0u32, PathBuf::from("/dest"), 64);
        assert!(work.source_path().is_none());
    }
}
