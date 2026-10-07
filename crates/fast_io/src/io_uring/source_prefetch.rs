//! io_uring backend for [`crate::source_prefetch`].
//!
//! Two submissions per batch on the calling thread's per-thread ring:
//!
//! 1. one `OPENAT` / `OPENAT2` SQE per file;
//! 2. per opened file a hard-linked `STATX(AT_EMPTY_PATH)` -> `READ` -> `CLOSE`
//!    chain. Hard links keep the chain ordered and guarantee the `CLOSE` runs
//!    whatever the `STATX` or `READ` returned, so the kernel owns every
//!    descriptor it opened from the moment the chain is queued.
//!
//! Every buffer an SQE points at lives in [`Batch`] until all of its CQEs are
//! reaped. If the ring fails in a way that leaves SQEs in flight, the batch is
//! leaked and the thread's ring discarded rather than freeing memory the
//! kernel may still write.

use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use io_uring::squeue::Flags;
use io_uring::{IoUring, opcode, types};

use super::config::is_io_uring_available;
use super::per_thread_ring::{discard_thread_ring, with_ring};
use crate::confined_open::{LeafPolicy, confined_source_relative};
use crate::source_prefetch::{PREFETCH_MAX_FILE_LEN, PrefetchOpen, PrefetchRequest};

/// SQEs one submission may carry; the per-thread ring's depth.
const SQ_DEPTH: usize = super::per_thread_ring::DEFAULT_RING_DEPTH as usize;
/// Each opened file queues a three-SQE chain.
const CHAIN_LEN: usize = 3;
/// Tag in the high bits of `user_data`, so a stray CQE left on the shared
/// per-thread ring by another consumer is never mistaken for one of ours.
const TAG: u64 = 0x5052_4546_0000_0000;
const TAG_MASK: u64 = 0xFFFF_FFFF_0000_0000;

const STATX_MASK: u32 = rustix::fs::StatxFlags::TYPE
    .union(rustix::fs::StatxFlags::SIZE)
    .bits();

/// Per-file state; every pointer handed to the kernel points into one of these.
struct Slot {
    path: Option<CString>,
    len: usize,
    fd: Option<OwnedFd>,
    statx: rustix::fs::Statx,
    buf: Vec<u8>,
    stat_ok: bool,
    read_len: i32,
}

struct Batch {
    anchor: Option<OwnedFd>,
    how: types::OpenHow,
    open_flags: i32,
    confined: bool,
    slots: Vec<Slot>,
}

/// See [`crate::source_prefetch::prefetch_sources`].
pub(crate) fn prefetch_sources(
    open: PrefetchOpen<'_>,
    noatime: bool,
    requests: &[PrefetchRequest<'_>],
) -> Vec<Option<Vec<u8>>> {
    let mut out = vec![None; requests.len()];
    if requests.is_empty() || !is_io_uring_available() {
        return out;
    }
    let Some(mut batch) = Batch::prepare(open, noatime, requests) else {
        return out;
    };
    let mut queued = false;
    let outcome = with_ring(|ring| {
        queued = true;
        batch.run(ring)
    });
    if let Err(error) = outcome {
        if queued {
            logging::debug_log!(
                Iouring,
                1,
                "io_uring source prefetch abandoned, ring discarded: {error}"
            );
            // SQEs may still be in flight against these buffers and fds.
            std::mem::forget(batch);
            discard_thread_ring();
        }
        // Otherwise the ring was never reached (borrowed further up the
        // stack, or it could not be built): nothing was queued.
        return out;
    }
    for (slot, dst) in batch.slots.iter_mut().zip(out.iter_mut()) {
        if slot.stat_ok && slot.read_len >= 0 && slot.read_len as usize == slot.len {
            *dst = Some(std::mem::take(&mut slot.buf));
        }
    }
    out
}

impl Batch {
    fn prepare(
        open: PrefetchOpen<'_>,
        noatime: bool,
        requests: &[PrefetchRequest<'_>],
    ) -> Option<Self> {
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC;
        if noatime {
            flags |= libc::O_NOATIME;
        }
        let (anchor, confined, root) = match open {
            PrefetchOpen::Path { nofollow } => {
                if nofollow {
                    flags |= libc::O_NOFOLLOW;
                }
                (None, false, None)
            }
            PrefetchOpen::Confined { root, leaf } => {
                if !crate::linux_capabilities::openat2_supported() {
                    return None;
                }
                if leaf == LeafPolicy::Nofollow {
                    flags |= libc::O_NOFOLLOW;
                }
                // upstream: syscall.c:102-107 open_anchor_dirfd() - the same
                // trusted-root open `open_source_confined` anchors on.
                let anchor = crate::secure_dir::open_trusted_dir(root).ok()?;
                (Some(anchor), true, Some(root))
            }
            PrefetchOpen::Anchored { anchor, root } => {
                if !crate::linux_capabilities::openat2_supported() {
                    return None;
                }
                flags |= libc::O_NOFOLLOW;
                (Some(anchor.try_clone_to_owned().ok()?), true, Some(root))
            }
        };
        let slots = requests
            .iter()
            .map(|request| Slot {
                path: (request.len <= PREFETCH_MAX_FILE_LEN)
                    .then(|| request_path(request, root))
                    .flatten(),
                len: request.len as usize,
                fd: None,
                statx: zeroed_statx(),
                buf: Vec::new(),
                stat_ok: false,
                read_len: -1,
            })
            .collect();
        Some(Self {
            anchor,
            how: types::OpenHow::new()
                .flags(flags as u64)
                .resolve(libc::RESOLVE_BENEATH | libc::RESOLVE_NO_MAGICLINKS),
            open_flags: flags,
            confined,
            slots,
        })
    }

    fn run(&mut self, ring: &mut IoUring) -> io::Result<()> {
        let pending: Vec<usize> = (0..self.slots.len())
            .filter(|&i| self.slots[i].path.is_some())
            .collect();
        for chunk in pending.chunks(SQ_DEPTH) {
            self.open_chunk(ring, chunk)?;
        }
        let opened: Vec<usize> = pending
            .into_iter()
            .filter(|&i| self.slots[i].fd.is_some())
            .collect();
        for chunk in opened.chunks(SQ_DEPTH / CHAIN_LEN) {
            self.read_chunk(ring, chunk)?;
        }
        Ok(())
    }

    #[allow(unsafe_code)]
    fn open_chunk(&mut self, ring: &mut IoUring, chunk: &[usize]) -> io::Result<()> {
        let dirfd = self
            .anchor
            .as_ref()
            .map_or(libc::AT_FDCWD, std::os::fd::AsRawFd::as_raw_fd);
        for &i in chunk {
            let path = self.slots[i]
                .path
                .as_ref()
                .expect("pending slots carry a path");
            let entry = if self.confined {
                opcode::OpenAt2::new(types::Fd(dirfd), path.as_ptr(), &self.how).build()
            } else {
                opcode::OpenAt::new(types::Fd(dirfd), path.as_ptr())
                    .flags(self.open_flags)
                    .build()
            };
            // SAFETY: the path CString, `self.how` and the anchor fd live in
            // `self`, which outlives the reap below (or is leaked on failure).
            unsafe { push(ring, &entry.user_data(tag(i, 0)))? };
        }
        let slots = &mut self.slots;
        reap(ring, chunk.len(), |i, _, res| {
            if res >= 0 {
                // SAFETY: a non-negative OPENAT result is a fresh descriptor
                // owned by nobody else.
                slots[i].fd = Some(unsafe { OwnedFd::from_raw_fd(res as RawFd) });
            }
        })
    }

    #[allow(unsafe_code)]
    fn read_chunk(&mut self, ring: &mut IoUring, chunk: &[usize]) -> io::Result<()> {
        for &i in chunk {
            let slot = &mut self.slots[i];
            slot.buf = vec![0u8; slot.len];
            let fd = slot
                .fd
                .take()
                .expect("opened slots carry a descriptor")
                .into_raw_fd();
            let statx = opcode::Statx::new(
                types::Fd(fd),
                c"".as_ptr(),
                (&mut slot.statx as *mut rustix::fs::Statx).cast::<types::statx>(),
            )
            .flags(libc::AT_EMPTY_PATH)
            .mask(STATX_MASK)
            .build()
            .flags(Flags::IO_HARDLINK)
            .user_data(tag(i, 1));
            let read = opcode::Read::new(types::Fd(fd), slot.buf.as_mut_ptr(), slot.len as u32)
                .offset(0)
                .build()
                .flags(Flags::IO_HARDLINK)
                .user_data(tag(i, 2));
            let close = opcode::Close::new(types::Fd(fd))
                .build()
                .user_data(tag(i, 3));
            // SAFETY: the statx buffer and read buffer live in `self.slots`,
            // which outlives the reap below (or is leaked on failure). The fd
            // was released from its `OwnedFd` above: the hard-linked CLOSE is
            // now its only closer. Capacity was reserved by the chunk size, so
            // the three pushes cannot split a chain.
            unsafe {
                push(ring, &statx)?;
                push(ring, &read)?;
                push(ring, &close)?;
            }
        }
        let slots = &mut self.slots;
        reap(ring, chunk.len() * CHAIN_LEN, |i, op, res| match op {
            1 => {
                let st = &slots[i].statx;
                slots[i].stat_ok = res == 0
                    && u32::from(st.stx_mode) & libc::S_IFMT == libc::S_IFREG
                    && st.stx_size == slots[i].len as u64;
            }
            2 => slots[i].read_len = res,
            _ => {}
        })
    }
}

fn request_path(request: &PrefetchRequest<'_>, root: Option<&Path>) -> Option<CString> {
    let path = match root {
        Some(root) => confined_source_relative(request.path.strip_prefix(root).ok()?).ok()?,
        None => request.path,
    };
    CString::new(path.as_os_str().as_bytes()).ok()
}

fn zeroed_statx() -> rustix::fs::Statx {
    // SAFETY: `Statx` is a plain struct of integer fields; all-zero is valid.
    #[allow(unsafe_code)]
    unsafe {
        std::mem::zeroed()
    }
}

const fn tag(slot: usize, op: u64) -> u64 {
    TAG | ((slot as u64) << 2) | op
}

/// Pushes one SQE.
///
/// # Safety
///
/// Every pointer `entry` carries must stay valid until its CQE is reaped.
#[allow(unsafe_code)]
unsafe fn push(ring: &mut IoUring, entry: &io_uring::squeue::Entry) -> io::Result<()> {
    // SAFETY: forwarded to the caller.
    unsafe { ring.submission().push(entry) }
        .map_err(|_| io::Error::other("io_uring submission queue full"))
}

/// Submits what is queued and reaps exactly `expected` of our CQEs, retrying
/// an interrupted wait. `on_cqe` gets `(slot, op, result)`.
fn reap(
    ring: &mut IoUring,
    expected: usize,
    mut on_cqe: impl FnMut(usize, u64, i32),
) -> io::Result<()> {
    let mut got = 0;
    while got < expected {
        match ring.submit_and_wait(expected - got) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        for cqe in ring.completion() {
            let data = cqe.user_data();
            if data & TAG_MASK != TAG {
                continue;
            }
            let low = data & !TAG_MASK;
            on_cqe((low >> 2) as usize, low & 3, cqe.result());
            got += 1;
        }
    }
    Ok(())
}
