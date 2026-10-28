//! Multiplex frame schedule of a local copy's two halves.
//!
//! Every flush of a multiplexed output buffer becomes one `MSG_DATA` frame
//! with a 4-byte header (`io.c:775-776`), and a process flushes whenever it
//! must wait for input (`io.c:perform_io()` writes buffered output before it
//! polls) or its 64 KiB buffer fills (`io.c:1567`, `IO_BUFFER_SIZE * 2`).
//! This module assigns the counted payload to frames the way the two halves'
//! wait points produce them when each half answers promptly.

use protocol::MESSAGE_HEADER_LEN;

/// Payload one frame can carry before the 64 KiB buffer forces a flush.
///
/// upstream: io.c:1567 sizes the output buffer `IO_BUFFER_SIZE * 2`, and
/// io.c:795 reserves the next frame's header inside it.
const FRAME_PAYLOAD: u64 = 2 * 32 * 1024 - MESSAGE_HEADER_LEN as u64;

/// Running `sent`/`received` totals, one protocol round at a time.
#[derive(Debug, Default)]
pub(super) struct FrameSchedule {
    sent: u64,
    received: u64,
}

/// Frame bytes for `payload` bytes written in one go: the payload plus one
/// header per buffer-sized frame. An empty round writes no frame.
fn framed(payload: u64) -> u64 {
    payload + payload.div_ceil(FRAME_PAYLOAD) * MESSAGE_HEADER_LEN as u64
}

#[cfg(test)]
pub(super) fn framed_for_test(payload: u64) -> u64 {
    framed(payload)
}

impl FrameSchedule {
    /// Adds the sender's file-list rounds.
    ///
    /// upstream: main.c:1398 `io_flush(NORMAL_FLUSH)` after `send_file_list()`
    /// closes the first round; the sub-lists `send_files()` sends before its
    /// first read (sender.c:516) go out when that read blocks.
    pub(super) fn sender_flist(&mut self, rounds: [u64; 2]) {
        self.sent += rounds.iter().map(|&p| framed(p)).sum::<u64>();
    }

    /// Adds the per-file exchanges and the NDX_DONE tail.
    ///
    /// Each round of the generator's requests is answered by one round of the
    /// sender's replies. The tail follows generator.c:2698 (one NDX_DONE per
    /// finished list) and generator.c:2861-2871 (three phase NDX_DONEs sent
    /// together); the sender echoes each and adds sender.c:820's own, less the
    /// one `read_final_goodbye()` consumes. With incremental recursion the
    /// generator declares the first list done on its own, the rest once the
    /// receiver finishes them; without it the list's NDX_DONE rides in the
    /// same frame as the last request (generator.c:2832). `done_width` is the
    /// encoded size of one NDX_DONE.
    pub(super) fn exchange_rounds(
        &mut self,
        sender_rounds: [u64; 2],
        generator_rounds: [u64; 2],
        list_count: usize,
        done_width: u64,
        inc_recurse: bool,
    ) {
        let done = |count: u64| framed(count * done_width);
        if inc_recurse {
            self.sent += sender_rounds.iter().map(|&p| framed(p)).sum::<u64>();
            self.received += generator_rounds.iter().map(|&p| framed(p)).sum::<u64>();
            let middle = (list_count as u64).saturating_sub(1);
            self.sent += done(1) + done(middle) + done(2);
            self.received += done(1) + done(middle) + done(3);
        } else {
            self.sent += framed(sender_rounds[0] + done_width) + done(2);
            self.received += framed(generator_rounds[0] + done_width) + done(3);
        }
    }

    /// Adds the generator's deletion reports to its phase tail.
    ///
    /// upstream: generator.c:2867-2871 writes the delete statistics between
    /// the phase NDX_DONEs, inside the same frame. A late delete pass
    /// (`--delete-after`/`--delete-delay`, generator.c:2899-2916) holds the
    /// last phase NDX_DONE back until after the pass, and the client caches
    /// its totals (main.c:1401 `handle_stats()`) once the first two have
    /// arrived: the statistics, every `MSG_DELETED` and that NDX_DONE land
    /// outside the counted window.
    pub(super) fn generator_deletions(
        &mut self,
        del_stats: u64,
        deleted: u64,
        late: bool,
        done_width: u64,
    ) {
        if late {
            self.received -= framed(3 * done_width) - framed(2 * done_width);
        } else {
            self.received += del_stats + deleted;
        }
    }

    /// The `(sent, received)` totals.
    pub(super) const fn totals(&self) -> (u64, u64) {
        (self.sent, self.received)
    }
}
