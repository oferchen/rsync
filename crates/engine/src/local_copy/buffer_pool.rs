//! Thread-safe buffer pool for reusing I/O buffers.
//!
//! This module provides a [`BufferPool`] that reduces allocation overhead during
//! file copy operations by reusing fixed-size buffers. Buffers are automatically
//! returned to the pool when the [`BufferGuard`] is dropped.
//!
//! # Design
//!
//! The pool uses a simple stack-based approach: buffers are pushed when released
//! and popped when acquired. This provides good cache locality as recently-used
//! buffers are reused first.
//!
//! # Thread Safety
//!
//! The pool uses [`std::sync::Mutex`] for thread-safe access. The lock is held
//! only briefly during acquire/release operations, minimizing contention.
//!
//! # Ownership Model
//!
//! The pool is wrapped in [`Arc`](std::sync::Arc) to allow [`BufferGuard`] to hold an owned
//! reference, avoiding borrow checker issues when the pool is part of a larger
//! context struct.
//!
//! # Example
//!
//! ```ignore
//! use engine::local_copy::buffer_pool::BufferPool;
//! use std::sync::Arc;
//!
//! let pool = Arc::new(BufferPool::new(4));
//! let buffer = BufferPool::acquire_from(Arc::clone(&pool));
//! // Use buffer for I/O...
//! // Buffer automatically returned to pool on drop
//! ```

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use super::COPY_BUFFER_SIZE;

/// A thread-safe pool of reusable I/O buffers.
///
/// Reduces allocation overhead during file copy operations by maintaining
/// a pool of fixed-size buffers that can be reused across operations.
///
/// # Capacity
///
/// The pool has a maximum capacity to prevent unbounded memory growth.
/// When the pool is full, returning a buffer simply drops it.
/// When the pool is empty, acquiring creates a new buffer.
#[derive(Debug)]
pub struct BufferPool {
    /// Stack of available buffers, protected by mutex.
    buffers: Mutex<Vec<Vec<u8>>>,
    /// Maximum number of buffers to retain in the pool.
    max_buffers: usize,
    /// Size of each buffer in bytes.
    buffer_size: usize,
}

impl BufferPool {
    /// Creates a new buffer pool with the specified maximum capacity.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain. Excess buffers
    ///   are dropped when returned to the pool.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let pool = BufferPool::new(num_cpus::get());
    /// ```
    #[must_use]
    pub fn new(max_buffers: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            max_buffers,
            buffer_size: COPY_BUFFER_SIZE,
        }
    }

    /// Creates a new buffer pool with custom buffer size.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain.
    /// * `buffer_size` - Size of each buffer in bytes.
    #[must_use]
    pub fn with_buffer_size(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            max_buffers,
            buffer_size,
        }
    }

    /// Acquires a buffer from the pool using an Arc reference.
    ///
    /// This is the preferred method when the pool is part of a larger struct
    /// that needs to be mutably borrowed while the buffer is in use.
    ///
    /// Returns a pooled buffer if available, otherwise allocates a new one.
    /// The returned [`BufferGuard`] automatically returns the buffer to the
    /// pool when dropped.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn acquire_from(pool: Arc<Self>) -> BufferGuard {
        let buffer = {
            let mut buffers = pool.buffers.lock().expect("buffer pool mutex poisoned");
            buffers.pop()
        };

        let buffer = buffer.unwrap_or_else(|| vec![0u8; pool.buffer_size]);

        BufferGuard {
            buffer: Some(buffer),
            pool,
        }
    }

    /// Acquires a buffer from the pool (borrows self).
    ///
    /// **Note:** This method returns a guard with a lifetime tied to `self`.
    /// Use [`acquire_from`](Self::acquire_from) when the pool is part of a
    /// larger context that needs to be mutably borrowed.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn acquire(&self) -> BorrowedBufferGuard<'_> {
        let buffer = {
            let mut pool = self.buffers.lock().expect("buffer pool mutex poisoned");
            pool.pop()
        };

        let buffer = buffer.unwrap_or_else(|| vec![0u8; self.buffer_size]);

        BorrowedBufferGuard {
            buffer: Some(buffer),
            pool: self,
        }
    }

    /// Returns a buffer to the pool.
    ///
    /// If the pool is at capacity, the buffer is dropped instead.
    fn return_buffer(&self, mut buffer: Vec<u8>) {
        // Clear sensitive data and reset length
        buffer.clear();
        buffer.resize(self.buffer_size, 0);

        let mut pool = self.buffers.lock().expect("buffer pool mutex poisoned");
        if pool.len() < self.max_buffers {
            pool.push(buffer);
        }
        // Otherwise, buffer is dropped
    }

    /// Returns the number of buffers currently in the pool.
    ///
    /// This is primarily useful for testing and monitoring.
    #[must_use]
    pub fn available(&self) -> usize {
        self.buffers
            .lock()
            .expect("buffer pool mutex poisoned")
            .len()
    }

    /// Returns the maximum number of buffers the pool will retain.
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        self.max_buffers
    }

    /// Returns the size of each buffer in bytes.
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }
}

impl Default for BufferPool {
    /// Creates a buffer pool with capacity based on available parallelism.
    fn default() -> Self {
        let max_buffers = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);
        Self::new(max_buffers)
    }
}

/// RAII guard that returns a buffer to the pool on drop (owned version).
///
/// This guard holds an [`Arc`] to the pool, allowing it to be used when
/// the pool is part of a larger context that needs to be mutably borrowed.
///
/// Provides transparent access to the underlying buffer via [`Deref`] and
/// [`DerefMut`], allowing it to be used wherever `&[u8]` or `&mut [u8]`
/// is expected.
#[derive(Debug)]
pub struct BufferGuard {
    /// The buffer, wrapped in Option for take-on-drop pattern.
    buffer: Option<Vec<u8>>,
    /// Arc reference to the pool for returning the buffer.
    pool: Arc<BufferPool>,
}

impl Deref for BufferGuard {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.buffer.as_ref().expect("buffer already taken")
    }
}

impl DerefMut for BufferGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}

impl BufferGuard {
    /// Returns the length of the buffer in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map(Vec::len).unwrap_or(0)
    }

    /// Returns true if the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the buffer as a mutable slice.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

/// RAII guard that returns a buffer to the pool on drop (borrowed version).
///
/// This guard borrows the pool, suitable for simple use cases where the pool
/// lifetime is clear.
#[derive(Debug)]
pub struct BorrowedBufferGuard<'a> {
    /// The buffer, wrapped in Option for take-on-drop pattern.
    buffer: Option<Vec<u8>>,
    /// Reference to the pool for returning the buffer.
    pool: &'a BufferPool,
}

impl Deref for BorrowedBufferGuard<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.buffer.as_ref().expect("buffer already taken")
    }
}

impl DerefMut for BorrowedBufferGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

impl Drop for BorrowedBufferGuard<'_> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}

impl BorrowedBufferGuard<'_> {
    /// Returns the length of the buffer in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map(Vec::len).unwrap_or(0)
    }

    /// Returns true if the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the buffer as a mutable slice.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_acquire_returns_buffer() {
        let pool = BufferPool::new(4);
        let buffer = pool.acquire();
        assert_eq!(buffer.len(), COPY_BUFFER_SIZE);
    }

    #[test]
    fn test_buffer_reuse() {
        let pool = BufferPool::new(4);

        // Acquire and release a buffer
        {
            let mut buffer = pool.acquire();
            // Write a marker to identify this buffer
            buffer[0] = 42;
        }

        // Pool should have one buffer
        assert_eq!(pool.available(), 1);

        // Acquire again - should get the same buffer
        let buffer = pool.acquire();
        assert_eq!(pool.available(), 0);
        // Buffer is zeroed on return
        assert_eq!(buffer[0], 0);
    }

    #[test]
    fn test_pool_capacity_limit() {
        let pool = BufferPool::new(2);

        // Acquire 3 buffers
        let b1 = pool.acquire();
        let b2 = pool.acquire();
        let b3 = pool.acquire();

        // Release all
        drop(b1);
        drop(b2);
        drop(b3);

        // Only 2 should be retained
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn test_concurrent_access() {
        let pool = Arc::new(BufferPool::new(8));
        let mut handles = vec![];

        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let mut buffer = pool.acquire();
                    buffer[0] = 1;
                    // Buffer returned on drop
                }
            }));
        }

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // Pool should have at most max_buffers
        assert!(pool.available() <= 8);
    }

    #[test]
    fn test_buffer_guard_deref() {
        let pool = BufferPool::new(4);
        let mut buffer = pool.acquire();

        // Write through DerefMut
        buffer[0] = 100;
        buffer[1] = 200;

        // Read through Deref
        assert_eq!(buffer[0], 100);
        assert_eq!(buffer[1], 200);

        // Use as slice
        let slice: &[u8] = &buffer;
        assert_eq!(slice[0], 100);
    }

    #[test]
    fn test_buffer_guard_as_mut_slice() {
        let pool = BufferPool::new(4);
        let mut buffer = pool.acquire();

        let slice = buffer.as_mut_slice();
        slice[0] = 42;

        assert_eq!(buffer[0], 42);
    }

    #[test]
    fn test_custom_buffer_size() {
        let pool = BufferPool::with_buffer_size(4, 1024);
        let buffer = pool.acquire();
        assert_eq!(buffer.len(), 1024);
        assert_eq!(pool.buffer_size(), 1024);
    }

    #[test]
    fn test_default_pool() {
        let pool = BufferPool::default();
        assert!(pool.max_buffers() > 0);
        assert_eq!(pool.buffer_size(), COPY_BUFFER_SIZE);
    }

    #[test]
    fn test_buffer_zeroed_on_return() {
        let pool = BufferPool::new(4);

        {
            let mut buffer = pool.acquire();
            // Fill with non-zero data
            for byte in buffer.iter_mut() {
                *byte = 0xFF;
            }
        }

        // Acquire again
        let buffer = pool.acquire();
        // Should be zeroed
        assert!(buffer.iter().all(|&b| b == 0));
    }
}
