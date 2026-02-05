//! Pipelined signature generation with double-buffered I/O.
//!
//! This module provides signature generation that overlaps file reading with
//! checksum computation using the `checksums::pipelined` double-buffering
//! infrastructure.
//!
//! # Performance Benefits
//!
//! For CPU-intensive checksums (MD4/MD5/SHA1) with large files on fast storage,
//! pipelined generation can provide 20-40% throughput improvement by hiding
//! I/O latency behind computation.

use std::io::{self, Read};

use checksums::RollingDigest;
use checksums::pipelined::{DoubleBufferedReader, PipelineConfig};

use crate::algorithm::SignatureAlgorithm;
use crate::block::SignatureBlock;
use crate::file::FileSignature;
use crate::generation::SignatureError;
use crate::layout::SignatureLayout;

/// Configuration for pipelined signature generation.
#[derive(Clone, Copy, Debug, Default)]
pub struct PipelinedSignatureConfig {
    /// Underlying pipeline configuration.
    pub pipeline: PipelineConfig,
}

impl PipelinedSignatureConfig {
    /// Creates a new configuration with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the block size for double-buffering.
    #[must_use]
    pub fn with_block_size(mut self, size: usize) -> Self {
        self.pipeline.block_size = size;
        self
    }

    /// Sets the minimum file size for enabling pipelining.
    #[must_use]
    pub fn with_min_file_size(mut self, size: u64) -> Self {
        self.pipeline.min_file_size = size;
        self
    }

    /// Enables or disables pipelining.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.pipeline.enabled = enabled;
        self
    }
}

/// Generates an rsync-compatible file signature using pipelined I/O.
///
/// This function uses double-buffering to overlap file reading with checksum
/// computation. While the main thread computes checksums for the current block,
/// a background thread pre-reads the next block.
///
/// # Arguments
///
/// * `reader` - The input reader (typically a file)
/// * `layout` - Signature layout specifying block size and count
/// * `algorithm` - Strong checksum algorithm to use
/// * `config` - Pipelining configuration
///
/// # Returns
///
/// The generated file signature containing rolling and strong checksums for
/// each block.
///
/// # Errors
///
/// - Returns [`SignatureError::DigestLengthMismatch`] when the layout requests
///   a strong checksum length that exceeds the algorithm's digest width.
/// - Returns [`SignatureError::TooManyBlocks`] if the layout describes more
///   blocks than can be addressed on the current platform.
/// - Propagates any I/O error from the reader.
pub fn generate_signature_pipelined<R: Read + Send + 'static>(
    reader: R,
    layout: SignatureLayout,
    algorithm: SignatureAlgorithm,
    config: PipelinedSignatureConfig,
) -> Result<FileSignature, SignatureError> {
    let strong_len = usize::from(layout.strong_sum_length().get());
    if strong_len > algorithm.digest_len() {
        return Err(SignatureError::DigestLengthMismatch {
            algorithm,
            requested: layout.strong_sum_length().into(),
        });
    }

    let block_len = layout.block_length().get() as usize;
    let expected_blocks = layout.block_count();
    let expected_blocks_usize = usize::try_from(expected_blocks)
        .map_err(|_| SignatureError::TooManyBlocks(expected_blocks))?;

    let file_size = layout.file_size();

    // Configure pipeline with block size matching signature blocks
    let pipeline_config = PipelineConfig {
        block_size: block_len,
        min_file_size: config.pipeline.min_file_size,
        enabled: config.pipeline.enabled,
    };

    let mut buffered_reader =
        DoubleBufferedReader::with_size_hint(reader, pipeline_config, Some(file_size));

    let mut blocks = Vec::with_capacity(expected_blocks_usize);
    let mut total_bytes: u64 = 0;
    let mut block_index: usize = 0;

    while let Some(chunk) = buffered_reader.next_block().map_err(SignatureError::Io)? {
        if block_index >= expected_blocks_usize {
            return Err(SignatureError::TrailingData {
                bytes: chunk.len() as u64,
            });
        }

        let is_last = block_index + 1 == expected_blocks_usize;
        let expected_len = if is_last && layout.remainder() != 0 {
            layout.remainder() as usize
        } else {
            block_len
        };

        if chunk.len() != expected_len {
            if chunk.len() < expected_len {
                return Err(SignatureError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "block {} expected {} bytes, got {}",
                        block_index,
                        expected_len,
                        chunk.len()
                    ),
                )));
            } else {
                return Err(SignatureError::TrailingData {
                    bytes: (chunk.len() - expected_len) as u64,
                });
            }
        }

        total_bytes = total_bytes.saturating_add(chunk.len() as u64);

        let rolling = RollingDigest::from_bytes(chunk);
        let mut strong = algorithm.compute_full(chunk);
        strong.truncate(strong_len);

        blocks.push(SignatureBlock::new(block_index as u64, rolling, strong));
        block_index += 1;
    }

    if block_index < expected_blocks_usize {
        return Err(SignatureError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("expected {expected_blocks_usize} blocks, got {block_index}"),
        )));
    }

    Ok(FileSignature::new(layout, blocks, total_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SignatureLayoutParams, calculate_signature_layout};
    use checksums::strong::Md5Seed;
    use protocol::ProtocolVersion;
    use std::io::Cursor;
    use std::num::{NonZeroU8, NonZeroU32};

    fn make_layout(len: u64, block_size: Option<u32>, checksum_len: u8) -> SignatureLayout {
        calculate_signature_layout(SignatureLayoutParams::new(
            len,
            block_size.and_then(NonZeroU32::new),
            ProtocolVersion::NEWEST,
            NonZeroU8::new(checksum_len).expect("checksum length"),
        ))
        .expect("layout")
    }

    #[test]
    fn pipelined_matches_sequential_single_block() {
        let data = b"hello world".to_vec();
        let layout = make_layout(data.len() as u64, None, 16);

        let config = PipelinedSignatureConfig::default().with_enabled(false);
        let sig_sequential = generate_signature_pipelined(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md4,
            config,
        )
        .expect("sequential signature");

        let layout = make_layout(data.len() as u64, None, 16);
        let config = PipelinedSignatureConfig::default()
            .with_min_file_size(0)
            .with_enabled(true);
        let sig_pipelined = generate_signature_pipelined(
            Cursor::new(data),
            layout,
            SignatureAlgorithm::Md4,
            config,
        )
        .expect("pipelined signature");

        assert_eq!(sig_sequential.blocks().len(), sig_pipelined.blocks().len());
        for (seq, pip) in sig_sequential
            .blocks()
            .iter()
            .zip(sig_pipelined.blocks().iter())
        {
            assert_eq!(seq.rolling(), pip.rolling());
            assert_eq!(seq.strong(), pip.strong());
        }
    }

    #[test]
    fn pipelined_matches_sequential_multiple_blocks() {
        let mut data = vec![0u8; 1024 + 111];
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = (i % 251) as u8;
        }

        let layout = make_layout(data.len() as u64, Some(512), 16);

        let config = PipelinedSignatureConfig::default().with_enabled(false);
        let sig_sequential = generate_signature_pipelined(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none(),
            },
            config,
        )
        .expect("sequential signature");

        let layout = make_layout(data.len() as u64, Some(512), 16);
        let config = PipelinedSignatureConfig::default()
            .with_min_file_size(0)
            .with_enabled(true);
        let sig_pipelined = generate_signature_pipelined(
            Cursor::new(data),
            layout,
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none(),
            },
            config,
        )
        .expect("pipelined signature");

        assert_eq!(sig_sequential.blocks().len(), 3);
        assert_eq!(sig_pipelined.blocks().len(), 3);

        for (i, (seq, pip)) in sig_sequential
            .blocks()
            .iter()
            .zip(sig_pipelined.blocks().iter())
            .enumerate()
        {
            assert_eq!(seq.rolling(), pip.rolling(), "block {i} rolling mismatch");
            assert_eq!(seq.strong(), pip.strong(), "block {i} strong mismatch");
            assert_eq!(seq.len(), pip.len(), "block {i} len mismatch");
        }
    }

    #[test]
    fn pipelined_empty_file() {
        let layout = make_layout(0, None, 16);
        let config = PipelinedSignatureConfig::default();

        let sig = generate_signature_pipelined(
            Cursor::new(Vec::new()),
            layout,
            SignatureAlgorithm::Md4,
            config,
        )
        .expect("signature");

        assert!(sig.blocks().is_empty());
        assert_eq!(sig.total_bytes(), 0);
    }

    #[test]
    fn pipelined_large_file() {
        let data = vec![0xAB; 1024 * 1024];
        let layout = make_layout(data.len() as u64, Some(64 * 1024), 16);

        let config = PipelinedSignatureConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(0);

        let sig = generate_signature_pipelined(
            Cursor::new(data),
            layout,
            SignatureAlgorithm::Sha1,
            config,
        )
        .expect("signature");

        assert_eq!(sig.blocks().len(), 16);
        assert_eq!(sig.total_bytes(), 1024 * 1024);
    }

    #[test]
    fn pipelined_digest_mismatch_error() {
        let layout = make_layout(256, None, 16);

        let result = generate_signature_pipelined(
            Cursor::new(vec![0u8; 256]),
            layout,
            SignatureAlgorithm::Xxh64 { seed: 0 },
            PipelinedSignatureConfig::default(),
        );

        assert!(matches!(
            result,
            Err(SignatureError::DigestLengthMismatch { .. })
        ));
    }

    #[test]
    fn pipelined_config_builder() {
        let config = PipelinedSignatureConfig::new()
            .with_block_size(128 * 1024)
            .with_min_file_size(512 * 1024)
            .with_enabled(false);

        assert_eq!(config.pipeline.block_size, 128 * 1024);
        assert_eq!(config.pipeline.min_file_size, 512 * 1024);
        assert!(!config.pipeline.enabled);
    }
}
