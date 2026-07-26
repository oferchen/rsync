use std::io::{self, Read};

use logging::debug_log;

use crate::envelope::{HEADER_LEN, MessageCode};

use super::super::helpers::{
    decode_header, stage_payload_buffer, truncated_frame_error, truncated_payload_error,
};

/// Resumable decoder for one multiplex frame at a time.
///
/// [`recv_msg_into`](super::recv::recv_msg_into) reads a frame with a single
/// `read_exact` pair and therefore cannot survive a reader that stops
/// mid-frame: the partial bytes are dropped and the error propagates, leaving
/// the demultiplexer's next call to resynchronise on whatever byte follows.
/// That is a desync, not a recoverable error.
///
/// This decoder keeps the frame's progress - how much of the 4-byte header and
/// how much of the payload has landed - across calls. A reader that returns
/// [`io::ErrorKind::WouldBlock`] (or any transient error) part-way through a
/// frame leaves the state intact, so the very next call resumes at the exact
/// byte where the stall happened. `EINTR` is retried inline, matching upstream
/// `perform_io`, which treats `EINTR`/`EAGAIN` alike as "zero bytes moved, not
/// an error" (io.c:800-808).
///
/// Upstream needs no such state machine because its decoders never touch the
/// descriptor: `read_buf()` copies out of the circular `iobuf.in`, and the sole
/// reader is `perform_io` (io.c:789). This type is the equivalent guarantee for
/// a `std::io::Read` stack, where the frame boundary and the syscall boundary
/// are not the same thing.
#[derive(Debug, Default)]
pub struct FrameReader {
    header: [u8; HEADER_LEN],
    header_read: usize,
    /// `Some` once the header has been decoded and the payload is in progress.
    code: Option<MessageCode>,
    payload_len: usize,
    payload_read: usize,
}

impl FrameReader {
    /// Creates a decoder positioned at a frame boundary.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` while a frame is partially read.
    ///
    /// A caller must not reuse `buffer` for anything else while this holds: the
    /// bytes already received live there and the next call resumes into them.
    #[must_use]
    pub fn in_progress(&self) -> bool {
        self.header_read != 0 || self.code.is_some()
    }

    /// Reads the next complete frame, resuming any frame left partially read.
    ///
    /// On success `buffer` holds exactly the payload and the frame's code is
    /// returned. On a transient error the frame's progress is retained and the
    /// call may simply be repeated. End-of-stream part-way through a frame is
    /// reported as a truncation, matching the existing readers.
    pub fn read_frame<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        buffer: &mut Vec<u8>,
    ) -> io::Result<MessageCode> {
        let code = match self.code {
            // Resuming after a stall: the previous call truncated `buffer` to
            // the bytes it had actually received so no uninitialised byte was
            // ever observable. Re-stage the tail before reading into it.
            Some(code) => {
                stage_payload_buffer(buffer, self.payload_len, self.payload_read)?;
                code
            }
            None => {
                while self.header_read < HEADER_LEN {
                    match reader.read(&mut self.header[self.header_read..]) {
                        Ok(0) => {
                            return Err(truncated_frame_error(HEADER_LEN, self.header_read));
                        }
                        Ok(n) => self.header_read += n,
                        Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
                        Err(err) => return Err(err),
                    }
                }

                let header = decode_header(&self.header)?;
                let len = header.payload_len_usize();
                debug_log!(Io, 3, "mux recv: code={:?} len={}", header.code(), len);
                stage_payload_buffer(buffer, len, 0)?;
                self.code = Some(header.code());
                self.payload_len = len;
                self.payload_read = 0;
                header.code()
            }
        };

        while self.payload_read < self.payload_len {
            match reader.read(&mut buffer[self.payload_read..]) {
                Ok(0) => {
                    let got = self.payload_read;
                    buffer.truncate(got);
                    return Err(truncated_payload_error(self.payload_len, got));
                }
                Ok(n) => self.payload_read += n,
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    buffer.truncate(self.payload_read);
                    return Err(err);
                }
            }
        }

        self.code = None;
        self.header_read = 0;
        Ok(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::MessageHeader;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Reader that hands back one byte per call and reports `WouldBlock` once a
    /// shared budget is exhausted, so a stall can be placed at every byte
    /// offset of a frame - including inside the 4-byte header.
    struct StallingDribble {
        data: Vec<u8>,
        pos: usize,
        budget: Arc<AtomicUsize>,
    }

    impl Read for StallingDribble {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.budget.load(Ordering::SeqCst) == 0 {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "stalled"));
            }
            if self.pos >= self.data.len() || out.is_empty() {
                return Ok(0);
            }
            out[0] = self.data[self.pos];
            self.pos += 1;
            self.budget.fetch_sub(1, Ordering::SeqCst);
            Ok(1)
        }
    }

    fn frame(code: MessageCode, payload: &[u8]) -> Vec<u8> {
        let header = MessageHeader::new(code, payload.len() as u32).unwrap();
        let mut out = Vec::from(header.encode());
        out.extend_from_slice(payload);
        out
    }

    /// A stall at any offset - mid-header or mid-payload - must be resumable.
    /// The pre-existing `read_exact` pair dropped the bytes it had already
    /// received, so the retry resynchronised on the middle of the frame.
    #[test]
    fn stall_at_any_offset_resumes_the_same_frame() {
        let payload: Vec<u8> = (0..64u8).collect();
        let wire = frame(MessageCode::Data, &payload);

        for stall_at in 0..wire.len() {
            let budget = Arc::new(AtomicUsize::new(stall_at));
            let mut reader = StallingDribble {
                data: wire.clone(),
                pos: 0,
                budget: Arc::clone(&budget),
            };
            let mut frames = FrameReader::new();
            let mut buffer = Vec::new();

            let err = frames
                .read_frame(&mut reader, &mut buffer)
                .expect_err("must stall");
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
            assert_eq!(
                frames.in_progress(),
                stall_at > 0,
                "a stall past the first byte must retain the frame's progress; \
                 a stall before it leaves us at a frame boundary"
            );
            assert!(
                buffer.len() <= payload.len(),
                "only received bytes are observable"
            );

            budget.store(usize::MAX, Ordering::SeqCst);
            let code = frames
                .read_frame(&mut reader, &mut buffer)
                .expect("resumes from the stall point");
            assert_eq!(code, MessageCode::Data);
            assert_eq!(buffer, payload, "stalled at offset {stall_at}");
            assert!(!frames.in_progress());
        }
    }

    /// Consecutive frames decode independently once the first completes.
    #[test]
    fn consecutive_frames_decode_in_order() {
        let mut wire = frame(MessageCode::Data, b"first");
        wire.extend_from_slice(&frame(MessageCode::Info, b"note"));
        wire.extend_from_slice(&frame(MessageCode::Data, b""));

        let budget = Arc::new(AtomicUsize::new(usize::MAX));
        let mut reader = StallingDribble {
            data: wire,
            pos: 0,
            budget,
        };
        let mut frames = FrameReader::new();
        let mut buffer = Vec::new();

        assert_eq!(
            frames.read_frame(&mut reader, &mut buffer).unwrap(),
            MessageCode::Data
        );
        assert_eq!(buffer, b"first");
        assert_eq!(
            frames.read_frame(&mut reader, &mut buffer).unwrap(),
            MessageCode::Info
        );
        assert_eq!(buffer, b"note");
        assert_eq!(
            frames.read_frame(&mut reader, &mut buffer).unwrap(),
            MessageCode::Data
        );
        assert!(buffer.is_empty(), "an empty payload clears the buffer");
    }

    /// End of stream part-way through a frame stays a truncation error, and the
    /// bytes actually received remain observable - never uninitialised memory.
    #[test]
    fn eof_mid_payload_truncates_to_what_arrived() {
        let mut wire = frame(MessageCode::Data, b"abcdefgh");
        wire.truncate(4 + 3);
        let budget = Arc::new(AtomicUsize::new(usize::MAX));
        let mut reader = StallingDribble {
            data: wire,
            pos: 0,
            budget,
        };
        let mut frames = FrameReader::new();
        let mut buffer = Vec::new();

        let err = frames
            .read_frame(&mut reader, &mut buffer)
            .expect_err("truncated frame");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(buffer, b"abc");
    }
}
