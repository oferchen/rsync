//! RAII buffer guards for the [`BufferPool`](super::BufferPool).
//!
//! This module provides two guard types that automatically return buffers
//! to the pool when dropped:
//!
//! - [`BufferGuard`] - holds an [`Arc`] to the pool (owned version).
//! - [`BorrowedBufferGuard`] - borrows the pool (lifetime-bound version).

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use super::BufferPool;

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
/// This guard borrows the pool, suitable for simple use cases where the pool
/// lifetime is clear.
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
