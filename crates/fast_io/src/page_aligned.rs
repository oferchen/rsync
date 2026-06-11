//! Page-aligned buffer allocations for unbuffered (direct) I/O.
//!
//! Windows `FILE_FLAG_NO_BUFFERING` and Linux `O_DIRECT` both require I/O
//! buffers whose backing memory is aligned to a multiple of the volume's
//! sector size. The system page size is always a multiple of the sector
//! size, so allocating page-aligned memory satisfies the requirement for
//! every supported filesystem.
//!
//! [`PageAlignedBuffer`] owns a heap allocation guaranteed to start on a
//! page boundary. On Windows the backing store comes from
//! `VirtualAlloc` so the
//! buffer can be handed directly to overlapped `WriteFile` / `ReadFile` calls
//! issued against a handle opened with `FILE_FLAG_NO_BUFFERING`. On every
//! other platform the buffer is allocated through [`std::alloc::alloc`] with
//! an explicit page-aligned `Layout`, matching the helper used by the
//! io_uring registered-buffer registry.
//!
//! Using a page-aligned buffer in the IOCP write path avoids the bounce
//! buffer copy that the kernel performs when an application submits a write
//! whose buffer alignment does not satisfy the no-buffering contract - the
//! kernel allocates a properly aligned scratch buffer, copies the caller's
//! data into it, and only then dispatches the I/O. The bounce copy doubles
//! the memory bandwidth consumed by each write and defeats the whole point
//! of the no-buffering flag for high-throughput transfers.

#[cfg(not(windows))]
use std::alloc::Layout;
use std::sync::OnceLock;

/// Returns the system memory page size in bytes.
///
/// Cached after the first call. The value is queried from the OS at runtime
/// because page size is fixed for the running kernel but varies across
/// platforms (4 KiB on x86, 4 KiB or 16 KiB on aarch64 macOS, 4 KiB / 64 KiB
/// on Linux depending on architecture).
#[must_use]
pub fn page_size() -> usize {
    static PAGE_SIZE: OnceLock<usize> = OnceLock::new();
    *PAGE_SIZE.get_or_init(query_page_size)
}

#[cfg(unix)]
fn query_page_size() -> usize {
    // SAFETY: sysconf(_SC_PAGESIZE) is documented to be safe with no
    // preconditions; the return value is always > 0 on supported Unix
    // platforms.
    #[allow(unsafe_code)]
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if size <= 0 {
        // sysconf returning -1 here would indicate a broken libc; fall back
        // to the universal lower bound rather than panic.
        4096
    } else {
        size as usize
    }
}

#[cfg(windows)]
fn query_page_size() -> usize {
    use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
    // SAFETY: SYSTEM_INFO is a POD struct that is valid when zeroed; the
    // subsequent GetSystemInfo call populates every field we read.
    #[allow(unsafe_code)]
    let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: GetSystemInfo writes into the SYSTEM_INFO out-parameter; the
    // pointer is valid for the lifetime of the call.
    #[allow(unsafe_code)]
    unsafe {
        GetSystemInfo(&mut info);
    }
    info.dwPageSize as usize
}

#[cfg(not(any(unix, windows)))]
fn query_page_size() -> usize {
    4096
}

/// Rounds `size` up to the next multiple of the system page size.
///
/// Returns the page size itself when `size` is zero so callers never receive
/// a zero-sized allocation, which is undefined behaviour for both
/// `std::alloc::alloc` and `VirtualAlloc`.
#[must_use]
pub fn round_up_to_page(size: usize) -> usize {
    let page = page_size();
    if size == 0 {
        page
    } else {
        size.next_multiple_of(page)
    }
}

/// Heap buffer whose backing pointer is guaranteed to be page-aligned.
///
/// The capacity is rounded up to a multiple of [`page_size`], matching the
/// constraint that direct I/O reads/writes must transfer whole sectors and
/// the buffer must cover the rounded-up length even when the caller writes
/// fewer bytes.
///
/// On Windows the allocation comes from
/// `VirtualAlloc` so the
/// memory can be handed straight to overlapped `WriteFile`/`ReadFile` on a
/// handle opened with `FILE_FLAG_NO_BUFFERING`. On all other platforms the
/// allocation is performed via [`std::alloc::alloc`] using a page-aligned
/// `Layout`. In both cases the buffer is freed with the matching API in
/// the [`Drop`] impl, so callers do not need to manage lifetime explicitly.
pub struct PageAlignedBuffer {
    ptr: *mut u8,
    capacity: usize,
    #[cfg(not(windows))]
    layout: Layout,
}

// SAFETY: `PageAlignedBuffer` owns its allocation exclusively. The raw
// pointer is never aliased and is freed in `Drop`, so the buffer is safe to
// move across threads.
#[allow(unsafe_code)]
unsafe impl Send for PageAlignedBuffer {}

// SAFETY: Shared references only expose `&[u8]` / `&mut [u8]` slices, which
// uphold Rust's aliasing rules; mutable access requires `&mut self`.
#[allow(unsafe_code)]
unsafe impl Sync for PageAlignedBuffer {}

impl PageAlignedBuffer {
    /// Allocates a zero-initialised buffer with at least `requested` bytes.
    ///
    /// The actual capacity is `requested` rounded up to the next multiple
    /// of [`page_size`]. The returned buffer's pointer is page-aligned and
    /// the contents are zeroed.
    ///
    /// # Panics
    ///
    /// Panics if the underlying allocator fails to satisfy the request.
    #[must_use]
    pub fn new(requested: usize) -> Self {
        let capacity = round_up_to_page(requested);
        let page = page_size();

        #[cfg(windows)]
        let ptr = {
            use windows_sys::Win32::System::Memory::{
                MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc,
            };
            // SAFETY: VirtualAlloc with a null base address and non-zero size
            // returns a page-aligned, zero-initialised region or null on
            // failure. MEM_COMMIT | MEM_RESERVE backs the region with
            // committed pages immediately so subsequent reads/writes do not
            // fault into the commit limit.
            #[allow(unsafe_code)]
            let raw = unsafe {
                VirtualAlloc(
                    std::ptr::null_mut(),
                    capacity,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_READWRITE,
                )
            };
            assert!(
                !raw.is_null(),
                "VirtualAlloc failed for {capacity}-byte page-aligned buffer"
            );
            raw.cast::<u8>()
        };

        #[cfg(not(windows))]
        let (ptr, layout) = {
            let layout =
                Layout::from_size_align(capacity, page).expect("page-aligned layout must be valid");
            // SAFETY: layout has non-zero size and a power-of-two alignment.
            // alloc_zeroed returns either a properly aligned pointer or null;
            // we treat null as fatal because the pool cannot proceed without
            // a buffer and the caller did not opt in to fallible allocation.
            #[allow(unsafe_code)]
            let raw = unsafe { std::alloc::alloc_zeroed(layout) };
            assert!(
                !raw.is_null(),
                "page-aligned allocation failed for {capacity} bytes (page={page})"
            );
            (raw, layout)
        };

        debug_assert_eq!(
            ptr as usize % page,
            0,
            "page-aligned buffer pointer must be a multiple of {page}"
        );

        Self {
            ptr,
            capacity,
            #[cfg(not(windows))]
            layout,
        }
    }

    /// Returns the buffer capacity in bytes (always a multiple of the
    /// system page size).
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns the raw pointer for FFI use.
    ///
    /// The pointer is page-aligned and valid for `self.capacity()` bytes.
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Returns a mutable raw pointer for FFI use.
    #[must_use]
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    /// Returns the buffer as an immutable byte slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr is valid for `capacity` bytes, the bytes are
        // initialised (zeroed at allocation, only overwritten through
        // `&mut self`), and the lifetime is tied to `&self`.
        #[allow(unsafe_code)]
        unsafe {
            std::slice::from_raw_parts(self.ptr, self.capacity)
        }
    }

    /// Returns the buffer as a mutable byte slice.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: ptr is valid for `capacity` bytes, the bytes are
        // initialised, and exclusive access is enforced by `&mut self`.
        #[allow(unsafe_code)]
        unsafe {
            std::slice::from_raw_parts_mut(self.ptr, self.capacity)
        }
    }
}

impl Drop for PageAlignedBuffer {
    fn drop(&mut self) {
        if self.ptr.is_null() {
            return;
        }

        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Memory::{MEM_RELEASE, VirtualFree};
            // SAFETY: ptr was returned by VirtualAlloc above with
            // MEM_RESERVE | MEM_COMMIT; VirtualFree with MEM_RELEASE and
            // dwSize == 0 releases the entire allocation as documented.
            #[allow(unsafe_code)]
            unsafe {
                let _ = VirtualFree(self.ptr.cast(), 0, MEM_RELEASE);
            }
        }

        #[cfg(not(windows))]
        {
            // SAFETY: ptr was returned by alloc_zeroed with `self.layout`;
            // dealloc with the original layout is the documented inverse.
            #[allow(unsafe_code)]
            unsafe {
                std::alloc::dealloc(self.ptr, self.layout);
            }
        }
    }
}

impl std::fmt::Debug for PageAlignedBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageAlignedBuffer")
            .field("ptr", &self.ptr)
            .field("capacity", &self.capacity)
            .field("page_size", &page_size())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_is_power_of_two() {
        let size = page_size();
        assert!(size > 0);
        assert!(
            size.is_power_of_two(),
            "page size {size} is not a power of two"
        );
    }

    #[test]
    fn round_up_handles_zero() {
        assert_eq!(round_up_to_page(0), page_size());
    }

    #[test]
    fn round_up_keeps_aligned_value() {
        let page = page_size();
        assert_eq!(round_up_to_page(page), page);
        assert_eq!(round_up_to_page(page * 4), page * 4);
    }

    #[test]
    fn round_up_promotes_partial_value() {
        let page = page_size();
        assert_eq!(round_up_to_page(1), page);
        assert_eq!(round_up_to_page(page + 1), page * 2);
    }

    #[test]
    fn buffer_pointer_is_page_aligned() {
        let buf = PageAlignedBuffer::new(8 * 1024);
        let addr = buf.as_ptr() as usize;
        assert_eq!(
            addr % page_size(),
            0,
            "buffer addr {addr:#x} not page-aligned"
        );
    }

    #[test]
    fn buffer_capacity_rounds_up() {
        let page = page_size();
        let buf = PageAlignedBuffer::new(1);
        assert_eq!(buf.capacity(), page);
        let buf = PageAlignedBuffer::new(page + 1);
        assert_eq!(buf.capacity(), page * 2);
    }

    #[test]
    fn buffer_is_writable_then_readable() {
        let mut buf = PageAlignedBuffer::new(4096);
        for (i, byte) in buf.as_mut_slice().iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }
        for (i, byte) in buf.as_slice().iter().enumerate() {
            assert_eq!(*byte, (i % 251) as u8);
        }
    }

    #[test]
    fn buffer_is_initially_zeroed() {
        let buf = PageAlignedBuffer::new(8192);
        assert!(buf.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn buffer_drop_does_not_leak() {
        // Allocate and drop a few buffers; the test passes if the process
        // completes without aborting (which would happen on a double-free
        // or freed-with-wrong-API path).
        for _ in 0..16 {
            let _buf = PageAlignedBuffer::new(64 * 1024);
        }
    }

    #[test]
    fn buffer_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PageAlignedBuffer>();
    }
}
