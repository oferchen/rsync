//! Sans-io decode seam shared by the sync and async token decoder drivers.
//!
//! Each per-algorithm decoder exposes a resumable `step` function that owns ALL
//! decode, decompression, dictionary, `saved_flag`, and run-index state
//! internally and NEVER reads from the wire directly. The step consumes the
//! bytes the previous step asked for and returns either [`TokenStep::Need`] (it
//! needs exactly that many more bytes before it can make progress) or
//! [`TokenStep::Emit`] (a fully decoded token, including end-of-stream).
//!
//! The reader lives entirely in a driver ([`drive_sync`] here, or the async
//! driver behind `tokio-transfer`). Because every wire read in the original
//! decoders was a `read_exact` of a statically known count at each decision
//! point (1-byte flag, 1-byte deflated-length low byte, N-byte payload, 2-byte
//! run count, 4-byte absolute token), the `Need(n)` / exact-`read_exact(n)`
//! handshake reproduces the original read pattern byte-for-byte. The former
//! Rust recursion for the zero-output DEFLATED_DATA case (`self.recv_token`)
//! becomes an internal state-machine continue: the step simply loops back to
//! its idle state and returns `Need(1)` for the next flag.

use std::io::{self, Read};

use super::{CHUNK_SIZE, CompressedToken, DEFLATED_DATA, END_FLAG, TOKEN_LONG, TOKEN_REL};

/// The outcome of a single decoder step.
///
/// The state machine that produces these owns all decode state; the driver only
/// pulls bytes. See the module docs for the byte-identical guarantee.
pub(super) enum TokenStep {
    /// The decoder needs exactly this many more bytes from the wire before it
    /// can make progress. The driver must `read_exact` that many bytes and feed
    /// them back into the next `step` call. A `Need(0)` is never produced.
    Need(usize),
    /// A fully decoded token (including [`CompressedToken::End`]).
    Emit(CompressedToken),
}

/// Algorithm-specific hook for decoding a DEFLATED_DATA sequence.
///
/// The shared [`TokenDecodeCore`] state machine handles all common wire framing
/// (idle flag read, run-token emission, TOKEN_REL / TOKEN_LONG parsing, output
/// chunking, and the deflated-length header reads). It defers only the parts
/// that differ per algorithm:
///
/// - whether a consecutive DEFLATED_DATA run is one continuous stream fed block
///   by block through a persistent inflate state (zlib) or a series of
///   independently decompressible blocks (zstd, lz4);
/// - how compressed input is turned into decompressed output.
///
/// Two decode paths exist, selected by [`accumulates`](Self::accumulates):
///
/// - Non-streaming (zstd/lz4): each DEFLATED_DATA block is a complete unit. The
///   core calls [`begin_block`](Self::begin_block) then
///   [`decompress_into`](Self::decompress_into) once per block.
/// - Streaming (zlib): a consecutive DEFLATED_DATA run is one deflate stream
///   split across wire blocks. The core feeds each block via
///   [`begin_block`](Self::begin_block) (first) or [`push_block`](Self::push_block)
///   (follow-on), pulls bounded output with [`stream_step`](Self::stream_step)
///   between and within blocks, and, when the run ends, restores the stripped
///   `Z_SYNC_FLUSH` trailer via [`finish_run`](Self::finish_run). This mirrors
///   upstream token.c:recv_deflated_token()'s r_inflating/r_inflated states:
///   O(CHUNK_SIZE) memory for a multi-GB transfer, no accumulation, no cap.
///
/// Implementors own the persistent decompression context; the core owns the run
/// index, saved flag, and output buffer.
pub(super) trait DeflateSink {
    /// Whether a consecutive DEFLATED_DATA run is one continuous inflate stream
    /// fed block by block (zlib) rather than a series of independent blocks
    /// (zstd/lz4). Streaming sinks use [`stream_step`](Self::stream_step) and
    /// [`finish_run`](Self::finish_run); non-streaming sinks use
    /// [`decompress_into`](Self::decompress_into).
    fn accumulates(&self) -> bool;

    /// Presents the first block of a DEFLATED_DATA run.
    ///
    /// Non-streaming sinks store `payload` for [`decompress_into`](Self::decompress_into);
    /// streaming sinks feed it into the persistent inflate stream for
    /// [`stream_step`](Self::stream_step).
    fn begin_block(&mut self, payload: &[u8]);

    /// Presents a consecutive follow-on block of the same DEFLATED_DATA run.
    ///
    /// Only called on streaming sinks; feeds `payload` into the persistent
    /// inflate stream. Non-streaming sinks never receive follow-on blocks.
    fn push_block(&mut self, payload: &[u8]) -> io::Result<()>;

    /// Decompresses one complete block into `output` (non-streaming sinks).
    ///
    /// The caller has already cleared `output`. Streaming sinks leave this
    /// defaulted and produce output through [`stream_step`](Self::stream_step)
    /// instead.
    fn decompress_into(&mut self, _output: &mut Vec<u8>) -> io::Result<()> {
        Ok(())
    }

    /// Inflates the current block incrementally into `output`, appending at most
    /// one CHUNK_SIZE worth (streaming sinks).
    ///
    /// Returns `true` once the current block's input is fully consumed with no
    /// further pending output. When output is produced it returns `false`, so
    /// the core emits that chunk and re-enters to drain the rest of the block.
    /// The default (non-streaming sinks) reports the block already exhausted.
    ///
    /// upstream: token.c recv_deflated_token() r_inflating (Z_NO_FLUSH into a
    /// fixed CHUNK_SIZE dbuf, emitting output incrementally).
    fn stream_step(&mut self, _output: &mut Vec<u8>) -> io::Result<bool> {
        Ok(true)
    }

    /// Ends a consecutive DEFLATED_DATA run (streaming sinks).
    ///
    /// Restores the `Z_SYNC_FLUSH` trailer (`00 00 FF FF`) the sender stripped,
    /// feeds it through the persistent inflate stream, and drains any residual
    /// output into `output` (which the caller has cleared). The default is a
    /// no-op for non-streaming sinks.
    ///
    /// upstream: token.c recv_deflated_token() r_inflated (feeds the 4-byte sync
    /// trailer with Z_SYNC_FLUSH).
    fn finish_run(&mut self, _output: &mut Vec<u8>) -> io::Result<()> {
        Ok(())
    }
}

/// Resumable, reader-free state of a common token decoder.
///
/// Owns the run index, saved flag, output buffer, and the phase of the wire
/// read currently in flight. The algorithm-specific decompression is delegated
/// to a [`DeflateSink`]. This is the single shared state machine that both the
/// sync and async drivers advance one [`TokenStep`] at a time.
pub(super) struct TokenDecodeCore {
    /// Decompressed output awaiting emission in CHUNK_SIZE pieces.
    decompress_buf: Vec<u8>,
    /// Read position within `decompress_buf`.
    decompress_pos: usize,
    /// Current absolute token index.
    rx_token: i32,
    /// Remaining tokens in the current run.
    rx_run: i32,
    /// A flag byte peeked past the end of a DEFLATED_DATA accumulation (zlib).
    saved_flag: Option<u8>,
    /// Whether output is chunked in CHUNK_SIZE pieces (zlib/zstd) or emitted
    /// whole (lz4).
    chunk_output: bool,
    /// The in-flight wire-read phase.
    phase: Phase,
    /// Whether the decoder has received its first token.
    pub(super) initialized: bool,
}

/// The wire-read phase of [`TokenDecodeCore`]: encodes which `read_exact` is in
/// flight so the state machine can resume after the driver supplies bytes.
#[derive(Clone, Copy)]
enum Phase {
    /// At an idle boundary: needs a flag byte (unless one was saved).
    Idle,
    /// Read a DEFLATED_DATA flag; needs the low length byte. `flag` carries the
    /// high length bits.
    DeflatedLen { flag: u8 },
    /// Have the full deflated length; needs the `len`-byte payload.
    DeflatedPayload { len: usize },
    /// (zlib streaming) inflating the current block into bounded output; needs
    /// no wire bytes. Re-entered until the block's input is fully consumed.
    Inflating,
    /// (zlib streaming) peeking the flag byte after a DEFLATED_DATA block: a
    /// follow-on DEFLATED_DATA continues the same inflate stream, anything else
    /// ends the run and triggers the sync-trailer restore.
    AccumPeek,
    /// (zlib streaming) read a follow-on DEFLATED_DATA flag; needs its low
    /// length byte.
    AccumLen { flag: u8 },
    /// (zlib streaming) needs the follow-on `len`-byte payload.
    AccumPayload { len: usize },
    /// Parsed a TOKEN_REL/TOKEN_LONG flag that carries a 2-byte run count.
    RunCount { token: i32 },
    /// Parsed a TOKEN_LONG flag; needs the 4-byte absolute token, then possibly
    /// a run count if `has_run`.
    LongToken { has_run: bool },
    /// Parsed a TOKEN_LONG absolute token that carries a 2-byte run count.
    LongRunCount { token: i32 },
}

impl TokenDecodeCore {
    pub(super) fn new(chunk_output: bool) -> Self {
        Self {
            decompress_buf: Vec::new(),
            decompress_pos: 0,
            rx_token: 0,
            rx_run: 0,
            saved_flag: None,
            chunk_output,
            phase: Phase::Idle,
            initialized: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.decompress_buf.clear();
        self.decompress_pos = 0;
        self.rx_token = 0;
        self.rx_run = 0;
        self.saved_flag = None;
        self.phase = Phase::Idle;
    }

    /// Emits the next available output chunk, or `None` if the output buffer is
    /// drained. Mirrors the entry-point drain in the original decoders.
    fn emit_pending_output(&mut self) -> Option<CompressedToken> {
        if self.decompress_pos < self.decompress_buf.len() {
            let remaining = &self.decompress_buf[self.decompress_pos..];
            let chunk_len = if self.chunk_output {
                remaining.len().min(CHUNK_SIZE)
            } else {
                remaining.len()
            };
            let data = remaining[..chunk_len].to_vec();
            self.decompress_pos += chunk_len;
            return Some(CompressedToken::Literal(data));
        }
        None
    }

    /// After a decompression completes, sets up output emission. Returns the
    /// first output chunk if any, otherwise `None` to continue the state
    /// machine (the original zero-output recursive re-read).
    fn take_first_output(&mut self) -> Option<CompressedToken> {
        self.decompress_pos = 0;
        if self.decompress_buf.is_empty() {
            return None;
        }
        let chunk_len = if self.chunk_output {
            self.decompress_buf.len().min(CHUNK_SIZE)
        } else {
            self.decompress_buf.len()
        };
        let data = self.decompress_buf[..chunk_len].to_vec();
        self.decompress_pos = chunk_len;
        Some(CompressedToken::Literal(data))
    }

    fn next_run_token(&mut self) -> io::Result<CompressedToken> {
        self.rx_run -= 1;
        self.rx_token = self.rx_token.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "token index overflow in compressed stream run",
            )
        })?;
        Ok(CompressedToken::BlockMatch(self.rx_token as u32))
    }

    /// Advances the shared decoder, delegating decompression to `sink`.
    ///
    /// `input` carries the bytes requested by the previous [`TokenStep::Need`]
    /// (empty on the first step of a token).
    pub(super) fn step<S: DeflateSink>(
        &mut self,
        sink: &mut S,
        input: &[u8],
    ) -> io::Result<TokenStep> {
        if !self.initialized {
            self.initialized = true;
        }

        // Most arms return; the phase encodes exactly which read is resuming. A
        // few streaming transitions (feed a block, then inflate it) need no wire
        // bytes, so they `continue` into `Phase::Inflating` within this call.
        // The `input` slice is only ever consumed by the arm that requested it;
        // the `continue` targets (`Inflating`) never read `input`.
        loop {
            match self.phase {
                Phase::Idle => {
                    // Drain any buffered decompressed output first.
                    if let Some(tok) = self.emit_pending_output() {
                        return Ok(TokenStep::Emit(tok));
                    }
                    // Emit pending run tokens.
                    if self.rx_run > 0 {
                        return Ok(TokenStep::Emit(self.next_run_token()?));
                    }
                    // Read the next flag byte, unless one was saved.
                    let flag = if let Some(f) = self.saved_flag.take() {
                        f
                    } else if input.is_empty() {
                        return Ok(TokenStep::Need(1));
                    } else {
                        input[0]
                    };
                    return self.dispatch_flag(flag);
                }
                Phase::DeflatedLen { flag } => {
                    // input holds the 1 low length byte.
                    let high = (flag & 0x3F) as usize;
                    let len = (high << 8) | (input[0] as usize);
                    if len == 0 {
                        // Zero-length payload: no wire read needed. Process an
                        // empty first block directly, matching the original
                        // read_exact(&mut buf[..0]) no-op.
                        sink.begin_block(&[]);
                        if sink.accumulates() {
                            self.phase = Phase::Inflating;
                            continue;
                        }
                        return self.finish_deflate(sink);
                    }
                    self.phase = Phase::DeflatedPayload { len };
                    return Ok(TokenStep::Need(len));
                }
                Phase::DeflatedPayload { len } => {
                    debug_assert_eq!(input.len(), len);
                    sink.begin_block(input);
                    if sink.accumulates() {
                        self.phase = Phase::Inflating;
                        continue;
                    }
                    return self.finish_deflate(sink);
                }
                Phase::Inflating => {
                    // Inflate the current block into a bounded (<= CHUNK_SIZE)
                    // output chunk and emit it, or advance to peek the next flag
                    // once the block's input is exhausted. This is upstream's
                    // r_inflating loop: no accumulation, O(CHUNK_SIZE) memory.
                    self.decompress_buf.clear();
                    let exhausted = sink.stream_step(&mut self.decompress_buf)?;
                    if let Some(tok) = self.take_first_output() {
                        // Stay in Inflating to drain the rest of this block.
                        return Ok(TokenStep::Emit(tok));
                    }
                    debug_assert!(exhausted);
                    self.phase = Phase::AccumPeek;
                    return Ok(TokenStep::Need(1));
                }
                Phase::AccumPeek => {
                    let next_flag = input[0];
                    if (next_flag & 0xC0) == DEFLATED_DATA {
                        self.phase = Phase::AccumLen { flag: next_flag };
                        return Ok(TokenStep::Need(1));
                    }
                    // Run ended: restore the sync trailer and drain, then
                    // dispatch the flag that ended the run.
                    self.saved_flag = Some(next_flag);
                    self.decompress_buf.clear();
                    sink.finish_run(&mut self.decompress_buf)?;
                    self.phase = Phase::Idle;
                    if let Some(tok) = self.take_first_output() {
                        return Ok(TokenStep::Emit(tok));
                    }
                    return self.step_idle_after_zero_output();
                }
                Phase::AccumLen { flag } => {
                    let high = (flag & 0x3F) as usize;
                    let len = (high << 8) | (input[0] as usize);
                    if len == 0 {
                        // Empty follow-on block contributes nothing; peek again.
                        self.phase = Phase::AccumPeek;
                        return Ok(TokenStep::Need(1));
                    }
                    self.phase = Phase::AccumPayload { len };
                    return Ok(TokenStep::Need(len));
                }
                Phase::AccumPayload { len } => {
                    debug_assert_eq!(input.len(), len);
                    sink.push_block(input)?;
                    self.phase = Phase::Inflating;
                    continue;
                }
                Phase::RunCount { token } => {
                    self.rx_token = token;
                    self.rx_run = u16::from_le_bytes([input[0], input[1]]) as i32;
                    self.phase = Phase::Idle;
                    return Ok(TokenStep::Emit(CompressedToken::BlockMatch(
                        self.rx_token as u32,
                    )));
                }
                Phase::LongToken { has_run } => {
                    let token = i32::from_le_bytes([input[0], input[1], input[2], input[3]]);
                    if token < 0 {
                        self.phase = Phase::Idle;
                        // upstream: token.c:528 invalid_compressed_token() ->
                        // exit_cleanup(RERR_PROTOCOL) (exit 2). Tag the error so
                        // the core exit-code mapper yields RERR_PROTOCOL, not
                        // RERR_STREAMIO(12).
                        return Err(crate::protocol_violation::protocol_violation(
                            "invalid token number in compressed stream",
                        ));
                    }
                    if has_run {
                        self.phase = Phase::LongRunCount { token };
                        return Ok(TokenStep::Need(2));
                    }
                    self.rx_token = token;
                    self.phase = Phase::Idle;
                    return Ok(TokenStep::Emit(CompressedToken::BlockMatch(
                        self.rx_token as u32,
                    )));
                }
                Phase::LongRunCount { token } => {
                    self.rx_token = token;
                    self.rx_run = u16::from_le_bytes([input[0], input[1]]) as i32;
                    self.phase = Phase::Idle;
                    return Ok(TokenStep::Emit(CompressedToken::BlockMatch(
                        self.rx_token as u32,
                    )));
                }
            }
        }
    }

    /// Dispatches a freshly read flag byte to the appropriate phase.
    fn dispatch_flag(&mut self, flag: u8) -> io::Result<TokenStep> {
        if (flag & 0xC0) == DEFLATED_DATA {
            self.phase = Phase::DeflatedLen { flag };
            return Ok(TokenStep::Need(1));
        }

        if flag == END_FLAG {
            self.phase = Phase::Idle;
            return Ok(TokenStep::Emit(CompressedToken::End));
        }

        if flag & TOKEN_REL != 0 {
            let rel = (flag & 0x3F) as i32;
            self.rx_token = self.rx_token.checked_add(rel).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "token index overflow in compressed stream",
                )
            })?;
            if (flag >> 6) & 1 != 0 {
                self.phase = Phase::RunCount {
                    token: self.rx_token,
                };
                return Ok(TokenStep::Need(2));
            }
            self.phase = Phase::Idle;
            Ok(TokenStep::Emit(CompressedToken::BlockMatch(
                self.rx_token as u32,
            )))
        } else if flag & 0xE0 == TOKEN_LONG {
            self.phase = Phase::LongToken {
                has_run: flag & 1 != 0,
            };
            Ok(TokenStep::Need(4))
        } else {
            self.phase = Phase::Idle;
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid compressed token flag: 0x{flag:02X}"),
            ))
        }
    }

    /// Decompresses one complete block (non-streaming sinks) and sets up output.
    ///
    /// Returns the first output chunk, or resumes the idle state when the block
    /// produced no output. The zero-output case replaces the original recursive
    /// re-read (`self.recv_token`) with an in-place state-machine transition:
    /// a fresh `Need(1)` for the next flag - exactly the read the recursion
    /// would have performed.
    fn finish_deflate<S: DeflateSink>(&mut self, sink: &mut S) -> io::Result<TokenStep> {
        self.decompress_buf.clear();
        sink.decompress_into(&mut self.decompress_buf)?;
        self.phase = Phase::Idle;
        if let Some(tok) = self.take_first_output() {
            return Ok(TokenStep::Emit(tok));
        }
        self.step_idle_after_zero_output()
    }

    /// Handles the idle re-entry after a zero-output DEFLATED_DATA block without
    /// asking the driver for bytes when a saved flag is already buffered.
    fn step_idle_after_zero_output(&mut self) -> io::Result<TokenStep> {
        if let Some(tok) = self.emit_pending_output() {
            return Ok(TokenStep::Emit(tok));
        }
        if self.rx_run > 0 {
            return Ok(TokenStep::Emit(self.next_run_token()?));
        }
        if let Some(f) = self.saved_flag.take() {
            return self.dispatch_flag(f);
        }
        Ok(TokenStep::Need(1))
    }
}

/// Blocking driver over a [`TokenDecodeCore`] + [`DeflateSink`]: pulls the exact
/// bytes each step requests via `read_exact` and returns the emitted token.
///
/// This reproduces the original blocking `recv_token` read pattern exactly: the
/// first step is fed no bytes, and every subsequent step is fed precisely the
/// `read_exact(n)` bytes it asked for.
pub(super) fn drive_sync<S: DeflateSink, R: Read + ?Sized>(
    core: &mut TokenDecodeCore,
    sink: &mut S,
    reader: &mut R,
) -> io::Result<CompressedToken> {
    let mut input: Vec<u8> = Vec::new();
    loop {
        match core.step(sink, &input)? {
            TokenStep::Emit(token) => return Ok(token),
            TokenStep::Need(n) => {
                input.resize(n, 0);
                reader.read_exact(&mut input)?;
            }
        }
    }
}

/// Async driver over a [`TokenDecodeCore`] + [`DeflateSink`], gated on
/// `tokio-transfer`.
///
/// Byte-for-byte equivalent to [`drive_sync`]: only the byte fetch differs
/// (`.await` on an [`AsyncRead`](tokio::io::AsyncRead) versus a blocking
/// `read_exact`). The same state machine backs both.
#[cfg(feature = "tokio-transfer")]
pub(super) async fn drive_async<S, R>(
    core: &mut TokenDecodeCore,
    sink: &mut S,
    reader: &mut R,
) -> io::Result<CompressedToken>
where
    S: DeflateSink,
    R: tokio::io::AsyncRead + Unpin + ?Sized,
{
    use tokio::io::AsyncReadExt;

    let mut input: Vec<u8> = Vec::new();
    loop {
        match core.step(sink, &input)? {
            TokenStep::Emit(token) => return Ok(token),
            TokenStep::Need(n) => {
                input.resize(n, 0);
                reader.read_exact(&mut input).await?;
            }
        }
    }
}
