//! Lock-free SPSC (single-producer, single-consumer) channel.
//!
//! Built on [`crossbeam_queue::ArrayQueue`] with [`std::sync::atomic::AtomicBool`]
//! disconnection flags and a bounded-spin-then-park wait strategy.  The
//! uncontended path stays syscall-free: a ready slot costs one lock-free queue
//! op plus one atomic flag load.  Only a *failed* probe enters the wait path,
//! which spins a short fixed budget (absorbing the sub-microsecond gaps of an
//! active pipeline) and then parks the thread on a per-side [`Condvar`] until
//! the counterpart signals.  A parked half releases its core entirely, so an
//! idle or slow-sink stage costs no CPU and no timed-sleep syscalls, and CPU
//! oversubscription cannot livelock the pipeline.
//!
//! Designed for the network → disk thread pipeline where exactly one producer
//! (network ingest) and one consumer (disk commit) exchange `FileMessage`
//! items at high throughput.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crossbeam_queue::ArrayQueue;

/// Fixed spin budget before parking, in failed-probe iterations.
///
/// When the pipeline is active, the counterpart typically frees a slot within
/// a few hundred [`std::hint::spin_loop`] hints (roughly single-digit
/// microseconds), so the budget keeps handoffs off the park path.  The budget
/// is deliberately static: the park path below makes longer waits free, so
/// there is nothing for an adaptive controller to optimize.
const SPIN_LIMIT: u32 = 512;

/// One side's park/wake primitive: a wake token guarded by a mutex plus a
/// `parked` flag that lets the signaling side skip the mutex entirely on the
/// hot path.
///
/// Lost-wakeup safety follows the standard park-loop protocol:
///
/// 1. The waiter publishes intent (`parked = true`, SeqCst store - a full
///    barrier), *re-checks* the queue condition, and only then blocks on the
///    condvar waiting for the token.
/// 2. The waker performs its queue transition, issues a [`fence`]`(SeqCst)`,
///    and loads `parked`.  The fence orders the queue op before the flag load,
///    so the classic store-buffering race is impossible: either the waiter's
///    re-check observes the transition (and it never parks), or the waker
///    observes `parked = true` (and delivers the token under the mutex).
/// 3. The token is set and consumed under the mutex, so a wake issued between
///    the waiter's re-check and its `Condvar::wait` is never lost.
///
/// A stale token (waker saw `parked` but the waiter's re-check succeeded) at
/// worst causes one spurious pass through the caller's probe loop.
struct Waiter {
    parked: AtomicBool,
    token: Mutex<bool>,
    cv: Condvar,
    /// Number of completed park episodes; diagnostic only (read by tests).
    parks: AtomicU64,
}

impl Waiter {
    fn new() -> Self {
        Self {
            parked: AtomicBool::new(false),
            token: Mutex::new(false),
            cv: Condvar::new(),
            parks: AtomicU64::new(0),
        }
    }

    /// Parks the current thread while `blocked()` holds, until [`wake`](Self::wake)
    /// delivers a token.  Returns immediately if `blocked()` is already false
    /// after intent is published.
    fn park_while<F: Fn() -> bool>(&self, blocked: F) {
        self.parked.store(true, Ordering::SeqCst);
        // Re-check after publishing intent: any queue transition from here on
        // is guaranteed to observe `parked` and deliver a token.
        if !blocked() {
            self.parked.store(false, Ordering::SeqCst);
            return;
        }
        self.parks.fetch_add(1, Ordering::Relaxed);
        let mut token = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        while !*token {
            token = self.cv.wait(token).unwrap_or_else(PoisonError::into_inner);
        }
        *token = false;
        drop(token);
        self.parked.store(false, Ordering::SeqCst);
    }

    /// Wakes the counterpart if it is parked (or about to park).
    ///
    /// Hot path: one SeqCst fence plus one atomic load; the mutex is touched
    /// only when the counterpart actually registered intent to park.
    fn wake(&self) {
        fence(Ordering::SeqCst);
        if !self.parked.load(Ordering::SeqCst) {
            return;
        }
        let mut token = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        *token = true;
        self.cv.notify_one();
    }
}

struct Shared<T> {
    queue: ArrayQueue<T>,
    producer_alive: AtomicBool,
    consumer_alive: AtomicBool,
    /// Producer parks here when the queue is full; woken by the consumer.
    send_waiter: Waiter,
    /// Consumer parks here when the queue is empty; woken by the producer.
    recv_waiter: Waiter,
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
    /// Sends `item` through the channel, blocking if the queue is full: a
    /// short fixed spin, then a park until the consumer frees a slot.
    ///
    /// Returns `Err(SendError(item))` if the receiver has been dropped.
    pub fn send(&self, mut item: T) -> Result<(), SendError<T>> {
        loop {
            let mut spins = 0;
            loop {
                if !self.0.consumer_alive.load(Ordering::Acquire) {
                    return Err(SendError(item));
                }
                match self.0.queue.push(item) {
                    Ok(()) => {
                        self.0.recv_waiter.wake();
                        return Ok(());
                    }
                    Err(returned) => item = returned,
                }
                spins += 1;
                if spins >= SPIN_LIMIT {
                    break;
                }
                std::hint::spin_loop();
            }
            self.0.send_waiter.park_while(|| {
                self.0.queue.is_full() && self.0.consumer_alive.load(Ordering::Acquire)
            });
        }
    }

    /// Attempts to send `item` without blocking.
    ///
    /// Returns `Err(SendError(item))` immediately if the queue is full or the
    /// receiver has been dropped, instead of blocking like [`send`](Self::send).
    /// This is the non-blocking analog used for the buffer-return path, where
    /// `item` is only a recycling spare (never live data): if the ring is full,
    /// the returned buffer is simply dropped and the consumer allocates a fresh
    /// one. Never use this for data-carrying channels.
    pub fn try_send(&self, item: T) -> Result<(), SendError<T>> {
        if !self.0.consumer_alive.load(Ordering::Acquire) {
            return Err(SendError(item));
        }
        self.0.queue.push(item).map_err(SendError)?;
        self.0.recv_waiter.wake();
        Ok(())
    }

    /// Returns `true` if the receiver has been dropped.
    #[cfg(test)]
    pub fn is_disconnected(&self) -> bool {
        !self.0.consumer_alive.load(Ordering::Acquire)
    }

    /// Number of times the consumer has parked waiting for an item.
    #[cfg(test)]
    pub fn consumer_parks(&self) -> u64 {
        self.0.recv_waiter.parks.load(Ordering::Relaxed)
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        self.0.producer_alive.store(false, Ordering::Release);
        // Wake a consumer parked on an empty queue so it can observe the
        // disconnect instead of sleeping forever.
        self.0.recv_waiter.wake();
    }
}

/// Receiving half of the SPSC channel.
pub struct Receiver<T>(Arc<Shared<T>>);

impl<T> Receiver<T> {
    /// Blocks until an item is available: a short fixed spin, then a park
    /// until the producer pushes an item or disconnects.
    ///
    /// Returns `Err(RecvError)` if the sender has been dropped and the
    /// queue is fully drained.
    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            let mut spins = 0;
            loop {
                if let Some(item) = self.0.queue.pop() {
                    self.0.send_waiter.wake();
                    return Ok(item);
                }
                if !self.0.producer_alive.load(Ordering::Acquire) {
                    // Producer is gone - drain one last time.
                    return self.0.queue.pop().ok_or(RecvError);
                }
                spins += 1;
                if spins >= SPIN_LIMIT {
                    break;
                }
                std::hint::spin_loop();
            }
            self.0.recv_waiter.park_while(|| {
                self.0.queue.is_empty() && self.0.producer_alive.load(Ordering::Acquire)
            });
        }
    }

    /// Non-blocking receive.  Returns `Err(TryRecvError::Empty)` if the
    /// queue is empty but the sender is alive, or
    /// `Err(TryRecvError::Disconnected)` if the sender is gone and the
    /// queue is drained.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        if let Some(item) = self.0.queue.pop() {
            self.0.send_waiter.wake();
            return Ok(item);
        }
        if !self.0.producer_alive.load(Ordering::Acquire) {
            // One more attempt after observing disconnection.
            return self.0.queue.pop().ok_or(TryRecvError::Disconnected);
        }
        Err(TryRecvError::Empty)
    }

    /// Number of times the producer has parked waiting for a free slot.
    #[cfg(test)]
    pub fn producer_parks(&self) -> u64 {
        self.0.send_waiter.parks.load(Ordering::Relaxed)
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.0.consumer_alive.store(false, Ordering::Release);
        // Wake a producer parked on a full queue so it can observe the
        // disconnect instead of sleeping forever.
        self.0.send_waiter.wake();
    }
}

/// Creates a bounded SPSC channel backed by a lock-free ring buffer.
///
/// `capacity` is the maximum number of items the channel can hold.
/// When the channel is full, [`Sender::send`] blocks (spin, then park)
/// until space is available.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        queue: ArrayQueue::new(capacity),
        producer_alive: AtomicBool::new(true),
        consumer_alive: AtomicBool::new(true),
        send_waiter: Waiter::new(),
        recv_waiter: Waiter::new(),
    });
    (Sender(Arc::clone(&shared)), Receiver(shared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::{Duration, Instant};

    /// Polls `cond` until it holds, failing the test after a generous
    /// deadline so a lost wakeup surfaces as a failure, not a hang.
    fn wait_for(what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::yield_now();
        }
    }

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

        // Queue of capacity 2 - sender must block when full.
        let (tx, rx) = channel::<i32>(2);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        // Queue is full.  Spawn a thread to drain all items, keeping rx
        // alive until after the sender unblocks (avoids race where rx
        // drops before the blocking send completes).
        //
        // Use an atomic flag instead of a fixed sleep so the drain thread
        // waits until the sender is about to enter its blocking wait.
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
        // block. This is the property the disk thread's buffer-return path
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

    /// A consumer parked on an empty queue must be woken by a push and
    /// deliver the item (no lost wakeup on the empty → non-empty edge).
    #[test]
    fn parked_consumer_wakes_on_push() {
        let (tx, rx) = channel::<i32>(4);
        let consumer = thread::spawn(move || rx.recv().unwrap());
        // Wait until the consumer has actually parked, not merely started.
        wait_for("consumer to park", || tx.consumer_parks() > 0);
        tx.send(42).unwrap();
        assert_eq!(consumer.join().unwrap(), 42);
    }

    /// An idle producer must leave the consumer *parked* - blocked on the
    /// condvar at ~0 CPU - not busy-spinning at the pipeline constraint.  This
    /// locks in the bounded-spin-then-park wait strategy (#7033): once the
    /// consumer parks on an empty queue it must STAY parked for a single, stable
    /// park episode until the producer pushes, then wake exactly once and
    /// deliver the item.  Matters because a regression back to a userspace spin
    /// burns a core while the pipeline waits on its slowest stage; the park
    /// counter is the observable that distinguishes blocking from spinning.
    ///
    /// The counter is asserted rather than wall-clock CPU% (which is flaky): a
    /// spin regression leaves it pinned at 0 (never parks, so `wait_for` times
    /// out and fails), while a busy re-park regression makes it climb during the
    /// idle window; a genuinely blocked consumer holds it at exactly 1.  The
    /// idle window samples the counter, it is not a synchronization primitive -
    /// `park_while` increments `parks` once and only returns on a real wake, so
    /// the count is deterministic regardless of scheduling.
    ///
    /// oc-specific infrastructure: the SPSC network -> disk pipeline has no
    /// upstream rsync analog (upstream has no such threaded pipeline).
    #[test]
    fn idle_producer_keeps_consumer_parked() {
        let (tx, rx) = channel::<i32>(4);
        let consumer = thread::spawn(move || rx.recv().unwrap());
        // Wait until the consumer has actually parked on the empty queue.
        wait_for("consumer to park", || tx.consumer_parks() == 1);
        // Hold the producer idle: the park episode count must stay pinned at
        // exactly 1 across the whole window.  Any climb betrays a busy re-park
        // or spin regression; a stall at 0 could not reach here at all.
        let idle_until = Instant::now() + Duration::from_millis(200);
        while Instant::now() < idle_until {
            assert_eq!(
                tx.consumer_parks(),
                1,
                "consumer re-parked while the producer was idle - busy-wait regression"
            );
            thread::yield_now();
        }
        // The producer finally pushes: the parked consumer wakes exactly once
        // and delivers the item, with no additional park episode.
        tx.send(7).unwrap();
        assert_eq!(consumer.join().unwrap(), 7);
        assert_eq!(
            tx.consumer_parks(),
            1,
            "consumer parked more than once for a single push"
        );
    }

    /// A producer parked on a full queue must be woken by a pop and complete
    /// its send (no lost wakeup on the full → non-full edge).
    #[test]
    fn parked_producer_wakes_on_pop() {
        let (tx, rx) = channel::<i32>(1);
        tx.send(1).unwrap();
        let producer = thread::spawn(move || tx.send(2).unwrap());
        wait_for("producer to park", || rx.producer_parks() > 0);
        assert_eq!(rx.recv().unwrap(), 1);
        producer.join().unwrap();
        assert_eq!(rx.recv().unwrap(), 2);
    }

    /// Dropping the sender while the consumer is parked must wake it and
    /// surface the disconnect (no hang on teardown).
    #[test]
    fn sender_drop_wakes_parked_consumer() {
        let (tx, rx) = channel::<i32>(4);
        let consumer = thread::spawn(move || rx.recv());
        wait_for("consumer to park", || tx.consumer_parks() > 0);
        drop(tx);
        assert_eq!(consumer.join().unwrap(), Err(RecvError));
    }

    /// Dropping the receiver while the producer is parked must wake it and
    /// surface the disconnect with the item returned (no hang on teardown).
    #[test]
    fn receiver_drop_wakes_parked_producer() {
        let (tx, rx) = channel::<i32>(1);
        tx.send(1).unwrap();
        let producer = thread::spawn(move || tx.send(2));
        wait_for("producer to park", || rx.producer_parks() > 0);
        drop(rx);
        let SendError(item) = producer
            .join()
            .unwrap()
            .expect_err("disconnect must reject");
        assert_eq!(item, 2);
    }

    /// With a consumer far slower than the spin budget, the producer must
    /// take the park path instead of busy-waiting.  Asserted via the parker's
    /// episode counter, not wall-clock.
    #[test]
    fn slow_consumer_parks_producer() {
        let n = 64;
        let (tx, rx) = channel::<usize>(2);
        let producer = thread::spawn(move || {
            for i in 0..n {
                tx.send(i).unwrap();
            }
        });
        for i in 0..n {
            // Sleep far longer than the spin budget so a full queue forces
            // the producer past spinning and into a park.
            thread::sleep(Duration::from_micros(200));
            assert_eq!(rx.recv().unwrap(), i);
        }
        producer.join().unwrap();
        assert!(
            rx.producer_parks() > 0,
            "producer never parked against a slow consumer - busy-wait regression"
        );
    }

    /// Oversubscription stress: more producer/consumer pipelines than cores,
    /// each with a capacity-1 ring, so both halves constantly hit the wait
    /// path while the scheduler time-slices.  A waiter that never released
    /// its core could starve its counterparty and livelock (the historical
    /// LTO-off, high-host-load daemon failure).  Parking makes completion
    /// event-driven - a blocked half sleeps until its counterpart signals -
    /// so every pipeline must finish and reconstruct its stream in order.
    #[test]
    fn oversubscription_no_livelock() {
        let pipelines = 4 * std::thread::available_parallelism().map_or(4, |n| n.get());
        let n: usize = 2_000;

        let mut handles = Vec::with_capacity(pipelines);
        for _ in 0..pipelines {
            // Capacity 1: producer blocks on nearly every send, consumer
            // blocks on nearly every recv - maximum time in the wait paths.
            let (tx, rx) = channel::<usize>(1);
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
            handles.push((producer, consumer));
        }

        let expected: Vec<usize> = (0..n).collect();
        for (producer, consumer) in handles {
            producer.join().unwrap();
            assert_eq!(consumer.join().unwrap(), expected);
        }
    }
}
