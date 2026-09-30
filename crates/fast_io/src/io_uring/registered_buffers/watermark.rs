//! Watermark resizing of a [`RegisteredBufferGroup`].
//!
//! A watermark pool sizes its registered bytes to the next file, whose
//! length the file list announces ahead of the read. Two mechanisms change
//! the kernel table:
//!
//! - **Sparse update** - the table is registered once as
//!   `IORING_REGISTER_BUFFERS2` with `IORING_RSRC_REGISTER_SPARSE`, and a
//!   resize issues one `IORING_REGISTER_BUFFERS_UPDATE` covering only the
//!   slots that change. Growing pins the new slots; shrinking installs empty
//!   iovecs, which unpins and unaccounts the dropped slots.
//! - **Re-register** - a dense `IORING_REGISTER_BUFFERS` table is dropped
//!   with `IORING_UNREGISTER_BUFFERS` and registered again at the new count,
//!   re-pinning every surviving slot.
//!
//! Either way a shrink frees the user memory behind the dropped slots.

use std::alloc::{self, Layout};
use std::io;
use std::os::fd::AsRawFd;
use std::sync::atomic::AtomicU64;

use io_uring::IoUring as RawIoUring;

use super::registry::{RegisteredBufferGroup, full_bitset};
use super::{MAX_REGISTERED_BUFFERS, page_size};

/// `IORING_REGISTER_BUFFERS_UPDATE` from `include/uapi/linux/io_uring.h`.
const IORING_REGISTER_BUFFERS_UPDATE: libc::c_uint = 16;

/// `struct io_uring_rsrc_update2` from `include/uapi/linux/io_uring.h`.
#[repr(C)]
struct RsrcUpdate2 {
    offset: u32,
    resv: u32,
    data: u64,
    tags: u64,
    nr: u32,
    resv2: u32,
}

/// An iovec the kernel reads as "leave this slot empty".
const EMPTY_SLOT: libc::iovec = libc::iovec {
    iov_base: std::ptr::null_mut(),
    iov_len: 0,
};

/// Applies `iovecs` to the ring's buffer table from slot `offset` and
/// returns how many entries the kernel applied.
///
/// Calls `io_uring_register(2)` directly because the `io-uring` crate's
/// `register_buffers_update` discards that count, and the kernel reports a
/// partial update (it stops at the first slot it cannot pin) as success.
///
/// # Safety
///
/// Every non-empty iovec must describe memory that stays allocated until
/// its slot is cleared or the ring closes.
unsafe fn update_buffers(
    ring: &RawIoUring,
    offset: usize,
    iovecs: &[libc::iovec],
) -> io::Result<usize> {
    let update = RsrcUpdate2 {
        offset: offset as u32,
        resv: 0,
        data: iovecs.as_ptr() as u64,
        tags: 0,
        nr: iovecs.len() as u32,
        resv2: 0,
    };
    // SAFETY: `update` points at `iovecs.len()` valid iovecs for the
    // duration of the call; the caller guarantees the memory they describe.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_io_uring_register,
            ring.as_raw_fd(),
            IORING_REGISTER_BUFFERS_UPDATE,
            &raw const update,
            size_of::<RsrcUpdate2>(),
        )
    };
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret as usize)
    }
}

/// Allocates `n` zeroed buffers of `layout`, freeing them all on failure.
fn alloc_buffers(layout: Layout, n: usize) -> io::Result<Vec<*mut u8>> {
    let mut ptrs = Vec::with_capacity(n);
    for _ in 0..n {
        // SAFETY: `layout` has a non-zero, page-multiple size.
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            free_buffers(&ptrs, layout);
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "failed to allocate page-aligned buffer",
            ));
        }
        ptrs.push(ptr);
    }
    Ok(ptrs)
}

/// Frees buffers allocated by [`alloc_buffers`] with the same `layout`.
fn free_buffers(ptrs: &[*mut u8], layout: Layout) {
    for &ptr in ptrs {
        // SAFETY: `ptr` came from `alloc_zeroed(layout)` and is freed once.
        unsafe { alloc::dealloc(ptr, layout) };
    }
}

fn iovecs_of(ptrs: &[*mut u8], len: usize) -> Vec<libc::iovec> {
    ptrs.iter()
        .map(|&p| libc::iovec {
            iov_base: p.cast(),
            iov_len: len,
        })
        .collect()
}

impl RegisteredBufferGroup {
    /// Registers a sparse table of `capacity` slots and fills the first
    /// `count` with `buffer_size`-byte buffers, so [`resize`](Self::resize)
    /// can later change only the slots that differ.
    ///
    /// # Errors
    ///
    /// `InvalidInput` unless `0 < count <= capacity <= 1024` and
    /// `buffer_size > 0`; otherwise the kernel's error, with the sparse table
    /// unregistered again.
    pub fn new_sparse(
        ring: &RawIoUring,
        buffer_size: usize,
        capacity: usize,
        count: usize,
    ) -> io::Result<Self> {
        if buffer_size == 0 || count == 0 || count > capacity || capacity > MAX_REGISTERED_BUFFERS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid sparse geometry: {count} of {capacity} x {buffer_size}"),
            ));
        }
        let page = page_size();
        let aligned = buffer_size.next_multiple_of(page);
        let layout = Layout::from_size_align(aligned, page)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        ring.submitter()
            .register_buffers_sparse(capacity as u32)
            .map_err(|e| io::Error::new(e.kind(), format!("sparse registration failed: {e}")))?;
        let mut group = Self {
            buffers: Vec::new(),
            layout,
            buffer_size: aligned,
            count: 0,
            free_bitset: Vec::new(),
            sparse_capacity: Some(capacity),
            register_calls: 1,
            total_acquires: AtomicU64::new(0),
            total_misses: AtomicU64::new(0),
        };
        if let Err(e) = group.resize(ring, count) {
            let _ = ring.submitter().unregister_buffers();
            return Err(e);
        }
        Ok(group)
    }

    /// Resizes the group to `count` slots of the unchanged buffer size.
    ///
    /// A sparse group updates only the changed slots; a dense group
    /// unregisters and re-registers the whole table. Shrinking frees the
    /// dropped slots' memory. On error the group keeps its previous size.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a zero count or one past the table capacity,
    /// `WouldBlock` while any slot is checked out, or the kernel's error
    /// (`ENOMEM` when `RLIMIT_MEMLOCK` cannot account the new pages).
    pub fn resize(&mut self, ring: &RawIoUring, count: usize) -> io::Result<()> {
        let limit = self.sparse_capacity.unwrap_or(MAX_REGISTERED_BUFFERS);
        if count == 0 || count > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("slot count {count} outside 1..={limit}"),
            ));
        }
        if self.available() != self.count {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "registered slots are checked out",
            ));
        }
        if count == self.buffers.len() {
            return Ok(());
        }
        match self.sparse_capacity {
            Some(_) if count > self.buffers.len() => self.grow_sparse(ring, count)?,
            Some(_) => self.shrink_sparse(ring, count)?,
            None => self.reregister(ring, count)?,
        }
        self.count = count;
        self.free_bitset = full_bitset(count);
        Ok(())
    }

    /// Total bytes currently registered with the kernel.
    #[must_use]
    pub fn registered_bytes(&self) -> usize {
        self.buffers.len() * self.buffer_size
    }

    /// Number of `io_uring_register(2)` calls this group has issued.
    #[must_use]
    pub fn register_calls(&self) -> u64 {
        self.register_calls
    }

    fn grow_sparse(&mut self, ring: &RawIoUring, count: usize) -> io::Result<()> {
        let from = self.buffers.len();
        let fresh = alloc_buffers(self.layout, count - from)?;
        let iovecs = iovecs_of(&fresh, self.buffer_size);
        self.register_calls += 1;
        // SAFETY: `fresh` stays owned by the group until a shrink clears the
        // slots or the group drops after the ring.
        let outcome = unsafe { update_buffers(ring, from, &iovecs) };
        if let Ok(n) = outcome
            && n == iovecs.len()
        {
            self.buffers.extend(fresh);
            return Ok(());
        }
        // A partial update left a prefix pinned; clear it before freeing.
        if let Ok(n) = outcome
            && n > 0
        {
            self.register_calls += 1;
            // SAFETY: empty iovecs describe no memory.
            unsafe { update_buffers(ring, from, &vec![EMPTY_SLOT; n]) }?;
        }
        free_buffers(&fresh, self.layout);
        Err(match outcome {
            Err(e) => e,
            Ok(n) => io::Error::new(
                io::ErrorKind::OutOfMemory,
                format!("kernel pinned {n} of {} new slots", iovecs.len()),
            ),
        })
    }

    fn shrink_sparse(&mut self, ring: &RawIoUring, count: usize) -> io::Result<()> {
        let empty = vec![EMPTY_SLOT; self.buffers.len() - count];
        self.register_calls += 1;
        // SAFETY: empty iovecs describe no memory.
        let n = unsafe { update_buffers(ring, count, &empty) }?;
        if n != empty.len() {
            return Err(io::Error::other(format!(
                "kernel cleared {n} of {} slots",
                empty.len()
            )));
        }
        free_buffers(&self.buffers.split_off(count), self.layout);
        Ok(())
    }

    fn reregister(&mut self, ring: &RawIoUring, count: usize) -> io::Result<()> {
        let fresh = alloc_buffers(self.layout, count.saturating_sub(self.buffers.len()))?;
        let submitter = ring.submitter();
        self.register_calls += 1;
        if let Err(e) = submitter.unregister_buffers() {
            free_buffers(&fresh, self.layout);
            return Err(e);
        }
        let mut ptrs = self.buffers.clone();
        ptrs.extend(&fresh);
        self.register_calls += 1;
        // SAFETY: the first `count` pointers stay owned by the group; the
        // rest are freed only after the kernel dropped them with the table.
        match unsafe { submitter.register_buffers(&iovecs_of(&ptrs[..count], self.buffer_size)) } {
            Ok(()) => {
                self.buffers = ptrs;
                free_buffers(&self.buffers.split_off(count), self.layout);
                Ok(())
            }
            Err(e) => {
                free_buffers(&fresh, self.layout);
                self.register_calls += 1;
                // SAFETY: the old buffers are still owned by the group.
                unsafe { submitter.register_buffers(&iovecs_of(&self.buffers, self.buffer_size)) }?;
                Err(e)
            }
        }
    }
}
