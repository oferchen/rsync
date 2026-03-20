//! Core types and constants for ACK batching.

use std::io::{self, Write};

/// Default number of ACKs to batch before flushing.
pub const DEFAULT_BATCH_SIZE: usize = 16;

/// Default time threshold before flushing (50ms).
pub const DEFAULT_BATCH_TIMEOUT_MS: u64 = 50;

/// Minimum batch size (must be at least 1).
pub const MIN_BATCH_SIZE: usize = 1;

/// Maximum batch size to prevent memory bloat.
pub const MAX_BATCH_SIZE: usize = 256;

/// Maximum batch timeout (1 second).
pub const MAX_BATCH_TIMEOUT_MS: u64 = 1000;

/// Status codes for individual ACK entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AckStatus {
    /// File transfer completed successfully.
    Success = 0,
    /// File transfer failed with an error.
    Error = 1,
    /// File was skipped (already up to date).
    Skipped = 2,
    /// File checksum verification failed.
    ChecksumError = 3,
    /// File I/O error during transfer.
    IoError = 4,
}

impl AckStatus {
    /// Returns true if this status represents an error condition.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(
            self,
            AckStatus::Error | AckStatus::ChecksumError | AckStatus::IoError
        )
    }

    /// Converts a `u8` to an `AckStatus`.
    ///
    /// Returns `AckStatus::Error` for unknown values.
    #[must_use]
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0 => AckStatus::Success,
            1 => AckStatus::Error,
            2 => AckStatus::Skipped,
            3 => AckStatus::ChecksumError,
            4 => AckStatus::IoError,
            _ => AckStatus::Error,
        }
    }
}

/// A single acknowledgment entry in a batch.
#[derive(Debug, Clone)]
pub struct AckEntry {
    /// File index (NDX) being acknowledged.
    pub ndx: i32,
    /// Status of the transfer.
    pub status: AckStatus,
    /// Optional error message for error statuses.
    pub error_msg: Option<String>,
}

impl AckEntry {
    /// Creates a new successful ACK entry.
    #[must_use]
    pub const fn success(ndx: i32) -> Self {
        Self {
            ndx,
            status: AckStatus::Success,
            error_msg: None,
        }
    }

    /// Creates a new skipped ACK entry.
    #[must_use]
    pub const fn skipped(ndx: i32) -> Self {
        Self {
            ndx,
            status: AckStatus::Skipped,
            error_msg: None,
        }
    }

    /// Creates a new error ACK entry.
    #[must_use]
    pub fn error(ndx: i32, msg: impl Into<String>) -> Self {
        Self {
            ndx,
            status: AckStatus::Error,
            error_msg: Some(msg.into()),
        }
    }

    /// Creates a new checksum error ACK entry.
    #[must_use]
    pub fn checksum_error(ndx: i32, msg: impl Into<String>) -> Self {
        Self {
            ndx,
            status: AckStatus::ChecksumError,
            error_msg: Some(msg.into()),
        }
    }

    /// Creates a new I/O error ACK entry.
    #[must_use]
    pub fn io_error(ndx: i32, msg: impl Into<String>) -> Self {
        Self {
            ndx,
            status: AckStatus::IoError,
            error_msg: Some(msg.into()),
        }
    }

    /// Writes this ACK entry to the wire.
    ///
    /// Wire format:
    /// - ndx: 4 bytes (i32 LE)
    /// - status: 1 byte
    /// - if error: error_len (u16 LE) + error_msg bytes
    pub fn write<W: Write + ?Sized>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.ndx.to_le_bytes())?;
        writer.write_all(&[self.status as u8])?;

        if let Some(ref msg) = self.error_msg {
            let msg_bytes = msg.as_bytes();
            let len = msg_bytes.len().min(u16::MAX as usize) as u16;
            writer.write_all(&len.to_le_bytes())?;
            writer.write_all(&msg_bytes[..len as usize])?;
        }

        Ok(())
    }

    /// Reads an ACK entry from the wire.
    pub fn read<R: io::Read + ?Sized>(reader: &mut R) -> io::Result<Self> {
        let mut ndx_buf = [0u8; 4];
        reader.read_exact(&mut ndx_buf)?;
        let ndx = i32::from_le_bytes(ndx_buf);

        let mut status_buf = [0u8; 1];
        reader.read_exact(&mut status_buf)?;
        let status = AckStatus::from_u8(status_buf[0]);

        let error_msg = if status.is_error() {
            let mut len_buf = [0u8; 2];
            reader.read_exact(&mut len_buf)?;
            let len = u16::from_le_bytes(len_buf) as usize;

            if len > 0 {
                let mut msg_buf = vec![0u8; len];
                reader.read_exact(&mut msg_buf)?;
                Some(String::from_utf8_lossy(&msg_buf).into_owned())
            } else {
                None
            }
        } else {
            None
        };

        Ok(Self {
            ndx,
            status,
            error_msg,
        })
    }
}

/// Configuration for the ACK batcher.
#[derive(Debug, Clone)]
pub struct AckBatcherConfig {
    /// Maximum number of ACKs to batch before flushing.
    pub batch_size: usize,
    /// Maximum time to wait before flushing (in milliseconds).
    pub batch_timeout_ms: u64,
    /// Whether batching is enabled (disabled for legacy protocol compatibility).
    pub enabled: bool,
}

impl Default for AckBatcherConfig {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            batch_timeout_ms: DEFAULT_BATCH_TIMEOUT_MS,
            enabled: true,
        }
    }
}

impl AckBatcherConfig {
    /// Creates a new configuration with the specified batch size.
    #[must_use]
    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.clamp(MIN_BATCH_SIZE, MAX_BATCH_SIZE);
        self
    }

    /// Creates a new configuration with the specified timeout.
    #[must_use]
    pub fn with_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.batch_timeout_ms = timeout_ms.min(MAX_BATCH_TIMEOUT_MS);
        self
    }

    /// Disables batching (for legacy protocol compatibility).
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            batch_size: 1,
            batch_timeout_ms: 0,
            enabled: false,
        }
    }

    /// Returns true if batching is enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Statistics about ACK batching performance.
#[derive(Debug, Clone, Default)]
pub struct AckBatcherStats {
    /// Total number of individual ACKs sent.
    pub total_sent: u64,
    /// Total number of batches sent.
    pub batches_sent: u64,
    /// Number of ACKs currently pending.
    pub currently_pending: usize,
    /// Average number of ACKs per batch.
    pub average_batch_size: f64,
}

impl AckBatcherStats {
    /// Returns the efficiency gain from batching.
    ///
    /// This is the ratio of individual ACKs that would have been sent
    /// to the number of batches actually sent. A value of 8.0 means
    /// batching reduced network messages by 8x.
    #[must_use]
    pub fn efficiency_ratio(&self) -> f64 {
        if self.batches_sent == 0 {
            return 1.0;
        }
        self.total_sent as f64 / self.batches_sent as f64
    }
}
