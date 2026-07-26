//! Fixed-size wire buffers mirroring upstream rsync's `iobuf` (io.c:91-100).
//!
//! Upstream keeps exactly two circular buffers on the transfer descriptor and
//! **never resizes either of them**: the input buffer is `IO_BUFFER_SIZE`
//! (32 KiB, io.c:1401) and the output buffer is twice that (64 KiB, io.c:1382).
//! The comments at io.c:579 (`"We never resize the circular input buffer."`)
//! and io.c:594 (the same for the output buffer) are load-bearing: a request
//! that cannot fit is a fatal protocol error, never a reallocation.
//!
//! Two upstream properties are reproduced here because they are what keeps a
//! multiplexed stream from tearing:
//!
//! * The 4-byte `MSG_DATA` header is **reserved in place** inside the output
//!   buffer when multiplexing starts (io.c:2461-2462) and **back-filled at
//!   flush time** (io.c:687-688). Header and payload are therefore adjacent
//!   bytes of one buffer before any syscall runs, so no control frame can be
//!   scheduled between them and no partial write can expose a header without
//!   its payload.
//! * Appending bytes that fit performs **no I/O at all** (io.c:2255-2284):
//!   `write_buf()` only reaches `perform_io()` when `out.len + len > out.size`.
//!
//! Upstream does *not* promise one `write()` per frame - a short write may
//! split anywhere, including mid-header, and resumes from `out->pos`
//! (io.c:838-877). [`OutBuf`] reproduces exactly that: the drain loop is
//! restartable and a failed drain leaves `pos`/`len` describing precisely what
//! is still owed to the wire.

mod input;
mod out;

#[cfg(test)]
mod tests;

pub use input::{IN_BUFFER_SIZE, InBuf, IoBufReader};
pub use out::{OUT_BUFFER_SIZE, OutBuf};

/// Rounds `size` up to a whole number of 1024-byte units.
///
/// upstream: `rsync.h` `ROUND_UP_1024()`. Every I/O buffer is allocated in
/// 1024-byte units so the low byte of `size` is always zero, which is what lets
/// [`OutBuf`] borrow that byte as the "size was temporarily reduced" marker
/// (io.c:129-136 `IOBUF_WAS_REDUCED` / `IOBUF_RESTORE_SIZE`).
pub(crate) const fn round_up_1024(size: usize) -> usize {
    size.div_ceil(1024) * 1024
}
