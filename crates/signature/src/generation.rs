//! crates/signature/src/generation.rs
//!
//! File signature generation from input data.

use std::io::{self, Read};
use std::num::NonZeroUsize;

use thiserror::Error;
#[cfg(feature = "tracing")]
use tracing::instrument;

use checksums::RollingDigest;

use crate::algorithm::SignatureAlgorithm;
use crate::block::SignatureBlock;
use crate::file::FileSignature;
use crate::layout::SignatureLayout;

/// Errors returned when generating file signatures.
#[derive(Debug, Error)]
pub enum SignatureError {
    /// Underlying I/O failure raised while reading file contents.
    #[error("failed to read input while generating signature: {0}")]
    Io(
        #[from]
        #[source]
        io::Error,
    ),
    /// Requested strong checksum length exceeds what the algorithm can provide.
    #[error("requested strong checksum length {requested} exceeds {algorithm:?} digest width")]
    DigestLengthMismatch {
        /// Strong checksum algorithm in use.
        algorithm: SignatureAlgorithm,
        /// Number of bytes requested by the layout.
        requested: NonZeroUsize,
    },
    /// Extra bytes were present in the input after consuming the advertised layout.
    #[error("input contained {bytes} trailing byte(s) beyond the expected layout")]
    TrailingData {
        /// Number of bytes observed beyond the expected layout.
        bytes: u64,
    },
    /// Number of blocks derived from the layout exceeded the platform's addressable range.
    #[error("signature layout describes {0} blocks which exceeds addressable memory")]
    TooManyBlocks(u64),
}

impl SignatureError {
    fn digest_mismatch(algorithm: SignatureAlgorithm, requested: usize) -> Self {
        let requested = NonZeroUsize::new(requested)
            .expect("strong digest length requested by layout must be non-zero");
        SignatureError::DigestLengthMismatch {
            algorithm,
            requested,
        }
    }
}

/// Generates an rsync-compatible file signature using the provided layout and strong checksum.
///
/// The reader must yield exactly the number of bytes implied by `layout`. Trailing data is
/// reported via [`SignatureError::TrailingData`].
///
/// # Errors
///
/// - Returns [`SignatureError::DigestLengthMismatch`] when the layout requests a strong checksum
///   length that exceeds the algorithm's digest width.
/// - Returns [`SignatureError::TooManyBlocks`] if the layout describes more blocks than can be
///   addressed on the current platform.
/// - Propagates any I/O error surfaced by the reader.
#[cfg_attr(feature = "tracing", instrument(skip(reader), fields(algorithm = ?algorithm, block_count = layout.block_count()), name = "generate_signature"))]
pub fn generate_file_signature<R: Read>(
    mut reader: R,
    layout: SignatureLayout,
    algorithm: SignatureAlgorithm,
) -> Result<FileSignature, SignatureError> {
    let strong_len = usize::from(layout.strong_sum_length().get());
    if strong_len > algorithm.digest_len() {
        return Err(SignatureError::digest_mismatch(algorithm, strong_len));
    }

    let block_len = layout.block_length().get() as usize;
    let expected_blocks = layout.block_count();
    let expected_blocks_usize = usize::try_from(expected_blocks)
        .map_err(|_| SignatureError::TooManyBlocks(expected_blocks))?;

    let mut blocks = Vec::with_capacity(expected_blocks_usize);
    let mut buffer = vec![0u8; block_len.max(1)];
    let mut total_bytes: u64 = 0;

    for index in 0..expected_blocks_usize {
        let is_last = index + 1 == expected_blocks_usize;
        let target_len = if is_last && layout.remainder() != 0 {
            layout.remainder() as usize
        } else {
            block_len
        };

        let chunk = &mut buffer[..target_len];
        reader.read_exact(chunk)?;
        let chunk = &chunk[..];
        total_bytes = total_bytes.saturating_add(target_len as u64);
        let rolling = RollingDigest::from_bytes(chunk);
        let mut strong = algorithm.compute_full(chunk);
        strong.truncate(strong_len);

        blocks.push(SignatureBlock::new(index as u64, rolling, strong));
    }

    let mut extra = [0u8; 1];
    if reader.read(&mut extra)? != 0 {
        return Err(SignatureError::TrailingData { bytes: 1 });
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

    fn layout(len: u64, checksum_len: u8) -> SignatureLayout {
        calculate_signature_layout(SignatureLayoutParams::new(
            len,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(checksum_len).expect("checksum length"),
        ))
        .expect("layout")
    }

    #[test]
    fn generate_signature_single_block_md4() {
        let layout = layout(11, 16);
        let input = Cursor::new(b"hello world".to_vec());
        let signature = generate_file_signature(input, layout, SignatureAlgorithm::Md4)
            .expect("signature generation succeeds");

        assert_eq!(signature.blocks().len(), 1);
        let block = &signature.blocks()[0];
        assert_eq!(block.index(), 0);
        assert_eq!(block.len(), 11);
        assert_eq!(block.rolling(), RollingDigest::from_bytes(b"hello world"));
        assert_eq!(block.strong().len(), 16);
    }

    #[test]
    fn generate_signature_multiple_blocks_with_remainder() {
        let mut data = vec![0u8; 1_024 + 111];
        for (index, byte) in data.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let layout = calculate_signature_layout(SignatureLayoutParams::new(
            data.len() as u64,
            NonZeroU32::new(512),
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        ))
        .expect("layout");

        assert_eq!(layout.block_length().get(), 512);
        assert_eq!(layout.block_count(), 3);
        assert_eq!(layout.remainder(), 111);

        let signature = generate_file_signature(
            Cursor::new(data.clone()),
            layout,
            SignatureAlgorithm::Md5 {
                seed_config: Md5Seed::none(),
            },
        )
        .expect("signature generation succeeds");

        assert_eq!(signature.blocks().len(), 3);
        assert_eq!(signature.total_bytes(), data.len() as u64);

        for (index, block) in signature.blocks().iter().enumerate() {
            let start = index * 512;
            let end = if index == 2 { data.len() } else { start + 512 };
            assert_eq!(block.len(), end - start);
            assert_eq!(
                block.rolling(),
                RollingDigest::from_bytes(&data[start..end])
            );
        }
    }

    #[test]
    fn digest_length_mismatch_is_reported() {
        let layout = layout(256, 16);
        let result = generate_file_signature(
            Cursor::new(vec![0u8; 256]),
            layout,
            SignatureAlgorithm::Xxh64 { seed: 0 },
        );

        let error = result.expect_err("xxh64 cannot provide 16-byte digests");
        match error {
            SignatureError::DigestLengthMismatch {
                algorithm,
                requested,
            } => {
                assert_eq!(algorithm, SignatureAlgorithm::Xxh64 { seed: 0 });
                assert_eq!(requested.get(), 16);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn trailing_data_detected() {
        let layout = layout(32, 16);
        let mut data = vec![0u8; 32 + 1];
        data[32] = 1;
        let error = generate_file_signature(Cursor::new(data), layout, SignatureAlgorithm::Md4)
            .expect_err("trailing byte should be detected");

        assert!(matches!(error, SignatureError::TrailingData { .. }));
    }

    #[test]
    fn empty_files_produce_empty_signature() {
        let layout = layout(0, 16);
        let signature =
            generate_file_signature(Cursor::new(Vec::new()), layout, SignatureAlgorithm::Md4)
                .expect("signature generation succeeds");

        assert!(signature.blocks().is_empty());
        assert_eq!(signature.total_bytes(), 0);
    }

    #[test]
    fn generate_signature_sha1() {
        let sig_layout = layout(11, 16);
        let input = Cursor::new(b"hello world".to_vec());
        let signature = generate_file_signature(input, sig_layout, SignatureAlgorithm::Sha1)
            .expect("signature generation succeeds");

        assert_eq!(signature.blocks().len(), 1);
        // SHA1 produces 20 bytes, truncated to 16 by layout
        assert_eq!(signature.blocks()[0].strong().len(), 16);
    }

    #[test]
    fn generate_signature_xxh3() {
        let sig_layout = layout(11, 8);
        let input = Cursor::new(b"hello world".to_vec());
        let signature =
            generate_file_signature(input, sig_layout, SignatureAlgorithm::Xxh3 { seed: 12345 })
                .expect("signature generation succeeds");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.blocks()[0].strong().len(), 8);
    }

    #[test]
    fn generate_signature_xxh3_128() {
        let sig_layout = layout(11, 16);
        let input = Cursor::new(b"hello world".to_vec());
        let signature = generate_file_signature(
            input,
            sig_layout,
            SignatureAlgorithm::Xxh3_128 { seed: 12345 },
        )
        .expect("signature generation succeeds");

        assert_eq!(signature.blocks().len(), 1);
        assert_eq!(signature.blocks()[0].strong().len(), 16);
    }

    #[test]
    fn signature_error_io_displays_message() {
        let error: SignatureError =
            io::Error::new(io::ErrorKind::NotFound, "file not found").into();
        let display = format!("{error}");
        assert!(display.contains("read"));
    }

    #[test]
    fn signature_error_trailing_data_displays_bytes() {
        let error = SignatureError::TrailingData { bytes: 42 };
        let display = format!("{error}");
        assert!(display.contains("42"));
    }

    #[test]
    fn signature_error_too_many_blocks_displays_count() {
        let error = SignatureError::TooManyBlocks(999999);
        let display = format!("{error}");
        assert!(display.contains("999999"));
    }
}
