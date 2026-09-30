//! One registered arena carved into `READ_FIXED` sub-ranges.
//!
//! The ring registers a single contiguous region once, as buffer index 0,
//! and never registers again. Each read takes a slice from the start of the
//! arena sized to the request, so adaptivity lives in userspace: the kernel
//! accepts any `addr`/`len` inside a registered buffer
//! (`io_uring/rsrc.c:io_import_fixed` bounds-checks the sub-range against
//! the registered `ubuf`/`len`). The returned lease borrows the ring, so the
//! bump allocator resets to offset 0 once the lease drops.
//!
//! With `direct_files` the ring also registers a one-slot sparse file table;
//! [`ArenaRing::open`] then opens straight into slot 0 with
//! `IORING_OP_OPENAT` and reads use the fixed file, so no fd is installed.

use std::alloc::{self, Layout};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use io_uring::IoUring as RawIoUring;
use io_uring::opcode::{OpenAt, ReadFixed};
use io_uring::types::{DestinationSlot, Fd, Fixed};

use super::page_size;

/// Counters for an [`ArenaRing`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ArenaStats {
    /// `io_uring_register(2)` calls: one for the arena, one for the file table.
    pub register_calls: u64,
    /// `READ_FIXED` SQEs submitted against arena sub-ranges.
    pub read_fixed_sqes: u64,
    /// Files opened into the fixed file table.
    pub direct_opens: u64,
}

/// A file read through an [`ArenaRing`].
pub enum ArenaFile {
    /// An ordinary descriptor.
    Plain(File),
    /// The file sitting in fixed-file slot 0.
    Direct,
}

/// Page-aligned arena memory, freed after the ring that pins it closes.
struct ArenaMemory {
    ptr: *mut u8,
    layout: Layout,
}

impl Drop for ArenaMemory {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `alloc_zeroed(layout)` and is freed once.
        unsafe { alloc::dealloc(self.ptr, self.layout) };
    }
}

/// An io_uring instance owning one registered arena.
pub struct ArenaRing {
    // Declared before `arena`: the ring closes, dropping the pin, first.
    ring: RawIoUring,
    arena: ArenaMemory,
    chunk: usize,
    direct_files: bool,
    stats: ArenaStats,
}

impl ArenaRing {
    /// Builds a ring of `sq_entries` and registers an arena of `arena_len`
    /// bytes (rounded up to a page). Reads split into `chunk`-byte SQEs.
    ///
    /// # Errors
    ///
    /// `InvalidInput` for a zero length or chunk, or the ring setup and
    /// registration errors (`ENOMEM` past `RLIMIT_MEMLOCK`).
    pub fn new(
        arena_len: usize,
        chunk: usize,
        sq_entries: u32,
        direct_files: bool,
    ) -> io::Result<Self> {
        if arena_len == 0 || chunk == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "arena length and chunk must be > 0",
            ));
        }
        let ring = RawIoUring::new(sq_entries)?;
        let page = page_size();
        let layout = Layout::from_size_align(arena_len.next_multiple_of(page), page)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        // SAFETY: `layout` has a non-zero, page-multiple size.
        let ptr = unsafe { alloc::alloc_zeroed(layout) };
        if ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "failed to allocate arena",
            ));
        }
        let arena = ArenaMemory { ptr, layout };
        let iovec = libc::iovec {
            iov_base: ptr.cast(),
            iov_len: layout.size(),
        };
        // SAFETY: `arena` outlives the ring's registration (field order).
        unsafe { ring.submitter().register_buffers(&[iovec]) }?;
        let mut stats = ArenaStats {
            register_calls: 1,
            ..ArenaStats::default()
        };
        if direct_files {
            ring.submitter().register_files_sparse(1)?;
            stats.register_calls += 1;
        }
        Ok(Self {
            ring,
            arena,
            chunk,
            direct_files,
            stats,
        })
    }

    /// Registered arena length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.arena.layout.size()
    }

    /// Always false: an arena is never empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> ArenaStats {
        self.stats
    }

    /// Opens `path` read-only; into fixed-file slot 0 when the ring was
    /// built with `direct_files`, replacing the previous file there.
    ///
    /// # Errors
    ///
    /// The open error, or a submission failure.
    pub fn open(&mut self, path: &Path) -> io::Result<ArenaFile> {
        if !self.direct_files {
            return File::open(path).map(ArenaFile::Plain);
        }
        let name = CString::new(path.as_os_str().as_bytes())?;
        let slot = DestinationSlot::try_from_slot_target(0)
            .map_err(|_| io::Error::other("invalid fixed-file slot"))?;
        let entry = OpenAt::new(Fd(libc::AT_FDCWD), name.as_ptr())
            // O_CLOEXEC is refused for a fixed-file target (io_uring/openclose.c:
            // __io_openat_prep): the file never enters the fd table.
            .flags(libc::O_RDONLY)
            .file_index(Some(slot))
            .build();
        // SAFETY: `name` lives until the CQE is reaped below.
        unsafe { self.ring.submission().push(&entry) }
            .map_err(|_| io::Error::other("submission queue full"))?;
        self.ring.submit_and_wait(1)?;
        let cqe = self
            .ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("missing CQE"))?;
        if cqe.result() < 0 {
            return Err(io::Error::from_raw_os_error(-cqe.result()));
        }
        self.stats.direct_opens += 1;
        Ok(ArenaFile::Direct)
    }

    /// Reads up to `len` bytes of `file` at `offset` into the arena and
    /// lends them in place. At most the arena length is read per call; a
    /// shorter slice than asked means EOF.
    ///
    /// # Errors
    ///
    /// Propagates a negative CQE result or a submission failure.
    pub fn read_lease(&mut self, file: &ArenaFile, offset: u64, len: usize) -> io::Result<&[u8]> {
        let want = len.min(self.len());
        let chunk = self.chunk;
        let target = |i: usize| chunk.min(want - i * chunk);
        let chunks = want.div_ceil(chunk);
        let mut done = vec![0usize; chunks];
        let mut eof = vec![false; chunks];
        let sq_capacity = self.ring.params().sq_entries() as usize;
        loop {
            let mut submitted = 0usize;
            for i in 0..chunks {
                if eof[i] || done[i] >= target(i) {
                    continue;
                }
                if submitted == sq_capacity {
                    break;
                }
                let start = i * chunk + done[i];
                // SAFETY: `start < want <= arena length`, so the pointer stays
                // inside the registered arena.
                let dst = unsafe { self.arena.ptr.add(start) };
                let n = (target(i) - done[i]) as u32;
                let entry = match file {
                    ArenaFile::Plain(f) => ReadFixed::new(Fd(f.as_raw_fd()), dst, n, 0),
                    ArenaFile::Direct => ReadFixed::new(Fixed(0), dst, n, 0),
                }
                .offset(offset + start as u64)
                .build()
                .user_data(i as u64);
                // SAFETY: the arena is borrowed mutably by this call and stays
                // registered until the ring closes.
                unsafe { self.ring.submission().push(&entry) }
                    .map_err(|_| io::Error::other("submission queue full"))?;
                submitted += 1;
            }
            if submitted == 0 {
                break;
            }
            self.stats.read_fixed_sqes += submitted as u64;
            self.ring.submit_and_wait(submitted)?;
            for _ in 0..submitted {
                let cqe = self
                    .ring
                    .completion()
                    .next()
                    .ok_or_else(|| io::Error::other("missing CQE"))?;
                let i = cqe.user_data() as usize;
                match cqe.result() {
                    r if r < 0 => return Err(io::Error::from_raw_os_error(-r)),
                    0 => eof[i] = true,
                    r => done[i] += r as usize,
                }
            }
        }
        // The contiguous prefix ends inside the first chunk EOF left short.
        let filled = (0..chunks)
            .find(|&i| done[i] < target(i))
            .map_or(want, |i| i * chunk + done[i]);
        // SAFETY: the kernel wrote `filled` bytes from the arena start, and
        // the slice borrows `self`, so no read reuses the arena meanwhile.
        Ok(unsafe { std::slice::from_raw_parts(self.arena.ptr, filled) })
    }
}
