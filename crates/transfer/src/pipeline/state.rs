//! Pipeline state management for tracking outstanding requests.
//!
//! Maintains the queue of in-flight file transfer requests and provides
//! methods for adding new requests and processing responses in order.

use std::collections::VecDeque;

use super::{PendingTransfer, PipelineConfig};

/// Manages the state of pipelined file transfer requests.
///
/// Tracks outstanding requests in a FIFO queue to ensure responses
/// are processed in the same order as requests (required by delta-encoded
/// NDX protocol).
///
/// # Example
///
/// ```ignore
/// use transfer::pipeline::{PipelineState, PipelineConfig, PendingTransfer};
///
/// let mut state = PipelineState::new(PipelineConfig::default());
///
/// // Queue requests up to window size
/// while state.can_send() && has_more_files() {
///     let transfer = create_pending_transfer();
///     state.push(transfer);
///     send_request_to_sender();
/// }
///
/// // Process a response
/// if let Some(transfer) = state.pop() {
///     process_delta_response(transfer);
/// }
/// ```
#[derive(Debug)]
pub struct PipelineState {
    /// Configuration for the pipeline.
    config: PipelineConfig,
    /// Queue of outstanding requests awaiting responses.
    /// Responses must be processed in FIFO order to match NDX delta encoding.
    pending: VecDeque<PendingTransfer>,
    /// Total number of requests sent (for statistics).
    total_sent: u64,
    /// Total number of responses processed (for statistics).
    total_processed: u64,
}

impl PipelineState {
    /// Creates a new pipeline state with the given configuration.
    #[must_use]
    pub fn new(config: PipelineConfig) -> Self {
        Self {
            pending: VecDeque::with_capacity(config.window_size),
            config,
            total_sent: 0,
            total_processed: 0,
        }
    }

    /// Returns true if we can send another request without exceeding the window.
    #[must_use]
    pub fn can_send(&self) -> bool {
        self.pending.len() < self.config.window_size
    }

    /// Returns the number of currently outstanding requests.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.pending.len()
    }

    /// Returns true if there are no outstanding requests.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns the configured window size.
    #[must_use]
    pub fn window_size(&self) -> usize {
        self.config.window_size
    }

    /// Returns the number of available slots in the pipeline window.
    #[must_use]
    pub fn available_slots(&self) -> usize {
        self.config.window_size.saturating_sub(self.pending.len())
    }

    /// Adds a pending transfer to the queue.
    ///
    /// # Panics
    ///
    /// Panics if the pipeline is full (outstanding >= window_size).
    /// Always check `can_send()` before calling this method.
    pub fn push(&mut self, transfer: PendingTransfer) {
        debug_assert!(
            self.can_send(),
            "pipeline full: {} outstanding, window {}",
            self.pending.len(),
            self.config.window_size
        );
        self.pending.push_back(transfer);
        self.total_sent += 1;
    }

    /// Removes and returns the oldest pending transfer.
    ///
    /// Returns `None` if there are no outstanding requests.
    pub fn pop(&mut self) -> Option<PendingTransfer> {
        let transfer = self.pending.pop_front();
        if transfer.is_some() {
            self.total_processed += 1;
        }
        transfer
    }

    /// Peeks at the oldest pending transfer without removing it.
    #[must_use]
    pub fn peek(&self) -> Option<&PendingTransfer> {
        self.pending.front()
    }

    /// Returns the expected NDX for the next response.
    ///
    /// Used to verify responses arrive in order.
    #[must_use]
    pub fn expected_ndx(&self) -> Option<i32> {
        self.pending.front().map(PendingTransfer::ndx)
    }

    /// Returns the total number of requests sent through this pipeline.
    #[must_use]
    pub const fn total_sent(&self) -> u64 {
        self.total_sent
    }

    /// Returns the total number of responses processed.
    #[must_use]
    pub const fn total_processed(&self) -> u64 {
        self.total_processed
    }

    /// Returns pipeline statistics.
    #[must_use]
    pub fn stats(&self) -> PipelineStats {
        PipelineStats {
            window_size: self.config.window_size,
            currently_outstanding: self.pending.len(),
            total_sent: self.total_sent,
            total_processed: self.total_processed,
        }
    }

    /// Drains all pending transfers, returning an iterator.
    ///
    /// Used for cleanup when an error occurs during transfer.
    pub fn drain(&mut self) -> impl Iterator<Item = PendingTransfer> + '_ {
        self.pending.drain(..)
    }
}

/// Statistics about pipeline operation.
#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineStats {
    /// Configured window size.
    pub window_size: usize,
    /// Number of currently outstanding requests.
    pub currently_outstanding: usize,
    /// Total requests sent.
    pub total_sent: u64,
    /// Total responses processed.
    pub total_processed: u64,
}

impl PipelineStats {
    /// Returns the average pipeline utilization (0.0 to 1.0).
    ///
    /// Higher values indicate better pipeline efficiency.
    #[must_use]
    pub fn utilization(&self) -> f64 {
        if self.total_sent == 0 {
            return 0.0;
        }
        // This is a simplified metric - real utilization would require
        // tracking time spent at each pipeline depth
        self.currently_outstanding as f64 / self.window_size as f64
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn make_transfer(ndx: i32) -> PendingTransfer {
        PendingTransfer::new_full_transfer(ndx, PathBuf::from(format!("/tmp/file{ndx}")), 100)
    }

    #[test]
    fn new_state_is_empty() {
        let state = PipelineState::new(PipelineConfig::default());
        assert!(state.is_empty());
        assert_eq!(state.outstanding(), 0);
        assert!(state.can_send());
    }

    #[test]
    fn push_increases_outstanding() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));
        assert_eq!(state.outstanding(), 0);

        state.push(make_transfer(0));
        assert_eq!(state.outstanding(), 1);

        state.push(make_transfer(1));
        assert_eq!(state.outstanding(), 2);
    }

    #[test]
    fn can_send_respects_window() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(2));

        assert!(state.can_send());
        state.push(make_transfer(0));

        assert!(state.can_send());
        state.push(make_transfer(1));

        assert!(!state.can_send()); // Window full
    }

    #[test]
    fn pop_returns_in_fifo_order() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(0));
        state.push(make_transfer(1));
        state.push(make_transfer(2));

        assert_eq!(state.pop().unwrap().ndx(), 0);
        assert_eq!(state.pop().unwrap().ndx(), 1);
        assert_eq!(state.pop().unwrap().ndx(), 2);
        assert!(state.pop().is_none());
    }

    #[test]
    fn pop_allows_more_sends() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(2));

        state.push(make_transfer(0));
        state.push(make_transfer(1));
        assert!(!state.can_send());

        state.pop();
        assert!(state.can_send());
    }

    #[test]
    fn peek_does_not_remove() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(0));

        assert_eq!(state.peek().unwrap().ndx(), 0);
        assert_eq!(state.outstanding(), 1); // Still there
        assert_eq!(state.peek().unwrap().ndx(), 0);
    }

    #[test]
    fn expected_ndx_returns_front() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        assert!(state.expected_ndx().is_none());

        state.push(make_transfer(5));
        state.push(make_transfer(10));

        assert_eq!(state.expected_ndx(), Some(5));
        state.pop();
        assert_eq!(state.expected_ndx(), Some(10));
    }

    #[test]
    fn available_slots_calculation() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        assert_eq!(state.available_slots(), 4);

        state.push(make_transfer(0));
        assert_eq!(state.available_slots(), 3);

        state.push(make_transfer(1));
        state.push(make_transfer(2));
        assert_eq!(state.available_slots(), 1);

        state.push(make_transfer(3));
        assert_eq!(state.available_slots(), 0);
    }

    #[test]
    fn stats_tracking() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(0));
        state.push(make_transfer(1));
        state.push(make_transfer(2));

        assert_eq!(state.total_sent(), 3);
        assert_eq!(state.total_processed(), 0);

        state.pop();
        state.pop();

        assert_eq!(state.total_sent(), 3);
        assert_eq!(state.total_processed(), 2);
    }

    #[test]
    fn drain_clears_pending() {
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(0));
        state.push(make_transfer(1));
        state.push(make_transfer(2));

        let drained: Vec<_> = state.drain().collect();
        assert_eq!(drained.len(), 3);
        assert!(state.is_empty());
    }

    #[test]
    fn synchronous_config_has_window_1() {
        let state = PipelineState::new(PipelineConfig::synchronous());
        assert_eq!(state.window_size(), 1);

        let mut state = state;
        state.push(make_transfer(0));
        assert!(!state.can_send());
    }

    // ============================================================================
    // Error state transition tests (task #66)
    // ============================================================================

    #[test]
    fn pop_on_empty_returns_none() {
        // Test underflow behavior - pop on empty state should return None gracefully
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));
        assert!(state.is_empty());

        // First pop on empty
        assert!(state.pop().is_none());

        // Multiple consecutive pops on empty should all return None
        assert!(state.pop().is_none());
        assert!(state.pop().is_none());

        // State should remain valid
        assert!(state.is_empty());
        assert_eq!(state.outstanding(), 0);
        assert!(state.can_send());
        assert_eq!(state.total_processed(), 0);
    }

    #[test]
    fn peek_on_empty_returns_none() {
        // Test peek on empty state
        let state = PipelineState::new(PipelineConfig::default().with_window_size(4));
        assert!(state.peek().is_none());
        assert!(state.expected_ndx().is_none());
    }

    #[test]
    fn available_slots_never_underflows() {
        // Test that available_slots uses saturating_sub for safety
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(2));

        // Fill exactly to capacity
        state.push(make_transfer(0));
        state.push(make_transfer(1));
        assert_eq!(state.available_slots(), 0);

        // Even at capacity, available_slots should not become negative/overflow
        // (this tests the saturating_sub behavior)
        assert_eq!(state.available_slots(), 0);
    }

    #[test]
    fn state_consistency_after_mixed_operations() {
        // Test that state remains consistent through a sequence of operations
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(3));

        // Push, pop, push, pop sequence
        state.push(make_transfer(0));
        assert_eq!(state.outstanding(), 1);

        state.push(make_transfer(1));
        assert_eq!(state.outstanding(), 2);

        let t0 = state.pop();
        assert!(t0.is_some());
        assert_eq!(t0.unwrap().ndx(), 0);
        assert_eq!(state.outstanding(), 1);

        state.push(make_transfer(2));
        assert_eq!(state.outstanding(), 2);

        state.push(make_transfer(3));
        assert_eq!(state.outstanding(), 3);
        assert!(!state.can_send()); // At capacity

        let t1 = state.pop();
        assert_eq!(t1.unwrap().ndx(), 1);
        assert!(state.can_send());
        assert_eq!(state.outstanding(), 2);

        // Verify statistics are consistent
        assert_eq!(state.total_sent(), 4);
        assert_eq!(state.total_processed(), 2);
    }

    #[test]
    fn drain_for_error_recovery() {
        // Test using drain() to clear state when an error occurs
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        // Simulate filling pipeline before encountering an error
        state.push(make_transfer(0));
        state.push(make_transfer(1));
        state.push(make_transfer(2));
        state.push(make_transfer(3));
        assert!(!state.can_send());

        // Simulate error - need to drain all pending transfers
        let drained: Vec<_> = state.drain().collect();
        assert_eq!(drained.len(), 4);

        // State should be back to empty
        assert!(state.is_empty());
        assert!(state.can_send());
        assert_eq!(state.outstanding(), 0);
        assert_eq!(state.available_slots(), 4);

        // But statistics should still reflect what was sent
        assert_eq!(state.total_sent(), 4);
        // Note: drain doesn't increment total_processed - those are "lost"
        assert_eq!(state.total_processed(), 0);
    }

    #[test]
    fn drain_returns_transfers_in_order() {
        // Verify drain returns transfers in FIFO order
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(10));
        state.push(make_transfer(20));
        state.push(make_transfer(30));

        let drained: Vec<_> = state.drain().collect();
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].ndx(), 10);
        assert_eq!(drained[1].ndx(), 20);
        assert_eq!(drained[2].ndx(), 30);
    }

    #[test]
    fn state_usable_after_drain() {
        // After draining, state should be fully usable again
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(2));

        state.push(make_transfer(0));
        state.push(make_transfer(1));

        let _ = state.drain().collect::<Vec<_>>();

        // Should be able to push again
        state.push(make_transfer(2));
        assert_eq!(state.outstanding(), 1);
        assert!(state.can_send());

        state.push(make_transfer(3));
        assert!(!state.can_send());

        let t = state.pop();
        assert_eq!(t.unwrap().ndx(), 2);
    }

    #[test]
    fn stats_utilization_edge_cases() {
        // Test PipelineStats::utilization edge cases
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        // Empty state - no sends yet
        let stats = state.stats();
        assert_eq!(stats.utilization(), 0.0);

        // After one send
        state.push(make_transfer(0));
        let stats = state.stats();
        assert!(stats.utilization() > 0.0);
        assert!((stats.utilization() - 0.25).abs() < 0.001); // 1/4 = 0.25

        // At full capacity
        state.push(make_transfer(1));
        state.push(make_transfer(2));
        state.push(make_transfer(3));
        let stats = state.stats();
        assert!((stats.utilization() - 1.0).abs() < 0.001);
    }

    #[test]
    fn window_boundary_transitions() {
        // Test transitions at window boundaries
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(2));

        // Start: 0/2 slots used
        assert!(state.can_send());
        assert_eq!(state.available_slots(), 2);

        // Transition to 1/2
        state.push(make_transfer(0));
        assert!(state.can_send());
        assert_eq!(state.available_slots(), 1);

        // Transition to 2/2 (full)
        state.push(make_transfer(1));
        assert!(!state.can_send());
        assert_eq!(state.available_slots(), 0);

        // Transition back to 1/2
        state.pop();
        assert!(state.can_send());
        assert_eq!(state.available_slots(), 1);

        // Transition back to 0/2
        state.pop();
        assert!(state.can_send());
        assert_eq!(state.available_slots(), 2);
    }

    #[test]
    fn interleaved_push_pop_stress() {
        // Stress test with many interleaved operations
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(3));

        let mut next_ndx = 0;
        let mut expected_front_ndx = 0;

        for _ in 0..100 {
            // Push up to window
            while state.can_send() {
                state.push(make_transfer(next_ndx));
                next_ndx += 1;
            }

            // Pop one
            if let Some(t) = state.pop() {
                assert_eq!(t.ndx(), expected_front_ndx);
                expected_front_ndx += 1;
            }
        }

        // Verify final state consistency
        assert_eq!(state.total_sent(), next_ndx as u64);
        assert_eq!(state.total_processed(), expected_front_ndx as u64);
        assert_eq!(
            state.outstanding(),
            (next_ndx - expected_front_ndx) as usize
        );
    }

    #[test]
    #[should_panic(expected = "pipeline full")]
    #[cfg(debug_assertions)]
    fn push_beyond_window_panics_in_debug() {
        // In debug mode, pushing beyond window should panic via debug_assert
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(2));

        state.push(make_transfer(0));
        state.push(make_transfer(1));
        // This should trigger the debug_assert
        state.push(make_transfer(2));
    }

    #[test]
    fn minimum_window_size() {
        // Test behavior with minimum window size (1)
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(1));

        assert_eq!(state.window_size(), 1);
        assert!(state.can_send());

        state.push(make_transfer(0));
        assert!(!state.can_send());
        assert_eq!(state.outstanding(), 1);
        assert_eq!(state.available_slots(), 0);

        let t = state.pop().unwrap();
        assert_eq!(t.ndx(), 0);
        assert!(state.can_send());
    }

    #[test]
    fn stats_snapshot_is_independent() {
        // Verify that stats() returns a snapshot, not a live reference
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(0));
        let stats1 = state.stats();

        state.push(make_transfer(1));
        let stats2 = state.stats();

        // stats1 should not have changed
        assert_eq!(stats1.currently_outstanding, 1);
        assert_eq!(stats2.currently_outstanding, 2);
    }

    #[test]
    fn ndx_tracking_with_gaps() {
        // Test that NDX values can have gaps (real scenario with skipped files)
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(1)); // Skip 0
        state.push(make_transfer(5)); // Skip 2,3,4
        state.push(make_transfer(100)); // Large gap

        assert_eq!(state.expected_ndx(), Some(1));
        state.pop();
        assert_eq!(state.expected_ndx(), Some(5));
        state.pop();
        assert_eq!(state.expected_ndx(), Some(100));
        state.pop();
        assert_eq!(state.expected_ndx(), None);
    }

    #[test]
    fn negative_ndx_values() {
        // NDX can be negative in some protocol scenarios
        let mut state = PipelineState::new(PipelineConfig::default().with_window_size(4));

        state.push(make_transfer(-1));
        state.push(make_transfer(-100));
        state.push(make_transfer(0));

        assert_eq!(state.pop().unwrap().ndx(), -1);
        assert_eq!(state.pop().unwrap().ndx(), -100);
        assert_eq!(state.pop().unwrap().ndx(), 0);
    }
}
