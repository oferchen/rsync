//! Safe wrapper around OVERLAPPED structures for async I/O.
//!
//! Each overlapped operation requires a stable OVERLAPPED pointer that remains
//! valid until the operation completes. This module provides `OverlappedOp`
//! which pins the OVERLAPPED in memory and owns the associated I/O buffer.

use std::pin::Pin;

use windows_sys::Win32::System::IO::OVERLAPPED;

/// A pinned overlapped I/O operation with its associated buffer.
///
/// The OVERLAPPED structure must remain at a stable memory address for the
/// duration of the async I/O operation. `Pin<Box<_>>` guarantees this.
/// The buffer is co-located to keep the operation self-contained.
pub(crate) struct OverlappedOp {
    /// The OVERLAPPED structure passed to ReadFile/WriteFile.
    pub(crate) overlapped: OVERLAPPED,
    /// The I/O buffer. For reads, data is written here by the OS.
    /// For writes, this contains the data to be written.
    pub(crate) buffer: Vec<u8>,
    /// Number of valid bytes in the buffer (for writes).
    #[allow(dead_code)]
    pub(crate) valid_bytes: usize,
}

impl OverlappedOp {
    /// Creates a new overlapped operation for a read at the given file offset.
    pub(crate) fn new_read(offset: u64, buffer_size: usize) -> Pin<Box<Self>> {
        let mut op = Box::pin(Self {
            overlapped: zeroed_overlapped(),
            buffer: vec![0u8; buffer_size],
            valid_bytes: 0,
        });
        set_offset(&mut op, offset);
        op
    }

    /// Creates a new overlapped operation for a write at the given file offset.
    pub(crate) fn new_write(offset: u64, data: &[u8]) -> Pin<Box<Self>> {
        let mut op = Box::pin(Self {
            overlapped: zeroed_overlapped(),
            buffer: data.to_vec(),
            valid_bytes: data.len(),
        });
        set_offset(&mut op, offset);
        op
    }

    /// Returns a mutable pointer to the OVERLAPPED for Win32 API calls.
    ///
    /// # Safety
    ///
    /// The returned pointer is valid as long as the `Pin<Box<OverlappedOp>>`
    /// is alive. The caller must ensure the operation completes before
    /// the OverlappedOp is dropped.
    pub(crate) fn as_overlapped_ptr(self: &mut Pin<Box<Self>>) -> *mut OVERLAPPED {
        // SAFETY: We only need a pointer to the overlapped field within
        // the pinned allocation. The Pin guarantees the memory won't move.
        #[allow(unsafe_code)]
        unsafe {
            &mut self.as_mut().get_unchecked_mut().overlapped as *mut OVERLAPPED
        }
    }

    /// Returns a mutable pointer to the buffer for ReadFile.
    pub(crate) fn buffer_ptr(self: &mut Pin<Box<Self>>) -> *mut u8 {
        // SAFETY: We need a pointer to the buffer for the OS to write into.
        // The Pin guarantees the OverlappedOp won't move, and Vec's buffer
        // is heap-allocated so it's stable independently.
        #[allow(unsafe_code)]
        unsafe {
            self.as_mut().get_unchecked_mut().buffer.as_mut_ptr()
        }
    }

    /// Returns the buffer capacity.
    #[allow(dead_code)]
    pub(crate) fn buffer_capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Sets the file offset for this operation.
    #[allow(dead_code)]
    pub(crate) fn set_offset(self: &mut Pin<Box<Self>>, offset: u64) {
        set_offset(self, offset);
    }
}

/// Creates a zeroed OVERLAPPED structure.
fn zeroed_overlapped() -> OVERLAPPED {
    // SAFETY: OVERLAPPED is a plain-old-data struct that is valid when zeroed.
    #[allow(unsafe_code)]
    unsafe {
        std::mem::zeroed()
    }
}

/// Sets the offset fields in a pinned OverlappedOp.
fn set_offset(op: &mut Pin<Box<OverlappedOp>>, offset: u64) {
    // SAFETY: We are only mutating the overlapped offset fields,
    // not moving the struct.
    #[allow(unsafe_code)]
    let inner = unsafe { op.as_mut().get_unchecked_mut() };
    inner.overlapped.Anonymous.Anonymous.Offset = offset as u32;
    inner.overlapped.Anonymous.Anonymous.OffsetHigh = (offset >> 32) as u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_read_creates_zeroed_buffer() {
        let op = OverlappedOp::new_read(0, 4096);
        assert_eq!(op.buffer.len(), 4096);
        assert!(op.buffer.iter().all(|&b| b == 0));
        assert_eq!(op.valid_bytes, 0);
    }

    #[test]
    fn new_write_copies_data() {
        let data = b"hello world";
        let op = OverlappedOp::new_write(0, data);
        assert_eq!(op.buffer, data);
        assert_eq!(op.valid_bytes, data.len());
    }

    #[test]
    fn offset_set_correctly() {
        let offset: u64 = 0x1_0000_0042;
        let op = OverlappedOp::new_read(offset, 64);
        // SAFETY: reading union fields that were set by set_offset
        #[allow(unsafe_code)]
        let (low, high) = unsafe {
            (
                op.overlapped.Anonymous.Anonymous.Offset,
                op.overlapped.Anonymous.Anonymous.OffsetHigh,
            )
        };
        assert_eq!(low, 0x0000_0042);
        assert_eq!(high, 0x0000_0001);
    }

    #[test]
    fn overlapped_ptr_is_stable() {
        let mut op = OverlappedOp::new_read(0, 64);
        let ptr1 = op.as_overlapped_ptr();
        let ptr2 = op.as_overlapped_ptr();
        assert_eq!(ptr1, ptr2, "pinned pointer must be stable");
    }
}
