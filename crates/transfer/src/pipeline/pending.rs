//! Pending transfer tracking for pipelined requests.
//!
//! Tracks metadata needed to process responses for in-flight file requests.

use std::path::PathBuf;

use engine::signature::FileSignature;

/// Tracks an in-flight file transfer request.
///
/// When we send a file request (NDX + iflags + sum_head + signature) to the
/// sender, we store this information to process the response when it arrives.
///
/// # Memory Layout
///
/// Approximately 500 bytes per pending transfer:
/// - ndx: 4 bytes
/// - file_path: 24 bytes + path length (~50 bytes average)
/// - basis_path: 24 bytes + optional path (~50 bytes)
/// - signature: ~300 bytes (for typical file with ~50 blocks)
/// - target_size: 8 bytes
/// - checksum_seed: 4 bytes
#[derive(Debug)]
pub struct PendingTransfer {
    /// File index (NDX) that identifies this transfer.
    ndx: i32,
    /// Destination path for the file being transferred.
    file_path: PathBuf,
    /// Path to the basis file used for delta transfer, if any.
    basis_path: Option<PathBuf>,
    /// FileSignature generated from basis file, if any.
    signature: Option<FileSignature>,
    /// Expected file size from file list.
    target_size: u64,
}

impl PendingTransfer {
    /// Creates a new pending transfer for a file that needs full transfer.
    ///
    /// Use this when no basis file exists and we're receiving the complete file.
    #[must_use]
    pub fn new_full_transfer(ndx: i32, file_path: PathBuf, target_size: u64) -> Self {
        Self {
            ndx,
            file_path,
            basis_path: None,
            signature: None,
            target_size,
        }
    }

    /// Creates a new pending transfer for a delta transfer.
    ///
    /// Use this when a basis file exists and we're receiving only changes.
    #[must_use]
    pub fn new_delta_transfer(
        ndx: i32,
        file_path: PathBuf,
        basis_path: PathBuf,
        signature: FileSignature,
        target_size: u64,
    ) -> Self {
        Self {
            ndx,
            file_path,
            basis_path: Some(basis_path),
            signature: Some(signature),
            target_size,
        }
    }

    /// Returns the file index (NDX) for this transfer.
    #[must_use]
    pub const fn ndx(&self) -> i32 {
        self.ndx
    }

    /// Returns the destination file path.
    #[must_use]
    pub fn file_path(&self) -> &PathBuf {
        &self.file_path
    }

    /// Returns the basis file path, if this is a delta transfer.
    #[must_use]
    pub fn basis_path(&self) -> Option<&PathBuf> {
        self.basis_path.as_ref()
    }

    /// Returns the signature, if this is a delta transfer.
    #[must_use]
    pub fn signature(&self) -> Option<&FileSignature> {
        self.signature.as_ref()
    }

    /// Returns the expected target file size.
    #[must_use]
    pub const fn target_size(&self) -> u64 {
        self.target_size
    }

    /// Returns true if this is a delta transfer (has basis file).
    #[must_use]
    pub const fn is_delta_transfer(&self) -> bool {
        self.basis_path.is_some()
    }

    /// Consumes the pending transfer and returns its components.
    ///
    /// Used when processing the response to avoid cloning large data.
    #[must_use]
    pub fn into_parts(self) -> (PathBuf, Option<PathBuf>, Option<FileSignature>) {
        (self.file_path, self.basis_path, self.signature)
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU8};

    use engine::SignatureLayout;

    use super::*;

    /// Creates an empty signature for testing.
    fn empty_signature() -> FileSignature {
        let layout = SignatureLayout::from_raw_parts(
            NonZeroU32::new(700).unwrap(),
            0,
            0,
            NonZeroU8::new(16).unwrap(),
        );
        FileSignature::from_raw_parts(layout, vec![], 0)
    }

    #[test]
    fn new_full_transfer_has_no_basis() {
        let transfer = PendingTransfer::new_full_transfer(0, PathBuf::from("/tmp/test"), 1024);

        assert_eq!(transfer.ndx(), 0);
        assert_eq!(transfer.file_path(), &PathBuf::from("/tmp/test"));
        assert!(transfer.basis_path().is_none());
        assert!(transfer.signature().is_none());
        assert_eq!(transfer.target_size(), 1024);
        assert!(!transfer.is_delta_transfer());
    }

    #[test]
    fn new_delta_transfer_has_basis() {
        let signature = empty_signature();
        let transfer = PendingTransfer::new_delta_transfer(
            5,
            PathBuf::from("/tmp/dest/file.txt"),
            PathBuf::from("/tmp/basis/file.txt"),
            signature,
            2048,
        );

        assert_eq!(transfer.ndx(), 5);
        assert_eq!(transfer.file_path(), &PathBuf::from("/tmp/dest/file.txt"));
        assert_eq!(
            transfer.basis_path(),
            Some(&PathBuf::from("/tmp/basis/file.txt"))
        );
        assert!(transfer.signature().is_some());
        assert_eq!(transfer.target_size(), 2048);
        assert!(transfer.is_delta_transfer());
    }

    #[test]
    fn into_parts_consumes_transfer() {
        let signature = empty_signature();
        let transfer = PendingTransfer::new_delta_transfer(
            1,
            PathBuf::from("/dest"),
            PathBuf::from("/basis"),
            signature,
            100,
        );

        let (file_path, basis_path, sig) = transfer.into_parts();

        assert_eq!(file_path, PathBuf::from("/dest"));
        assert_eq!(basis_path, Some(PathBuf::from("/basis")));
        assert!(sig.is_some());
    }
}
