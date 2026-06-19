//! Mode-switching writer enum dispatching plain, multiplex, and compressed I/O.
//!
//! Mirrors upstream rsync's `io_start_multiplex_out()` / `io_start_buffering_out()`
//! transitions where the global I/O buffer state is modified at runtime.

use std::io::{self, IoSlice, Write};
use std::net::{Shutdown, TcpStream};
use std::num::NonZeroU8;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use protocol::MessageCode;

use super::multiplex::MultiplexWriter;
use crate::compressed_writer::CompressedWriter;
use crate::is_early_close_error;

/// Writer that can switch from plain to multiplex mode after protocol setup.
///
/// Upstream rsync modifies global I/O buffer state via `io_start_multiplex_out()`.
/// We achieve the same by wrapping the writer and delegating based on mode.
#[allow(clippy::large_enum_variant)]
#[allow(private_interfaces)]
pub enum ServerWriter<W: Write> {
    /// Plain mode - write data directly without framing.
    Plain(W),
    /// Multiplex mode - wrap data in MSG_DATA frames.
    Multiplex(MultiplexWriter<W>),
    /// Compressed+Multiplex mode - compress then multiplex.
    Compressed(CompressedWriter<MultiplexWriter<W>>),
    /// Temporary state during in-place transformations.
    /// Any operation on a Taken writer panics.
    #[doc(hidden)]
    Taken,
}

impl<W: Write> ServerWriter<W> {
    /// Creates a new plain-mode writer.
    #[inline]
    pub const fn new_plain(writer: W) -> Self {
        Self::Plain(writer)
    }

    /// Activates multiplex mode (mirrors upstream `io_start_multiplex_out`).
    pub fn activate_multiplex(self) -> io::Result<Self> {
        match self {
            Self::Plain(writer) => Ok(Self::Multiplex(MultiplexWriter::new(writer))),
            Self::Multiplex(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "multiplex already active",
            )),
            Self::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
        }
    }

    /// Activates compression on top of multiplex mode.
    ///
    /// This must be called AFTER `activate_multiplex()` to match upstream behavior.
    /// Upstream rsync activates compression in `io.c:io_start_buffering_out()`
    /// which wraps the already-multiplexed stream.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The writer is not in multiplex mode (compression requires multiplex first)
    /// - Compression is already active
    /// - The compression algorithm is not supported in this build
    pub fn activate_compression(
        self,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
    ) -> io::Result<Self> {
        self.activate_compression_with_workers(algorithm, level, None)
    }

    /// Like [`activate_compression`](Self::activate_compression) but plumbs
    /// `--compress-threads=N` into `ZSTD_c_nbWorkers`. upstream: `token.c:701`.
    pub fn activate_compression_with_workers(
        self,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
        workers: Option<NonZeroU8>,
    ) -> io::Result<Self> {
        match self {
            Self::Multiplex(mux) => {
                // upstream: io.c:write_buf() tees data to batch_fd at the
                // write_buf level, which is AFTER compression. Keep the
                // batch recorder on MultiplexWriter so it captures the
                // compressed wire bytes, matching upstream behavior. The
                // batch header records do_compression=true so replay knows
                // to decompress.
                let compressed = CompressedWriter::with_workers(mux, algorithm, level, workers)?;
                Ok(Self::Compressed(compressed))
            }
            Self::Plain(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "compression requires multiplex mode first",
            )),
            Self::Compressed(_) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "compression already active",
            )),
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
        }
    }

    /// Returns true if multiplex is active.
    #[inline]
    pub const fn is_multiplexed(&self) -> bool {
        matches!(self, Self::Multiplex(_) | Self::Compressed(_))
    }

    /// Activates multiplex mode in place (mirrors upstream `io_start_multiplex_out`).
    ///
    /// Used when the generator needs to activate multiplex AFTER sending
    /// the file list but before sending file data. Upstream rsync client sender
    /// calls `io_start_multiplex_out()` after `send_file_list()`.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Multiplex mode is already active
    /// - The writer is in an invalid Taken state
    pub fn activate_multiplex_in_place(&mut self) -> io::Result<()> {
        let old_self = std::mem::replace(self, Self::Taken);

        match old_self {
            Self::Plain(writer) => {
                *self = Self::Multiplex(MultiplexWriter::new(writer));
                Ok(())
            }
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
            other => {
                *self = other;
                Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "multiplex already active",
                ))
            }
        }
    }

    /// Sends a control message (non-DATA message) through the multiplexed stream.
    ///
    /// Control messages bypass the compression layer (if active) to match upstream
    /// rsync behavior where they go directly through the multiplex layer.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The writer is not in multiplex mode
    /// - The underlying I/O operation fails
    pub fn send_message(&mut self, code: MessageCode, payload: &[u8]) -> io::Result<()> {
        match self {
            Self::Multiplex(mux) => mux.send_message(code, payload),
            Self::Compressed(compressed) => compressed.inner_mut().send_message(code, payload),
            Self::Plain(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot send control messages in plain mode",
            )),
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
        }
    }

    /// Sends a `MSG_NO_SEND` message for the given file index.
    ///
    /// Indicates that the sender could not open the requested file and will
    /// not be sending delta data for it. The receiver should skip waiting
    /// for this file's transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:367-368`: `send_msg_int(MSG_NO_SEND, ndx)` when file open fails
    ///   and `protocol_version >= 30`.
    /// - `io.c:1618-1627`: receiver-side handling of `MSG_NO_SEND`.
    ///
    /// # Errors
    ///
    /// Returns an error if the writer is not in multiplex mode or the underlying
    /// I/O operation fails.
    pub fn send_no_send(&mut self, ndx: i32) -> io::Result<()> {
        self.send_message(MessageCode::NoSend, &ndx.to_le_bytes())
    }

    /// Sends a `MSG_REDO` message for the given file index.
    ///
    /// Indicates that the receiver detected a whole-file checksum failure and
    /// the file should be re-transferred with full checksum length (no delta basis).
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:970-974`: `send_msg_int(MSG_REDO, ndx)` on checksum failure
    /// - `io.c:1514-1519`: generator-side handler queues index to `redo_list`
    ///
    /// # Errors
    ///
    /// Returns an error if the writer is not in multiplex mode or the underlying
    /// I/O operation fails.
    pub fn send_redo(&mut self, ndx: i32) -> io::Result<()> {
        self.send_message(MessageCode::Redo, &ndx.to_le_bytes())
    }

    /// Attaches a batch recorder for capturing compressed protocol data.
    ///
    /// The recorder is always attached to the `MultiplexWriter` so it captures
    /// data at the same level as upstream rsync's `write_buf()` tee to `batch_fd`.
    /// When compression is active, this means capturing **post-compression**
    /// (compressed) wire bytes. The batch header records `do_compression: true`
    /// so replay knows to decompress the tokens.
    ///
    /// upstream: io.c:write_buf() tees to batch_fd after compression but before
    /// multiplex framing.
    pub fn set_batch_recorder(&mut self, recorder: Arc<Mutex<dyn Write + Send>>) -> io::Result<()> {
        match self {
            Self::Multiplex(mux) => {
                mux.batch_recorder = Some(recorder);
                Ok(())
            }
            Self::Compressed(compressed) => {
                // upstream: io.c:write_buf() tees at the write_buf level, which
                // is the MultiplexWriter's input (compressed data). Attach to the
                // inner MultiplexWriter to capture compressed wire bytes.
                compressed.inner_mut().batch_recorder = Some(recorder);
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "batch recorder requires multiplex mode",
            )),
        }
    }

    /// Finalises an active compression stream so the inner codec emits any
    /// trailing bytes (zlib Z_FINISH end-of-stream marker, lz4 frame footer,
    /// zstd end-of-frame epilogue) before the connection is closed.
    ///
    /// In Plain or Multiplex mode this is a plain [`flush`](Self::flush). In
    /// Compressed mode it consumes the [`CompressedWriter`], calls its
    /// `finish()` to emit the end-of-stream trailer, and downgrades the
    /// `ServerWriter` back to [`Self::Multiplex`]. After this the underlying
    /// multiplex stream is fully drained and any subsequent writes go through
    /// the plain multiplex path.
    ///
    /// upstream: `token.c:send_deflated_token()` issues `Z_FINISH` via
    /// `deflateEnd()` when the sender tears down its compression context at
    /// end of transfer. Without this finalisation, the receiver's
    /// [`CompressedReader`](crate::compressed_reader::CompressedReader) may be
    /// waiting on the closing block of the deflate stream while the daemon
    /// has already shut down the socket, producing the "connection
    /// unexpectedly closed (N bytes received so far)" error mid-goodbye.
    ///
    /// # Errors
    ///
    /// Returns an error if the codec or the underlying writer report one
    /// while emitting trailer bytes. Early-close errors are surfaced to the
    /// caller; callers that tolerate dry-run / daemon-killed scenarios should
    /// inspect them via `is_early_close_error`.
    pub fn finalize_compression(&mut self) -> io::Result<()> {
        let old_self = std::mem::replace(self, Self::Taken);

        match old_self {
            Self::Compressed(compressed) => match compressed.finish() {
                Ok(mut mux) => {
                    mux.flush()?;
                    *self = Self::Multiplex(mux);
                    Ok(())
                }
                Err(e) => {
                    *self = Self::Taken;
                    Err(e)
                }
            },
            other => {
                *self = other;
                self.flush()
            }
        }
    }

    /// Drains every user-space buffer in the writer graph so the next byte
    /// any caller emits goes straight to the kernel.
    ///
    /// Idempotent. Calls [`finalize_compression`](Self::finalize_compression)
    /// to emit the codec end-of-stream trailer (zlib `Z_FINISH`, lz4 footer,
    /// zstd epilogue), which itself flushes the multiplex `BufWriter`. In
    /// Multiplex / Plain mode it degrades to a plain [`flush`](Self::flush).
    ///
    /// Peer-already-closed errors are downgraded to `Ok(())` because the
    /// transfer is finished by the time this barrier runs - the peer has
    /// already FINed in the early-close case and there is no useful byte
    /// left to deliver. Every other I/O error surfaces (Rule 12: fail loud).
    ///
    /// # UTS-V3.A drain-barrier contract
    ///
    /// The audit traced the cluster-A wire-cutoffs (~2.25 MB on batch-mode,
    /// alt-dest, and `daemon-refuse-compress`; ~615 KB on
    /// `daemon-gzip-download`) to user-space bytes still sitting in the
    /// multiplex `BufWriter` / codec trailer when the daemon's
    /// `SO_LINGER` + `shutdown(SHUT_WR)` teardown fired. Driving
    /// `flush_all_pending` after `handle_goodbye_with_finalizer` returns
    /// makes the drain observable and error-surfacing instead of relying
    /// on the implicit `Drop` + `SO_LINGER` hand-off.
    ///
    /// # Upstream Reference
    ///
    /// - `cleanup.c::handle_cleanup()` brackets the sender's final
    ///   `io_flush(FULL_FLUSH)` with the process exit so every user-space
    ///   byte hits the wire before the kernel queues `FIN`.
    /// - `main.c:983` calls `io_flush(FULL_FLUSH)` after
    ///   `read_final_goodbye()` returns, mirroring the same barrier intent.
    ///
    /// # Errors
    ///
    /// Returns an error if a non-early-close I/O error fires during codec
    /// finalisation or multiplex flush.
    pub fn flush_all_pending(&mut self) -> io::Result<()> {
        match self.finalize_compression() {
            Ok(()) => Ok(()),
            Err(e) if is_early_close_error(&e) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Writes raw bytes directly to the underlying stream, bypassing multiplexing.
    ///
    /// Used for protocol exchanges like the final goodbye handshake where
    /// upstream rsync's `write_ndx()` writes directly without `MSG_DATA` framing.
    /// Flushes any buffered multiplexed data before writing raw bytes
    /// to maintain proper message ordering.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying I/O operation fails.
    pub fn write_raw(&mut self, data: &[u8]) -> io::Result<()> {
        self.flush()?;

        match self {
            Self::Plain(w) => {
                w.write_all(data)?;
                w.flush()
            }
            Self::Multiplex(mux) => mux.write_raw(data),
            Self::Compressed(compressed) => compressed.inner_mut().write_raw(data),
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
        }
    }
}

impl<W: Write> Write for ServerWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Multiplex(w) => w.write(buf),
            Self::Compressed(w) => w.write(buf),
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
        }
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write_vectored(bufs),
            Self::Multiplex(w) => w.write_vectored(bufs),
            Self::Compressed(w) => w.write_vectored(bufs),
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Multiplex(w) => w.flush(),
            Self::Compressed(w) => w.flush(),
            Self::Taken => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ServerWriter in invalid Taken state",
            )),
        }
    }
}

/// Half-closes the send side of an accepted daemon connection after every
/// in-flight TX byte has been ACKed.
///
/// Companion to [`ServerWriter::flush_all_pending`]: that drains the
/// user-space writer graph; this drains the kernel send buffer. Together
/// they replace the implicit `Drop` + `SO_LINGER` hand-off with an
/// observable, timeout-bounded, error-surfacing two-stage barrier.
///
/// Sequence:
/// 1. `SO_LINGER(timeout)` so `shutdown(SHUT_WR)` blocks until the queued
///    TX bytes are ACKed (or the linger window expires). This is the
///    catastrophic-failure fallback: even if the peer never reads, we
///    refuse to wait beyond `timeout` before queuing `FIN`.
/// 2. `shutdown(SHUT_WR)` issues the explicit half-close so the peer
///    observes `FIN` exactly once `SO_LINGER` has drained the kernel
///    queue. Any `NotConnected` / `BrokenPipe` is downgraded to `Ok(())`
///    because that means the peer already closed.
///
/// Use only after the application-level write half is fully drained
/// (e.g. [`ServerWriter::flush_all_pending`] returned `Ok(())`) AND the
/// daemon-side read-drain loop has consumed the peer's trailing goodbye.
/// Calling earlier risks the same race the audit fixed.
///
/// # Upstream Reference
///
/// - `io.c:943-963 noop_io_until_death()` keeps the sender's read side
///   open until the peer FINs.
/// - `cleanup.c:265 close_all()` finally drops the fds; the kernel
///   queues `FIN` on the write side as a side effect of the exit.
///
/// # Errors
///
/// Returns the underlying `io::Error` from `shutdown` when it is not a
/// peer-already-closed condition. The `SO_LINGER` set failure is
/// surfaced because it would silently defeat the drain guarantee.
pub fn shutdown_send_side(stream: &TcpStream, timeout: Duration) -> io::Result<()> {
    // SO_LINGER bounds the kernel-level drain. If the peer is wedged,
    // shutdown(SHUT_WR) will not block past `timeout`. Even if SO_LINGER
    // is unsupported on the underlying socket, surfacing the failure is
    // safer than silently degrading: callers can re-fall-back to the
    // legacy SO_LINGER-then-drop pattern higher up the stack.
    let sock = socket2::SockRef::from(stream);
    sock.set_linger(Some(timeout))?;

    match stream.shutdown(Shutdown::Write) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotConnected | io::ErrorKind::BrokenPipe
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}
