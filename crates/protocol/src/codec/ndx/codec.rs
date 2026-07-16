//! NDX codec trait and protocol-versioned implementations.
//!
//! Provides the Strategy pattern for NDX encoding/decoding across protocol
//! versions, with [`LegacyNdxCodec`] (protocol < 30) and [`ModernNdxCodec`]
//! (protocol >= 30).

use std::io::{self, Read, Write};

use super::constants::NDX_DONE;

/// Classification of the leading byte of a modern (protocol 30+) NDX value.
///
/// Shared by the sync and async modern-codec read leaves so the branch on the
/// first byte can never diverge between them.
enum NdxLead {
    /// Leading `0x00`: `NDX_DONE` sentinel, no further bytes.
    Done,
    /// Leading `0xFF`: a negative index; the diff/tag byte follows.
    Negative,
    /// Any other leading byte: a positive index whose diff/tag is this byte.
    Positive,
}

/// Classifies the leading byte of a modern NDX value.
///
/// Upstream `io.c:2290-2299` - `read_ndx()` first-byte dispatch.
#[inline]
fn classify_ndx_lead(lead: u8) -> NdxLead {
    if lead == 0xFF {
        NdxLead::Negative
    } else if lead == 0 {
        NdxLead::Done
    } else {
        NdxLead::Positive
    }
}

/// Reconstructs the full 4-byte modern NDX value from the `0xFE`/high-bit form.
///
/// `tag` is the byte whose high bit was set; `b0`, `b1`, `b2` are the three
/// following bytes. Upstream `io.c:2307-2311`.
#[inline]
fn decode_ndx_extended_full(tag: u8, b0: u8, b1: u8, b2: u8) -> i32 {
    let high = (tag & !0x80) as i32;
    (high << 24) | (b0 as i32) | ((b1 as i32) << 8) | ((b2 as i32) << 16)
}

/// Reconstructs a modern NDX value from the `0xFE` 2-byte diff form.
///
/// Upstream `io.c:2312-2314`.
#[inline]
fn decode_ndx_extended_diff(hi: u8, lo: u8, prev_val: i32) -> i32 {
    let diff = ((hi as i32) << 8) | (lo as i32);
    prev_val + diff
}

/// Reconstructs a modern NDX value from the single-byte short-diff form.
///
/// Upstream `io.c:2316`.
#[inline]
fn decode_ndx_short(diff_byte: u8, prev_val: i32) -> i32 {
    prev_val + diff_byte as i32
}

/// Strategy trait for NDX encoding/decoding.
///
/// Implementations provide protocol-version-specific wire formats for file
/// list indices. Use [`create_ndx_codec`] to get the appropriate implementation.
///
/// # Example
///
/// ```ignore
/// use protocol::ndx::{create_ndx_codec, NDX_DONE};
///
/// // Protocol 29: uses legacy 4-byte LE format
/// let mut codec = create_ndx_codec(29);
/// let mut buf = Vec::new();
/// codec.write_ndx(&mut buf, 5).unwrap();
/// assert_eq!(buf, vec![5, 0, 0, 0]); // 4-byte LE
///
/// // Protocol 32: uses modern delta encoding
/// let mut codec = create_ndx_codec(32);
/// let mut buf = Vec::new();
/// codec.write_ndx(&mut buf, 0).unwrap();
/// assert_eq!(buf, vec![0x01]); // delta from prev=-1: diff=1
/// ```
pub trait NdxCodec {
    /// Writes an NDX value to the given writer.
    ///
    /// The wire format depends on the codec implementation:
    /// - [`LegacyNdxCodec`]: 4-byte little-endian integer
    /// - [`ModernNdxCodec`]: delta-encoded byte-reduction format
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()>;

    /// Writes NDX_DONE (-1) to signal end of file requests.
    ///
    /// This is a convenience method that handles NDX_DONE specifically:
    /// - Legacy: writes `[0xFF, 0xFF, 0xFF, 0xFF]`
    /// - Modern: writes `[0x00]`
    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()>;

    /// Reads an NDX value from the given reader.
    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32>;

    /// Returns the protocol version this codec is configured for.
    fn protocol_version(&self) -> u8;
}

/// Legacy NDX codec for protocol versions < 30.
///
/// Uses simple 4-byte little-endian signed integers for all NDX values.
/// No delta encoding or byte reduction is applied.
///
/// # Wire Format
///
/// - All values are 4-byte little-endian signed integers
/// - NDX_DONE (-1) = `[0xFF, 0xFF, 0xFF, 0xFF]`
/// - NDX_FLIST_EOF (-2) = `[0xFE, 0xFF, 0xFF, 0xFF]`
/// - Positive index N = N as 4-byte LE
///
/// # Upstream Reference
///
/// `io.c:2246-2248`:
/// ```c
/// if (protocol_version < 30)
///     return read_int(f);
/// ```
#[derive(Debug, Clone)]
pub struct LegacyNdxCodec {
    protocol_version: u8,
}

impl LegacyNdxCodec {
    /// Creates a new legacy NDX codec.
    ///
    /// # Panics
    ///
    /// Panics if `protocol_version >= 30`. Use [`ModernNdxCodec`] for protocol 30+.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        assert!(
            protocol_version < 30,
            "LegacyNdxCodec is for protocol < 30, got {protocol_version}"
        );
        Self { protocol_version }
    }

    /// Async twin of the legacy-codec `read_ndx` (a plain 4-byte LE integer).
    #[cfg(feature = "tokio-transfer")]
    async fn read_ndx_async<R>(&mut self, reader: &mut R) -> io::Result<i32>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        use tokio::io::AsyncReadExt;

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).await?;
        Ok(i32::from_le_bytes(buf))
    }
}

impl NdxCodec for LegacyNdxCodec {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        writer.write_all(&ndx.to_le_bytes())
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&NDX_DONE.to_le_bytes())
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf))
    }

    fn protocol_version(&self) -> u8 {
        self.protocol_version
    }
}

/// Modern NDX codec for protocol versions >= 30.
///
/// Uses delta-encoded byte-reduction format for bandwidth efficiency.
/// Tracks previous values to enable delta encoding.
///
/// # Wire Format
///
/// - `0x00`: NDX_DONE (-1)
/// - `0xFF prefix`: negative values (other than -1)
/// - `1-253`: delta from previous positive value
/// - `0xFE prefix`: extended encoding for larger deltas
///
/// # Upstream Reference
///
/// `io.c:2243-2287` - `write_ndx()` function
/// `io.c:2289-2318` - `read_ndx()` function
#[derive(Debug, Clone)]
pub struct ModernNdxCodec {
    protocol_version: u8,
    /// Previous positive index value for delta encoding.
    prev_positive: i32,
    /// Previous negative index value (as positive) for delta encoding.
    prev_negative: i32,
}

impl ModernNdxCodec {
    /// Creates a new modern NDX codec with upstream's initial values.
    ///
    /// # Panics
    ///
    /// Panics if `protocol_version < 30`. Use [`LegacyNdxCodec`] for protocol < 30.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        assert!(
            protocol_version >= 30,
            "ModernNdxCodec is for protocol >= 30, got {protocol_version}"
        );
        Self {
            protocol_version,
            prev_positive: -1,
            prev_negative: 1,
        }
    }

    /// Returns the delta base for the given sign, matching the sync/async reads.
    #[inline]
    fn prev_val(&self, is_negative: bool) -> i32 {
        if is_negative {
            self.prev_negative
        } else {
            self.prev_positive
        }
    }

    /// Commits a freshly decoded magnitude, updating delta state and returning
    /// the signed NDX. Shared by the sync and async modern-codec reads so the
    /// state mutation can never diverge.
    #[inline]
    fn commit_ndx(&mut self, is_negative: bool, num: i32) -> i32 {
        if is_negative {
            self.prev_negative = num;
            -num
        } else {
            self.prev_positive = num;
            num
        }
    }

    /// Async twin of the modern-codec `read_ndx`.
    ///
    /// Reads the identical byte sequence (`.await`-driven) and drives the same
    /// shared decode helpers (`classify_ndx_lead`, `decode_ndx_*`,
    /// [`commit_ndx`](Self::commit_ndx)) as the sync leaf, so it returns the
    /// same value and consumes the same bytes for the same wire input.
    #[cfg(feature = "tokio-transfer")]
    async fn read_ndx_async<R>(&mut self, reader: &mut R) -> io::Result<i32>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        use tokio::io::AsyncReadExt;

        let mut b = [0u8; 4];
        reader.read_exact(&mut b[..1]).await?;

        let is_negative = match classify_ndx_lead(b[0]) {
            NdxLead::Done => return Ok(NDX_DONE),
            NdxLead::Negative => {
                reader.read_exact(&mut b[..1]).await?;
                true
            }
            NdxLead::Positive => false,
        };

        let prev_val = self.prev_val(is_negative);

        let num = if b[0] == 0xFE {
            reader.read_exact(&mut b[..1]).await?;
            if b[0] & 0x80 != 0 {
                let high = b[0];
                reader.read_exact(&mut b[..3]).await?;
                decode_ndx_extended_full(high, b[0], b[1], b[2])
            } else {
                let hi = b[0];
                reader.read_exact(&mut b[1..2]).await?;
                decode_ndx_extended_diff(hi, b[1], prev_val)
            }
        } else {
            decode_ndx_short(b[0], prev_val)
        };

        Ok(self.commit_ndx(is_negative, num))
    }
}

impl NdxCodec for ModernNdxCodec {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        let mut buf = [0u8; 6];
        let mut cnt = 0;

        let (diff, ndx_positive) = if ndx >= 0 {
            let diff = ndx - self.prev_positive;
            self.prev_positive = ndx;
            (diff, ndx)
        } else if ndx == NDX_DONE {
            // NDX_DONE is sent as single-byte 0 with no side effects
            // Upstream io.c:2259-2262
            return writer.write_all(&[0x00]);
        } else {
            // All negative index bytes start with 0xFF
            // Upstream io.c:2263-2268
            buf[cnt] = 0xFF;
            cnt += 1;
            let ndx_abs = -ndx;
            let diff = ndx_abs - self.prev_negative;
            self.prev_negative = ndx_abs;
            (diff, ndx_abs)
        };

        // Encode the diff value
        // Upstream io.c:2270-2285
        if diff > 0 && diff < 0xFE {
            buf[cnt] = diff as u8;
            cnt += 1;
        } else if !(0..=0x7FFF).contains(&diff) {
            // Full 4-byte encoding with high bit set
            // Upstream io.c:2275-2280
            buf[cnt] = 0xFE;
            cnt += 1;
            buf[cnt] = ((ndx_positive >> 24) as u8) | 0x80;
            cnt += 1;
            buf[cnt] = ndx_positive as u8;
            cnt += 1;
            buf[cnt] = (ndx_positive >> 8) as u8;
            cnt += 1;
            buf[cnt] = (ndx_positive >> 16) as u8;
            cnt += 1;
        } else {
            // 2-byte diff encoding
            // Upstream io.c:2281-2284
            buf[cnt] = 0xFE;
            cnt += 1;
            buf[cnt] = (diff >> 8) as u8;
            cnt += 1;
            buf[cnt] = diff as u8;
            cnt += 1;
        }

        writer.write_all(&buf[..cnt])
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&[0x00])
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        let mut b = [0u8; 4];
        reader.read_exact(&mut b[..1])?;

        let is_negative = match classify_ndx_lead(b[0]) {
            NdxLead::Done => return Ok(NDX_DONE),
            NdxLead::Negative => {
                reader.read_exact(&mut b[..1])?;
                true
            }
            NdxLead::Positive => false,
        };

        let prev_val = self.prev_val(is_negative);

        let num = if b[0] == 0xFE {
            // Extended encoding
            // Upstream io.c:2305-2314
            reader.read_exact(&mut b[..1])?;
            if b[0] & 0x80 != 0 {
                // 4-byte full value
                // Upstream io.c:2307-2311
                let high = b[0];
                reader.read_exact(&mut b[..3])?;
                decode_ndx_extended_full(high, b[0], b[1], b[2])
            } else {
                let hi = b[0];
                reader.read_exact(&mut b[1..2])?;
                decode_ndx_extended_diff(hi, b[1], prev_val)
            }
        } else {
            decode_ndx_short(b[0], prev_val)
        };

        Ok(self.commit_ndx(is_negative, num))
    }

    fn protocol_version(&self) -> u8 {
        self.protocol_version
    }
}

/// An NDX codec that handles both legacy and modern protocol formats.
///
/// This enum wraps both [`LegacyNdxCodec`] and [`ModernNdxCodec`] and dispatches
/// to the appropriate implementation based on the protocol version.
///
/// Use [`NdxCodecEnum::new`] or [`create_ndx_codec`] to create an instance.
#[derive(Debug, Clone)]
pub enum NdxCodecEnum {
    /// Legacy codec for protocol < 30
    Legacy(LegacyNdxCodec),
    /// Modern codec for protocol >= 30
    Modern(ModernNdxCodec),
}

impl NdxCodecEnum {
    /// Creates a new NDX codec for the given protocol version.
    ///
    /// Returns [`NdxCodecEnum::Legacy`] for protocol < 30 and
    /// [`NdxCodecEnum::Modern`] for protocol >= 30.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        if protocol_version < 30 {
            Self::Legacy(LegacyNdxCodec::new(protocol_version))
        } else {
            Self::Modern(ModernNdxCodec::new(protocol_version))
        }
    }

    /// Async twin of `read_ndx`, dispatching to the legacy or modern async leaf.
    ///
    /// Byte-identical to [`NdxCodec::read_ndx`] on this enum for the same wire
    /// bytes: it forwards to the same variant's async read, which shares the
    /// sync leaf's decode helpers.
    #[cfg(feature = "tokio-transfer")]
    pub async fn read_ndx_async<R>(&mut self, reader: &mut R) -> io::Result<i32>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        match self {
            Self::Legacy(codec) => codec.read_ndx_async(reader).await,
            Self::Modern(codec) => codec.read_ndx_async(reader).await,
        }
    }
}

impl NdxCodec for NdxCodecEnum {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        match self {
            Self::Legacy(codec) => codec.write_ndx(writer, ndx),
            Self::Modern(codec) => codec.write_ndx(writer, ndx),
        }
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        match self {
            Self::Legacy(codec) => codec.write_ndx_done(writer),
            Self::Modern(codec) => codec.write_ndx_done(writer),
        }
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        match self {
            Self::Legacy(codec) => codec.read_ndx(reader),
            Self::Modern(codec) => codec.read_ndx(reader),
        }
    }

    fn protocol_version(&self) -> u8 {
        match self {
            Self::Legacy(codec) => codec.protocol_version(),
            Self::Modern(codec) => codec.protocol_version(),
        }
    }
}

#[cfg(feature = "tokio-transfer")]
impl NdxCodecEnum {
    /// Reads one NDX value off an [`AsyncRead`](tokio::io::AsyncRead) through a
    /// shared carry-over buffer, awaiting the wire, gated on `tokio-transfer`.
    ///
    /// This is the flist-reader twin of [`read_ndx_async`](Self::read_ndx_async):
    /// where `read_ndx_async` reads NDX bytes directly off the stream, this drains
    /// from a caller-owned `carry` buffer first so the INC_RECURSE segment loop can
    /// hand bytes the *entry* decoder already read past its last entry (which are
    /// the next segment's NDX header) straight into the NDX decode. Sharing the one
    /// `carry` with [`read_entry_with_flist_async`](crate::flist::read_entry_with_flist_async)
    /// is what keeps the async segment framing byte-identical to the sync path - a
    /// direct-off-stream NDX read would lose those already-buffered bytes.
    ///
    /// Byte-for-byte equivalent to [`NdxCodec::read_ndx`]: it drives the identical
    /// sync `read_ndx` speculatively over the growing in-memory buffer. The codec's
    /// delta state (`prev_positive` / `prev_negative`) is snapshotted via `Clone`
    /// before each attempt and restored when the buffer is too short, so a chunked,
    /// retried read produces the same value and the same state transition as a
    /// single blocking read. On return, only the NDX bytes are drained from the
    /// front of `carry`; leftover bytes (belonging to the following flist entry)
    /// remain for the next reader.
    pub async fn read_ndx_from_carry_async<R>(
        &mut self,
        src: &mut R,
        carry: &mut Vec<u8>,
    ) -> io::Result<i32>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        use std::io::Cursor;
        use tokio::io::AsyncReadExt;

        let mut read_buf = [0u8; 64];
        loop {
            if !carry.is_empty() {
                let snapshot = self.clone();
                let mut cursor = Cursor::new(&carry[..]);
                match self.read_ndx(&mut cursor) {
                    Ok(ndx) => {
                        let consumed = cursor.position() as usize;
                        carry.drain(..consumed);
                        return Ok(ndx);
                    }
                    Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                        *self = snapshot;
                    }
                    Err(err) => return Err(err),
                }
            }
            let n = src.read(&mut read_buf).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "NDX header truncated: stream ended mid-value",
                ));
            }
            carry.extend_from_slice(&read_buf[..n]);
        }
    }
}

/// Creates an NDX codec appropriate for the given protocol version.
///
/// Returns [`NdxCodecEnum::Legacy`] for protocol < 30 and
/// [`NdxCodecEnum::Modern`] for protocol >= 30.
///
/// # Example
///
/// ```ignore
/// use protocol::ndx::create_ndx_codec;
///
/// let mut legacy_codec = create_ndx_codec(29);
/// assert_eq!(legacy_codec.protocol_version(), 29);
///
/// let mut modern_codec = create_ndx_codec(32);
/// assert_eq!(modern_codec.protocol_version(), 32);
/// ```
#[must_use]
pub fn create_ndx_codec(protocol_version: u8) -> NdxCodecEnum {
    NdxCodecEnum::new(protocol_version)
}

/// NDX codec wrapper that guards forward-pass ordering of positive file
/// indices, with a redo-pass exemption.
///
/// Wraps an [`NdxCodecEnum`] and adds a debug-only guard in `write_ndx` that
/// tracks the highest positive NDX (file index) emitted so far. On the forward
/// pass file indices are strictly increasing, and a jump backwards there would
/// signal an ordering bug from parallel processing. Negative sentinel values
/// (NDX_DONE, NDX_FLIST_EOF, etc.) are control signals, not file indices, and
/// are excluded.
///
/// A redo pass legitimately re-emits an already-sent index: upstream
/// `io.c:write_ndx` imposes no ordering, and `sender.c:442` echoes the redo
/// NDX via the same `write_ndx_and_attrs` path used for the first send
/// (`generator.c:2178-2216` re-requests the file on the redo). Once such a
/// re-emission is seen the guard latches off, since no monotonic invariant
/// holds across redo. The tracking is `debug_assert`-only, so release builds
/// compile it out and this wrapper has zero overhead.
#[derive(Debug, Clone)]
pub struct MonotonicNdxWriter {
    /// Inner codec that performs the actual wire encoding.
    inner: NdxCodecEnum,
    /// Highest positive NDX written so far, for the forward-pass ordering guard.
    #[cfg(debug_assertions)]
    last_positive: Option<i32>,
    /// Latches true once a redo re-emits a non-increasing index, disabling the
    /// forward-pass ordering guard for the rest of the transfer.
    #[cfg(debug_assertions)]
    redo_seen: bool,
}

impl MonotonicNdxWriter {
    /// Creates a new monotonic NDX writer for the given protocol version.
    #[must_use]
    pub fn new(protocol_version: u8) -> Self {
        Self {
            inner: NdxCodecEnum::new(protocol_version),
            #[cfg(debug_assertions)]
            last_positive: None,
            #[cfg(debug_assertions)]
            redo_seen: false,
        }
    }

    /// Returns the wrapped codec so callers that must share the SAME wire NDX
    /// diff-state (`prev_positive`/`prev_negative`) can route their writes
    /// through it.
    ///
    /// Upstream `io.c::write_ndx` keeps a single connection-wide state for ALL
    /// NDX writes - file indices, `NDX_FLIST_OFFSET` sub-list headers,
    /// `NDX_FLIST_EOF`, and `NDX_DONE` alike. The negative-diff byte length and
    /// decoded value depend on `prev_negative`, so the sub-list writer MUST NOT
    /// keep an independent state or its offsets desync against the receiver's
    /// unified read state. The monotonicity check is positive-only and the
    /// sub-list writes are negative, so bypassing the wrapper here is wire-safe.
    pub fn inner_mut(&mut self) -> &mut NdxCodecEnum {
        &mut self.inner
    }

    /// Async twin of `read_ndx`, forwarding to the wrapped codec's async read.
    ///
    /// The monotonicity check is a write-side, debug-only guard, so the read
    /// path is a straight delegate - byte-identical to the sync `read_ndx`.
    #[cfg(feature = "tokio-transfer")]
    pub async fn read_ndx_async<R>(&mut self, reader: &mut R) -> io::Result<i32>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        self.inner.read_ndx_async(reader).await
    }
}

impl NdxCodec for MonotonicNdxWriter {
    fn write_ndx<W: Write + ?Sized>(&mut self, writer: &mut W, ndx: i32) -> io::Result<()> {
        // Only check positive file indices - negative values are sentinels
        // (NDX_DONE, NDX_FLIST_EOF, NDX_DEL_STATS, NDX_FLIST_OFFSET). A redo
        // pass re-emits an already-sent index (sender.c:442 echoes the redo NDX
        // via write_ndx_and_attrs), so a non-increasing value latches the guard
        // off rather than tripping it.
        #[cfg(debug_assertions)]
        if ndx >= 0 {
            match self.last_positive {
                Some(prev) if ndx <= prev => self.redo_seen = true,
                _ => {}
            }
            if !self.redo_seen {
                if let Some(prev) = self.last_positive {
                    debug_assert!(
                        ndx > prev,
                        "NDX monotonicity violation: emitted {ndx} after {prev} \
                         on the forward pass - file indices must be strictly \
                         increasing until a redo re-emits one"
                    );
                }
                self.last_positive = Some(ndx);
            }
        }
        self.inner.write_ndx(writer, ndx)
    }

    fn write_ndx_done<W: Write + ?Sized>(&mut self, writer: &mut W) -> io::Result<()> {
        self.inner.write_ndx_done(writer)
    }

    fn read_ndx<R: Read + ?Sized>(&mut self, reader: &mut R) -> io::Result<i32> {
        self.inner.read_ndx(reader)
    }

    fn protocol_version(&self) -> u8 {
        self.inner.protocol_version()
    }
}
