//! Safe wrapper around OVERLAPPED structures for async I/O.
//!
//! Each overlapped operation requires a stable OVERLAPPED pointer that remains
//! valid until the operation completes. This module provides `OverlappedOp`
//! which pins the OVERLAPPED in memory and owns the associated I/O buffer.
//!
//! The buffer can be backed by either a standard `Vec<u8>` (the default for
//! buffered I/O) or a [`PageAlignedBuffer`](crate::page_aligned::PageAlignedBuffer)
//! used by the no-buffering write path. Page-aligned storage avoids the
//! per-write bounce copy the kernel performs when the application buffer is
//! not sector-aligned on a `FILE_FLAG_NO_BUFFERING` handle.

use std::pin::Pin;

use windows_sys::Win32::System::IO::OVERLAPPED;

use crate::page_aligned::PageAlignedBuffer;

/// Backing storage for an [`OverlappedOp`] buffer.
///
/// The two variants share the same `&[u8]` view but allocate from different
/// arenas. The `Vec` variant uses the default heap allocator and is suitable
/// for buffered I/O. The `PageAligned` variant is backed by
/// [`PageAlignedBuffer`] (Windows `VirtualAlloc`) so the kernel can hand the
/// pointer straight to a `FILE_FLAG_NO_BUFFERING` overlapped write without
/// the alignment-fixup bounce copy.
pub(crate) enum BufferStorage {
    /// Standard heap-allocated buffer (no alignment guarantee).
    Vec(Vec<u8>),
    /// Page-aligned buffer holding `valid` initialised bytes.
    ///
    /// The underlying [`PageAlignedBuffer`] capacity is rounded up to a page
    /// multiple, so `valid` may be less than the capacity. The slice views
    /// expose only the first `valid` bytes to keep length semantics aligned
    /// with the `Vec` variant.
    PageAligned {
        /// The aligned allocation.
        buffer: PageAlignedBuffer,
        /// Number of initialised bytes the consumer asked for. The buffer
        /// capacity may be larger because page rounding inflates the
        /// allocation; only the first `valid` bytes contain caller data.
        valid: usize,
    },
}

impl BufferStorage {
    /// Returns the number of valid bytes (matches the legacy `Vec::len`).
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Vec(v) => v.len(),
            Self::PageAligned { valid, .. } => *valid,
        }
    }

    /// Returns the valid bytes as an immutable slice.
    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            Self::Vec(v) => v.as_slice(),
            Self::PageAligned { buffer, valid } => &buffer.as_slice()[..*valid],
        }
    }

    /// Returns a mutable raw pointer to the first byte of the buffer.
    fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            Self::Vec(v) => v.as_mut_ptr(),
            Self::PageAligned { buffer, .. } => buffer.as_mut_ptr(),
        }
    }

    /// Returns an immutable raw pointer to the first byte of the buffer.
    pub(crate) fn as_ptr(&self) -> *const u8 {
        match self {
            Self::Vec(v) => v.as_ptr(),
            Self::PageAligned { buffer, .. } => buffer.as_ptr(),
        }
    }

    /// Returns whether the backing storage is page-aligned.
    pub(crate) fn is_page_aligned(&self) -> bool {
        matches!(self, Self::PageAligned { .. })
    }
}

impl std::ops::Index<std::ops::RangeFrom<usize>> for BufferStorage {
    type Output = [u8];

    fn index(&self, range: std::ops::RangeFrom<usize>) -> &[u8] {
        &self.as_slice()[range]
    }
}

impl std::ops::Index<std::ops::Range<usize>> for BufferStorage {
    type Output = [u8];

    fn index(&self, range: std::ops::Range<usize>) -> &[u8] {
        &self.as_slice()[range]
    }
}

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
    pub(crate) buffer: BufferStorage,
    /// Number of valid bytes in the buffer (for writes).
    #[allow(dead_code)] // REASON: field set in constructor, read only in tests
    pub(crate) valid_bytes: usize,
}

impl OverlappedOp {
    /// Creates a new overlapped operation for a read at the given file offset.
    pub(crate) fn new_read(offset: u64, buffer_size: usize) -> Pin<Box<Self>> {
        let mut op = Box::pin(Self {
            overlapped: zeroed_overlapped(),
            buffer: BufferStorage::Vec(vec![0u8; buffer_size]),
            valid_bytes: 0,
        });
        set_offset(&mut op, offset);
        op
    }

    /// Creates a new overlapped operation for a write at the given file offset.
    pub(crate) fn new_write(offset: u64, data: &[u8]) -> Pin<Box<Self>> {
        let mut op = Box::pin(Self {
            overlapped: zeroed_overlapped(),
            buffer: BufferStorage::Vec(data.to_vec()),
            valid_bytes: data.len(),
        });
        set_offset(&mut op, offset);
        op
    }

    /// Creates a write op backed by a page-aligned buffer.
    ///
    /// `data` is copied into a freshly allocated [`PageAlignedBuffer`] whose
    /// pointer is guaranteed to be page-aligned. Use this constructor when
    /// the operation will be submitted to a handle opened with
    /// `FILE_FLAG_NO_BUFFERING` so the kernel can issue the I/O without a
    /// bounce-buffer copy.
    pub(crate) fn new_write_aligned(offset: u64, data: &[u8]) -> Pin<Box<Self>> {
        let mut buffer = PageAlignedBuffer::new(data.len().max(1));
        // Copy caller data into the leading bytes; the trailing pad (if any
        // from page rounding) stays zeroed by VirtualAlloc / alloc_zeroed.
        buffer.as_mut_slice()[..data.len()].copy_from_slice(data);
        let mut op = Box::pin(Self {
            overlapped: zeroed_overlapped(),
            buffer: BufferStorage::PageAligned {
                buffer,
                valid: data.len(),
            },
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
        // The Pin guarantees the OverlappedOp won't move, and both backing
        // storages keep heap-allocated memory whose address is stable for
        // the operation's lifetime.
        #[allow(unsafe_code)]
        unsafe {
            self.as_mut().get_unchecked_mut().buffer.as_mut_ptr()
        }
    }

    /// Returns the buffer capacity.
    #[allow(dead_code)] // REASON: IOCP API completeness; used in tests
    pub(crate) fn buffer_capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Sets the file offset for this operation.
    #[allow(dead_code)] // REASON: IOCP API completeness; used in tests
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
    use crate::page_aligned::page_size;

    #[test]
    fn new_read_creates_zeroed_buffer() {
        let op = OverlappedOp::new_read(0, 4096);
        assert_eq!(op.buffer.len(), 4096);
        assert!(op.buffer.as_slice().iter().all(|&b| b == 0));
        assert_eq!(op.valid_bytes, 0);
    }

    #[test]
    fn new_write_copies_data() {
        let data = b"hello world";
        let op = OverlappedOp::new_write(0, data);
        assert_eq!(op.buffer.as_slice(), data);
        assert_eq!(op.valid_bytes, data.len());
    }

    #[test]
    fn new_write_aligned_uses_page_aligned_buffer() {
        let data = b"page aligned write payload";
        let op = OverlappedOp::new_write_aligned(0, data);
        assert!(op.buffer.is_page_aligned());
        assert_eq!(op.buffer.as_slice(), data);
        assert_eq!(op.valid_bytes, data.len());
        let addr = op.buffer.as_ptr() as usize;
        assert_eq!(
            addr % page_size(),
            0,
            "aligned buffer pointer {addr:#x} must be page-aligned"
        );
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
