use std::collections::VecDeque;
use std::io::{self, Read};

use rsync_checksums::RollingChecksum;

use crate::delta::index::DeltaSignatureIndex;
use crate::delta::script::{DeltaScript, DeltaToken};

/// Default buffer size used by [`DeltaGenerator::generate`].
const DEFAULT_BUFFER_LEN: usize = 128 * 1024;

/// Produces rsync-style delta tokens by comparing an input stream against a signature index.
#[derive(Clone, Debug)]
pub struct DeltaGenerator {
    buffer_len: usize,
}

impl DeltaGenerator {
    /// Creates a new generator with default buffering.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer_len: DEFAULT_BUFFER_LEN,
        }
    }

    /// Overrides the buffer length used when reading from the input stream.
    #[must_use]
    pub fn with_buffer_len(mut self, buffer_len: usize) -> Self {
        self.buffer_len = buffer_len.max(1);
        self
    }

    /// Generates a [`DeltaScript`] for the provided reader using the supplied signature index.
    pub fn generate<R: Read>(
        &self,
        mut reader: R,
        index: &DeltaSignatureIndex,
    ) -> io::Result<DeltaScript> {
        let block_len = index.block_length();
        let mut window: VecDeque<u8> = VecDeque::with_capacity(block_len);
        let mut pending_literals = Vec::with_capacity(block_len);
        let mut scratch = Vec::with_capacity(block_len);
        let mut rolling = RollingChecksum::new();
        let mut outgoing: Option<u8> = None;
        let mut tokens = Vec::new();
        let mut total_bytes = 0u64;
        let mut literal_bytes = 0u64;

        let mut buffer = vec![0u8; self.buffer_len.max(block_len)];
        let mut buffer_pos = 0usize;
        let mut buffer_len = 0usize;

        loop {
            if buffer_pos == buffer_len {
                buffer_len = reader.read(&mut buffer)?;
                buffer_pos = 0;
                if buffer_len == 0 {
                    break;
                }
            }

            let byte = buffer[buffer_pos];
            buffer_pos += 1;

            window.push_back(byte);
            if let Some(outgoing_byte) = outgoing.take() {
                rolling
                    .roll_many(&[outgoing_byte], &[byte])
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            } else {
                rolling.update(&[byte]);
            }

            if window.len() < block_len {
                continue;
            }

            let digest = rolling.digest();
            if let Some(block_index) = index.find_match_window(digest, &window, &mut scratch) {
                if !pending_literals.is_empty() {
                    literal_bytes += pending_literals.len() as u64;
                    total_bytes += pending_literals.len() as u64;
                    tokens.push(DeltaToken::Literal(std::mem::take(&mut pending_literals)));
                }

                let block = index.block(block_index);
                tokens.push(DeltaToken::Copy {
                    index: block.index(),
                    len: block.len(),
                });
                total_bytes += block.len() as u64;

                window.clear();
                rolling.reset();
                outgoing = None;
                continue;
            }

            if let Some(front) = window.pop_front() {
                pending_literals.push(front);
                outgoing = Some(front);
            }
        }

        while let Some(byte) = window.pop_front() {
            pending_literals.push(byte);
        }

        if !pending_literals.is_empty() {
            literal_bytes += pending_literals.len() as u64;
            total_bytes += pending_literals.len() as u64;
            tokens.push(DeltaToken::Literal(pending_literals));
        }

        Ok(DeltaScript::new(tokens, total_bytes, literal_bytes))
    }
}

impl Default for DeltaGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience helper that generates a delta using the default [`DeltaGenerator`] configuration.
pub fn generate_delta<R: Read>(reader: R, index: &DeltaSignatureIndex) -> io::Result<DeltaScript> {
    DeltaGenerator::new().generate(reader, index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::script::apply_delta;
    use crate::delta::{SignatureLayoutParams, calculate_signature_layout};
    use crate::signature::{SignatureAlgorithm, generate_file_signature};
    use rsync_protocol::ProtocolVersion;
    use std::io::Cursor;
    use std::num::NonZeroU8;

    fn build_index(data: &[u8]) -> DeltaSignatureIndex {
        let params = SignatureLayoutParams::new(
            data.len() as u64,
            None,
            ProtocolVersion::NEWEST,
            NonZeroU8::new(16).unwrap(),
        );
        let layout = calculate_signature_layout(params).expect("layout");
        let signature =
            generate_file_signature(data, layout, SignatureAlgorithm::Md4).expect("signature");
        DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4).expect("index")
    }

    #[test]
    fn generate_delta_produces_literals_when_no_matches() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = b"new data";

        let script = generate_delta(&input[..], &index).expect("script");
        assert_eq!(script.tokens().len(), 1);
        assert!(
            matches!(script.tokens()[0], DeltaToken::Literal(ref bytes) if bytes == b"new data")
        );
        assert_eq!(script.literal_bytes(), input.len() as u64);
    }

    #[test]
    fn generate_delta_finds_matching_blocks() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let block_len = index.block_length();
        let mut input = Vec::new();
        input.extend_from_slice(&basis[..block_len]);
        input.extend_from_slice(b"extra");

        let script = generate_delta(&input[..], &index).expect("script");
        assert!(matches!(script.tokens()[0], DeltaToken::Copy { .. }));
        assert!(matches!(script.tokens()[1], DeltaToken::Literal(ref bytes) if bytes == b"extra"));

        let mut basis_cursor = Cursor::new(basis);
        let mut output = Vec::new();
        apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");
        assert_eq!(output, input);
    }
}
