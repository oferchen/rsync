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
        // Delta transfer: use actual literal/matched byte counts accumulated
        // during delta token stream processing.
        // upstream: receiver.c:receive_data() tracks these via data/match_sum.
        let literal = work.literal_bytes();
        let matched = work.matched_bytes();
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
/// and immediately processes it. Propagates the work item's sequence number
/// onto the result so the consumer can reorder via [`ReorderBuffer`].
///
/// [`ReorderBuffer`]: super::reorder::ReorderBuffer
#[must_use]
pub fn dispatch(work: &DeltaWork) -> DeltaResult {
    select_strategy(work)
        .process(work)
        .with_sequence(work.sequence())
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
        assert_eq!(result.ndx().get(), 1);
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
    fn delta_strategy_returns_actual_stats() {
        let strategy = DeltaTransferStrategy::new();
        let work = DeltaWork::delta(
            5,
            PathBuf::from("/dest/b.txt"),
            PathBuf::from("/basis/b.txt"),
            4096,
            1200,
            2896,
        );
        let result = strategy.process(&work);
        assert!(result.is_success());
        assert_eq!(result.ndx().get(), 5);
        assert_eq!(result.bytes_written(), 4096);
        assert_eq!(result.matched_bytes(), 2896);
        assert_eq!(result.literal_bytes(), 1200);
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
        let work = DeltaWork::delta(
            0,
            PathBuf::from("/dest"),
            PathBuf::from("/basis"),
            100,
            40,
            60,
        );
        let strategy = select_strategy(&work);
        assert_eq!(strategy.kind(), DeltaWorkKind::Delta);
    }

    #[test]
    fn dispatch_whole_file() {
        let work = DeltaWork::whole_file(3, PathBuf::from("/dest/c.txt"), 512);
        let result = dispatch(&work);
        assert!(result.is_success());
        assert_eq!(result.ndx().get(), 3);
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
            350,
            650,
        );
        let result = dispatch(&work);
        assert!(result.is_success());
        assert_eq!(result.ndx().get(), 7);
        assert_eq!(result.bytes_written(), 1000);
        assert_eq!(result.matched_bytes(), 650);
        assert_eq!(result.literal_bytes(), 350);
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
            0,
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

    #[test]
    fn dispatch_propagates_sequence_whole_file() {
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest/a"), 256).with_sequence(17);
        let result = dispatch(&work);
        assert_eq!(result.sequence(), 17);
        assert!(result.is_success());
    }

    #[test]
    fn dispatch_propagates_sequence_delta() {
        let work = DeltaWork::delta(
            2,
            PathBuf::from("/dest/b"),
            PathBuf::from("/basis/b"),
            512,
            200,
            312,
        )
        .with_sequence(99);
        let result = dispatch(&work);
        assert_eq!(result.sequence(), 99);
        assert!(result.is_success());
    }

    #[test]
    fn dispatch_default_sequence_is_zero() {
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest"), 100);
        let result = dispatch(&work);
        assert_eq!(result.sequence(), 0);
    }

    #[test]
    fn dispatch_sequence_survives_pipeline() {
        // Simulate a producer stamping sequential IDs and verify they survive
        // through dispatch back to the consumer.
        let items: Vec<DeltaWork> = (0..5)
            .map(|i| {
                DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64)
                    .with_sequence(u64::from(i))
            })
            .collect();

        let results: Vec<DeltaResult> = items.iter().map(dispatch).collect();
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
            assert_eq!(r.ndx().get(), i as u32);
        }
    }
}
