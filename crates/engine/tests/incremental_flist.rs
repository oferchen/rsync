//! Integration tests for incremental file list processing with streaming transfer.
//!
//! These tests verify the PipelineController state machine end-to-end:
//! - Full lifecycle from enqueue to completion
//! - Streaming vs batch processing behavior
//! - Pipeline capacity management
//! - Priority-driven execution order
//! - Stats accuracy
//! - Error state handling
//! - Large file count scenarios

use engine::local_copy::pipelined_state::{
    PipelineController, PipelinePriority, PipelineState, PipelineStats,
};

// ==================== Helper Functions ====================

/// Simulates a complete file entry lifecycle through the pipeline.
fn process_entry(controller: &mut PipelineController, entry_id: usize) {
    controller.mark_ready(entry_id);
    let dequeued = controller.dequeue_ready();
    assert_eq!(dequeued, Some(entry_id));
    controller.submit_response(entry_id);
    let response = controller.dequeue_response();
    assert_eq!(response, Some(entry_id));
}

/// Processes all entries currently in the pipeline.
fn process_all_pending(controller: &mut PipelineController) {
    while controller.has_ready_entries() {
        if let Some(entry_id) = controller.dequeue_ready() {
            controller.submit_response(entry_id);
        }
    }
    while controller.has_pending_responses() {
        let _ = controller.dequeue_response();
    }
}

/// Enqueues entries in batch mode (all at once).
#[allow(dead_code)]
fn enqueue_batch(controller: &mut PipelineController, count: usize) {
    for i in 0..count {
        if controller.can_fill() {
            controller.enqueue_entry(i);
        }
    }
}

/// Simulates streaming arrival: enqueue, immediately mark ready, process.
fn process_streaming(controller: &mut PipelineController, total_count: usize) {
    let mut enqueued = 0;

    while !controller.is_complete() {
        match controller.next_priority() {
            Some(PipelinePriority::ProcessReadyEntries) => {
                if let Some(entry_id) = controller.dequeue_ready() {
                    controller.submit_response(entry_id);
                }
            }
            Some(PipelinePriority::FillPipeline) => {
                if enqueued < total_count {
                    controller.enqueue_entry(enqueued);
                    controller.mark_ready(enqueued);
                    enqueued += 1;
                } else {
                    controller.mark_wire_exhausted();
                }
            }
            Some(PipelinePriority::ReadMoreEntries) => {
                // In real code, this would read from network/disk
                // For testing, we just let it continue to next priority
            }
            Some(PipelinePriority::ProcessOneResponse) => {
                let _ = controller.dequeue_response();
            }
            None => break,
        }
    }
}

// ==================== Full Lifecycle Tests ====================

#[test]
fn full_lifecycle_single_entry() {
    let mut controller = PipelineController::new(8);

    // Start idle
    assert_eq!(*controller.state(), PipelineState::Idle);
    assert!(!controller.is_complete());

    // Enqueue entry
    controller.enqueue_entry(0);
    assert_eq!(controller.stats().entries_enqueued, 1);
    assert_eq!(controller.stats().pipeline_depth, 1);

    // Mark ready
    controller.mark_ready(0);
    assert!(controller.has_ready_entries());

    // Process entry
    let entry_id = controller.dequeue_ready().unwrap();
    assert_eq!(entry_id, 0);
    assert_eq!(controller.stats().entries_processed, 1);

    // Submit response
    controller.submit_response(entry_id);
    assert!(controller.has_pending_responses());

    // Process response
    let response = controller.dequeue_response().unwrap();
    assert_eq!(response, 0);
    assert_eq!(controller.stats().responses_processed, 1);

    // Mark complete
    controller.mark_wire_exhausted();
    assert!(controller.is_complete());
    assert_eq!(controller.next_priority(), None);
}

#[test]
fn full_lifecycle_multiple_entries_in_order() {
    let mut controller = PipelineController::new(4);

    // Enqueue multiple entries
    for i in 0..4 {
        controller.enqueue_entry(i);
    }
    assert_eq!(controller.stats().entries_enqueued, 4);

    // Process all in order
    for i in 0..4 {
        process_entry(&mut controller, i);
    }

    // Verify stats
    assert_eq!(controller.stats().entries_processed, 4);
    assert_eq!(controller.stats().responses_processed, 4);
    assert_eq!(controller.stats().pipeline_depth, 0);

    // Complete
    controller.mark_wire_exhausted();
    assert!(controller.is_complete());
}

#[test]
fn full_lifecycle_with_priority_ordering() {
    let mut controller = PipelineController::new(2);

    // Enqueue entries to fill pipeline
    controller.enqueue_entry(0);
    controller.enqueue_entry(1);
    controller.mark_ready(0);

    // Priority 1: Process ready entries (highest)
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ProcessReadyEntries)
    );

    // Process the ready entry
    let _ = controller.dequeue_ready();
    controller.submit_response(0);

    // Still has pending entry, can fill more (depth now 1, capacity 2)
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::FillPipeline)
    );

    // Fill pipeline again
    controller.enqueue_entry(2);

    // Now pipeline is full (entries 1 and 2), wire not exhausted, have pending
    // Priority 3: ReadMoreEntries
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ReadMoreEntries)
    );

    // Mark entries ready
    controller.mark_ready(1);
    controller.mark_ready(2);

    // Back to processing ready entries (highest priority)
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ProcessReadyEntries)
    );

    // Process all
    let _ = controller.dequeue_ready();
    controller.submit_response(1);
    let _ = controller.dequeue_ready();
    controller.submit_response(2);

    // Now only responses pending, but can still fill
    // Mark wire exhausted first
    controller.mark_wire_exhausted();

    // Now only responses pending and wire exhausted
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ProcessOneResponse)
    );

    let _ = controller.dequeue_response();
    let _ = controller.dequeue_response();
    let _ = controller.dequeue_response();

    assert!(controller.is_complete());
}

// ==================== Streaming Behavior Tests ====================

#[test]
fn streaming_arrival_simulation() {
    let mut controller = PipelineController::new(8);

    // Simulate streaming: entries arrive one at a time
    let total_entries = 10;
    let mut arrived = 0;
    let mut processed = 0;
    let mut wire_marked_exhausted = false;

    while !controller.is_complete() {
        // Simulate arrival
        if arrived < total_entries && controller.can_fill() {
            controller.enqueue_entry(arrived);
            controller.mark_ready(arrived);
            arrived += 1;
        } else if arrived >= total_entries && !wire_marked_exhausted {
            controller.mark_wire_exhausted();
            wire_marked_exhausted = true;
        }

        // Process what's ready
        if let Some(entry_id) = controller.dequeue_ready() {
            controller.submit_response(entry_id);
            processed += 1;
        }

        // Process responses
        if controller.dequeue_response().is_some() {
            // Response processed
        }
    }

    assert_eq!(arrived, total_entries);
    assert_eq!(processed, total_entries);
    assert_eq!(controller.stats().entries_enqueued, total_entries);
    assert_eq!(controller.stats().entries_processed, total_entries);
}

#[test]
fn streaming_with_interleaved_processing() {
    let mut controller = PipelineController::new(4);

    // Entry 0 arrives and is processed
    controller.enqueue_entry(0);
    controller.mark_ready(0);
    process_entry(&mut controller, 0);

    // Entries 1-2 arrive
    controller.enqueue_entry(1);
    controller.enqueue_entry(2);

    // Entry 1 becomes ready and is processed while 2 is still pending
    controller.mark_ready(1);
    process_entry(&mut controller, 1);

    // More entries arrive
    controller.enqueue_entry(3);
    controller.mark_ready(2);
    controller.mark_ready(3);

    // Process remaining
    process_entry(&mut controller, 2);
    process_entry(&mut controller, 3);

    controller.mark_wire_exhausted();
    assert!(controller.is_complete());
    assert_eq!(controller.stats().entries_processed, 4);
}

#[test]
fn streaming_vs_batch_produces_same_results() {
    let count = 50;

    // Batch mode: enqueue all in waves, then process all
    let mut batch_controller = PipelineController::new(16);
    for i in 0..count {
        while !batch_controller.can_fill() {
            if batch_controller.has_ready_entries() {
                if let Some(entry) = batch_controller.dequeue_ready() {
                    batch_controller.submit_response(entry);
                }
            }
            if batch_controller.has_pending_responses() {
                let _ = batch_controller.dequeue_response();
            }
        }
        batch_controller.enqueue_entry(i);
        batch_controller.mark_ready(i);
    }
    process_all_pending(&mut batch_controller);
    batch_controller.mark_wire_exhausted();

    // Streaming mode: process as we go
    let mut streaming_controller = PipelineController::new(16);
    process_streaming(&mut streaming_controller, count);

    // Both should have same final stats
    assert_eq!(
        batch_controller.stats().entries_enqueued,
        streaming_controller.stats().entries_enqueued
    );
    assert_eq!(
        batch_controller.stats().entries_processed,
        streaming_controller.stats().entries_processed
    );
    assert!(batch_controller.is_complete());
    assert!(streaming_controller.is_complete());
}

// ==================== Pipeline Capacity Tests ====================

#[test]
fn pipeline_capacity_management() {
    let mut controller = PipelineController::new(4);

    // Fill to capacity
    for i in 0..4 {
        assert!(controller.can_fill());
        controller.enqueue_entry(i);
    }

    // Cannot fill more
    assert!(!controller.can_fill());
    assert_eq!(controller.stats().pipeline_depth, 4);

    // Process one entry
    controller.mark_ready(0);
    let _ = controller.dequeue_ready();
    assert_eq!(controller.stats().pipeline_depth, 3);

    // Can fill again
    assert!(controller.can_fill());
    controller.enqueue_entry(4);
    assert_eq!(controller.stats().pipeline_depth, 4);

    // Process remaining
    for i in 1..=4 {
        controller.mark_ready(i);
        let _ = controller.dequeue_ready();
        controller.submit_response(i);
    }

    assert_eq!(controller.stats().pipeline_depth, 0);
}

#[test]
fn pipeline_full_can_still_process() {
    let mut controller = PipelineController::new(2);

    // Fill pipeline
    controller.enqueue_entry(0);
    controller.enqueue_entry(1);
    assert!(!controller.can_fill());

    // Mark ready and process even though full
    controller.mark_ready(0);
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ProcessReadyEntries)
    );

    let _ = controller.dequeue_ready();
    controller.submit_response(0);

    // Now can fill again
    assert!(controller.can_fill());
}

#[test]
fn pipeline_capacity_enforced() {
    let mut controller = PipelineController::new(3);

    controller.enqueue_entry(0);
    controller.enqueue_entry(1);
    controller.enqueue_entry(2);

    // Should panic if we try to exceed capacity
    assert!(!controller.can_fill());
}

#[test]
#[should_panic(expected = "Pipeline is at capacity")]
fn pipeline_enqueue_over_capacity_panics() {
    let mut controller = PipelineController::new(2);

    controller.enqueue_entry(0);
    controller.enqueue_entry(1);
    controller.enqueue_entry(2); // Should panic
}

#[test]
fn wire_exhaustion_prevents_filling() {
    let mut controller = PipelineController::new(8);

    assert!(controller.can_fill());

    controller.mark_wire_exhausted();

    assert!(!controller.can_fill());
}

// ==================== Priority Tests ====================

#[test]
fn priority_ready_entries_trumps_all() {
    let mut controller = PipelineController::new(8);

    // Setup: pending entry, ready entry, responses
    controller.enqueue_entry(0);
    controller.enqueue_entry(1);
    controller.mark_ready(1);
    controller.submit_response(99);

    // Even with responses and ability to fill, ready entries have priority
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ProcessReadyEntries)
    );
}

#[test]
fn priority_fill_pipeline_second() {
    let mut controller = PipelineController::new(8);

    // No ready entries, can fill
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::FillPipeline)
    );

    // Even with responses pending, filling comes first
    controller.submit_response(99);
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::FillPipeline)
    );
}

#[test]
fn priority_read_more_entries_third() {
    let mut controller = PipelineController::new(2);

    // Fill pipeline (no ready entries)
    controller.enqueue_entry(0);
    controller.enqueue_entry(1);

    // Pipeline full, wire not exhausted, has pending
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ReadMoreEntries)
    );
}

#[test]
fn priority_process_response_lowest() {
    let mut controller = PipelineController::new(8);

    controller.submit_response(0);
    controller.mark_wire_exhausted();

    // Only responses, wire exhausted
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::ProcessOneResponse)
    );
}

// ==================== Stats Tests ====================

#[test]
fn stats_tracking_accuracy() {
    let mut controller = PipelineController::new(8);

    // Initial stats
    assert_eq!(controller.stats().entries_enqueued, 0);
    assert_eq!(controller.stats().entries_processed, 0);
    assert_eq!(controller.stats().responses_processed, 0);

    // Enqueue entries
    for i in 0..5 {
        controller.enqueue_entry(i);
    }
    assert_eq!(controller.stats().entries_enqueued, 5);
    assert_eq!(controller.stats().pipeline_depth, 5);

    // Process some
    controller.mark_ready(0);
    controller.mark_ready(1);
    let _ = controller.dequeue_ready();
    let _ = controller.dequeue_ready();
    assert_eq!(controller.stats().entries_processed, 2);
    assert_eq!(controller.stats().pipeline_depth, 3);

    // Submit and process responses
    controller.submit_response(0);
    controller.submit_response(1);
    let _ = controller.dequeue_response();
    let _ = controller.dequeue_response();
    assert_eq!(controller.stats().responses_processed, 2);
}

#[test]
fn stats_max_pipeline_depth_tracking() {
    let mut controller = PipelineController::new(10);

    // Ramp up
    controller.enqueue_entry(0);
    assert_eq!(controller.stats().max_pipeline_depth, 1);

    controller.enqueue_entry(1);
    controller.enqueue_entry(2);
    assert_eq!(controller.stats().max_pipeline_depth, 3);

    // Process one
    controller.mark_ready(0);
    let _ = controller.dequeue_ready();
    assert_eq!(controller.stats().pipeline_depth, 2);
    assert_eq!(controller.stats().max_pipeline_depth, 3); // Still remembers max

    // Add more
    controller.enqueue_entry(3);
    controller.enqueue_entry(4);
    controller.enqueue_entry(5);
    assert_eq!(controller.stats().max_pipeline_depth, 5);
}

#[test]
fn stats_responses_dont_affect_pipeline_depth() {
    let mut controller = PipelineController::new(8);

    controller.enqueue_entry(0);
    controller.mark_ready(0);
    let _ = controller.dequeue_ready();

    // Submit response - pipeline depth should be 0
    controller.submit_response(0);
    assert_eq!(controller.stats().pipeline_depth, 0);
    assert!(controller.has_pending_responses());
}

// ==================== Error State Tests ====================

#[test]
fn error_state_blocks_all_operations() {
    let mut controller = PipelineController::new(8);

    controller.enqueue_entry(0);
    controller.mark_ready(0);
    controller.submit_response(99);

    // Transition to error state
    controller.transition_to(PipelineState::Error("test error".to_string()));

    // All priorities should be blocked
    assert_eq!(controller.next_priority(), None);

    // Error state is terminal
    assert!(matches!(controller.state(), PipelineState::Error(_)));
}

#[test]
fn error_state_preserves_stats() {
    let mut controller = PipelineController::new(8);

    controller.enqueue_entry(0);
    controller.enqueue_entry(1);
    controller.mark_ready(0);
    let _ = controller.dequeue_ready();

    let stats_before = controller.stats().clone();

    controller.transition_to(PipelineState::Error("failure".to_string()));

    // Stats should be preserved
    assert_eq!(
        controller.stats().entries_enqueued,
        stats_before.entries_enqueued
    );
    assert_eq!(
        controller.stats().entries_processed,
        stats_before.entries_processed
    );
}

#[test]
fn error_state_equality() {
    let err1 = PipelineState::Error("test".to_string());
    let err2 = PipelineState::Error("test".to_string());
    let err3 = PipelineState::Error("different".to_string());

    assert_eq!(err1, err2);
    assert_ne!(err1, err3);
}

// ==================== Large File Count Tests ====================

#[test]
fn small_file_set_5_files() {
    let mut controller = PipelineController::new(8);

    for i in 0..5 {
        controller.enqueue_entry(i);
        controller.mark_ready(i);
    }

    for i in 0..5 {
        process_entry(&mut controller, i);
    }

    controller.mark_wire_exhausted();
    assert!(controller.is_complete());
    assert_eq!(controller.stats().entries_enqueued, 5);
    assert_eq!(controller.stats().entries_processed, 5);
    assert_eq!(controller.stats().responses_processed, 5);
}

#[test]
fn medium_file_set_100_files() {
    let mut controller = PipelineController::new(16);

    let count = 100;
    for i in 0..count {
        while !controller.can_fill() {
            if let Some(entry) = controller.dequeue_ready() {
                controller.submit_response(entry);
            }
            if controller.has_pending_responses() {
                let _ = controller.dequeue_response();
            }
        }
        controller.enqueue_entry(i);
        controller.mark_ready(i);
    }

    // Process remaining
    process_all_pending(&mut controller);
    controller.mark_wire_exhausted();

    assert!(controller.is_complete());
    assert_eq!(controller.stats().entries_enqueued, count);
    assert_eq!(controller.stats().entries_processed, count);
    assert_eq!(controller.stats().responses_processed, count);
}

#[test]
fn large_file_set_1000_files() {
    let mut controller = PipelineController::new(32);

    let count = 1000;
    for i in 0..count {
        while !controller.can_fill() {
            if let Some(entry) = controller.dequeue_ready() {
                controller.submit_response(entry);
            }
            if controller.has_pending_responses() {
                let _ = controller.dequeue_response();
            }
        }
        controller.enqueue_entry(i);
        controller.mark_ready(i);
    }

    // Process remaining
    process_all_pending(&mut controller);
    controller.mark_wire_exhausted();

    assert!(controller.is_complete());
    assert_eq!(controller.stats().entries_enqueued, count);
    assert_eq!(controller.stats().entries_processed, count);
    assert_eq!(controller.stats().responses_processed, count);
    assert!(controller.stats().max_pipeline_depth <= 32);
}

// ==================== Concurrent Enqueue and Process Tests ====================

#[test]
fn concurrent_enqueue_and_process() {
    let mut controller = PipelineController::new(8);

    // Simulate: enqueue some, process some, enqueue more, process more
    controller.enqueue_entry(0);
    controller.enqueue_entry(1);

    controller.mark_ready(0);
    process_entry(&mut controller, 0);

    controller.enqueue_entry(2);
    controller.enqueue_entry(3);

    controller.mark_ready(1);
    controller.mark_ready(2);
    process_entry(&mut controller, 1);
    process_entry(&mut controller, 2);

    controller.mark_ready(3);
    process_entry(&mut controller, 3);

    controller.mark_wire_exhausted();
    assert!(controller.is_complete());
}

// ==================== Wire Exhaustion Tests ====================

#[test]
fn wire_exhaustion_with_pending_entries() {
    let mut controller = PipelineController::new(8);

    controller.enqueue_entry(0);
    controller.enqueue_entry(1);
    controller.mark_wire_exhausted();

    // Not complete - still have pending entries
    assert!(!controller.is_complete());

    // Process them
    controller.mark_ready(0);
    controller.mark_ready(1);
    process_entry(&mut controller, 0);
    process_entry(&mut controller, 1);

    // Now complete
    assert!(controller.is_complete());
}

#[test]
fn wire_exhaustion_with_responses_pending() {
    let mut controller = PipelineController::new(8);

    controller.enqueue_entry(0);
    controller.mark_ready(0);
    let _ = controller.dequeue_ready();
    controller.submit_response(0);
    controller.mark_wire_exhausted();

    // Not complete - still have pending response
    assert!(!controller.is_complete());

    let _ = controller.dequeue_response();

    // Now complete
    assert!(controller.is_complete());
}

#[test]
fn wire_exhaustion_immediate_completion() {
    let mut controller = PipelineController::new(8);

    // Empty pipeline, mark exhausted
    controller.mark_wire_exhausted();

    // Should be immediately complete
    assert!(controller.is_complete());
    assert_eq!(controller.next_priority(), None);
}

// ==================== Multiple Complete Cycles Tests ====================

#[test]
fn multiple_complete_cycles_not_supported() {
    let mut controller = PipelineController::new(8);

    // First cycle
    controller.enqueue_entry(0);
    controller.mark_ready(0);
    process_entry(&mut controller, 0);
    controller.mark_wire_exhausted();
    assert!(controller.is_complete());

    // Cannot restart - wire exhaustion is permanent
    assert!(!controller.can_fill());
    assert_eq!(controller.next_priority(), None);
}

// ==================== Empty File List Tests ====================

#[test]
fn empty_file_list_handling() {
    let mut controller = PipelineController::new(8);

    // Immediately mark exhausted without enqueueing anything
    controller.mark_wire_exhausted();

    assert!(controller.is_complete());
    assert_eq!(controller.stats().entries_enqueued, 0);
    assert_eq!(controller.stats().entries_processed, 0);
    assert_eq!(controller.stats().responses_processed, 0);
    assert_eq!(controller.next_priority(), None);
}

#[test]
fn empty_queue_states() {
    let controller = PipelineController::new(8);

    assert!(!controller.has_ready_entries());
    assert!(!controller.has_pending_responses());
    assert!(controller.can_fill());
    assert!(!controller.is_complete());
    assert_eq!(
        controller.next_priority(),
        Some(PipelinePriority::FillPipeline)
    );
}

// ==================== State Transition Tests ====================

#[test]
fn state_transitions_through_lifecycle() {
    let mut controller = PipelineController::new(4);

    assert_eq!(*controller.state(), PipelineState::Idle);

    controller.transition_to(PipelineState::FillingPipeline);
    assert_eq!(*controller.state(), PipelineState::FillingPipeline);

    controller.transition_to(PipelineState::ProcessingEntry);
    assert_eq!(*controller.state(), PipelineState::ProcessingEntry);

    controller.transition_to(PipelineState::ReadingWire);
    assert_eq!(*controller.state(), PipelineState::ReadingWire);

    controller.transition_to(PipelineState::ProcessingResponse);
    assert_eq!(*controller.state(), PipelineState::ProcessingResponse);

    controller.transition_to(PipelineState::Completed);
    assert_eq!(*controller.state(), PipelineState::Completed);
}

// ==================== Edge Cases ====================

#[test]
fn mark_ready_nonexistent_entry_no_panic() {
    let mut controller = PipelineController::new(8);

    controller.enqueue_entry(0);

    // Mark a different entry as ready (not in pending queue)
    controller.mark_ready(999);

    // Should not panic, just no-op
    assert!(!controller.has_ready_entries());
}

#[test]
fn dequeue_ready_when_empty_returns_none() {
    let mut controller = PipelineController::new(8);

    assert_eq!(controller.dequeue_ready(), None);
    assert_eq!(controller.stats().entries_processed, 0);
}

#[test]
fn dequeue_response_when_empty_returns_none() {
    let mut controller = PipelineController::new(8);

    assert_eq!(controller.dequeue_response(), None);
    assert_eq!(controller.stats().responses_processed, 0);
}

#[test]
fn pipeline_stats_clone_and_equality() {
    let stats1 = PipelineStats {
        entries_enqueued: 10,
        entries_processed: 8,
        responses_processed: 6,
        pipeline_depth: 2,
        max_pipeline_depth: 5,
    };

    let stats2 = stats1.clone();
    assert_eq!(stats1, stats2);
}

#[test]
fn capacity_zero_cannot_fill() {
    let controller = PipelineController::new(0);

    assert!(!controller.can_fill());
}

#[test]
fn large_capacity_allows_many_entries() {
    let mut controller = PipelineController::new(10000);

    for i in 0..1000 {
        assert!(controller.can_fill());
        controller.enqueue_entry(i);
    }

    assert_eq!(controller.stats().entries_enqueued, 1000);
    assert_eq!(controller.stats().pipeline_depth, 1000);
}
