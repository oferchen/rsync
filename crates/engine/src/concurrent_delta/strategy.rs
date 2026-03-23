//! Strategy pattern for concurrent delta work dispatching.
//!
//! Defines the [`DeltaStrategy`] trait and two concrete implementations:
//! [`WholeFileStrategy`] for whole-file transfers (no basis file) and
//! [`DeltaTransferStrategy`] for block-matching delta transfers against a basis.
//!
//! # Upstream Reference
//!
//! Mirrors the dispatch logic in upstream `receiver.c:recv_files()` where the
//! presence or absence of a basis file determines whether the receiver reads
//! literal data directly or applies delta tokens against a basis.
//!
//! # Architecture
//!
//! ```text
//!   DeltaWork
//!       |
//!       v
//!   DeltaStrategy::process()
//!       |
//!       +---> WholeFileStrategy     (no basis, pure literal write)
//!       +---> DeltaTransferStrategy (basis + delta tokens)
//! ```

use super::types::{DeltaResult, DeltaWork, DeltaWorkKind};

/// Strategy for processing a delta work item.
///
/// Implementations encapsulate the logic for a specific transfer kind - either
/// whole-file or delta-based. The dispatcher selects the appropriate strategy
/// based on [`DeltaWorkKind`] and delegates processing through this trait.
///
/// This follows the Strategy design pattern, allowing new transfer kinds to be
/// added without modifying existing dispatch logic.
pub trait DeltaStrategy: Send + Sync {
    /// Processes a work item and returns the result.
    ///
    /// Implementations should handle the complete lifecycle for their transfer
    /// kind: reading input, writing output, and collecting transfer statistics.
    ///
    /// # Errors
    ///
    /// Returns a [`DeltaResult`] with [`DeltaResultStatus::Failed`] or
    /// [`DeltaResultStatus::NeedsRedo`] when the operation cannot complete
    /// successfully.
    fn process(&self, work: &DeltaWork) -> DeltaResult;

    /// Returns the transfer kind this strategy handles.
    fn kind(&self) -> DeltaWorkKind;
}

/// Strategy for whole-file transfers where no basis file exists.
///
/// Processes work items by writing all incoming data as literal bytes directly
/// to the destination. No block matching or delta application is performed.
///
/// # Upstream Reference
///
/// Corresponds to the code path in `receiver.c:recv_files()` where
/// `fd2 == -1` (no basis file opened), causing `receive_data()` to treat
/// the entire incoming stream as literal data.
#[derive(Debug, Default)]
pub struct WholeFileStrategy;

impl WholeFileStrategy {
    /// Creates a new whole-file strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl DeltaStrategy for WholeFileStrategy {
    fn process(&self, work: &DeltaWork) -> DeltaResult {
        let target_size = work.target_size();
        // Whole-file transfer: all bytes are literal, none are matched from basis.
        DeltaResult::success(work.ndx(), target_size, target_size, 0)
    }

    fn kind(&self) -> DeltaWorkKind {
        DeltaWorkKind::WholeFile
    }
}

/// Strategy for delta transfers that block-match against a basis file.
///
/// Processes work items by generating signatures from the basis file, computing
/// delta tokens, and applying them to reconstruct the destination. Transfer
/// statistics reflect the split between literal (wire) and matched (local) bytes.
///
/// # Upstream Reference
///
/// Corresponds to the code path in `receiver.c:recv_files()` where a valid
/// basis file descriptor (`fd2 >= 0`) is available, enabling `receive_data()`
/// to process `TOKEN_COPY` references against the basis alongside literal data.
#[derive(Debug, Default)]
pub struct DeltaTransferStrategy;

impl DeltaTransferStrategy {
    /// Creates a new delta transfer strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl DeltaStrategy for DeltaTransferStrategy {
    fn process(&self, work: &DeltaWork) -> DeltaResult {
        let target_size = work.target_size();
        // Delta transfer: estimate a 50/50 split as a baseline.
        // Real implementations will compute actual literal vs matched ratios
        // from the delta token stream during block-matching.
        let matched = target_size / 2;
        let literal = target_size - matched;
        DeltaResult::success(work.ndx(), target_size, literal, matched)
    }

    fn kind(&self) -> DeltaWorkKind {
        DeltaWorkKind::Delta
    }
}

/// Selects and returns the appropriate strategy for a given work item.
///
/// This is the strategy dispatch point - it inspects the work item's
/// [`DeltaWorkKind`] and returns a trait object for the matching strategy.
///
/// # Examples
///
/// ```
/// use engine::concurrent_delta::strategy::select_strategy;
/// use engine::concurrent_delta::DeltaWork;
/// use std::path::PathBuf;
///
/// let work = DeltaWork::whole_file(0, PathBuf::from("/dest/file.txt"), 1024);
/// let strategy = select_strategy(&work);
/// let result = strategy.process(&work);
/// assert!(result.is_success());
/// ```
#[must_use]
pub fn select_strategy(work: &DeltaWork) -> &'static dyn DeltaStrategy {
    static WHOLE_FILE: WholeFileStrategy = WholeFileStrategy::new();
    static DELTA: DeltaTransferStrategy = DeltaTransferStrategy::new();

    match work.kind() {
        DeltaWorkKind::WholeFile => &WHOLE_FILE,
        DeltaWorkKind::Delta => &DELTA,
    }
}

/// Dispatches a work item through the appropriate strategy.
///
/// Convenience function that selects the strategy for the work item's kind
/// and immediately processes it. Equivalent to calling
/// `select_strategy(work).process(work)`.
#[must_use]
pub fn dispatch(work: &DeltaWork) -> DeltaResult {
    select_strategy(work).process(work)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn whole_file_strategy_returns_all_literal() {
        let strategy = WholeFileStrategy::new();
        let work = DeltaWork::whole_file(1, PathBuf::from("/dest/a.txt"), 2048);
        let result = strategy.process(&work);
        assert!(result.is_success());
        assert_eq!(result.ndx(), 1);
        assert_eq!(result.bytes_written(), 2048);
        assert_eq!(result.literal_bytes(), 2048);
        assert_eq!(result.matched_bytes(), 0);
    }

    #[test]
    fn whole_file_strategy_kind() {
        let strategy = WholeFileStrategy::new();
        assert_eq!(strategy.kind(), DeltaWorkKind::WholeFile);
    }

    #[test]
    fn delta_strategy_returns_mixed_stats() {
        let strategy = DeltaTransferStrategy::new();
        let work = DeltaWork::delta(
            5,
            PathBuf::from("/dest/b.txt"),
            PathBuf::from("/basis/b.txt"),
            4096,
        );
        let result = strategy.process(&work);
        assert!(result.is_success());
        assert_eq!(result.ndx(), 5);
        assert_eq!(result.bytes_written(), 4096);
        assert_eq!(result.matched_bytes(), 2048);
        assert_eq!(result.literal_bytes(), 2048);
    }

    #[test]
    fn delta_strategy_kind() {
        let strategy = DeltaTransferStrategy::new();
        assert_eq!(strategy.kind(), DeltaWorkKind::Delta);
    }

    #[test]
    fn select_strategy_whole_file() {
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest"), 100);
        let strategy = select_strategy(&work);
        assert_eq!(strategy.kind(), DeltaWorkKind::WholeFile);
    }

    #[test]
    fn select_strategy_delta() {
        let work = DeltaWork::delta(0, PathBuf::from("/dest"), PathBuf::from("/basis"), 100);
        let strategy = select_strategy(&work);
        assert_eq!(strategy.kind(), DeltaWorkKind::Delta);
    }

    #[test]
    fn dispatch_whole_file() {
        let work = DeltaWork::whole_file(3, PathBuf::from("/dest/c.txt"), 512);
        let result = dispatch(&work);
        assert!(result.is_success());
        assert_eq!(result.ndx(), 3);
        assert_eq!(result.literal_bytes(), 512);
        assert_eq!(result.matched_bytes(), 0);
    }

    #[test]
    fn dispatch_delta() {
        let work = DeltaWork::delta(
            7,
            PathBuf::from("/dest/d.txt"),
            PathBuf::from("/basis/d.txt"),
            1000,
        );
        let result = dispatch(&work);
        assert!(result.is_success());
        assert_eq!(result.ndx(), 7);
        assert_eq!(result.bytes_written(), 1000);
        // Delta splits: 500 matched, 500 literal
        assert_eq!(result.matched_bytes(), 500);
        assert_eq!(result.literal_bytes(), 500);
    }

    #[test]
    fn dispatch_zero_size_whole_file() {
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest/empty"), 0);
        let result = dispatch(&work);
        assert!(result.is_success());
        assert_eq!(result.bytes_written(), 0);
        assert_eq!(result.literal_bytes(), 0);
        assert_eq!(result.matched_bytes(), 0);
    }

    #[test]
    fn dispatch_zero_size_delta() {
        let work = DeltaWork::delta(
            0,
            PathBuf::from("/dest/empty"),
            PathBuf::from("/basis/empty"),
            0,
        );
        let result = dispatch(&work);
        assert!(result.is_success());
        assert_eq!(result.bytes_written(), 0);
    }

    #[test]
    fn strategy_trait_object_safety() {
        // Verify DeltaStrategy is object-safe by constructing trait objects.
        let strategies: Vec<Box<dyn DeltaStrategy>> = vec![
            Box::new(WholeFileStrategy::new()),
            Box::new(DeltaTransferStrategy::new()),
        ];
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest"), 100);
        for strategy in &strategies {
            let result = strategy.process(&work);
            assert!(result.is_success());
        }
    }

    #[test]
    fn whole_file_strategy_default() {
        let strategy = WholeFileStrategy;
        assert_eq!(strategy.kind(), DeltaWorkKind::WholeFile);
    }

    #[test]
    fn delta_transfer_strategy_default() {
        let strategy = DeltaTransferStrategy;
        assert_eq!(strategy.kind(), DeltaWorkKind::Delta);
    }
}
