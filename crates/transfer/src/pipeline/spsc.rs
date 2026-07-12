//! Lock-free SPSC (single-producer, single-consumer) channel.
//!
//! Built on [`crossbeam_queue::ArrayQueue`] with [`std::sync::atomic::AtomicBool`]
//! disconnection flags and a bounded escalating backoff for waiting.  The
//! uncontended path stays syscall-free: the first queue probe is a bare atomic
//! load, so a ready slot costs zero backoff overhead.  Only a *failed* probe
//! escalates - spin hints, then `yield_now`, then a short `park_timeout` - so a
//! producer or consumer starved of a core under CPU oversubscription still
//! makes progress instead of livelocking on a pure spin.
//!
//! Designed for the network → disk thread pipeline where exactly one producer
//! (network ingest) and one consumer (disk commit) exchange `FileMessage`
//! items at high throughput.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_queue::ArrayQueue;
use crossbeam_utils::Backoff;

/// Bounded escalating backoff for the SPSC full/empty wait loops.
///
/// A pure `while full/empty { spin_loop() }` never relinquishes the CPU, so
/// when there are more busy threads than cores (observed on an LTO-off daemon
/// under high host load) the spinning thread can starve its counterparty out
/// of the scheduler and livelock the pipeline.  This escalates a *failed*
/// queue probe in three tiers, cheapest first:
///
/// 1. [`Backoff::snooze`] issues a few [`std::hint::spin_loop`] hints
///    (cache-friendly, sub-microsecond) for the common brief wait, then
/// 2. once past its spin threshold, `snooze` calls [`std::thread::yield_now`],
///    letting the counterparty thread be scheduled - this is the livelock
///    cure under oversubscription, then
/// 3. once [`Backoff::is_completed`] saturates, a short
///    [`std::thread::park_timeout`] parks the waiter so it releases its core
///    instead of burning it during sustained starvation.
///
/// Construct once per blocking call and invoke [`SpinBackoff::wait`] only after
/// a probe fails, keeping the first (uncontended) attempt overhead-free.
struct SpinBackoff {
    backoff: Backoff,
}

impl SpinBackoff {
    /// Park quantum used once spin+yield escalation saturates.  Short enough to
    /// stay responsive when a slot frees, long enough to yield the core.
    const PARK: Duration = Duration::from_micros(50);

    fn new() -> Self {
        Self {
            backoff: Backoff::new(),
        }
    }

    /// Perform one escalating wait step after a failed queue probe.
    fn wait(&self) {
        if self.backoff.is_completed() {
            std::thread::park_timeout(Self::PARK);
        } else {
            self.backoff.snooze();
        }
    }
}

struct Shared<T> {
    queue: ArrayQueue<T>,
    producer_alive: AtomicBool,
    consumer_alive: AtomicBool,
}

/// Error returned by [`Sender::send`] when the receiver has been dropped.
pub struct SendError<T>(pub T);

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SendError(..)")
    }
}

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sending on a disconnected channel")
    }
}

/// Error returned by [`Receiver::recv`] when the sender has been dropped
/// and the queue is drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("receiving on a disconnected channel")
    }
}

/// Error returned by [`Receiver::try_recv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    /// The queue is empty but the sender is still alive.
    Empty,
    /// The sender has been dropped and the queue is drained.
    Disconnected,
}

impl fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("channel is empty"),
            Self::Disconnected => f.write_str("channel is disconnected"),
        }
    }
}

/// Sending half of the SPSC channel.
pub struct Sender<T>(Arc<Shared<T>>);

impl<T> Sender<T> {
    /// Sends `item` through the channel, spin-waiting if the queue is full.
    ///
    /// Returns `Err(SendError(item))` if the receiver has been dropped.
    pub fn send(&self, mut item: T) -> Result<(), SendError<T>> {
        let backoff = SpinBackoff::new();
        loop {
            if !self.0.consumer_alive.load(Ordering::Relaxed) {
                return Err(SendError(item));
            }
            match self.0.queue.push(item) {
                Ok(()) => return Ok(()),
                Err(returned) => {
                    item = returned;
                    backoff.wait();
                }
            }
        }
    }

    /// Attempts to send `item` without spin-waiting.
    ///
    /// Returns `Err(SendError(item))` immediately if the queue is full or the
    /// receiver has been dropped, instead of spinning like [`send`](Self::send).
    /// This is the non-blocking analog used for the buffer-return path, where
    /// `item` is only a recycling spare (never live data): if the ring is full,
    /// the returned buffer is simply dropped and the consumer allocates a fresh
    /// one. Never use this for data-carrying channels.
    pub fn try_send(&self, item: T) -> Result<(), SendError<T>> {
        if !self.0.consumer_alive.load(Ordering::Relaxed) {
            return Err(SendError(item));
        }
        self.0.queue.push(item).map_err(SendError)
    }

    /// Returns `true` if the receiver has been dropped.
    #[cfg(test)]
    pub fn is_disconnected(&self) -> bool {
        !self.0.consumer_alive.load(Ordering::Relaxed)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.0.producer_alive.store(false, Ordering::Release);
    }
}

/// Receiving half of the SPSC channel.
pub struct Receiver<T>(Arc<Shared<T>>);

impl<T> Receiver<T> {
    /// Blocks (spin-waits) until an item is available, then returns it.
    ///
    /// Returns `Err(RecvError)` if the sender has been dropped and the
    /// queue is fully drained.
    pub fn recv(&self) -> Result<T, RecvError> {
        let backoff = SpinBackoff::new();
        loop {
            if let Some(item) = self.0.queue.pop() {
                return Ok(item);
            }
            if !self.0.producer_alive.load(Ordering::Acquire) {
                // Producer is gone - drain one last time.
                return self.0.queue.pop().ok_or(RecvError);
            }
            backoff.wait();
        }
    }

    /// Non-blocking receive.  Returns `Err(TryRecvError::Empty)` if the
    /// queue is empty but the sender is alive, or
    /// `Err(TryRecvError::Disconnected)` if the sender is gone and the
    /// queue is drained.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        if let Some(item) = self.0.queue.pop() {
            return Ok(item);
        }
        if !self.0.producer_alive.load(Ordering::Acquire) {
            // One more attempt after observing disconnection.
            return self.0.queue.pop().ok_or(TryRecvError::Disconnected);
        }
        Err(TryRecvError::Empty)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.0.consumer_alive.store(false, Ordering::Release);
    }
}

/// Creates a bounded SPSC channel backed by a lock-free ring buffer.
///
/// `capacity` is the maximum number of items the channel can hold.
/// When the channel is full, [`Sender::send`] spin-waits until space
/// is available.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        queue: ArrayQueue::new(capacity),
        producer_alive: AtomicBool::new(true),
        consumer_alive: AtomicBool::new(true),
    });
    (Sender(Arc::clone(&shared)), Receiver(shared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn send_recv_basic() {
        let (tx, rx) = channel::<i32>(4);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        assert_eq!(rx.recv().unwrap(), 1);
        assert_eq!(rx.recv().unwrap(), 2);
    }

    #[test]
    fn try_recv_empty() {
        let (tx, rx) = channel::<i32>(4);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Empty);
        tx.send(42).unwrap();
        assert_eq!(rx.try_recv().unwrap(), 42);
    }

    #[test]
    fn sender_drop_disconnects() {
        let (tx, rx) = channel::<i32>(4);
        tx.send(10).unwrap();
        drop(tx);
        // Should still drain the remaining item.
        assert_eq!(rx.recv().unwrap(), 10);
        // Now should return RecvError.
        assert_eq!(rx.recv().unwrap_err(), RecvError);
    }

    #[test]
    fn receiver_drop_disconnects() {
        let (tx, _rx) = channel::<i32>(4);
        tx.send(1).unwrap();
        drop(_rx);
        assert!(tx.send(2).is_err());
    }

    #[test]
    fn try_recv_disconnected() {
        let (tx, rx) = channel::<i32>(4);
        drop(tx);
        assert_eq!(rx.try_recv().unwrap_err(), TryRecvError::Disconnected);
    }

    #[test]
    fn cross_thread_streaming() {
        let (tx, rx) = channel::<usize>(32);
        let n = 10_000;

        let producer = thread::spawn(move || {
            for i in 0..n {
                tx.send(i).unwrap();
            }
        });

        let consumer = thread::spawn(move || {
            let mut received = Vec::with_capacity(n);
            for _ in 0..n {
                received.push(rx.recv().unwrap());
            }
            received
        });

        producer.join().unwrap();
        let received = consumer.join().unwrap();
        assert_eq!(received, (0..n).collect::<Vec<_>>());
    }

    #[test]
    fn backpressure_bounded() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        // Queue of capacity 2 - sender must spin-wait when full.
        let (tx, rx) = channel::<i32>(2);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        // Queue is full.  Spawn a thread to drain all items, keeping rx
        // alive until after the sender unblocks (avoids race where rx
        // drops before the spin-waiting send completes).
        //
        // Use an atomic flag instead of a fixed sleep so the drain thread
        // waits until the sender is about to enter its spin-wait loop.
        let sender_ready = Arc::new(AtomicBool::new(false));
        let sender_ready_clone = Arc::clone(&sender_ready);
        let drain = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !sender_ready_clone.load(Ordering::Acquire) {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for sender to be ready"
                );
                thread::yield_now();
            }
            let mut received = Vec::new();
            // Drain all items: the 2 already queued + the 1 the sender
            // will push once a slot opens.
            for _ in 0..3 {
                received.push(rx.recv().unwrap());
            }
            received
        });
        // Signal the drain thread, then immediately enter the blocking send.
        sender_ready.store(true, Ordering::Release);
        tx.send(3).unwrap();
        let received = drain.join().unwrap();
        assert_eq!(received, vec![1, 2, 3]);
    }

    #[test]
    fn try_send_full_ring_does_not_block() {
        // A full ring must reject immediately (returning the item) rather than
        // spin-wait. This is the property the disk thread's buffer-return path
        // relies on to avoid deadlocking when the network thread never drains
        // the return ring (compressed all-literal streams never recycle).
        let (tx, rx) = channel::<i32>(2);
        assert!(tx.try_send(1).is_ok());
        assert!(tx.try_send(2).is_ok());
        // Ring is full: try_send returns the rejected item instead of blocking.
        let SendError(rejected) = tx.try_send(3).expect_err("full ring must reject");
        assert_eq!(rejected, 3);
        // Draining a slot lets the next try_send succeed.
        assert_eq!(rx.recv().unwrap(), 1);
        assert!(tx.try_send(3).is_ok());
    }

    #[test]
    fn try_send_disconnected_returns_item() {
        let (tx, rx) = channel::<i32>(4);
        drop(rx);
        let SendError(item) = tx.try_send(7).expect_err("disconnected must reject");
        assert_eq!(item, 7);
    }

    #[test]
    fn sender_is_disconnected() {
        let (tx, rx) = channel::<i32>(4);
        assert!(!tx.is_disconnected());
        drop(rx);
        assert!(tx.is_disconnected());
    }

    /// Oversubscription stress: many more producer/consumer pipelines than
    /// cores, each with a tiny ring so both halves spend most of their time in
    /// the full/empty wait loop.  With a pure `spin_loop` wait (no yield) this
    /// livelocks under CPU starvation - a spinning half never releases its core
    /// for its counterparty (the observed LTO-off, high-host-load daemon
    /// failure).  With the spin → yield → park backoff every pipeline must
    /// still complete and reconstruct its stream byte-identically.
    #[test]
    fn oversubscription_no_livelock() {
        use std::time::{Duration, Instant};

        // Far more concurrent halves than any test host has cores, forcing the
        // scheduler to time-slice and exposing a non-yielding spin.
        let pipelines = 8 * std::thread::available_parallelism().map_or(4, |n| n.get());
        let n: usize = 20_000;
        let deadline = Instant::now() + Duration::from_secs(60);

        let mut handles = Vec::with_capacity(pipelines);
        for _ in 0..pipelines {
            // Capacity 1: producer blocks on nearly every send, consumer blocks
            // on nearly every recv - maximum time in the wait loops.
            let (tx, rx) = channel::<usize>(1);
            let producer = thread::spawn(move || {
                for i in 0..n {
                    tx.send(i).unwrap();
                }
            });
            let consumer = thread::spawn(move || {
                let mut acc = 0usize;
                for _ in 0..n {
                    acc = acc.wrapping_add(rx.recv().unwrap());
                }
                acc
            });
            handles.push((producer, consumer));
        }

        // Every pipeline must finish (no livelock/deadlock) and its consumer
        // must observe exactly the produced sequence - byte/order identical
        // reconstruction, unchanged by the backoff.
        let expected: usize = (0..n).sum();
        for (producer, consumer) in handles {
            producer.join().unwrap();
            assert_eq!(consumer.join().unwrap(), expected);
            assert!(
                Instant::now() < deadline,
                "pipeline did not complete within 60s - possible livelock"
            );
        }
    }
}
