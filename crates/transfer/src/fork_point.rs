//! Client-pull model of upstream's receiver/generator fork.
//!
//! A pulling upstream client receives the file list as a single process, then
//! forks in `do_recv()` (main.c:1077): the child becomes the receiver
//! (main.c:1083) and the parent the generator (main.c:1142). Each prints its
//! own diagnostics and trailers, so who reports a failure depends on whether
//! it struck before or after the fork. oc runs both halves in one context; the
//! [`ForkPoint`] records when the transfer crosses upstream's fork so the
//! client can name the role upstream would.
//!
//! After the fork the generator reads nothing from the peer: its
//! `stats.total_read` grows only by what the receiver child writes it over the
//! multiplexed error pipe (main.c:1098 `io_start_multiplex_out(f_out)`). The
//! marker models those writes so a failure names the generator's own count.
//!
//! upstream: rsync.c:987-995 `who_am_i()`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Sentinel for "the fork has not happened yet".
const NOT_FORKED: u64 = u64::MAX;

/// A multiplex frame header (io.c:1003-1011 `mplex_write()`).
const FRAME_HEADER: u64 = 4;

/// The payload of `send_msg_int()`: one 4-byte integer (io.c:1196-1203).
const INT_PAYLOAD: u64 = 4;

/// Shared marker for the point where an upstream pull client would fork.
///
/// The receiver marks it once the file list is in and the destination is
/// prepared; the client reads it when the transfer fails.
#[derive(Debug)]
pub struct ForkPoint {
    /// The generator's raw read count: the bytes read when the fork happened
    /// plus every frame the receiver has written it since, or [`NOT_FORKED`].
    total_read_at_fork: AtomicU64,
    /// Whether the receiver's last write to the generator was data, so the
    /// next data write extends that `MSG_DATA` frame instead of opening one.
    data_frame_open: AtomicBool,
}

impl Default for ForkPoint {
    fn default() -> Self {
        Self {
            total_read_at_fork: AtomicU64::new(NOT_FORKED),
            data_frame_open: AtomicBool::new(false),
        }
    }
}

impl ForkPoint {
    /// Records the fork, with the raw read count both halves inherit.
    ///
    /// upstream: main.c:1075-1077 - `io_flush(FULL_FLUSH)` then `do_fork()`;
    /// the generator keeps the `stats.total_read` it had at that moment.
    pub fn mark(&self, total_read: u64) {
        self.total_read_at_fork.store(total_read, Ordering::Relaxed);
    }

    /// Returns the generator's raw read count, or `None` when the transfer
    /// never reached the fork (a failure upstream reports as `[Receiver]`).
    #[must_use]
    pub fn total_read_at_fork(&self) -> Option<u64> {
        let total = self.total_read_at_fork.load(Ordering::Relaxed);
        (total != NOT_FORKED).then_some(total)
    }

    /// Counts `len` bytes the receiver writes the generator as stream data:
    /// `write_int()` of `NDX_FLIST_EOF` (rsync.c:359) or a forwarded sub-list
    /// (rsync.c:377-381, io.c:1669-1672 and io.c:2167-2188).
    ///
    /// Consecutive data shares one `MSG_DATA` frame. Upstream also closes a
    /// frame whenever `perform_io()` flushes before blocking on input, which
    /// depends on how the peer's bytes arrive; the model assumes no such flush.
    pub(crate) fn forward_data(&self, len: u64) {
        if self.total_read_at_fork().is_none() {
            return;
        }
        let header = if self.data_frame_open.swap(true, Ordering::Relaxed) {
            0
        } else {
            FRAME_HEADER
        };
        self.total_read_at_fork
            .fetch_add(header + len, Ordering::Relaxed);
    }

    /// Counts one `send_msg_int()` frame from the receiver to the generator:
    /// `MSG_SUCCESS`, `MSG_REDO` or `MSG_NO_SEND` (receiver.c:1341-1385).
    pub(crate) fn send_int_message(&self) {
        if self.total_read_at_fork().is_none() {
            return;
        }
        self.data_frame_open.store(false, Ordering::Relaxed);
        self.total_read_at_fork
            .fetch_add(FRAME_HEADER + INT_PAYLOAD, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unforked_until_marked() {
        let point = ForkPoint::default();
        assert_eq!(point.total_read_at_fork(), None);
        point.mark(0);
        assert_eq!(point.total_read_at_fork(), Some(0));
    }

    #[test]
    fn keeps_the_count_seen_at_the_fork() {
        let point = ForkPoint::default();
        point.mark(326);
        assert_eq!(point.total_read_at_fork(), Some(326));
    }

    /// Nothing reaches a generator that does not exist yet.
    #[test]
    fn writes_before_the_fork_are_not_counted() {
        let point = ForkPoint::default();
        point.forward_data(4);
        point.send_int_message();
        assert_eq!(point.total_read_at_fork(), None);
    }

    /// Measured on 3.5.1: a flat INC_RECURSE pull forks at 50 bytes; the
    /// NDX_FLIST_EOF forward makes it 58 and each completed file's MSG_SUCCESS
    /// adds 8 (66, 74).
    #[test]
    fn the_flist_eof_forward_and_each_success_add_one_frame() {
        let point = ForkPoint::default();
        point.mark(50);
        point.forward_data(4);
        assert_eq!(point.total_read_at_fork(), Some(58));
        point.send_int_message();
        assert_eq!(point.total_read_at_fork(), Some(66));
        point.send_int_message();
        assert_eq!(point.total_read_at_fork(), Some(74));
    }

    /// Measured on 3.5.1: two sub-lists and the EOF marker forwarded back to
    /// back reach the generator as one 82-byte read, a single MSG_DATA frame.
    #[test]
    fn consecutive_forwards_share_one_data_frame() {
        let point = ForkPoint::default();
        point.mark(128);
        point.forward_data(4 + 33);
        point.forward_data(4 + 33);
        point.forward_data(4);
        assert_eq!(point.total_read_at_fork(), Some(128 + 82));
    }

    /// A message between two data writes closes the first frame.
    #[test]
    fn a_message_closes_the_open_data_frame() {
        let point = ForkPoint::default();
        point.mark(0);
        point.forward_data(10);
        point.send_int_message();
        point.forward_data(10);
        assert_eq!(point.total_read_at_fork(), Some(14 + 8 + 14));
    }
}
