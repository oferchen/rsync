//! RAII buffer guards for the [`BufferPool`](super::BufferPool).
//!
//! This module provides two guard types that automatically return buffers
//! to the pool when dropped. Both use an internal `Option<Vec<u8>>` with
//! take-on-drop semantics - the `Drop` impl calls `Option::take` to move
//! the buffer out, then passes it to `BufferPool::return_buffer`. This
//! guarantees the buffer is returned exactly once, even during panic unwind.
//!
//! - [`BufferGuard`] - holds an [`Arc`] to the pool, decoupling the guard
//!   lifetime from the pool borrow. Preferred when the pool is part of a
//!   larger struct that may be mutably borrowed while buffers are checked out.
//! - [`BorrowedBufferGuard`] - borrows the pool by reference, tying the
//!   guard lifetime to the pool. Lighter weight when the borrow checker
//!   permits it (single-thread or scoped usage).

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use super::BufferPool;

/// RAII guard that returns a buffer to the pool on drop (owned version).
///
/// Holds an [`Arc<BufferPool>`] so the guard's lifetime is independent of
/// any borrow on the pool. This is the preferred variant for concurrent
/// use with rayon, where buffers are acquired in worker threads and may
/// outlive the scope that created the pool reference.
///
/// Provides transparent access to the underlying `Vec<u8>` via [`Deref`]
/// and [`DerefMut`] to `[u8]`, so the guard can be passed to any API
/// expecting `&[u8]` or `&mut [u8]`.
///
/// On drop, the buffer is passed to [`BufferPool::return_buffer`](super::BufferPool),
/// which restores its length to the pool default and pushes it back onto
/// the lock-free queue. If the pool is at capacity, the buffer is dropped.
#[derive(Debug)]
pub struct BufferGuard {
    /// The buffer, wrapped in Option for take-on-drop pattern.
    pub(super) buffer: Option<Vec<u8>>,
    /// Arc reference to the pool for returning the buffer.
    pub(super) pool: Arc<BufferPool>,
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
/// Borrows the pool by reference, so the guard's lifetime is tied to the
/// pool via `'a`. This avoids the `Arc` overhead and is suitable for
/// single-thread or scoped usage where the pool outlives all guards.
///
/// Behaves identically to [`BufferGuard`] in all other respects: derefs
/// to `[u8]`, and the `Drop` impl returns the buffer to the pool.
#[derive(Debug)]
pub struct BorrowedBufferGuard<'a> {
    /// The buffer, wrapped in Option for take-on-drop pattern.
    pub(super) buffer: Option<Vec<u8>>,
    /// Reference to the pool for returning the buffer.
    pub(super) pool: &'a BufferPool,
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
