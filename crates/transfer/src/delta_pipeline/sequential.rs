//! Sequential delta pipeline that processes each item immediately.

use std::io;

use engine::concurrent_delta::strategy::dispatch;
use engine::concurrent_delta::{DeltaResult, DeltaWork};

use super::ReceiverDeltaPipeline;

/// Sequential delta pipeline that processes each item immediately.
///
/// This is the default pipeline implementation. Each call to
/// [`submit_work`](ReceiverDeltaPipeline::submit_work) synchronously
/// dispatches the work item through the appropriate
/// [`DeltaStrategy`](engine::concurrent_delta::DeltaStrategy) and buffers
/// the result for the next [`poll_result`](ReceiverDeltaPipeline::poll_result)
/// call.
///
/// No threads are spawned. Processing order matches submission order, which
/// is identical to upstream rsync's sequential `recv_files()` loop.
///
/// # Upstream Reference
///
/// Mirrors the 1:1 dispatch in `receiver.c:recv_files()` where each file is
/// fully processed before moving to the next.
#[derive(Debug, Default)]
pub struct SequentialDeltaPipeline {
    /// Sequence counter for stamping work items before dispatch.
    next_sequence: u64,
    /// Results waiting to be polled, in submission order.
    ready: Vec<DeltaResult>,
    /// Read cursor into `ready` for FIFO delivery.
    cursor: usize,
}

impl SequentialDeltaPipeline {
    /// Creates a new sequential pipeline.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ReceiverDeltaPipeline for SequentialDeltaPipeline {
    fn submit_work(&mut self, mut work: DeltaWork) -> io::Result<()> {
        let seq = self.next_sequence;
        self.next_sequence += 1;
        work.set_sequence(seq);
        let result = dispatch(&work);
        self.ready.push(result);
        Ok(())
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        if self.cursor < self.ready.len() {
            let result = self.ready[self.cursor].clone();
            self.cursor += 1;
            Some(result)
        } else {
            None
        }
    }

    fn flush(self: Box<Self>) -> Vec<DeltaResult> {
        if self.cursor >= self.ready.len() {
            return Vec::new();
        }
        self.ready.into_iter().skip(self.cursor).collect()
    }
}
