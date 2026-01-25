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
}
