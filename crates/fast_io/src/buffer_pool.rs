//! Generic thread-safe buffer pool with RAII guards.
//!
//! This is a generalized version of the buffer pool pattern, supporting
//! any buffer type and custom initialization.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

/// Default buffer size (128 KB).
pub const DEFAULT_BUFFER_SIZE: usize = 128 * 1024;

/// A thread-safe pool of reusable buffers.
///
/// Reduces allocation overhead by maintaining a pool of buffers that can be
/// reused across operations. Uses a stack-based approach for good cache locality.
///
/// # Type Parameters
///
/// * `T` - The buffer type (usually `Vec<u8>`)
///
/// # Example
///
/// ```
/// use fast_io::BufferPool;
/// use std::sync::Arc;
///
/// let pool = Arc::new(BufferPool::new(4, 1024));
/// let buffer = BufferPool::acquire(Arc::clone(&pool));
/// // Use buffer...
/// // Automatically returned on drop
/// ```
#[derive(Debug)]
pub struct BufferPool<T = Vec<u8>> {
    buffers: Mutex<Vec<T>>,
    max_buffers: usize,
    buffer_size: usize,
    initializer: fn(usize) -> T,
    resetter: fn(&mut T, usize),
}

impl BufferPool<Vec<u8>> {
    /// Creates a new buffer pool for byte vectors.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain
    /// * `buffer_size` - Size of each buffer in bytes
    #[must_use]
    pub fn new(max_buffers: usize, buffer_size: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            max_buffers,
            buffer_size,
            initializer: |size| vec![0u8; size],
            resetter: |buf, size| {
                buf.clear();
                buf.resize(size, 0);
            },
        }
    }

    /// Creates a pool with default settings based on available parallelism.
    #[must_use]
    pub fn default_sized() -> Self {
        let max_buffers = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4);
        Self::new(max_buffers, DEFAULT_BUFFER_SIZE)
    }
}

impl<T> BufferPool<T> {
    /// Creates a custom buffer pool with user-defined initialization.
    ///
    /// # Arguments
    ///
    /// * `max_buffers` - Maximum number of buffers to retain
    /// * `buffer_size` - Logical size passed to initializer
    /// * `initializer` - Function to create new buffers
    /// * `resetter` - Function to reset buffers when returned to pool
    #[must_use]
    pub fn with_custom(
        max_buffers: usize,
        buffer_size: usize,
        initializer: fn(usize) -> T,
        resetter: fn(&mut T, usize),
    ) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            max_buffers,
            buffer_size,
            initializer,
            resetter,
        }
    }

    /// Acquires a buffer from the pool.
    ///
    /// Returns a pooled buffer if available, otherwise creates a new one.
    /// The returned guard automatically returns the buffer on drop.
    #[must_use]
    pub fn acquire(pool: Arc<Self>) -> BufferGuard<T> {
        let buffer = {
            let mut buffers = pool.buffers.lock().expect("buffer pool mutex poisoned");
            buffers.pop()
        };

        let buffer = buffer.unwrap_or_else(|| (pool.initializer)(pool.buffer_size));

        BufferGuard {
            buffer: Some(buffer),
            pool,
        }
    }

    /// Returns a buffer to the pool.
    fn return_buffer(&self, mut buffer: T) {
        (self.resetter)(&mut buffer, self.buffer_size);

        let mut buffers = self.buffers.lock().expect("buffer pool mutex poisoned");
        if buffers.len() < self.max_buffers {
            buffers.push(buffer);
        }
    }

    /// Returns the number of buffers currently available in the pool.
    #[must_use]
    pub fn available(&self) -> usize {
        self.buffers.lock().expect("mutex poisoned").len()
    }

    /// Returns the maximum number of buffers the pool will retain.
    #[must_use]
    pub fn max_buffers(&self) -> usize {
        self.max_buffers
    }

    /// Returns the configured buffer size.
    #[must_use]
    pub fn buffer_size(&self) -> usize {
        self.buffer_size
    }
}

/// RAII guard that returns a buffer to the pool on drop.
#[derive(Debug)]
pub struct BufferGuard<T> {
    buffer: Option<T>,
    pool: Arc<BufferPool<T>>,
}

impl<T> Deref for BufferGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.buffer.as_ref().expect("buffer already taken")
    }
}

impl<T> DerefMut for BufferGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.buffer.as_mut().expect("buffer already taken")
    }
}

impl<T> Drop for BufferGuard<T> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            self.pool.return_buffer(buffer);
        }
    }
}

impl<T> BufferGuard<T> {
    /// Takes ownership of the buffer, removing it from the pool.
    ///
    /// After calling this, the guard will not return the buffer to the pool.
    #[must_use]
    pub fn take(mut self) -> T {
        self.buffer.take().expect("buffer already taken")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn acquire_returns_buffer() {
        let pool = Arc::new(BufferPool::new(4, 1024));
        let buffer = BufferPool::acquire(Arc::clone(&pool));
        assert_eq!(buffer.len(), 1024);
    }

    #[test]
    fn buffer_reuse() {
        let pool = Arc::new(BufferPool::new(4, 1024));

        {
            let mut buffer = BufferPool::acquire(Arc::clone(&pool));
            buffer[0] = 42;
        }

        assert_eq!(pool.available(), 1);

        let buffer = BufferPool::acquire(Arc::clone(&pool));
        assert_eq!(pool.available(), 0);
        // Buffer is zeroed on return
        assert_eq!(buffer[0], 0);
    }

    #[test]
    fn pool_capacity_limit() {
        let pool = Arc::new(BufferPool::new(2, 1024));

        let b1 = BufferPool::acquire(Arc::clone(&pool));
        let b2 = BufferPool::acquire(Arc::clone(&pool));
        let b3 = BufferPool::acquire(Arc::clone(&pool));

        drop(b1);
        drop(b2);
        drop(b3);

        // Only 2 should be retained
        assert_eq!(pool.available(), 2);
    }

    #[test]
    fn concurrent_access() {
        let pool = Arc::new(BufferPool::new(8, 1024));
        let mut handles = vec![];

        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let mut buffer = BufferPool::acquire(Arc::clone(&pool));
                    buffer[0] = 1;
                }
            }));
        }

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        assert!(pool.available() <= 8);
    }

    #[test]
    fn take_removes_from_pool() {
        let pool = Arc::new(BufferPool::new(4, 1024));

        let buffer = BufferPool::acquire(Arc::clone(&pool));
        let owned = buffer.take();

        assert_eq!(owned.len(), 1024);
        assert_eq!(pool.available(), 0); // Not returned
    }
}
