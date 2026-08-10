#![deny(unsafe_code)]

/// Maximum chunk size for literal data (matches upstream CHUNK_SIZE).
pub const CHUNK_SIZE: usize = 32 * 1024;

/// Delta operation for file reconstruction.
///
/// Represents the internal format for delta operations (not the upstream wire format).
/// This opcode-based format is used for backward compatibility with earlier versions
/// of this implementation.
///
/// For upstream rsync compatibility, use the token-based functions like
/// [`super::write_token_stream`] and [`super::read_token`] instead.
///
/// # Examples
///
/// ```
/// use protocol::wire::DeltaOp;
///
/// let lit = DeltaOp::Literal(vec![1, 2, 3, 4, 5]);
///
/// let copy = DeltaOp::Copy {
///     block_index: 0,
///     length: 4096,
/// };
/// ```
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DeltaOp {
    /// Write literal bytes to output.
    ///
    /// The contained data should be written directly to the output stream
    /// at the current position.
    Literal(Vec<u8>),

    /// Copy bytes from basis file at given block index.
    ///
    /// The receiver should copy `length` bytes from the basis file starting
    /// at the position indicated by `block_index * block_size`, where
    /// `block_size` comes from the signature header.
    Copy {
        /// Block index in basis file (0-based).
        block_index: u32,
        /// Number of bytes to copy.
        length: u32,
    },
}
