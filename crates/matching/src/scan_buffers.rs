//! Scratch storage a delta scan can reuse across files.

use crate::ring_buffer::RingBuffer;

/// Read buffer and rolling window for [`crate::DeltaGenerator`] scans.
///
/// A sender scans one file after another. Handing the same `ScanBuffers` to
/// every scan lets each one after the first run without allocating these
/// buffers or zero-filling the read buffer. Every scan resets both before its
/// first byte, so nothing from an earlier file reaches a later scan.
///
/// upstream: match.c:78-96 `build_hash_table()` keeps its table in a `static`
/// across files and reallocates it only when the size changes; this applies
/// the same reuse to the scan's own buffers.
#[derive(Debug, Default)]
pub struct ScanBuffers {
    read: Vec<u8>,
    window: Option<RingBuffer>,
}

impl ScanBuffers {
    /// Returns a `read_len`-byte read buffer and an empty `block_len` window.
    ///
    /// The read buffer only ever grows. Its bytes are stale, which is safe
    /// because a scan reads only the prefix its source just filled.
    pub(crate) fn prepare(
        &mut self,
        read_len: usize,
        block_len: usize,
    ) -> (&mut [u8], &mut RingBuffer) {
        if self.read.len() < read_len {
            self.read.resize(read_len, 0);
        }
        let window = self
            .window
            .get_or_insert_with(|| RingBuffer::with_capacity(block_len));
        window.reset(block_len);
        (&mut self.read[..read_len], window)
    }
}

#[cfg(test)]
mod tests {
    use super::ScanBuffers;

    // WHY: a reused window that kept the previous file's bytes would leak
    // them into the next file's first rolling checksum and token stream.
    #[test]
    fn prepare_returns_an_empty_window_of_the_new_block_length() {
        let mut buffers = ScanBuffers::default();
        let (_, window) = buffers.prepare(16, 8);
        for byte in 0..8 {
            window.push_back(byte);
        }
        assert!(window.is_full());

        let (read, window) = buffers.prepare(16, 4);
        assert_eq!(read.len(), 16);
        assert!(window.is_empty(), "a reused window must start empty");
        assert_eq!(
            window.capacity(),
            4,
            "the window must take the new block length"
        );
    }

    // WHY: the read buffer is the per-file allocation being removed, so a
    // smaller follow-up request must reuse it rather than shrink-and-regrow.
    #[test]
    fn prepare_keeps_the_read_allocation_across_smaller_requests() {
        let mut buffers = ScanBuffers::default();
        let first = buffers.prepare(64, 8).0.as_ptr();
        let (read, _) = buffers.prepare(32, 8);
        assert_eq!(read.len(), 32);
        assert_eq!(
            read.as_ptr(),
            first,
            "a smaller scan must reuse the allocation"
        );
    }
}
