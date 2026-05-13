//! Double-buffered reader that overlaps I/O with computation.
//!
//! Spawns a background thread to read the next block while the main thread
//! processes the current block, achieving I/O-computation overlap with exactly
//! two pre-allocated buffers.
//!
//! # Strict Two-Buffer Invariant
//!
//! The reader pre-allocates exactly two buffers and swaps their roles between
//! I/O and computation on every iteration. Bounded channels enforce that at
//! most one block is in-flight, so the total memory footprint is always
//! `2 * block_size` regardless of I/O speed or computation latency.
//!
//! ```text
//! ┌─────────────┐   sync_channel(1): Block(buf)   ┌───────────────┐
//! │  I/O Thread  │ ───────────────────────────────► │  Main Thread   │
//! │  (reader)    │                                  │  (checksums)   │
//! │              │ ◄─────────────────────────────── │                │
//! └─────────────┘   recycle channel: buf            └───────────────┘
//! ```
//!
//! Timeline with two buffers A and B:
//!
//! 1. Constructor reads block 0 into buffer A (synchronous).
//! 2. I/O thread receives buffer B via the recycle channel and reads block 1.
//! 3. Main thread calls `next_block()`:
//!    - Returns buffer A (block 0) for checksum computation.
//!    - Receives buffer B (block 1) via the bounded data channel.
//! 4. Main thread calls `next_block()` again:
//!    - Recycles buffer A back to the I/O thread.
//!    - Returns buffer B (block 1) for checksum computation.
//!    - I/O thread receives buffer A and reads block 2.
//! 5. The two buffers keep swapping roles until EOF.

use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};

use super::config::PipelineConfig;

/// Message sent from the I/O thread to the main thread.
enum IoMessage {
    /// A block of data was successfully read.
    Block(Vec<u8>),
    /// End of input reached.
    Eof,
    /// An I/O error occurred.
    Error(io::Error),
}

/// Double-buffered reader for pipelined checksum computation.
///
/// Uses a background thread to read the next block while the main thread
/// processes the current block. Exactly two pre-allocated buffers are swapped
/// between the I/O and main threads via bounded channels, enforcing constant
/// memory usage of `2 * block_size`.
///
/// # Thread Safety
///
/// The reader spawns a background thread for I/O. The thread is automatically
/// joined when the reader is dropped or when EOF/error is encountered.
pub struct DoubleBufferedReader<R> {
    config: PipelineConfig,
    receiver: Option<Receiver<IoMessage>>,
    /// Channel for returning consumed buffers to the I/O thread.
    recycle_sender: Option<Sender<Vec<u8>>>,
    io_thread: Option<JoinHandle<()>>,
    current_block: Option<Vec<u8>>,
    prefetched_block: Option<Vec<u8>>,
    eof_reached: bool,
    direct_reader: Option<R>,
    synchronous: bool,
    /// Reusable buffer for synchronous mode to avoid per-block allocation.
    sync_buffer: Option<Vec<u8>>,
}

impl<R: Read + Send + 'static> DoubleBufferedReader<R> {
    /// Creates a new double-buffered reader.
    ///
    /// If the file is smaller than `config.min_file_size` or pipelining is
    /// disabled, the reader operates in synchronous mode without spawning
    /// a background thread.
    #[must_use]
    pub fn new(reader: R, config: PipelineConfig) -> Self {
        Self::with_size_hint(reader, config, None)
    }

    /// Creates a new double-buffered reader with a size hint.
    ///
    /// The size hint decides whether to enable pipelining. If the size is
    /// smaller than `config.min_file_size`, synchronous mode is used.
    ///
    /// In pipelined mode, two buffers are pre-allocated. Buffer A is filled
    /// synchronously with the first block. Buffer B is sent to the I/O thread
    /// via the recycle channel so it can begin reading block 1 immediately.
    #[must_use]
    pub fn with_size_hint(mut reader: R, config: PipelineConfig, size_hint: Option<u64>) -> Self {
        let should_pipeline =
            config.enabled && size_hint.is_none_or(|size| size >= config.min_file_size);

        if !should_pipeline {
            let sync_buf = vec![0u8; config.block_size];
            return Self {
                config,
                receiver: None,
                recycle_sender: None,
                io_thread: None,
                current_block: None,
                prefetched_block: None,
                eof_reached: false,
                direct_reader: Some(reader),
                synchronous: true,
                sync_buffer: Some(sync_buf),
            };
        }

        // Bounded data channel: at most one block in-flight from I/O to main.
        let (sender, receiver) = mpsc::sync_channel(1);
        let (recycle_tx, recycle_rx) = mpsc::channel();
        let block_size = config.block_size;

        // Pre-allocate buffer A and read the first block synchronously so
        // the first `next_block()` call returns immediately.
        let mut buf_a = vec![0u8; block_size];
        let first_read = match read_exact_or_eof(&mut reader, &mut buf_a) {
            Ok(0) => {
                return Self {
                    config,
                    receiver: None,
                    recycle_sender: None,
                    io_thread: None,
                    current_block: None,
                    prefetched_block: None,
                    eof_reached: true,
                    direct_reader: None,
                    synchronous: false,
                    sync_buffer: None,
                };
            }
            Ok(n) => {
                buf_a.truncate(n);
                Some(buf_a)
            }
            Err(_) => {
                return Self {
                    config,
                    receiver: None,
                    recycle_sender: None,
                    io_thread: None,
                    current_block: None,
                    prefetched_block: None,
                    eof_reached: true,
                    direct_reader: Some(reader),
                    synchronous: true,
                    sync_buffer: None,
                };
            }
        };

        // Pre-allocate buffer B and seed the recycle channel so the I/O thread
        // can begin reading immediately without waiting for a recycled buffer.
        let buf_b = vec![0u8; block_size];
        let _ = recycle_tx.send(buf_b);

        let io_thread = thread::spawn(move || {
            io_thread_main(reader, sender, recycle_rx, block_size);
        });

        Self {
            config,
            receiver: Some(receiver),
            recycle_sender: Some(recycle_tx),
            io_thread: Some(io_thread),
            current_block: first_read,
            prefetched_block: None,
            eof_reached: false,
            direct_reader: None,
            synchronous: false,
            sync_buffer: None,
        }
    }

    /// Returns the next block of data, or `None` if EOF reached.
    ///
    /// Returns data that was pre-read while the previous block was being
    /// processed, then initiates reading the next block. The previously
    /// returned buffer is recycled back to the I/O thread for reuse.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying reader encounters an I/O error.
    #[must_use]
    pub fn next_block(&mut self) -> io::Result<Option<&[u8]>> {
        if self.eof_reached {
            return Ok(None);
        }

        if self.synchronous {
            return self.next_block_sync();
        }

        // Recycle the previously consumed buffer back to the I/O thread.
        if let Some(old_buf) = self.prefetched_block.take() {
            self.recycle_buffer(old_buf);
        }

        let current = self.current_block.take();

        if current.is_none() {
            self.eof_reached = true;
            return Ok(None);
        }

        if let Some(ref receiver) = self.receiver {
            match receiver.recv() {
                Ok(IoMessage::Block(data)) => {
                    self.current_block = Some(data);
                }
                Ok(IoMessage::Eof) => {
                    self.eof_reached = true;
                    self.current_block = None;
                }
                Ok(IoMessage::Error(e)) => {
                    self.eof_reached = true;
                    return Err(e);
                }
                Err(_) => {
                    self.eof_reached = true;
                    self.current_block = None;
                }
            }
        }

        self.prefetched_block = current;
        Ok(self.prefetched_block.as_deref())
    }

    /// Synchronous block reading for small files.
    ///
    /// Reuses a single pre-allocated buffer across calls, avoiding per-block
    /// heap allocation. The buffer cycles through `sync_buffer` (idle) ->
    /// `current_block` (in use by caller) -> `sync_buffer` (reclaimed on
    /// next call).
    fn next_block_sync(&mut self) -> io::Result<Option<&[u8]>> {
        if let Some(ref mut reader) = self.direct_reader {
            // Reclaim the buffer from the previous call if it was not already
            // returned to sync_buffer (it lives in current_block while the
            // caller holds a reference).
            let mut buffer = self
                .current_block
                .take()
                .or_else(|| self.sync_buffer.take())
                .unwrap_or_else(|| vec![0u8; self.config.block_size]);

            // Restore full capacity for reading. The buffer may have been
            // truncated for a short final block on a previous call.
            let block_size = self.config.block_size;
            if buffer.len() < block_size {
                buffer.resize(block_size, 0);
            }

            let bytes_read = read_exact_or_eof(reader, &mut buffer[..block_size])?;

            if bytes_read == 0 {
                self.eof_reached = true;
                self.sync_buffer = Some(buffer);
                return Ok(None);
            }

            buffer.truncate(bytes_read);
            self.current_block = Some(buffer);
            Ok(self.current_block.as_deref())
        } else {
            self.eof_reached = true;
            Ok(None)
        }
    }

    /// Sends a consumed buffer back to the I/O thread for reuse.
    ///
    /// If the recycle channel is disconnected (I/O thread has exited), the
    /// buffer is silently dropped.
    fn recycle_buffer(&self, buffer: Vec<u8>) {
        if let Some(ref tx) = self.recycle_sender {
            // Ignore send errors - the I/O thread may have already exited.
            let _ = tx.send(buffer);
        }
    }

    /// Returns true if pipelining is active (background thread running).
    #[must_use]
    pub fn is_pipelined(&self) -> bool {
        !self.synchronous && self.io_thread.is_some()
    }

    /// Returns the configured block size.
    #[must_use]
    pub fn block_size(&self) -> usize {
        self.config.block_size
    }
}

impl<R> Drop for DoubleBufferedReader<R> {
    fn drop(&mut self) {
        // Drop channels first to signal I/O thread to stop.
        drop(self.receiver.take());
        drop(self.recycle_sender.take());

        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Main loop for the background I/O thread.
///
/// Waits for a recycled buffer from the main thread before each read. The
/// constructor seeds the recycle channel with one pre-allocated buffer, so
/// the first iteration never blocks. Subsequent iterations block until the
/// main thread returns a consumed buffer, enforcing the two-buffer invariant.
fn io_thread_main<R: Read>(
    mut reader: R,
    sender: SyncSender<IoMessage>,
    recycle_rx: Receiver<Vec<u8>>,
    block_size: usize,
) {
    // Wait for a recycled buffer on each iteration. The constructor pre-seeds
    // the channel with one buffer, so the first recv() returns immediately.
    // After that, each recv() blocks until the main thread recycles a buffer
    // via `recycle_buffer()`. This guarantees exactly two buffers exist.
    while let Ok(mut buffer) = recycle_rx.recv() {
        // Restore full capacity for reading. The main thread may have
        // truncated the buffer for a short final block.
        if buffer.len() < block_size {
            buffer.resize(block_size, 0);
        }

        match read_exact_or_eof(&mut reader, &mut buffer) {
            Ok(0) => {
                let _ = sender.send(IoMessage::Eof);
                break;
            }
            Ok(n) => {
                buffer.truncate(n);
                if sender.send(IoMessage::Block(buffer)).is_err() {
                    break;
                }
            }
            Err(e) => {
                let _ = sender.send(IoMessage::Error(e));
                break;
            }
        }
    }
}

/// Reads up to `buffer.len()` bytes, handling partial reads and EOF gracefully.
///
/// Unlike `read_exact`, returns the number of bytes read instead of erroring on EOF.
fn read_exact_or_eof<R: Read>(reader: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    let mut total_read = 0;

    while total_read < buffer.len() {
        match reader.read(&mut buffer[total_read..]) {
            Ok(0) => break,
            Ok(n) => total_read += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    Ok(total_read)
}
