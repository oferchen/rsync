//! Lock-free SPSC (single-producer, single-consumer) channel.
//!
//! Built on [`crossbeam_queue::ArrayQueue`] with [`std::sync::atomic::AtomicBool`]
//! disconnection flags.  The uncontended path stays syscall-free: a ready slot
//! costs one lock-free queue op plus one fence and one atomic flag load.  Only
//! a *failed* probe enters a wait path, and the two sides wait differently:
//!
//! - The consumer on an empty queue spins a short fixed budget (absorbing the
//!   sub-microsecond gaps of an active pipeline), then parks until the
//!   producer pushes.  Every push wakes it: an item it is waiting for must
//!   never sit in the queue behind a batching threshold.
//! - The producer on a full queue parks at once and is woken only when the
//!   consumer has drained the queue to its low-water mark (half capacity).
//!   A full queue already holds a backlog for the consumer, so spinning
//!   cannot raise throughput; re-filling one slot per pop would only trade a
//!   spin or a futex round trip per item for nothing.  The hysteresis turns
//!   that into one park and one wake per half-queue of items.
//!
//! A parked half releases its core entirely, so a slow stage costs no CPU and
//! no timed-sleep syscalls, and CPU oversubscription cannot livelock the
//! pipeline.
//!
//! Designed for the network → disk thread pipeline where exactly one producer
//! (network ingest) and one consumer (disk commit) exchange `FileMessage`
//! items at high throughput.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use crossbeam_queue::ArrayQueue;

/// Consumer spin budget before parking on an empty queue, in failed-probe
/// iterations.
const SPIN_LIMIT: u32 = 512;

/// The occupancy at or below which a pop wakes a producer parked on a full
/// queue: half the capacity, so a woken producer refills half the ring in one
/// run while the consumer still has the other half to work on.
const fn low_water(capacity: usize) -> usize {
    capacity / 2
}

/// One side's park/wake primitive: a wake token guarded by a mutex plus a
/// `parked` flag that lets the signaling side skip the mutex entirely on the
/// hot path.
///
/// Lost-wakeup invariant.  The waiter blocks on a condition `blocked()` over
/// the queue and the disconnect flags; the waker's `ready()` is its negation.
/// While a waiter is parked, only the waker moves the queue towards
/// `ready()`, so once `ready()` holds it holds until the waiter runs.
///
/// - Waiter, in order: (W1) store `parked = true`; (W2) full fence; (W3)
///   evaluate `blocked()`, returning at once if it is false; (W4) under the
///   mutex, wait until the token is set, consume it, clear `parked`.
/// - Waker, in order: (K1) its queue or disconnect transition; (K2) full
///   fence; (K3) load `parked`, returning if false; (K4) evaluate `ready()`,
///   returning if false; (K5) under the mutex, set the token and notify.
///
/// The two fences order W1 before W3 and K1 before K3, so the store-buffering
/// outcome where W3 misses K1 *and* K3 misses W1 is impossible: either the
/// waiter's re-check sees the transition and never blocks, or the waker sees
/// `parked` and goes on to K4.  If K4 finds the queue not yet ready, a later
/// transition re-runs K1-K5 with `parked` still set, and the one that makes
/// it ready delivers the token.  The token is set and consumed under the
/// mutex, so a K5 that lands between W3 and the condvar wait is seen by W4's
/// check and never lost.  A stale token (K5 ran but W3 had already returned)
/// costs at most one spurious return to the caller's retry loop.
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

    /// Parks the current thread while `blocked()` holds, until a wake
    /// delivers a token (W1-W4 above).  Returns immediately if `blocked()` is
    /// already false after intent is published.
    fn park_while<F: Fn() -> bool>(&self, blocked: F) {
        self.parked.store(true, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        checkpoint(Checkpoint::Published);
        if !blocked() {
            self.parked.store(false, Ordering::SeqCst);
            return;
        }
        checkpoint(Checkpoint::Rechecked);
        self.parks.fetch_add(1, Ordering::Relaxed);
        let mut token = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        while !*token {
            checkpoint(Checkpoint::Blocking);
            token = self.cv.wait(token).unwrap_or_else(PoisonError::into_inner);
        }
        *token = false;
        drop(token);
        self.parked.store(false, Ordering::SeqCst);
    }

    /// Wakes the counterpart if it is parked (or about to park) and `ready()`
    /// holds (K2-K5 above).
    ///
    /// Hot path: one SeqCst fence plus one atomic load; `ready()` and the
    /// mutex are touched only when the counterpart registered intent to park.
    fn wake_if(&self, ready: impl FnOnce() -> bool) {
        fence(Ordering::SeqCst);
        if !self.parked.load(Ordering::SeqCst) || !ready() {
            return;
        }
        let mut token = self.token.lock().unwrap_or_else(PoisonError::into_inner);
        *token = true;
        self.cv.notify_one();
    }

    /// Wakes the counterpart unconditionally if it is parked.
    fn wake(&self) {
        self.wake_if(|| true);
    }
}

/// Points inside [`Waiter::park_while`] where a test can run the
/// counterpart's transition on the waiting thread, forcing one interleaving
/// of the lost-wakeup protocol deterministically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Checkpoint {
    /// After W1-W2, before the W3 re-check.
    Published,
    /// After W3 found the queue blocked, before taking the mutex.
    Rechecked,
    /// Under the mutex with no token: the thread is about to sleep.
    Blocking,
}

/// The hook a test installs for [`checkpoint`].
#[cfg(test)]
type CheckpointHook = Box<dyn FnMut(Checkpoint)>;

#[cfg(test)]
thread_local! {
    static CHECKPOINT_HOOK: std::cell::RefCell<Option<CheckpointHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn checkpoint(at: Checkpoint) {
    let hook = CHECKPOINT_HOOK.with(|slot| slot.borrow_mut().take());
    if let Some(mut hook) = hook {
        hook(at);
        CHECKPOINT_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
    }
}

#[cfg(not(test))]
#[inline(always)]
const fn checkpoint(_: Checkpoint) {}

struct Shared<T> {
    queue: ArrayQueue<T>,
    producer_alive: AtomicBool,
    consumer_alive: AtomicBool,
    /// Producer parks here when the queue is full; woken by the consumer once
    /// the queue is at or below `low_water`, or on disconnect.
    send_waiter: Waiter,
    /// Consumer parks here when the queue is empty; woken by every push, or
    /// on disconnect.
    recv_waiter: Waiter,
    low_water: usize,
}

impl<T> Shared<T> {
    /// After a pop: wakes a parked producer once the queue has drained to the
    /// low-water mark.
    fn popped(&self) {
        self.send_waiter
            .wake_if(|| self.queue.len() <= self.low_water);
    }
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
    /// Sends `item` through the channel.  On a full queue it parks until
    /// the consumer has drained the queue to its low-water mark.
    ///
    /// Returns `Err(SendError(item))` if the receiver has been dropped.
    pub fn send(&self, mut item: T) -> Result<(), SendError<T>> {
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
            self.0.send_waiter.park_while(|| {
                self.0.queue.len() > self.0.low_water
                    && self.0.consumer_alive.load(Ordering::Acquire)
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
                    self.0.popped();
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
            self.0.popped();
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
/// When the channel is full, [`Sender::send`] parks until the consumer has
/// drained it to half capacity.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>) {
    let shared = Arc::new(Shared {
        queue: ArrayQueue::new(capacity),
        producer_alive: AtomicBool::new(true),
        consumer_alive: AtomicBool::new(true),
        send_waiter: Waiter::new(),
        recv_waiter: Waiter::new(),
        low_water: low_water(capacity),
    });
    (Sender(Arc::clone(&shared)), Receiver(shared))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::rc::Rc;
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

    /// Runs `act` on this thread the first time it passes `at` inside
    /// `park_while`, and panics if the thread then reaches the point where it
    /// would sleep: with the counterpart's transition already done, nothing
    /// is left to wake it, so reaching `Blocking` is a lost wakeup.
    fn interleave(at: Checkpoint, act: impl FnOnce() + 'static) {
        let mut act = Some(act);
        CHECKPOINT_HOOK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |point| {
                if point == at
                    && let Some(act) = act.take()
                {
                    act();
                }
                assert_ne!(point, Checkpoint::Blocking, "lost wakeup after {at:?}");
            }));
        });
    }

    fn clear_interleave() {
        CHECKPOINT_HOOK.with(|slot| *slot.borrow_mut() = None);
    }

    /// The consumer drains to the low-water mark after the producer published
    /// its intent to park but before its re-check (between W2 and W3): the
    /// re-check must see the room and the producer must not park at all.
    #[test]
    fn a_drain_before_the_producer_recheck_is_seen_by_the_recheck() {
        let (tx, rx) = channel::<u32>(2);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        let rx = Rc::new(rx);
        let consumer = Rc::clone(&rx);
        interleave(Checkpoint::Published, move || {
            assert_eq!(consumer.try_recv(), Ok(1));
        });
        tx.send(3).unwrap();
        clear_interleave();
        assert_eq!(rx.producer_parks(), 0);
        assert_eq!(rx.try_recv(), Ok(2));
        assert_eq!(rx.try_recv(), Ok(3));
    }

    /// The consumer drains to the low-water mark after the producer's
    /// re-check found the queue full but before it takes the mutex (between
    /// W3 and W4): the wake must leave a token, so the producer returns
    /// without sleeping.
    #[test]
    fn a_drain_between_the_producer_recheck_and_its_sleep_leaves_a_token() {
        let (tx, rx) = channel::<u32>(2);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        let rx = Rc::new(rx);
        let consumer = Rc::clone(&rx);
        interleave(Checkpoint::Rechecked, move || {
            assert_eq!(consumer.try_recv(), Ok(1));
        });
        tx.send(3).unwrap();
        clear_interleave();
        assert_eq!(rx.producer_parks(), 1);
        assert_eq!(rx.try_recv(), Ok(2));
        assert_eq!(rx.try_recv(), Ok(3));
    }

    /// The producer is woken only once the queue is at its low-water mark
    /// (half of 4), not by the first pop: one pop leaves it about to sleep,
    /// and the second pop is the one that wakes it.  This hysteresis is what
    /// turns one park per item into one park per half-queue when the
    /// consumer is the slower stage.
    #[test]
    fn a_full_producer_wakes_at_low_water_not_on_the_first_pop() {
        let (tx, rx) = channel::<u32>(4);
        for i in 0..4 {
            tx.send(i).unwrap();
        }
        let rx = Rc::new(rx);

        let consumer = Rc::clone(&rx);
        CHECKPOINT_HOOK.with(|slot| {
            let mut popped = false;
            *slot.borrow_mut() = Some(Box::new(move |point| match point {
                Checkpoint::Rechecked if !popped => {
                    popped = true;
                    assert_eq!(consumer.try_recv(), Ok(0));
                }
                Checkpoint::Blocking => panic!("asleep above low water"),
                _ => {}
            }));
        });
        let asleep = catch_unwind(AssertUnwindSafe(|| tx.send(4)))
            .expect_err("one pop above low water must not wake the producer");
        assert_eq!(
            asleep.downcast_ref::<&str>(),
            Some(&"asleep above low water")
        );
        clear_interleave();

        let (tx, rx) = channel::<u32>(4);
        for i in 0..4 {
            tx.send(i).unwrap();
        }
        let rx = Rc::new(rx);
        let consumer = Rc::clone(&rx);
        interleave(Checkpoint::Rechecked, move || {
            assert_eq!(consumer.try_recv(), Ok(0));
            assert_eq!(consumer.try_recv(), Ok(1));
        });
        tx.send(4).unwrap();
        clear_interleave();
        let rest: Vec<u32> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(rest, [2, 3, 4]);
    }

    /// The producer pushes after the consumer published its intent to park
    /// but before its re-check: the consumer must take the item, not park.
    #[test]
    fn a_push_before_the_consumer_recheck_is_seen_by_the_recheck() {
        let (tx, rx) = channel::<u32>(2);
        interleave(Checkpoint::Published, move || tx.send(7).unwrap());
        assert_eq!(rx.recv(), Ok(7));
        clear_interleave();
        assert_eq!(rx.producer_parks(), 0);
    }

    /// The producer pushes between the consumer's re-check and its sleep:
    /// the push's wake must leave a token, so the consumer takes the item.
    #[test]
    fn a_push_between_the_consumer_recheck_and_its_sleep_leaves_a_token() {
        let (tx, rx) = channel::<u32>(2);
        let producer = Rc::new(tx);
        let pushes = Rc::clone(&producer);
        interleave(Checkpoint::Rechecked, move || pushes.send(7).unwrap());
        assert_eq!(rx.recv(), Ok(7));
        clear_interleave();
        assert_eq!(producer.consumer_parks(), 1);
    }

    /// A disconnect that lands between a waiter's re-check and its sleep
    /// must wake it: the consumer reports the closed channel and the producer
    /// gets its item back, neither sleeps.
    #[test]
    fn a_disconnect_between_the_recheck_and_the_sleep_wakes_the_waiter() {
        let (tx, rx) = channel::<u32>(2);
        interleave(Checkpoint::Rechecked, move || drop(tx));
        assert_eq!(rx.recv(), Err(RecvError));
        clear_interleave();

        let (tx, rx) = channel::<u32>(1);
        tx.send(1).unwrap();
        interleave(Checkpoint::Rechecked, move || drop(rx));
        let SendError(item) = tx.send(2).expect_err("a dropped receiver must reject");
        clear_interleave();
        assert_eq!(item, 2);
    }
}
