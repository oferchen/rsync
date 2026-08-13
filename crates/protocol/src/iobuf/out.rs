use std::io::{self, Write};

use crate::envelope::{HEADER_LEN, MAX_PAYLOAD_LENGTH, MessageCode, MessageHeader};

use super::round_up_1024;

/// The `MSG_DATA` tag byte pre-shifted into its wire position.
///
/// Built through [`MessageHeader`] so the tag encoding has exactly one
/// definition, and folded at compile time so the back-fill in [`OutBuf::flush`]
/// is the infallible `tag | length` OR that upstream writes with `SIVAL`
/// (io.c:687-688).
const DATA_HEADER_TAG: u32 = match MessageHeader::new(MessageCode::Data, 0) {
    Ok(header) => header.encode_raw(),
    Err(_) => panic!("a zero-length MSG_DATA header is always representable"),
};

/// Upstream's raw output buffer size.
///
/// upstream: `io.c:1541` `alloc_xbuf(&iobuf.out, ROUND_UP_1024(IO_BUFFER_SIZE * 2))`
/// with `IO_BUFFER_SIZE = 32*1024` (`rsync.h:160`). Never reallocated
/// (io.c:594 "We never resize the circular output buffer.").
pub const OUT_BUFFER_SIZE: usize = 64 * 1024;

/// Circular output buffer with an in-place `MSG_DATA` header reservation.
///
/// Mirrors upstream's `iobuf.out` (`xbuf` in `rsync.h:1096-1101`, driven by
/// `io.c:write_buf()` and the drain block at io.c:838-877):
///
/// * `pos` is the offset of the next byte owed to the wire, `len` the number of
///   buffered bytes, and both wrap around `size`.
/// * When multiplexing is active, `out_empty_len` is 4 and the first four bytes
///   of the buffered run are a *reserved* `MSG_DATA` header. "Empty" therefore
///   means `len == 4`, not `len == 0` - the pervasive off-by-4 that upstream
///   spells `out_empty_len` (io.c:2457).
/// * The header is back-filled only at flush time (io.c:687-688), so header and
///   payload are contiguous bytes of one buffer before any syscall runs.
///
/// The buffer is allocated once and never grows. `size` may be *temporarily
/// reduced* so a 4-byte header never straddles the physical end of the buffer
/// (io.c:480-513 `reduce_iobuf_size` / `restore_iobuf_size`); because every
/// allocation is a whole number of 1024-byte units, the low byte of `size` is
/// free to act as the "was reduced" marker (io.c:129-136).
pub struct OutBuf {
    buf: Vec<u8>,
    pos: usize,
    len: usize,
    /// Current (possibly temporarily reduced) circular size.
    size: usize,
    /// 0 when raw, 4 once multiplexing reserves a `MSG_DATA` header.
    /// upstream: `io.c:2652` `iobuf.out_empty_len = 4`.
    out_empty_len: usize,
    /// Offset of the reserved `MSG_DATA` header inside `buf`.
    /// upstream: `iobuf.raw_data_header_pos`.
    raw_data_header_pos: usize,
    /// End (as an unwrapped `pos + len`) of the run currently being flushed, or
    /// 0 when no flush is in progress. upstream: `iobuf.raw_flushing_ends_before`,
    /// which "can point off the end of the iobuf.out buffer for a while, for
    /// easier subtracting" (io.c:684-685).
    flush_ends_before: usize,
}

impl OutBuf {
    /// Allocates a raw (non-multiplexed) output buffer able to hold
    /// `data_capacity` payload bytes plus one reserved multiplex header.
    ///
    /// The allocation is rounded up to a whole number of 1024-byte units so the
    /// low byte of `size` stays free for the reduced-size marker.
    ///
    /// # Panics
    ///
    /// Panics if `data_capacity` could produce a payload beyond the 24-bit
    /// multiplex length limit; that would make the header back-fill fallible,
    /// which upstream's `SIVAL` is not.
    #[must_use]
    pub fn new(data_capacity: usize) -> Self {
        let size = round_up_1024(data_capacity + HEADER_LEN);
        assert!(
            size - HEADER_LEN <= MAX_PAYLOAD_LENGTH as usize,
            "output buffer capacity exceeds the 24-bit multiplex payload limit"
        );
        Self {
            buf: vec![0u8; size],
            pos: 0,
            len: 0,
            size,
            out_empty_len: 0,
            raw_data_header_pos: 0,
            flush_ends_before: 0,
        }
    }

    /// Reserves the first `MSG_DATA` header in place, activating multiplexing.
    ///
    /// upstream: `io.c:2455-2462` `io_start_multiplex_out()` - after a full
    /// flush it sets `out_empty_len = 4`, records `raw_data_header_pos` and
    /// bumps `out.len` by 4 without writing anything.
    ///
    /// # Panics
    ///
    /// Panics if the buffer is not empty; upstream reaches this only after
    /// `io_flush(FULL_FLUSH)`.
    pub fn start_multiplex(&mut self) {
        assert!(
            self.len == 0 && self.out_empty_len == 0,
            "multiplexing must start on an empty raw buffer"
        );
        self.pos = 0;
        self.out_empty_len = HEADER_LEN;
        self.raw_data_header_pos = 0;
        self.len = HEADER_LEN;
    }

    /// Returns the number of payload bytes buffered, excluding any reserved
    /// header. upstream: `out.len - out_empty_len`.
    #[must_use]
    pub fn data_len(&self) -> usize {
        self.len - self.out_empty_len
    }

    /// Returns `true` when nothing but a reserved header is buffered.
    ///
    /// upstream: `out.len == out_empty_len` (io.c:681, io.c:1472). This is the
    /// only correct emptiness test on a multiplexed buffer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == self.out_empty_len
    }

    /// Returns the payload bytes this buffer can hold between flushes.
    #[must_use]
    pub fn data_capacity(&self) -> usize {
        self.size - self.out_empty_len
    }

    /// Appends `data` to the buffered run without performing any I/O.
    ///
    /// This is the memcpy half of upstream `write_buf()` (io.c:2266-2279),
    /// including the split copy that wraps the circular end. Requirement 13 of
    /// the port - "a write whose bytes fit performs no I/O at all" - is a
    /// property of this function having no writer argument at all.
    ///
    /// # Panics
    ///
    /// Panics if `data` does not fit; upstream's caller guarantees room by
    /// draining first (io.c:2263-2264), and so must ours.
    pub fn push(&mut self, data: &[u8]) {
        assert!(
            self.len + data.len() <= self.size,
            "OutBuf::push overflow: caller must flush before appending"
        );

        let mut pos = self.pos + self.len;
        if pos >= self.size {
            pos -= self.size;
        }

        // upstream: io.c:2270-2276 - split the copy when it wraps the end.
        if pos >= self.pos {
            let contiguous = self.size - pos;
            if contiguous < data.len() {
                self.buf[pos..pos + contiguous].copy_from_slice(&data[..contiguous]);
                self.buf[..data.len() - contiguous].copy_from_slice(&data[contiguous..]);
                self.len += data.len();
                return;
            }
        }

        self.buf[pos..pos + data.len()].copy_from_slice(data);
        self.len += data.len();
    }

    /// Back-fills the reserved header and reserves the next one.
    ///
    /// upstream: io.c:682-708. The header records everything buffered since the
    /// last flush (`out.len - 4`), the run being flushed is pinned by
    /// `flush_ends_before`, and a fresh 4-byte header is reserved immediately
    /// after it - reducing the buffer's size first if those four bytes would
    /// straddle the physical end.
    fn begin_flush(&mut self) {
        if !self.is_multiplexed() {
            return;
        }

        self.flush_ends_before = self.pos + self.len;

        // upstream: io.c:687-688 - SIVAL(buf + raw_data_header_pos, 0,
        // ((MPLEX_BASE + MSG_DATA) << 24) + out.len - 4).
        let raw = DATA_HEADER_TAG | (self.len - HEADER_LEN) as u32;
        // The reserved slot never straddles the end: begin_flush relocates it.
        self.buf[self.raw_data_header_pos..self.raw_data_header_pos + HEADER_LEN]
            .copy_from_slice(&raw.to_le_bytes());

        // upstream: io.c:695-707 - reserve room for the next MSG_DATA header.
        let mut next = self.flush_ends_before;
        if next >= self.size {
            next -= self.size;
        } else if next + HEADER_LEN > self.size {
            self.reduce_size(next);
            next = 0;
        }
        self.raw_data_header_pos = next;
        // upstream: "Yes, it is possible for this to make len > size for a while."
        self.len += HEADER_LEN;
    }

    /// Writes one contiguous run to `writer` and accounts for what left.
    ///
    /// Mirrors upstream's single `write()` inside `perform_io` (io.c:838-877):
    /// at most one contiguous span is offered, `pos`/`len` advance by exactly
    /// the accepted byte count, and `EINTR` is reported as a zero-byte write
    /// rather than an error (io.c:840-841). Any other error leaves `pos`/`len`
    /// describing precisely what is still owed, so the caller may retry without
    /// ever re-sending or dropping a byte.
    fn drain_once<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<usize> {
        let mut want = if self.flush_ends_before != 0 {
            self.flush_ends_before - self.pos
        } else {
            self.len
        };
        if self.pos + want > self.size {
            want = self.size - self.pos;
        }
        if want == 0 {
            // Unreachable: `flush` only drains while `!is_empty()`, and both
            // emptiness and the end of the pinned run are reached on the same
            // byte. Surfacing it beats spinning if that invariant ever breaks.
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "multiplex output buffer has no drainable run",
            ));
        }

        let n = match writer.write(&self.buf[self.pos..self.pos + want]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write buffered multiplex output",
                ));
            }
            Ok(n) => n,
            // upstream: io.c:840-841 - EINTR is n = 0, not an error.
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => 0,
            Err(err) => return Err(err),
        };

        // upstream: io.c:869-877 - advance pos with wrap, then len; reaching
        // out_empty_len rewinds the buffer and relocates the reserved header.
        self.pos += n;
        if self.pos == self.size {
            if self.flush_ends_before != 0 {
                self.flush_ends_before -= self.size;
            }
            self.pos = 0;
            self.restore_size();
        } else if self.pos == self.flush_ends_before {
            self.flush_ends_before = 0;
        }

        self.len -= n;
        if self.len == self.out_empty_len {
            self.pos = 0;
            self.restore_size();
            if self.is_multiplexed() {
                self.raw_data_header_pos = 0;
            }
        }

        Ok(n)
    }

    /// Drains everything buffered, back-filling the `MSG_DATA` header first.
    ///
    /// upstream: `io_flush(FULL_FLUSH)` (io.c:2136-2147) driving `perform_io`.
    /// The header is written into the buffer *before* the first syscall, so a
    /// short write can split the frame anywhere - including mid-header, exactly
    /// as upstream's can - without ever exposing a header whose payload is not
    /// already queued behind it in the same buffer. Re-entering after a failed
    /// drain resumes at `pos`; the header is not back-filled twice.
    ///
    /// Bytes appended while a run was stalled become their own `MSG_DATA`
    /// frame, exactly as upstream's next pass through the fd-selection test
    /// (io.c:680-708) pins a fresh run behind the reserved header.
    pub fn flush<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        loop {
            if self.flush_ends_before == 0 {
                if self.is_empty() {
                    return Ok(());
                }
                self.begin_flush();
            }
            self.drain_once(writer)?;
        }
    }

    /// Returns whether a `MSG_DATA` header slot is reserved.
    fn is_multiplexed(&self) -> bool {
        self.out_empty_len != 0
    }

    /// upstream: `io.c:480-495 reduce_iobuf_size()`.
    fn reduce_size(&mut self, new_size: usize) {
        if new_size < self.size {
            self.size = new_size;
        }
    }

    /// upstream: `io.c:497-513 restore_iobuf_size()` - the low byte of a
    /// 1024-aligned size is non-zero only while the size is reduced.
    fn restore_size(&mut self) {
        if self.size & 0xFF != 0 {
            self.size = (self.size | 0xFF) + 1;
        }
    }

    /// Test hook: the current circular offsets (`pos`, `len`, `size`).
    #[cfg(test)]
    pub(super) fn cursors(&self) -> (usize, usize, usize) {
        (self.pos, self.len, self.size)
    }
}
