//! State machine for 4-priority streaming file list processing pipeline.
//!
//! This module implements a priority-driven main loop controller used by
//! `run_pipelined()` to coordinate concurrent file list reception, pipeline
//! filling, entry processing, and response handling.
//!
//! # Priority Model
//!
//! The state machine enforces a 4-level priority hierarchy:
//!
//! 1. **Process Ready Entries** (highest) - Process entries that are ready
//! 2. **Fill Pipeline** - Add new entries to the pipeline
//! 3. **Read More Entries** - Read additional entries from wire
//! 4. **Process One Response** (lowest) - Process completed responses
//!
//! # State Machine
//!
//! ```text
//! Idle → FillingPipeline → ProcessingEntry
//!     ↓                   ↓              ↓
//!     → ReadingWire ----→ ProcessingResponse
//!                         ↓
//!                       Completed
//! ```
//!
//! # Examples
//!
//! ```ignore
//! // Internal module - this example shows usage for run_pipelined()
//! use engine::local_copy::pipelined_state::{PipelineController, PipelinePriority};
//!
//! let mut controller = PipelineController::new(8);
//!
//! // Main loop
//! while !controller.is_complete() {
//!     match controller.next_priority() {
//!         Some(PipelinePriority::ProcessReadyEntries) => {
//!             if let Some(entry_id) = controller.dequeue_ready() {
//!                 // Process entry...
//!                 controller.submit_response(entry_id);
//!             }
//!         }
//!         Some(PipelinePriority::FillPipeline) => {
//!             let entry_id = 1; // Get next entry
//!             controller.enqueue_entry(entry_id);
//!         }
//!         Some(PipelinePriority::ReadMoreEntries) => {
//!             // Read from wire...
//!             controller.mark_wire_exhausted();
//!         }
//!         Some(PipelinePriority::ProcessOneResponse) => {
//!             if let Some(entry_id) = controller.dequeue_response() {
//!                 // Process response...
//!             }
//!         }
//!         None => break,
//!     }
//! }
//! ```

use std::collections::VecDeque;

/// State of the pipeline processing system.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineState {
    /// Waiting for work to begin.
    Idle,
    /// Currently filling the pipeline with new entries.
    FillingPipeline,
    /// Processing a ready entry.
    ProcessingEntry,
    /// Reading more entries from the wire.
    ReadingWire,
    /// Processing one response.
    ProcessingResponse,
    /// All entries have been processed.
    Completed,
    /// Terminal error state with description.
    Error(String),
}

/// Priority levels for pipeline operations, ordered from highest to lowest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PipelinePriority {
    /// Highest priority: process entries that are ready.
    ProcessReadyEntries = 1,
    /// Fill the pipeline with new entries.
    FillPipeline = 2,
    /// Read more entries from the wire.
    ReadMoreEntries = 3,
    /// Lowest priority: process one response.
    ProcessOneResponse = 4,
}

/// Statistics about pipeline activity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PipelineStats {
    /// Total entries enqueued to the pipeline.
    pub entries_enqueued: usize,
    /// Total entries processed.
    pub entries_processed: usize,
    /// Total responses processed.
    pub responses_processed: usize,
    /// Current pipeline depth.
    pub pipeline_depth: usize,
    /// Maximum pipeline depth reached.
    pub max_pipeline_depth: usize,
}

/// Controller for the pipelined file list processing state machine.
///
/// Manages the lifecycle of entries through the pipeline: enqueuing, marking
/// ready, dequeuing for processing, submitting responses, and processing responses.
pub struct PipelineController {
    /// Current state of the pipeline.
    state: PipelineState,
    /// Maximum number of entries allowed in the pipeline simultaneously.
    capacity: usize,
    /// Queue of entries waiting to be processed.
    pending_entries: VecDeque<usize>,
    /// Queue of entries that are ready to process.
    ready_entries: VecDeque<usize>,
    /// Queue of processed responses waiting to be handled.
    pending_responses: VecDeque<usize>,
    /// Whether the wire has been exhausted (no more entries to read).
    wire_exhausted: bool,
    /// Pipeline statistics.
    stats: PipelineStats,
}

impl PipelineController {
    /// Creates a new pipeline controller with the specified capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of entries in the pipeline simultaneously
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let controller = PipelineController::new(8);
    /// assert_eq!(controller.capacity(), 8);
    /// ```
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            state: PipelineState::Idle,
            capacity,
            pending_entries: VecDeque::new(),
            ready_entries: VecDeque::new(),
            pending_responses: VecDeque::new(),
            wire_exhausted: false,
            stats: PipelineStats::default(),
        }
    }

    /// Returns the current pipeline state.
    #[must_use]
    pub const fn state(&self) -> &PipelineState {
        &self.state
    }

    /// Returns the pipeline capacity.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the highest priority action available, or `None` if no action is possible.
    ///
    /// Priority ordering:
    /// 1. Process ready entries
    /// 2. Fill pipeline
    /// 3. Read more entries
    /// 4. Process responses
    #[must_use]
    pub fn next_priority(&self) -> Option<PipelinePriority> {
        // Error state is terminal
        if matches!(self.state, PipelineState::Error(_)) {
            return None;
        }

        // Check if complete
        if self.is_complete() {
            return None;
        }

        // Priority 1: Process ready entries
        if self.has_ready_entries() {
            return Some(PipelinePriority::ProcessReadyEntries);
        }

        // Priority 2: Fill pipeline (if not full and wire not exhausted)
        if self.can_fill() {
            return Some(PipelinePriority::FillPipeline);
        }

        // Priority 3: Read more entries (if wire not exhausted and pipeline not empty)
        if !self.wire_exhausted && !self.pending_entries.is_empty() {
            return Some(PipelinePriority::ReadMoreEntries);
        }

        // Priority 4: Process responses
        if self.has_pending_responses() {
            return Some(PipelinePriority::ProcessOneResponse);
        }

        None
    }

    /// Returns `true` if the pipeline can accept more entries.
    #[must_use]
    pub fn can_fill(&self) -> bool {
        let current_depth = self.pending_entries.len() + self.ready_entries.len();
        current_depth < self.capacity && !self.wire_exhausted
    }

    /// Returns `true` if there are entries ready to process.
    #[must_use]
    pub fn has_ready_entries(&self) -> bool {
        !self.ready_entries.is_empty()
    }

    /// Returns `true` if there are pending responses to process.
    #[must_use]
    pub fn has_pending_responses(&self) -> bool {
        !self.pending_responses.is_empty()
    }

    /// Enqueues a new entry into the pipeline.
    ///
    /// # Arguments
    ///
    /// * `entry_id` - Unique identifier for the entry
    ///
    /// # Panics
    ///
    /// Panics if the pipeline is at capacity.
    pub fn enqueue_entry(&mut self, entry_id: usize) {
        assert!(self.can_fill(), "Pipeline is at capacity");

        self.pending_entries.push_back(entry_id);
        self.stats.entries_enqueued += 1;

        let current_depth = self.pending_entries.len() + self.ready_entries.len();
        self.stats.pipeline_depth = current_depth;
        self.stats.max_pipeline_depth = self.stats.max_pipeline_depth.max(current_depth);
    }

    /// Marks an entry as ready to process.
    ///
    /// The entry must currently be in the pending queue.
    ///
    /// # Arguments
    ///
    /// * `entry_id` - Entry to mark as ready
    pub fn mark_ready(&mut self, entry_id: usize) {
        if let Some(pos) = self.pending_entries.iter().position(|&id| id == entry_id) {
            self.pending_entries.remove(pos);
            self.ready_entries.push_back(entry_id);
        }
    }

    /// Dequeues the next ready entry for processing.
    ///
    /// Returns `None` if no entries are ready.
    #[must_use]
    pub fn dequeue_ready(&mut self) -> Option<usize> {
        let entry_id = self.ready_entries.pop_front();
        if entry_id.is_some() {
            self.stats.entries_processed += 1;
            let current_depth = self.pending_entries.len() + self.ready_entries.len();
            self.stats.pipeline_depth = current_depth;
        }
        entry_id
    }

    /// Submits a processed response.
    ///
    /// # Arguments
    ///
    /// * `entry_id` - Entry whose response is being submitted
    pub fn submit_response(&mut self, entry_id: usize) {
        self.pending_responses.push_back(entry_id);
    }

    /// Dequeues the next response for processing.
    ///
    /// Returns `None` if no responses are pending.
    #[must_use]
    pub fn dequeue_response(&mut self) -> Option<usize> {
        let response = self.pending_responses.pop_front();
        if response.is_some() {
            self.stats.responses_processed += 1;
        }
        response
    }

    /// Marks the wire as exhausted (no more entries available).
    pub fn mark_wire_exhausted(&mut self) {
        self.wire_exhausted = true;
    }

    /// Returns `true` if all processing is complete.
    ///
    /// Complete means:
    /// - Wire is exhausted
    /// - No pending entries
    /// - No ready entries
    /// - No pending responses
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.wire_exhausted
            && self.pending_entries.is_empty()
            && self.ready_entries.is_empty()
            && self.pending_responses.is_empty()
    }

    /// Explicitly transitions to a new state.
    ///
    /// # Arguments
    ///
    /// * `state` - Target state
    pub fn transition_to(&mut self, state: PipelineState) {
        self.state = state;
    }

    /// Returns current pipeline statistics.
    #[must_use]
    pub const fn stats(&self) -> &PipelineStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== PipelineState tests ====================

    #[test]
    fn pipeline_state_idle_equality() {
        assert_eq!(PipelineState::Idle, PipelineState::Idle);
    }

    #[test]
    fn pipeline_state_clone() {
        let state = PipelineState::FillingPipeline;
        let cloned = state.clone();
        assert_eq!(state, cloned);
    }

    #[test]
    fn pipeline_state_error_equality() {
        let err1 = PipelineState::Error("test".to_string());
        let err2 = PipelineState::Error("test".to_string());
        let err3 = PipelineState::Error("other".to_string());
        assert_eq!(err1, err2);
        assert_ne!(err1, err3);
    }

    #[test]
    fn pipeline_state_debug_format() {
        let state = PipelineState::ProcessingEntry;
        let debug = format!("{state:?}");
        assert!(debug.contains("ProcessingEntry"));
    }

    // ==================== PipelinePriority tests ====================

    #[test]
    fn pipeline_priority_ordering() {
        assert!(PipelinePriority::ProcessReadyEntries < PipelinePriority::FillPipeline);
        assert!(PipelinePriority::FillPipeline < PipelinePriority::ReadMoreEntries);
        assert!(PipelinePriority::ReadMoreEntries < PipelinePriority::ProcessOneResponse);
    }

    #[test]
    fn pipeline_priority_equality() {
        assert_eq!(
            PipelinePriority::ProcessReadyEntries,
            PipelinePriority::ProcessReadyEntries
        );
        assert_ne!(
            PipelinePriority::ProcessReadyEntries,
            PipelinePriority::FillPipeline
        );
    }

    #[test]
    fn pipeline_priority_clone_copy() {
        let priority = PipelinePriority::FillPipeline;
        let copied = priority;
        assert_eq!(priority, copied);
    }

    // ==================== PipelineController tests ====================

    #[test]
    fn controller_new_creates_idle_state() {
        let controller = PipelineController::new(8);
        assert_eq!(*controller.state(), PipelineState::Idle);
        assert_eq!(controller.capacity(), 8);
        assert!(!controller.is_complete());
    }

    #[test]
    fn controller_can_fill_when_empty() {
        let controller = PipelineController::new(4);
        assert!(controller.can_fill());
    }

    #[test]
    fn controller_cannot_fill_when_full() {
        let mut controller = PipelineController::new(2);
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);
        assert!(!controller.can_fill());
    }

    #[test]
    fn controller_enqueue_entry_increments_stats() {
        let mut controller = PipelineController::new(4);
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);

        let stats = controller.stats();
        assert_eq!(stats.entries_enqueued, 2);
        assert_eq!(stats.pipeline_depth, 2);
        assert_eq!(stats.max_pipeline_depth, 2);
    }

    #[test]
    fn controller_mark_ready_moves_to_ready_queue() {
        let mut controller = PipelineController::new(4);
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);

        assert!(!controller.has_ready_entries());
        controller.mark_ready(1);
        assert!(controller.has_ready_entries());
    }

    #[test]
    fn controller_dequeue_ready_returns_in_order() {
        let mut controller = PipelineController::new(4);
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);
        controller.mark_ready(1);
        controller.mark_ready(2);

        assert_eq!(controller.dequeue_ready(), Some(1));
        assert_eq!(controller.dequeue_ready(), Some(2));
        assert_eq!(controller.dequeue_ready(), None);
    }

    #[test]
    fn controller_submit_and_dequeue_response() {
        let mut controller = PipelineController::new(4);

        assert!(!controller.has_pending_responses());
        controller.submit_response(1);
        assert!(controller.has_pending_responses());

        assert_eq!(controller.dequeue_response(), Some(1));
        assert!(!controller.has_pending_responses());
    }

    #[test]
    fn controller_wire_exhausted_affects_completion() {
        let mut controller = PipelineController::new(4);

        assert!(!controller.is_complete());
        controller.mark_wire_exhausted();
        assert!(controller.is_complete());
    }

    #[test]
    fn controller_not_complete_with_pending_entries() {
        let mut controller = PipelineController::new(4);
        controller.enqueue_entry(1);
        controller.mark_wire_exhausted();

        assert!(!controller.is_complete());
    }

    #[test]
    fn controller_not_complete_with_ready_entries() {
        let mut controller = PipelineController::new(4);
        controller.enqueue_entry(1);
        controller.mark_ready(1);
        controller.mark_wire_exhausted();

        assert!(!controller.is_complete());
    }

    #[test]
    fn controller_not_complete_with_pending_responses() {
        let mut controller = PipelineController::new(4);
        controller.submit_response(1);
        controller.mark_wire_exhausted();

        assert!(!controller.is_complete());
    }

    #[test]
    fn controller_transition_to_changes_state() {
        let mut controller = PipelineController::new(4);
        controller.transition_to(PipelineState::FillingPipeline);
        assert_eq!(*controller.state(), PipelineState::FillingPipeline);
    }

    #[test]
    fn controller_next_priority_ready_entries_highest() {
        let mut controller = PipelineController::new(4);
        controller.enqueue_entry(1);
        controller.mark_ready(1);

        // Even with other work available, ready entries have highest priority
        assert_eq!(
            controller.next_priority(),
            Some(PipelinePriority::ProcessReadyEntries)
        );
    }

    #[test]
    fn controller_next_priority_fill_pipeline_second() {
        let mut controller = PipelineController::new(4);

        // No ready entries, can fill
        assert_eq!(
            controller.next_priority(),
            Some(PipelinePriority::FillPipeline)
        );
    }

    #[test]
    fn controller_next_priority_read_more_third() {
        let mut controller = PipelineController::new(2);
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);

        // Pipeline full, but has pending entries and wire not exhausted
        assert_eq!(
            controller.next_priority(),
            Some(PipelinePriority::ReadMoreEntries)
        );
    }

    #[test]
    fn controller_next_priority_process_response_lowest() {
        let mut controller = PipelineController::new(4);
        controller.submit_response(1);
        controller.mark_wire_exhausted();

        // Wire exhausted, no pending/ready entries, only responses
        assert_eq!(
            controller.next_priority(),
            Some(PipelinePriority::ProcessOneResponse)
        );
    }

    #[test]
    fn controller_next_priority_none_when_complete() {
        let mut controller = PipelineController::new(4);
        controller.mark_wire_exhausted();

        assert_eq!(controller.next_priority(), None);
    }

    #[test]
    fn controller_next_priority_none_on_error() {
        let mut controller = PipelineController::new(4);
        controller.transition_to(PipelineState::Error("test error".to_string()));

        assert_eq!(controller.next_priority(), None);
    }

    #[test]
    fn controller_error_state_is_terminal() {
        let mut controller = PipelineController::new(4);
        controller.enqueue_entry(1);
        controller.transition_to(PipelineState::Error("fatal".to_string()));

        // Even with pending work, error state blocks all priorities
        assert_eq!(controller.next_priority(), None);
    }

    #[test]
    fn controller_max_pipeline_depth_tracking() {
        let mut controller = PipelineController::new(8);

        controller.enqueue_entry(1);
        assert_eq!(controller.stats().max_pipeline_depth, 1);

        controller.enqueue_entry(2);
        controller.enqueue_entry(3);
        assert_eq!(controller.stats().max_pipeline_depth, 3);

        controller.dequeue_ready();
        controller.enqueue_entry(4);
        controller.enqueue_entry(5);
        assert_eq!(controller.stats().max_pipeline_depth, 5);
    }

    #[test]
    fn controller_complete_lifecycle() {
        let mut controller = PipelineController::new(2);

        // Fill pipeline
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);
        assert_eq!(controller.stats().entries_enqueued, 2);

        // Mark ready and process
        controller.mark_ready(1);
        let entry = controller.dequeue_ready().unwrap();
        assert_eq!(entry, 1);
        assert_eq!(controller.stats().entries_processed, 1);

        // Submit response
        controller.submit_response(entry);
        assert!(controller.has_pending_responses());

        // Process response
        let response = controller.dequeue_response().unwrap();
        assert_eq!(response, 1);
        assert_eq!(controller.stats().responses_processed, 1);

        // Process second entry
        controller.mark_ready(2);
        controller.dequeue_ready();
        controller.submit_response(2);
        controller.dequeue_response();

        // Mark complete
        controller.mark_wire_exhausted();
        assert!(controller.is_complete());
    }

    #[test]
    fn controller_multiple_entries_simultaneously() {
        let mut controller = PipelineController::new(4);

        // Enqueue multiple
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);
        controller.enqueue_entry(3);

        // Mark some ready
        controller.mark_ready(1);
        controller.mark_ready(3);

        // Should maintain separate pending and ready queues
        assert_eq!(controller.pending_entries.len(), 1); // entry 2
        assert_eq!(controller.ready_entries.len(), 2); // entries 1, 3

        // Dequeue in ready order
        assert_eq!(controller.dequeue_ready(), Some(1));
        assert_eq!(controller.dequeue_ready(), Some(3));
    }

    #[test]
    fn controller_pipeline_depth_decreases_on_dequeue() {
        let mut controller = PipelineController::new(4);

        controller.enqueue_entry(1);
        controller.enqueue_entry(2);
        assert_eq!(controller.stats().pipeline_depth, 2);

        controller.mark_ready(1);
        controller.dequeue_ready();
        assert_eq!(controller.stats().pipeline_depth, 1);

        controller.mark_ready(2);
        controller.dequeue_ready();
        assert_eq!(controller.stats().pipeline_depth, 0);
    }

    #[test]
    fn controller_responses_dont_affect_pipeline_depth() {
        let mut controller = PipelineController::new(4);

        controller.submit_response(1);
        controller.submit_response(2);

        // Responses are tracked separately from pipeline depth
        assert_eq!(controller.stats().pipeline_depth, 0);
        assert_eq!(controller.pending_responses.len(), 2);
    }

    #[test]
    #[should_panic(expected = "Pipeline is at capacity")]
    fn controller_panics_when_enqueue_over_capacity() {
        let mut controller = PipelineController::new(2);
        controller.enqueue_entry(1);
        controller.enqueue_entry(2);
        controller.enqueue_entry(3); // Should panic
    }

    #[test]
    fn controller_empty_pipeline_behavior() {
        let controller = PipelineController::new(4);

        assert!(!controller.has_ready_entries());
        assert!(!controller.has_pending_responses());
        assert!(controller.can_fill());
        assert!(!controller.is_complete());
    }

    #[test]
    fn pipeline_stats_default() {
        let stats = PipelineStats::default();
        assert_eq!(stats.entries_enqueued, 0);
        assert_eq!(stats.entries_processed, 0);
        assert_eq!(stats.responses_processed, 0);
        assert_eq!(stats.pipeline_depth, 0);
        assert_eq!(stats.max_pipeline_depth, 0);
    }

    #[test]
    fn pipeline_stats_clone_equality() {
        let stats = PipelineStats {
            entries_enqueued: 10,
            entries_processed: 8,
            responses_processed: 5,
            pipeline_depth: 2,
            max_pipeline_depth: 4,
        };
        let cloned = stats.clone();
        assert_eq!(stats, cloned);
    }
}
