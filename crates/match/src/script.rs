//! crates/match/src/script.rs
//!
//! Delta script representation and application.

use std::cmp::min;
use std::io::{self, Read, Seek, SeekFrom, Write};

use logging::debug_log;

#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::index::DeltaSignatureIndex;

/// Token describing how to reconstruct a target file from an rsync delta stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeltaToken {
    /// Literal byte payload that must be written to the receiver.
    Literal(Vec<u8>),
    /// Reference to a block from the basis file identified by index.
    Copy {
        /// Zero-based index of the signature block being reused.
        index: u64,
        /// Number of bytes copied from the referenced block.
        len: usize,
    },
}

impl DeltaToken {
    /// Returns the number of bytes contributed by this token.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        match self {
            DeltaToken::Literal(bytes) => bytes.len(),
            DeltaToken::Copy { len, .. } => *len,
        }
    }

    /// Returns `true` when the token is a literal payload.
    #[must_use]
    pub const fn is_literal(&self) -> bool {
        matches!(self, DeltaToken::Literal(_))
    }
}

/// Ordered collection of [`DeltaToken`] values that reconstruct a target file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeltaScript {
    tokens: Vec<DeltaToken>,
    total_bytes: u64,
    literal_bytes: u64,
}

impl DeltaScript {
    /// Creates a new script from the provided token stream.
    #[must_use]
    pub fn new(tokens: Vec<DeltaToken>, total_bytes: u64, literal_bytes: u64) -> Self {
        Self {
            tokens,
            total_bytes,
            literal_bytes,
        }
    }

    /// Returns the underlying token stream.
    #[must_use]
    pub fn tokens(&self) -> &[DeltaToken] {
        &self.tokens
    }

    /// Consumes the script and returns its token list.
    #[must_use]
    pub fn into_tokens(self) -> Vec<DeltaToken> {
        self.tokens
    }

    /// Returns the total number of bytes described by the script.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Returns the number of bytes emitted as literals.
    #[must_use]
    pub const fn literal_bytes(&self) -> u64 {
        self.literal_bytes
    }

    /// Returns the number of bytes that will be copied from the basis file.
    #[must_use]
    pub fn copy_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.literal_bytes)
    }

    /// Returns `true` when the script does not contain any tokens.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Applies a [`DeltaScript`] to an existing basis file, producing the target payload.
#[cfg_attr(
    feature = "tracing",
    instrument(skip(basis, output, index, script), name = "apply_delta")
)]
pub fn apply_delta<R, W>(
    mut basis: R,
    mut output: W,
    index: &DeltaSignatureIndex,
    script: &DeltaScript,
) -> io::Result<()>
where
    R: Read + Seek,
    W: Write,
{
    debug_log!(
        Recv,
        2,
        "applying delta: {} tokens, {} total bytes, {} literal bytes",
        script.tokens().len(),
        script.total_bytes(),
        script.literal_bytes()
    );

    let block_length = index.block_length();
    let block_length_u64 = u64::try_from(block_length)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block length overflow"))?;
    let mut buffer = vec![0u8; block_length.max(8 * 1024)];
    let mut basis_position: Option<u64> = None;

    for token in script.tokens() {
        match token {
            DeltaToken::Literal(bytes) => {
                output.write_all(bytes)?;
            }
            DeltaToken::Copy {
                index: block_index,
                len,
            } => {
                let offset = block_index.checked_mul(block_length_u64).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "delta block offset overflow")
                })?;

                if basis_position != Some(offset) {
                    basis.seek(SeekFrom::Start(offset))?;
                    basis_position = Some(offset);
                }

                let mut remaining = *len;
                while remaining > 0 {
                    let chunk = min(remaining, buffer.len());
                    basis.read_exact(&mut buffer[..chunk])?;
                    output.write_all(&buffer[..chunk])?;
                    remaining -= chunk;

                    if let Some(position) = basis_position {
                        let advanced = u64::try_from(chunk).map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidInput, "chunk length overflow")
                        })?;
                        basis_position = Some(position.checked_add(advanced).ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "delta block offset overflow",
                            )
                        })?);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::ProtocolVersion;
    use signature::{
        SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout,
        generate_file_signature,
    };
    use std::io::{Cursor, ErrorKind};
    use std::num::NonZeroU8;

    #[test]
    fn apply_delta_reconstructs_literal_only_script() {
        let index_data = vec![0u8; 1024];
        let params = SignatureLayoutParams::new(
            index_data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(index_data.as_slice(), layout, SignatureAlgorithm::Md4)
                .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let script = DeltaScript::new(vec![DeltaToken::Literal(b"hello".to_vec())], 5, 5);

        let mut basis = Cursor::new(index_data);
        let mut output = Vec::new();
        apply_delta(&mut basis, &mut output, &index, &script).expect("apply");
        assert_eq!(output, b"hello");
    }

    #[test]
    fn apply_delta_reuses_basis_blocks() {
        let mut index_data = Vec::new();
        index_data.extend((0..2048).map(|byte| (byte % 251) as u8));
        let params = SignatureLayoutParams::new(
            index_data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(index_data.as_slice(), layout, SignatureAlgorithm::Md4)
                .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let block_len = index.block_length();
        let script = DeltaScript::new(
            vec![
                DeltaToken::Copy {
                    index: 0,
                    len: block_len,
                },
                DeltaToken::Literal(b"tail".to_vec()),
            ],
            block_len as u64 + 4,
            4,
        );

        let mut basis = Cursor::new(index_data.clone());
        let mut output = Vec::new();
        apply_delta(&mut basis, &mut output, &index, &script).expect("apply");

        let mut expected = index_data[..block_len].to_vec();
        expected.extend_from_slice(b"tail");
        assert_eq!(output, expected);
    }

    #[test]
    fn apply_delta_rejects_offset_overflow() {
        let index_data = vec![0u8; 4096];
        let params = SignatureLayoutParams::new(
            index_data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(index_data.as_slice(), layout, SignatureAlgorithm::Md4)
                .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let script = DeltaScript::new(
            vec![DeltaToken::Copy {
                index: u64::MAX,
                len: index.block_length(),
            }],
            index.block_length() as u64,
            0,
        );

        let mut basis = Cursor::new(index_data);
        let mut output = Vec::new();
        let error = apply_delta(&mut basis, &mut output, &index, &script).expect_err("overflow");
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    // DeltaToken tests
    #[test]
    fn delta_token_literal_byte_len() {
        let token = DeltaToken::Literal(vec![1, 2, 3, 4, 5]);
        assert_eq!(token.byte_len(), 5);
    }

    #[test]
    fn delta_token_copy_byte_len() {
        let token = DeltaToken::Copy { index: 0, len: 100 };
        assert_eq!(token.byte_len(), 100);
    }

    #[test]
    fn delta_token_is_literal_true_for_literal() {
        let token = DeltaToken::Literal(vec![1, 2, 3]);
        assert!(token.is_literal());
    }

    #[test]
    fn delta_token_is_literal_false_for_copy() {
        let token = DeltaToken::Copy { index: 0, len: 100 };
        assert!(!token.is_literal());
    }

    #[test]
    fn delta_token_empty_literal_byte_len_is_zero() {
        let token = DeltaToken::Literal(vec![]);
        assert_eq!(token.byte_len(), 0);
    }

    #[test]
    fn delta_token_clone() {
        let token = DeltaToken::Literal(vec![1, 2, 3]);
        let cloned = token.clone();
        assert_eq!(token, cloned);
    }

    #[test]
    fn delta_token_debug() {
        let token = DeltaToken::Literal(vec![1, 2, 3]);
        let debug = format!("{token:?}");
        assert!(debug.contains("Literal"));
    }

    #[test]
    fn delta_token_eq() {
        let a = DeltaToken::Literal(vec![1, 2, 3]);
        let b = DeltaToken::Literal(vec![1, 2, 3]);
        let c = DeltaToken::Literal(vec![4, 5, 6]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // DeltaScript tests
    #[test]
    fn delta_script_new_and_accessors() {
        let tokens = vec![
            DeltaToken::Literal(vec![1, 2, 3]),
            DeltaToken::Copy { index: 0, len: 10 },
        ];
        let script = DeltaScript::new(tokens, 13, 3);

        assert_eq!(script.tokens().len(), 2);
        assert_eq!(script.total_bytes(), 13);
        assert_eq!(script.literal_bytes(), 3);
        assert_eq!(script.copy_bytes(), 10);
    }

    #[test]
    fn delta_script_is_empty_true_for_empty() {
        let script = DeltaScript::new(vec![], 0, 0);
        assert!(script.is_empty());
    }

    #[test]
    fn delta_script_is_empty_false_for_non_empty() {
        let script = DeltaScript::new(vec![DeltaToken::Literal(vec![1])], 1, 1);
        assert!(!script.is_empty());
    }

    #[test]
    fn delta_script_into_tokens() {
        let tokens = vec![DeltaToken::Literal(vec![1, 2, 3])];
        let script = DeltaScript::new(tokens.clone(), 3, 3);
        let result = script.into_tokens();
        assert_eq!(result, tokens);
    }

    #[test]
    fn delta_script_copy_bytes_all_literal() {
        let script = DeltaScript::new(vec![DeltaToken::Literal(vec![1, 2, 3])], 3, 3);
        assert_eq!(script.copy_bytes(), 0);
    }

    #[test]
    fn delta_script_copy_bytes_all_copy() {
        let script = DeltaScript::new(vec![DeltaToken::Copy { index: 0, len: 100 }], 100, 0);
        assert_eq!(script.copy_bytes(), 100);
    }

    #[test]
    fn delta_script_clone() {
        let script = DeltaScript::new(vec![DeltaToken::Literal(vec![1])], 1, 1);
        let cloned = script.clone();
        assert_eq!(script, cloned);
    }

    #[test]
    fn delta_script_debug() {
        let script = DeltaScript::new(vec![], 0, 0);
        let debug = format!("{script:?}");
        assert!(debug.contains("DeltaScript"));
    }

    #[test]
    fn apply_delta_empty_script() {
        let index_data = vec![0u8; 1024];
        let params = SignatureLayoutParams::new(
            index_data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(index_data.as_slice(), layout, SignatureAlgorithm::Md4)
                .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let script = DeltaScript::new(vec![], 0, 0);

        let mut basis = Cursor::new(index_data);
        let mut output = Vec::new();
        apply_delta(&mut basis, &mut output, &index, &script).expect("apply");
        assert!(output.is_empty());
    }

    #[test]
    fn apply_delta_multiple_literals() {
        let index_data = vec![0u8; 1024];
        let params = SignatureLayoutParams::new(
            index_data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(index_data.as_slice(), layout, SignatureAlgorithm::Md4)
                .expect("signature");
        let index = DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4)
            .expect("index");

        let script = DeltaScript::new(
            vec![
                DeltaToken::Literal(b"hello ".to_vec()),
                DeltaToken::Literal(b"world".to_vec()),
            ],
            11,
            11,
        );

        let mut basis = Cursor::new(index_data);
        let mut output = Vec::new();
        apply_delta(&mut basis, &mut output, &index, &script).expect("apply");
        assert_eq!(output, b"hello world");
    }
}
