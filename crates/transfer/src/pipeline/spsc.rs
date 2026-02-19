//! Lock-free SPSC (single-producer, single-consumer) channel.
//!
//! Built on [`crossbeam_queue::ArrayQueue`] with [`AtomicBool`] disconnection
//! flags and [`std::hint::spin_loop`] for waiting.  Zero syscalls — pure
//! userspace synchronization with no futex, no `thread::park`, no condvar.
//!
//! Designed for the network → disk thread pipeline where exactly one producer
//! (network ingest) and one consumer (disk commit) exchange [`FileMessage`]
//! items at high throughput.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossbeam_queue::ArrayQueue;

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
        loop {
            if !self.0.consumer_alive.load(Ordering::Relaxed) {
                return Err(SendError(item));
            }
            match self.0.queue.push(item) {
                Ok(()) => return Ok(()),
                Err(returned) => {
                    item = returned;
                    std::hint::spin_loop();
                }
            }
        }
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
        loop {
            if let Some(item) = self.0.queue.pop() {
                return Ok(item);
            }
            if !self.0.producer_alive.load(Ordering::Acquire) {
                // Producer is gone — drain one last time.
                return self.0.queue.pop().ok_or(RecvError);
            }
            std::hint::spin_loop();
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
        // Queue of capacity 2 — sender must spin-wait when full.
        let (tx, rx) = channel::<i32>(2);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        // Queue is full.  Spawn a thread to drain all items, keeping rx
        // alive until after the sender unblocks (avoids race where rx
        // drops before the spin-waiting send completes).
        let drain = thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(10));
            let mut received = Vec::new();
            // Drain all items: the 2 already queued + the 1 the sender
            // will push once a slot opens.
            for _ in 0..3 {
                received.push(rx.recv().unwrap());
            }
            received
        });
        // This send should spin-wait until the drain thread pops.
        tx.send(3).unwrap();
        let received = drain.join().unwrap();
        assert_eq!(received, vec![1, 2, 3]);
    }

    #[test]
    fn sender_is_disconnected() {
        let (tx, rx) = channel::<i32>(4);
        assert!(!tx.is_disconnected());
        drop(rx);
        assert!(tx.is_disconnected());
    }
}
