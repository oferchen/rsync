//! Fixed-capacity ring buffer optimized for rsync's sliding window.
//!
//! This implementation is specifically designed for delta generation where:
//! - The buffer is always filled to capacity during steady-state operation
//! - A contiguous slice view is needed frequently for checksum computation
//! - Push/pop operations happen once per byte processed
//!
//! Unlike [`std::collections::VecDeque`], this ring buffer:
//! - Uses a single pre-allocated contiguous buffer
//! - Provides O(1) contiguous slice access when full (no copying)
//! - Has lower memory overhead (no capacity/head/tail tracking beyond what's needed)

/// A fixed-capacity ring buffer optimized for sliding window operations.
///
/// The buffer maintains a contiguous view when full, eliminating the need for
/// scratch buffer copies during strong checksum computation.
///
/// # Example
///
/// ```ignore
/// let mut buf = RingBuffer::with_capacity(3);
///
/// // Fill the buffer
/// assert_eq!(buf.push_back(1), None);  // No overflow
/// assert_eq!(buf.push_back(2), None);
/// assert_eq!(buf.push_back(3), None);
/// assert!(buf.is_full());
///
/// // Sliding window: new bytes push out old ones
/// assert_eq!(buf.push_back(4), Some(1));  // 1 is pushed out
/// assert_eq!(buf.push_back(5), Some(2));  // 2 is pushed out
/// ```
#[derive(Clone, Debug)]
pub struct RingBuffer {
    /// The backing storage, always exactly `capacity` bytes.
    buffer: Vec<u8>,
    /// Write position (next byte will be written here when full).
    head: usize,
    /// Number of bytes currently in the buffer.
    len: usize,
}

impl RingBuffer {
    /// Creates a new ring buffer with the specified capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "ring buffer capacity must be non-zero");
        Self {
            buffer: vec![0u8; capacity],
            head: 0,
            len: 0,
        }
    }

    /// Returns the maximum capacity of the buffer.
    #[inline]
    #[must_use]
    #[allow(dead_code)]
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Returns the number of bytes currently in the buffer.
    #[inline]
    #[must_use]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if the buffer contains no bytes.
    #[inline]
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns `true` if the buffer is at capacity.
    #[inline]
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.len == self.buffer.len()
    }

    /// Adds a byte to the back of the buffer.
    ///
    /// If the buffer is full, the oldest byte is overwritten and returned.
    /// Returns `None` if the buffer was not full.
    #[inline]
    pub fn push_back(&mut self, byte: u8) -> Option<u8> {
        if self.len < self.buffer.len() {
            // Buffer not yet full: append at len position
            let pos = (self.head + self.len) % self.buffer.len();
            self.buffer[pos] = byte;
            self.len += 1;
            None
        } else {
            // Buffer full: overwrite oldest byte at head
            let outgoing = self.buffer[self.head];
            self.buffer[self.head] = byte;
            self.head = (self.head + 1) % self.buffer.len();
            Some(outgoing)
        }
    }

    /// Removes and returns the oldest byte from the buffer.
    ///
    /// Returns `None` if the buffer is empty.
    #[inline]
    pub fn pop_front(&mut self) -> Option<u8> {
        if self.len == 0 {
            None
        } else {
            let byte = self.buffer[self.head];
            self.head = (self.head + 1) % self.buffer.len();
            self.len -= 1;
            Some(byte)
        }
    }

    /// Clears the buffer, removing all bytes.
    #[inline]
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    /// Returns a contiguous slice view of the buffer contents.
    ///
    /// When the buffer hasn't wrapped (head == 0), this returns a direct slice
    /// with no copying (O(1)). Otherwise, it rotates the internal buffer to make
    /// the contents contiguous (O(n)).
    ///
    /// # Performance Note
    ///
    /// For hot paths where rotation overhead matters, use [`Self::as_slices`]
    /// or [`Self::copy_to_slice`] to avoid mutation.
    #[must_use]
    pub fn as_slice(&mut self) -> &[u8] {
        if self.len == 0 {
            return &[];
        }

        // Fast path: already contiguous
        if self.head == 0 {
            return &self.buffer[..self.len];
        }

        // Check if data is contiguous even with non-zero head
        let end = self.head + self.len;
        if end <= self.buffer.len() {
            // Data is contiguous in the middle of the buffer
            return &self.buffer[self.head..end];
        }

        // Slow path: need to rotate to make contiguous
        self.buffer.rotate_left(self.head);
        self.head = 0;
        &self.buffer[..self.len]
    }

    /// Returns a contiguous slice if available, without mutation.
    ///
    /// Returns `Some(slice)` if the buffer contents are already contiguous,
    /// or `None` if the buffer has wrapped and rotation would be needed.
    ///
    /// This is useful in hot paths where avoiding mutation is critical.
    #[inline]
    #[must_use]
    #[allow(dead_code)] // API method for callers needing zero-copy access
    pub fn try_as_slice(&self) -> Option<&[u8]> {
        if self.len == 0 {
            return Some(&[]);
        }

        let end = self.head + self.len;
        if end <= self.buffer.len() {
            // Data is contiguous
            Some(&self.buffer[self.head..end])
        } else {
            // Wrapped, would need rotation
            None
        }
    }

    /// Returns a contiguous slice if possible without rotation, or two slices if wrapped.
    ///
    /// This is useful when the caller can handle non-contiguous data.
    #[must_use]
    #[allow(dead_code)]
    pub fn as_slices(&self) -> (&[u8], &[u8]) {
        if self.len == 0 {
            return (&[], &[]);
        }

        let end = self.head + self.len;
        if end <= self.buffer.len() {
            // No wrap-around: single contiguous slice
            (&self.buffer[self.head..end], &[])
        } else {
            // Wrapped: two slices
            let first_len = self.buffer.len() - self.head;
            let second_len = self.len - first_len;
            (&self.buffer[self.head..], &self.buffer[..second_len])
        }
    }

    /// Copies the buffer contents into a contiguous destination slice.
    ///
    /// # Panics
    ///
    /// Panics if `dest.len() < self.len()`.
    #[allow(dead_code)]
    pub fn copy_to_slice(&self, dest: &mut [u8]) {
        assert!(dest.len() >= self.len, "destination too small");
        let (first, second) = self.as_slices();
        dest[..first.len()].copy_from_slice(first);
        if !second.is_empty() {
            dest[first.len()..first.len() + second.len()].copy_from_slice(second);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_is_empty() {
        let buf = RingBuffer::with_capacity(10);
        assert!(buf.is_empty());
        assert!(!buf.is_full());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.capacity(), 10);
    }

    #[test]
    fn push_increases_len() {
        let mut buf = RingBuffer::with_capacity(5);
        assert_eq!(buf.push_back(1), None);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.push_back(2), None);
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn push_when_full_returns_outgoing() {
        let mut buf = RingBuffer::with_capacity(3);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);
        assert!(buf.is_full());

        assert_eq!(buf.push_back(4), Some(1));
        assert_eq!(buf.push_back(5), Some(2));
    }

    #[test]
    fn pop_decreases_len() {
        let mut buf = RingBuffer::with_capacity(5);
        buf.push_back(1);
        buf.push_back(2);
        assert_eq!(buf.pop_front(), Some(1));
        assert_eq!(buf.len(), 1);
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut buf = RingBuffer::with_capacity(5);
        assert_eq!(buf.pop_front(), None);
    }

    #[test]
    fn clear_resets_buffer() {
        let mut buf = RingBuffer::with_capacity(5);
        buf.push_back(1);
        buf.push_back(2);
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn as_slice_returns_correct_content() {
        let mut buf = RingBuffer::with_capacity(5);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);
        assert_eq!(buf.as_slice(), &[1, 2, 3]);
    }

    #[test]
    fn as_slice_after_wrap() {
        let mut buf = RingBuffer::with_capacity(3);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);
        buf.push_back(4); // overwrites 1, head moves
        buf.push_back(5); // overwrites 2, head moves

        // Buffer now contains [5, 3, 4] with head at position 2
        // Logical order is [3, 4, 5]
        assert_eq!(buf.as_slice(), &[3, 4, 5]);
    }

    #[test]
    fn as_slices_no_wrap() {
        let buf = {
            let mut b = RingBuffer::with_capacity(5);
            b.push_back(1);
            b.push_back(2);
            b.push_back(3);
            b
        };
        let (first, second) = buf.as_slices();
        assert_eq!(first, &[1, 2, 3]);
        assert!(second.is_empty());
    }

    #[test]
    fn as_slices_with_wrap() {
        let mut buf = RingBuffer::with_capacity(3);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);
        buf.push_back(4); // [4, 2, 3] head=1

        let (first, second) = buf.as_slices();
        assert_eq!(first, &[2, 3]);
        assert_eq!(second, &[4]);
    }

    #[test]
    fn copy_to_slice_works() {
        let mut buf = RingBuffer::with_capacity(3);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);
        buf.push_back(4);

        let mut dest = [0u8; 3];
        buf.copy_to_slice(&mut dest);
        assert_eq!(dest, [2, 3, 4]);
    }

    #[test]
    fn fifo_order_preserved() {
        let mut buf = RingBuffer::with_capacity(3);
        for i in 0..10u8 {
            buf.push_back(i);
        }
        // After 10 pushes into capacity-3 buffer, contains [7, 8, 9]
        assert_eq!(buf.pop_front(), Some(7));
        assert_eq!(buf.pop_front(), Some(8));
        assert_eq!(buf.pop_front(), Some(9));
        assert_eq!(buf.pop_front(), None);
    }

    #[test]
    #[should_panic(expected = "capacity must be non-zero")]
    fn zero_capacity_panics() {
        let _ = RingBuffer::with_capacity(0);
    }

    #[test]
    fn sliding_window_simulation() {
        // Simulate the delta generator's sliding window behavior
        let mut buf = RingBuffer::with_capacity(4);
        let data = b"hello world";

        let mut outgoing_bytes = Vec::new();
        for &byte in data {
            if let Some(out) = buf.push_back(byte) {
                outgoing_bytes.push(out);
            }
        }

        // After processing "hello world" with window size 4:
        // Window contains "orld" (last 4 bytes)
        assert_eq!(buf.as_slice(), b"orld");
        // Outgoing bytes are "hello w" (first 7 bytes)
        assert_eq!(outgoing_bytes, b"hello w");
    }

    #[test]
    fn try_as_slice_no_wrap() {
        let mut buf = RingBuffer::with_capacity(5);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);

        // Not wrapped, should return slice
        assert_eq!(buf.try_as_slice(), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn try_as_slice_wrapped() {
        let mut buf = RingBuffer::with_capacity(3);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);
        buf.push_back(4); // Overwrites 1, buffer wraps

        // Wrapped, should return None
        assert_eq!(buf.try_as_slice(), None);
    }

    #[test]
    fn try_as_slice_empty() {
        let buf = RingBuffer::with_capacity(5);
        assert_eq!(buf.try_as_slice(), Some(&[][..]));
    }

    #[test]
    fn as_slice_contiguous_middle() {
        // Test the new optimization: contiguous data in the middle
        let mut buf = RingBuffer::with_capacity(5);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);
        buf.pop_front(); // head moves to 1
        buf.pop_front(); // head moves to 2

        // Now only [3] at position 2, head=2, len=1
        // This is contiguous in the middle
        assert_eq!(buf.as_slice(), &[3]);
    }

    // ==== Capacity Edge Case Tests ====

    #[test]
    fn capacity_one_buffer() {
        // Edge case: buffer with capacity 1
        let mut buf = RingBuffer::with_capacity(1);
        assert!(buf.is_empty());
        assert_eq!(buf.capacity(), 1);

        // Push first byte
        assert_eq!(buf.push_back(42), None);
        assert!(buf.is_full());
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.as_slice(), &[42]);

        // Push second byte - should evict first
        assert_eq!(buf.push_back(99), Some(42));
        assert!(buf.is_full());
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.as_slice(), &[99]);

        // Pop should return the byte
        assert_eq!(buf.pop_front(), Some(99));
        assert!(buf.is_empty());
    }

    #[test]
    #[should_panic(expected = "ring buffer capacity must be non-zero")]
    fn capacity_zero_panics() {
        // Edge case: capacity 0 should panic
        let _ = RingBuffer::with_capacity(0);
    }

    #[test]
    #[should_panic(expected = "destination too small")]
    fn copy_to_slice_insufficient_capacity_panics() {
        let mut buf = RingBuffer::with_capacity(5);
        buf.push_back(1);
        buf.push_back(2);
        buf.push_back(3);

        // Destination is too small (2 bytes for 3 bytes of data)
        let mut dest = [0u8; 2];
        buf.copy_to_slice(&mut dest);
    }
}
