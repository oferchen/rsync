//! Bandwidth-throttling writer decorator for the network sender path.
//!
//! Mirrors upstream rsync's socket-write pacing in `io.c:834-862`: the sender's
//! writer clamps every outbound write to `bwlimit_writemax` bytes
//! (`options.c:2394-2397`, `bwlimit * 128` floored at 512) and calls
//! `sleep_for_bwlimit(n)` after each chunk. The receiver never throttles -
//! `main.c:1068` sets `bwlimit_writemax = 0` once it becomes the receiver - so
//! callers install a limiter only on a sender-role (`am_sender`) writer.
//!
//! Placed at the bottom of the sender's writer stack (below the multiplex
//! framer and any compression), so it paces the exact wire bytes the raw
//! descriptor accepts, matching upstream's `perform_io()` write path where the
//! clamp and sleep sit on the raw `write()` of the outermost output buffer.

use std::io::{self, IoSlice, Write};

use bandwidth::BandwidthLimiter;

/// Writer decorator that paces outbound writes to a bandwidth limit.
///
/// When `limiter` is `Some`, every write is split into
/// [`BandwidthLimiter::write_max_bytes`]-sized chunks and each chunk is
/// registered with the limiter (which sleeps once the accumulated debt exceeds
/// the pacing threshold), mirroring the clamp-then-`sleep_for_bwlimit` sequence
/// in `io.c:846-862`.
///
/// When `limiter` is `None` (`--bwlimit=0`, or the receiver role) the decorator
/// is a zero-overhead passthrough: `write`, `write_vectored`, and `flush`
/// delegate straight to the inner writer, preserving the un-throttled path's
/// exact syscall and vectored-batching behaviour.
pub struct ThrottlingWriter<W> {
    inner: W,
    limiter: Option<BandwidthLimiter>,
}

impl<W: Write> ThrottlingWriter<W> {
    /// Wraps `inner`, throttling writes when `limiter` is `Some`.
    pub const fn new(inner: W, limiter: Option<BandwidthLimiter>) -> Self {
        Self { inner, limiter }
    }

    /// Consumes the decorator, returning the inner writer.
    #[allow(dead_code)] // REASON: symmetry with the other writer wrappers; used by tests.
    pub fn into_inner(self) -> W {
        self.inner
    }

    /// Returns whether a bandwidth limiter is active on this writer.
    #[allow(dead_code)] // REASON: exercised by tests asserting role-gated wiring.
    pub const fn is_throttled(&self) -> bool {
        self.limiter.is_some()
    }
}

impl<W: Write> Write for ThrottlingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Destructure so `inner` and `limiter` can be borrowed independently.
        let Self { inner, limiter } = self;
        match limiter {
            // upstream: io.c:846 `if (bwlimit_writemax && len > bwlimit_writemax)
            // len = bwlimit_writemax;` then io.c:861 `sleep_for_bwlimit(n)`.
            Some(limiter) => {
                let max = limiter.write_max_bytes().max(1);
                for chunk in buf.chunks(max) {
                    inner.write_all(chunk)?;
                    let _ = limiter.register(chunk.len());
                }
                Ok(buf.len())
            }
            None => inner.write(buf),
        }
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        let Self { inner, limiter } = self;
        match limiter {
            Some(limiter) => {
                let max = limiter.write_max_bytes().max(1);
                let mut total = 0;
                for slice in bufs {
                    for chunk in slice.chunks(max) {
                        inner.write_all(chunk)?;
                        let _ = limiter.register(chunk.len());
                        total += chunk.len();
                    }
                }
                Ok(total)
            }
            None => inner.write_vectored(bufs),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;
    use std::time::Instant;

    /// Records the size of every inner `write` so tests can assert clamping.
    #[derive(Default)]
    struct RecordingSink {
        writes: Vec<usize>,
        total: usize,
    }

    impl Write for RecordingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.push(buf.len());
            self.total += buf.len();
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn limiter(bytes_per_sec: u64) -> BandwidthLimiter {
        BandwidthLimiter::new(NonZeroU64::new(bytes_per_sec).expect("non-zero rate"))
    }

    /// A `None` limiter is a byte-and-syscall-identical passthrough.
    ///
    /// WHY: `--bwlimit=0` and the receiver role (`main.c:1068`) must not perturb
    /// the un-throttled wire path, so a single `write` reaches the inner writer
    /// whole - no clamping, no sleeping.
    #[test]
    fn unlimited_is_passthrough_no_clamp() {
        let mut writer = ThrottlingWriter::new(RecordingSink::default(), None);
        let payload = vec![0u8; 1 << 20];
        let n = writer.write(&payload).expect("write");
        assert_eq!(n, payload.len());
        let sink = writer.into_inner();
        assert_eq!(
            sink.writes,
            vec![payload.len()],
            "no chunking when unlimited"
        );
    }

    /// An active limiter clamps each inner write to `write_max_bytes`.
    ///
    /// WHY: upstream `io.c:846` caps every socket write to `bwlimit_writemax`
    /// (`options.c:2395` `bwlimit * 128`), so a single large logical write must
    /// reach the descriptor as multiple bounded writes, never one oversized one.
    #[test]
    fn limited_clamps_each_write_to_write_max() {
        let lim = limiter(64 * 1024);
        let write_max = lim.write_max_bytes();
        let mut writer = ThrottlingWriter::new(RecordingSink::default(), Some(lim));
        let payload = vec![0u8; write_max * 3 + 7];
        let n = writer.write(&payload).expect("write");
        assert_eq!(n, payload.len(), "reports the full logical length");
        let sink = writer.into_inner();
        assert_eq!(sink.total, payload.len());
        assert!(
            sink.writes.iter().all(|&len| len <= write_max),
            "every inner write must be clamped to write_max: {:?}",
            sink.writes
        );
        assert!(sink.writes.len() >= 4, "large write split into chunks");
    }

    /// The limiter paces throughput: sending more than one pacing interval's
    /// worth of data blocks for a robust lower bound of wall-clock time.
    ///
    /// WHY: this is the whole point of `--bwlimit` on the sender - upstream
    /// `io.c:861 sleep_for_bwlimit(n)` makes the sender wait so the average
    /// egress rate tracks the configured limit. The bound is deliberately loose
    /// (a fraction of the ideal sleep) to stay CI-robust while still failing
    /// closed if the limiter is never invoked (the pre-fix inert behaviour).
    #[test]
    fn limited_paces_throughput_lower_bound() {
        // 64 KiB/s rate, send 2x32 KiB -> ideal steady-state ~1.0 s. The first
        // pacing interval only fires once accumulated debt exceeds 100 ms
        // (io.c ONE_SEC/10), so assert a conservative >= 150 ms lower bound.
        let rate = 64 * 1024;
        let mut writer = ThrottlingWriter::new(io::sink(), Some(limiter(rate)));
        let payload = vec![0u8; 32 * 1024];
        let start = Instant::now();
        writer.write_all(&payload).expect("write");
        writer.write_all(&payload).expect("write");
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() >= 150,
            "bwlimit must pace the sender; elapsed={elapsed:?}"
        );
    }

    /// `write_vectored` throttles every slice and reports the full byte count.
    ///
    /// WHY: the multiplex framer issues vectored writes (frame header + payload);
    /// the pacing must cover those bytes too, matching upstream where the clamp
    /// sits on the single raw descriptor buffer regardless of how it was framed.
    #[test]
    fn vectored_writes_are_throttled_and_counted() {
        let lim = limiter(64 * 1024);
        let write_max = lim.write_max_bytes();
        let mut writer = ThrottlingWriter::new(RecordingSink::default(), Some(lim));
        let a = vec![1u8; write_max + 1];
        let b = vec![2u8; write_max + 1];
        let bufs = [IoSlice::new(&a), IoSlice::new(&b)];
        let n = writer.write_vectored(&bufs).expect("vectored");
        assert_eq!(n, a.len() + b.len());
        let sink = writer.into_inner();
        assert_eq!(sink.total, a.len() + b.len());
        assert!(sink.writes.iter().all(|&len| len <= write_max));
    }
}
