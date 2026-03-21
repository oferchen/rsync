//! Double-buffered reader that overlaps I/O with computation.
//!
//! Spawns a background thread to read the next block while the main thread
//! processes the current block, achieving I/O-computation overlap.

use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender};
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
/// processes the current block. This overlaps I/O with computation.
///
/// # Thread Safety
///
/// The reader spawns a background thread for I/O. The thread is automatically
/// joined when the reader is dropped or when EOF/error is encountered.
pub struct DoubleBufferedReader<R> {
    config: PipelineConfig,
    receiver: Option<Receiver<IoMessage>>,
    io_thread: Option<JoinHandle<()>>,
    current_block: Option<Vec<u8>>,
    prefetched_block: Option<Vec<u8>>,
    eof_reached: bool,
    direct_reader: Option<R>,
    synchronous: bool,
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
    #[must_use]
    pub fn with_size_hint(mut reader: R, config: PipelineConfig, size_hint: Option<u64>) -> Self {
        let should_pipeline =
            config.enabled && size_hint.is_none_or(|size| size >= config.min_file_size);

        if !should_pipeline {
            return Self {
                config,
                receiver: None,
                io_thread: None,
                current_block: None,
                prefetched_block: None,
                eof_reached: false,
                direct_reader: Some(reader),
                synchronous: true,
            };
        }

        let (sender, receiver) = mpsc::channel();
        let block_size = config.block_size;

        // Read first block synchronously to have it ready immediately
        let mut first_block = vec![0u8; block_size];
        let first_read = match read_exact_or_eof(&mut reader, &mut first_block) {
            Ok(0) => {
                return Self {
                    config,
                    receiver: None,
                    io_thread: None,
                    current_block: None,
                    prefetched_block: None,
                    eof_reached: true,
                    direct_reader: None,
                    synchronous: false,
                };
            }
            Ok(n) => {
                first_block.truncate(n);
                Some(first_block)
            }
            Err(_) => {
                return Self {
                    config,
                    receiver: None,
                    io_thread: None,
                    current_block: None,
                    prefetched_block: None,
                    eof_reached: true,
                    direct_reader: Some(reader),
                    synchronous: true,
                };
            }
        };

        let io_thread = thread::spawn(move || {
            io_thread_main(reader, sender, block_size);
        });

        Self {
            config,
            receiver: Some(receiver),
            io_thread: Some(io_thread),
            current_block: first_read,
            prefetched_block: None,
            eof_reached: false,
            direct_reader: None,
            synchronous: false,
        }
    }

    /// Returns the next block of data, or `None` if EOF reached.
    ///
    /// Returns data that was pre-read while the previous block was being
    /// processed, then initiates reading the next block.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying reader encounters an I/O error.
    pub fn next_block(&mut self) -> io::Result<Option<&[u8]>> {
        if self.eof_reached {
            return Ok(None);
        }

        if self.synchronous {
            return self.next_block_sync();
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
    fn next_block_sync(&mut self) -> io::Result<Option<&[u8]>> {
        if let Some(ref mut reader) = self.direct_reader {
            let mut buffer = vec![0u8; self.config.block_size];
            let bytes_read = read_exact_or_eof(reader, &mut buffer)?;

            if bytes_read == 0 {
                self.eof_reached = true;
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
        // Drop receiver first to signal I/O thread to stop
        drop(self.receiver.take());

        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Main loop for the background I/O thread.
fn io_thread_main<R: Read>(mut reader: R, sender: Sender<IoMessage>, block_size: usize) {
    loop {
        let mut buffer = vec![0u8; block_size];

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
