//! Token reader abstraction for delta transfer.
//!
//! Switches between plain 4-byte LE token format and compressed token
//! format based on negotiated compression. This mirrors upstream rsync's
//! `recv_token()` (token.c:271) which dispatches to `simple_recv_token()`
//! or `recv_deflated_token()` depending on whether compression is active.
//!
//! # Wire Formats
//!
//! ## Plain Token Format (no compression)
//!
//! Each token is a 4-byte little-endian i32:
//! - `token > 0`: literal data of `token` bytes follows
//! - `token < 0`: block reference at index `-(token + 1)`
//! - `token == 0`: end of file marker
//!
//! ## Compressed Token Format (`-z` flag)
//!
//! Tokens use flag-byte framing with DEFLATED_DATA headers for compressed
//! literal data. See [`protocol::wire::compressed_token`] for wire format details.
//!
//! # Upstream Reference
//!
//! - `token.c:271` - `recv_token()` dispatch
//! - `token.c:284` - `simple_recv_token()` plain format
//! - `token.c:500` - `recv_deflated_token()` compressed format

use std::io::{self, Read};

use protocol::wire::{CompressedToken, CompressedTokenDecoder};
use protocol::CompressionAlgorithm;

/// Result of reading a single token from the delta stream.
///
/// Unifies the plain and compressed token formats into a common
/// representation for the receiver pipeline.
pub enum DeltaToken {
    /// Literal data to write to the output file.
    Literal(LiteralData),
    /// Copy a block from the basis file at the given index.
    BlockRef(usize),
    /// End of file marker — no more tokens follow.
    End,
}

/// Literal data from a delta token.
///
/// For plain tokens, the receiver reads the data itself after getting the
/// length. For compressed tokens, the decoder returns decompressed data
/// directly. This enum lets the receiver handle both without allocation
/// overhead on the plain path.
pub enum LiteralData {
    /// Plain token: the receiver must read `len` bytes from the stream.
    /// The data has NOT been read yet.
    Pending(usize),
    /// Compressed token: decompressed literal data already available.
    Ready(Vec<u8>),
}

/// Reads delta tokens from the wire in either plain or compressed format.
///
/// Implements the Strategy pattern: the concrete reading strategy is selected
/// once based on negotiated compression, then used for all tokens in the
/// transfer. This mirrors upstream rsync's `recv_token()` function pointer
/// dispatch in `token.c`.
///
/// # Lifetime
///
/// A `TokenReader` is created per-file (reset between files) because the
/// compressed token decoder maintains per-file inflate state that must be
/// reset for each new file transfer.
pub enum TokenReader {
    /// Plain 4-byte LE token format (no compression).
    Plain,
    /// Compressed token format using DEFLATED_DATA headers.
    Compressed(CompressedTokenDecoder),
}

impl TokenReader {
    /// Creates a token reader based on the negotiated compression algorithm.
    ///
    /// Returns `Plain` when compression is `None` or an algorithm that does
    /// not use token-level compression. Returns `Compressed` for `Zlib` and
    /// `ZlibX` which use the compressed token wire format.
    ///
    /// # Arguments
    ///
    /// * `compression` - The negotiated compression algorithm, if any
    #[must_use]
    pub fn new(compression: Option<CompressionAlgorithm>) -> Self {
        match compression {
            Some(CompressionAlgorithm::Zlib | CompressionAlgorithm::ZlibX) => {
                Self::Compressed(CompressedTokenDecoder::new())
            }
            _ => Self::Plain,
        }
    }

    /// Returns true if this reader uses compressed token format.
    #[must_use]
    pub const fn is_compressed(&self) -> bool {
        matches!(self, Self::Compressed(_))
    }

    /// Reads the next token from the stream.
    ///
    /// For plain mode, reads a 4-byte i32 and returns the token type.
    /// Literal tokens return `LiteralData::Pending` — the caller must
    /// read the data bytes from the stream.
    ///
    /// For compressed mode, delegates to `CompressedTokenDecoder::recv_token()`
    /// which returns fully decompressed literal data in `LiteralData::Ready`.
    ///
    /// # Errors
    ///
    /// Returns an error if reading from the stream fails or if the
    /// compressed token stream contains invalid data.
    pub fn read_token<R: Read>(&mut self, reader: &mut R) -> io::Result<DeltaToken> {
        match self {
            Self::Plain => {
                let mut buf = [0u8; 4];
                reader.read_exact(&mut buf)?;
                let token = i32::from_le_bytes(buf);

                match token.cmp(&0) {
                    std::cmp::Ordering::Equal => Ok(DeltaToken::End),
                    std::cmp::Ordering::Greater => {
                        Ok(DeltaToken::Literal(LiteralData::Pending(token as usize)))
                    }
                    std::cmp::Ordering::Less => Ok(DeltaToken::BlockRef(-(token + 1) as usize)),
                }
            }
            Self::Compressed(decoder) => match decoder.recv_token(reader)? {
                CompressedToken::Literal(data) => Ok(DeltaToken::Literal(LiteralData::Ready(data))),
                CompressedToken::BlockMatch(idx) => Ok(DeltaToken::BlockRef(idx as usize)),
                CompressedToken::End => Ok(DeltaToken::End),
            },
        }
    }

    /// Feeds block data into the decompressor's dictionary after a block match.
    ///
    /// Only needed for compressed mode — keeps the decompressor's dictionary
    /// synchronized with the sender's compressor. In plain mode this is a no-op.
    ///
    /// Must be called after processing each `BlockRef` token with the actual
    /// block data copied from the basis file.
    ///
    /// # Upstream Reference
    ///
    /// - `token.c:631` - `see_deflate_token()` called after each block match
    ///
    /// # Errors
    ///
    /// Returns an error if the decompression dictionary update fails.
    pub fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        match self {
            Self::Plain => Ok(()),
            Self::Compressed(decoder) => decoder.see_token(data),
        }
    }

    /// Resets the reader state for a new file.
    ///
    /// In compressed mode, resets the inflate context and all internal
    /// buffers. In plain mode this is a no-op.
    pub fn reset(&mut self) {
        match self {
            Self::Plain => {}
            Self::Compressed(decoder) => decoder.reset(),
        }
    }
}

impl std::fmt::Debug for DeltaToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Literal(LiteralData::Pending(len)) => {
                write!(f, "Literal(Pending({len}))")
            }
            Self::Literal(LiteralData::Ready(data)) => {
                write!(f, "Literal(Ready({} bytes))", data.len())
            }
            Self::BlockRef(idx) => write!(f, "BlockRef({idx})"),
            Self::End => write!(f, "End"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::wire::CompressedTokenEncoder;
    use std::io::Cursor;

    #[test]
    fn plain_reader_literal_token() {
        let mut reader = TokenReader::new(None);
        // token = 5 means 5 bytes of literal data follow
        let data = 5_i32.to_le_bytes();
        let mut cursor = Cursor::new(data.to_vec());

        match reader.read_token(&mut cursor).unwrap() {
            DeltaToken::Literal(LiteralData::Pending(len)) => assert_eq!(len, 5),
            other => panic!("expected Literal(Pending(5)), got {other:?}"),
        }
    }

    #[test]
    fn plain_reader_block_ref_token() {
        let mut reader = TokenReader::new(None);
        // token = -1 means block ref at index 0 (-((-1)+1) = 0)
        let data = (-1_i32).to_le_bytes();
        let mut cursor = Cursor::new(data.to_vec());

        match reader.read_token(&mut cursor).unwrap() {
            DeltaToken::BlockRef(idx) => assert_eq!(idx, 0),
            other => panic!("expected BlockRef(0), got {other:?}"),
        }
    }

    #[test]
    fn plain_reader_end_token() {
        let mut reader = TokenReader::new(None);
        let data = 0_i32.to_le_bytes();
        let mut cursor = Cursor::new(data.to_vec());

        match reader.read_token(&mut cursor).unwrap() {
            DeltaToken::End => {}
            other => panic!("expected End, got {other:?}"),
        }
    }

    #[test]
    fn plain_reader_block_ref_index_mapping() {
        let mut reader = TokenReader::new(None);
        // token = -6 means block index 5 (-((-6)+1) = 5)
        let data = (-6_i32).to_le_bytes();
        let mut cursor = Cursor::new(data.to_vec());

        match reader.read_token(&mut cursor).unwrap() {
            DeltaToken::BlockRef(idx) => assert_eq!(idx, 5),
            other => panic!("expected BlockRef(5), got {other:?}"),
        }
    }

    #[test]
    fn compressed_reader_literal_roundtrip() {
        // Encode with CompressedTokenEncoder
        let literal_data = b"Hello, compressed tokens!";
        let mut encoded = Vec::new();
        let mut encoder = CompressedTokenEncoder::default();
        encoder.send_literal(&mut encoded, literal_data).unwrap();
        encoder.finish(&mut encoded).unwrap();

        // Decode with TokenReader
        let mut reader = TokenReader::new(Some(CompressionAlgorithm::Zlib));
        let mut cursor = Cursor::new(encoded);
        let mut decoded = Vec::new();

        loop {
            match reader.read_token(&mut cursor).unwrap() {
                DeltaToken::Literal(LiteralData::Ready(data)) => {
                    decoded.extend_from_slice(&data);
                }
                DeltaToken::End => break,
                other => panic!("unexpected token: {other:?}"),
            }
        }

        assert_eq!(decoded, literal_data);
    }

    #[test]
    fn compressed_reader_block_match() {
        let mut encoded = Vec::new();
        let mut encoder = CompressedTokenEncoder::default();
        encoder.send_block_match(&mut encoded, 3).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut reader = TokenReader::new(Some(CompressionAlgorithm::Zlib));
        let mut cursor = Cursor::new(encoded);
        let mut blocks = Vec::new();

        loop {
            match reader.read_token(&mut cursor).unwrap() {
                DeltaToken::BlockRef(idx) => blocks.push(idx),
                DeltaToken::End => break,
                DeltaToken::Literal(_) => {}
            }
        }

        assert_eq!(blocks, vec![3]);
    }

    #[test]
    fn compressed_reader_mixed_tokens() {
        let literal1 = b"first part";
        let literal2 = b"second part";

        let mut encoded = Vec::new();
        let mut encoder = CompressedTokenEncoder::default();
        encoder.send_literal(&mut encoded, literal1).unwrap();
        encoder.send_block_match(&mut encoded, 7).unwrap();
        encoder.send_literal(&mut encoded, literal2).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut reader = TokenReader::new(Some(CompressionAlgorithm::ZlibX));
        let mut cursor = Cursor::new(encoded);
        let mut literals = Vec::new();
        let mut blocks = Vec::new();

        loop {
            match reader.read_token(&mut cursor).unwrap() {
                DeltaToken::Literal(LiteralData::Ready(data)) => {
                    literals.extend_from_slice(&data);
                }
                DeltaToken::BlockRef(idx) => blocks.push(idx),
                DeltaToken::End => break,
                other => panic!("unexpected token: {other:?}"),
            }
        }

        let expected: Vec<u8> = [literal1.as_slice(), literal2.as_slice()].concat();
        assert_eq!(literals, expected);
        assert_eq!(blocks, vec![7]);
    }

    #[test]
    fn new_returns_plain_for_none() {
        let reader = TokenReader::new(None);
        assert!(!reader.is_compressed());
    }

    #[test]
    fn new_returns_plain_for_compression_none() {
        let reader = TokenReader::new(Some(CompressionAlgorithm::None));
        assert!(!reader.is_compressed());
    }

    #[test]
    fn new_returns_compressed_for_zlib() {
        let reader = TokenReader::new(Some(CompressionAlgorithm::Zlib));
        assert!(reader.is_compressed());
    }

    #[test]
    fn new_returns_compressed_for_zlibx() {
        let reader = TokenReader::new(Some(CompressionAlgorithm::ZlibX));
        assert!(reader.is_compressed());
    }

    #[test]
    fn see_token_noop_for_plain() {
        let mut reader = TokenReader::new(None);
        reader.see_token(b"block data").unwrap();
    }

    #[test]
    fn reset_noop_for_plain() {
        let mut reader = TokenReader::new(None);
        reader.reset();
    }

    #[test]
    fn reset_clears_compressed_state() {
        let mut reader = TokenReader::new(Some(CompressionAlgorithm::Zlib));
        reader.reset();
        assert!(reader.is_compressed());
    }

    #[test]
    fn plain_reader_eof_returns_error() {
        let mut reader = TokenReader::new(None);
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let result = reader.read_token(&mut cursor);
        assert!(result.is_err());
    }
}
