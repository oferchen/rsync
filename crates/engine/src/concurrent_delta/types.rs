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
    ndx: u32,
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
    pub fn whole_file(ndx: u32, dest_path: PathBuf, target_size: u64) -> Self {
        Self {
            ndx,
            sequence: 0,
            dest_path,
            basis_path: None,
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
        ndx: u32,
        dest_path: PathBuf,
        basis_path: PathBuf,
        target_size: u64,
        literal_bytes: u64,
        matched_bytes: u64,
    ) -> Self {
        Self {
            ndx,
            sequence: 0,
            dest_path,
            basis_path: Some(basis_path),
            target_size,
            literal_bytes,
            matched_bytes,
            kind: DeltaWorkKind::Delta,
        }
    }

    /// Returns the file list index (NDX).
    #[must_use]
    pub const fn ndx(&self) -> u32 {
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
    pub fn into_parts(self) -> (u32, PathBuf, Option<PathBuf>, u64, u64, u64) {
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
    ndx: u32,
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
    pub fn success(ndx: u32, bytes_written: u64, literal_bytes: u64, matched_bytes: u64) -> Self {
        Self {
            ndx,
            sequence: 0,
            bytes_written,
            literal_bytes,
            matched_bytes,
            status: DeltaResultStatus::Success,
        }
    }

    /// Creates a redo result (checksum mismatch in phase 1).
    #[must_use]
    pub fn needs_redo(ndx: u32, reason: String) -> Self {
        Self {
            ndx,
            status: DeltaResultStatus::NeedsRedo { reason },
            ..Default::default()
        }
    }

    /// Creates a failed result (non-recoverable error).
    #[must_use]
    pub fn failed(ndx: u32, reason: String) -> Self {
        Self {
            ndx,
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
    pub const fn ndx(&self) -> u32 {
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
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest/file.txt"), 1024);
        assert_eq!(work.ndx(), 0);
        assert_eq!(work.dest_path(), std::path::Path::new("/dest/file.txt"));
        assert!(work.basis_path().is_none());
        assert_eq!(work.target_size(), 1024);
        assert_eq!(work.kind(), DeltaWorkKind::WholeFile);
        assert!(!work.is_delta());
    }

    #[test]
    fn delta_work_has_basis() {
        let work = DeltaWork::delta(
            5,
            PathBuf::from("/dest/file.txt"),
            PathBuf::from("/basis/file.txt"),
            2048,
            800,
            1248,
        );
        assert_eq!(work.ndx(), 5);
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
            3,
            PathBuf::from("/dest"),
            PathBuf::from("/basis"),
            500,
            200,
            300,
        );
        let (ndx, dest, basis, size, literal, matched) = work.into_parts();
        assert_eq!(ndx, 3);
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
        assert_eq!(cloned.ndx(), 7);
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
        let result = DeltaResult::success(42, 1000, 300, 700);
        assert_eq!(result.ndx(), 42);
        assert_eq!(result.bytes_written(), 1000);
        assert_eq!(result.literal_bytes(), 300);
        assert_eq!(result.matched_bytes(), 700);
        assert!(result.is_success());
        assert!(!result.needs_retry());
    }

    #[test]
    fn needs_redo_result() {
        let result = DeltaResult::needs_redo(10, "checksum mismatch".to_string());
        assert_eq!(result.ndx(), 10);
        assert_eq!(result.bytes_written(), 0);
        assert!(!result.is_success());
        assert!(result.needs_retry());
    }

    #[test]
    fn failed_result() {
        let result = DeltaResult::failed(99, "I/O error".to_string());
        assert_eq!(result.ndx(), 99);
        assert!(!result.is_success());
        assert!(!result.needs_retry());
    }

    #[test]
    fn default_result_is_success_with_zeroes() {
        let result = DeltaResult::default();
        assert_eq!(result.ndx(), 0);
        assert_eq!(result.bytes_written(), 0);
        assert_eq!(result.literal_bytes(), 0);
        assert_eq!(result.matched_bytes(), 0);
        assert!(result.is_success());
    }

    #[test]
    fn result_clone() {
        let result = DeltaResult::success(5, 500, 200, 300);
        let cloned = result.clone();
        assert_eq!(cloned.ndx(), 5);
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
        assert_eq!(result.ndx(), 1);
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
        let result = DeltaResult::failed(7, "I/O error".to_string()).with_sequence(42);
        assert_eq!(result.sequence(), 42);
        assert!(!result.is_success());
    }
}
