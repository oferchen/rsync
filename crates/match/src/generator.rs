//! crates/match/src/generator.rs
//!
//! Delta token generation pipeline.

use std::io::{self, Read};

use checksums::RollingChecksum;
use logging::debug_log;

#[cfg(feature = "tracing")]
use tracing::instrument;

use crate::index::DeltaSignatureIndex;
use crate::ring_buffer::RingBuffer;
use crate::script::{DeltaScript, DeltaToken};

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

    /// Generates a [`DeltaScript`] describing how to reconstruct the input from basis blocks.
    ///
    /// This implements rsync's delta generation algorithm:
    ///
    /// 1. Slide a window of `block_length` bytes over the input
    /// 2. At each position, compute the rolling checksum
    /// 3. If the checksum matches a known block, verify with the strong checksum
    /// 4. On match: emit a `Copy` token referencing the basis block
    /// 5. On no match: accumulate the byte as a literal and advance by 1
    ///
    /// # Arguments
    ///
    /// * `reader` - Source data to generate delta for
    /// * `index` - Pre-built signature index from the basis file
    ///
    /// # Returns
    ///
    /// A [`DeltaScript`] containing `Copy` and `Literal` tokens that, when applied
    /// to the basis file, reconstruct the input.
    ///
    /// # Upstream Reference
    ///
    /// See `match.c:hash_search()` for the matching algorithm.
    pub fn generate<R: Read>(
        &self,
        mut reader: R,
        index: &DeltaSignatureIndex,
    ) -> io::Result<DeltaScript> {
        let block_len = index.block_length();
        let mut window = RingBuffer::with_capacity(block_len);
        let mut pending_literals = Vec::with_capacity(block_len);
        let mut rolling = RollingChecksum::new();
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

            // Ring buffer auto-evicts when at capacity, returning the evicted byte
            let evicted = window.push_back(byte);

            // Rolling checksum update: use evicted byte directly (if any)
            if let Some(outgoing_byte) = evicted {
                // Window was full: roll with evicted byte leaving, new byte entering
                // Use roll() directly for single-byte operations (faster than roll_many)
                rolling
                    .roll(outgoing_byte, byte)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                // Evicted byte becomes a literal
                pending_literals.push(outgoing_byte);
            } else {
                // Window not yet full: use optimized single-byte update
                rolling.update_byte(byte);
            }

            // Only check for matches when window is full
            if !window.is_full() {
                continue;
            }

            let digest = rolling.digest();
            // Get contiguous slice for matching (O(1) when not wrapped, rare O(n) rotation otherwise)
            if let Some(block_index) = index.find_match_bytes(digest, window.as_slice()) {
                // Flush any pending literals before the copy token
                if !pending_literals.is_empty() {
                    literal_bytes += pending_literals.len() as u64;
                    total_bytes += pending_literals.len() as u64;
                    // Use replace instead of take to preserve capacity for next literals
                    let filled =
                        std::mem::replace(&mut pending_literals, Vec::with_capacity(block_len));
                    tokens.push(DeltaToken::Literal(filled));
                }

                let block = index.block(block_index);
                tokens.push(DeltaToken::Copy {
                    index: block.index(),
                    len: block.len(),
                });
                total_bytes += block.len() as u64;

                window.clear();
                rolling.reset();
                continue;
            }
        }

        // Drain remaining bytes from window as literals
        while let Some(byte) = window.pop_front() {
            pending_literals.push(byte);
        }

        if !pending_literals.is_empty() {
            literal_bytes += pending_literals.len() as u64;
            total_bytes += pending_literals.len() as u64;
            tokens.push(DeltaToken::Literal(pending_literals));
        }

        debug_log!(
            Deltasum,
            2,
            "delta generated: {} tokens, {} total bytes, {} literal bytes",
            tokens.len(),
            total_bytes,
            literal_bytes
        );

        Ok(DeltaScript::new(tokens, total_bytes, literal_bytes))
    }
}

impl Default for DeltaGenerator {
    fn default() -> Self {
        Self::new()
    }
}

/// Convenience helper that generates a delta using the default [`DeltaGenerator`] configuration.
#[cfg_attr(
    feature = "tracing",
    instrument(skip(reader, index), name = "generate_delta")
)]
pub fn generate_delta<R: Read>(reader: R, index: &DeltaSignatureIndex) -> io::Result<DeltaScript> {
    DeltaGenerator::new().generate(reader, index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::apply_delta;
    use protocol::ProtocolVersion;
    use signature::{
        SignatureAlgorithm, SignatureLayoutParams, calculate_signature_layout,
        generate_file_signature,
    };
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

    // DeltaGenerator constructor tests
    #[test]
    fn delta_generator_new_uses_default_buffer_len() {
        let generator = DeltaGenerator::new();
        assert_eq!(generator.buffer_len, DEFAULT_BUFFER_LEN);
    }

    #[test]
    fn delta_generator_default_matches_new() {
        let new = DeltaGenerator::new();
        let default = DeltaGenerator::default();
        assert_eq!(new.buffer_len, default.buffer_len);
    }

    #[test]
    fn delta_generator_with_buffer_len_sets_custom_length() {
        let generator = DeltaGenerator::new().with_buffer_len(4096);
        assert_eq!(generator.buffer_len, 4096);
    }

    #[test]
    fn delta_generator_with_buffer_len_zero_becomes_one() {
        let generator = DeltaGenerator::new().with_buffer_len(0);
        assert_eq!(generator.buffer_len, 1);
    }

    #[test]
    fn delta_generator_with_buffer_len_chain() {
        let generator = DeltaGenerator::new()
            .with_buffer_len(1024)
            .with_buffer_len(2048);
        assert_eq!(generator.buffer_len, 2048);
    }

    #[test]
    fn delta_generator_clone() {
        let generator = DeltaGenerator::new().with_buffer_len(512);
        let cloned = generator.clone();
        assert_eq!(generator.buffer_len, cloned.buffer_len);
    }

    #[test]
    fn delta_generator_debug() {
        let generator = DeltaGenerator::new();
        let debug = format!("{generator:?}");
        assert!(debug.contains("DeltaGenerator"));
        assert!(debug.contains("buffer_len"));
    }

    // Empty input tests
    #[test]
    fn generate_delta_empty_input_produces_empty_script() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input: &[u8] = &[];

        let script = generate_delta(input, &index).expect("script");
        assert!(script.tokens().is_empty());
        assert_eq!(script.total_bytes(), 0);
        assert_eq!(script.literal_bytes(), 0);
    }

    #[test]
    fn generate_delta_single_byte_produces_literal() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = [42u8];

        let script = generate_delta(&input[..], &index).expect("script");
        assert_eq!(script.tokens().len(), 1);
        assert!(matches!(script.tokens()[0], DeltaToken::Literal(ref bytes) if bytes == &[42]));
        assert_eq!(script.literal_bytes(), 1);
    }

    #[test]
    fn generate_delta_all_literal_counts_correctly() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = b"unique data that won't match any blocks";

        let script = generate_delta(&input[..], &index).expect("script");
        assert_eq!(script.literal_bytes(), input.len() as u64);
        assert_eq!(script.total_bytes(), input.len() as u64);
    }

    // Buffer length effects
    #[test]
    fn generate_delta_with_small_buffer_produces_same_result() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let input = b"test input data";

        let default_gen = DeltaGenerator::new();
        let small_gen = DeltaGenerator::new().with_buffer_len(64);

        let script1 = default_gen.generate(&input[..], &index).expect("script1");
        let script2 = small_gen.generate(&input[..], &index).expect("script2");

        assert_eq!(script1.literal_bytes(), script2.literal_bytes());
        assert_eq!(script1.total_bytes(), script2.total_bytes());
    }

    #[test]
    fn generate_delta_with_large_buffer_produces_same_result() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let input = b"test input data";

        let default_gen = DeltaGenerator::new();
        let large_gen = DeltaGenerator::new().with_buffer_len(1024 * 1024);

        let script1 = default_gen.generate(&input[..], &index).expect("script1");
        let script2 = large_gen.generate(&input[..], &index).expect("script2");

        assert_eq!(script1.literal_bytes(), script2.literal_bytes());
        assert_eq!(script1.total_bytes(), script2.total_bytes());
    }

    // Copy token tests
    #[test]
    fn generate_delta_copy_only_has_zero_literal_bytes() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let block_len = index.block_length();
        // Use exact block boundaries
        let input = basis[..block_len].to_vec();

        let script = generate_delta(&input[..], &index).expect("script");
        // Should be all copy, no literals
        assert_eq!(script.literal_bytes(), 0);
    }

    #[test]
    fn generate_delta_mixed_literal_and_copy() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        let block_len = index.block_length();

        let mut input = vec![1u8, 2u8, 3u8]; // 3 literal bytes
        input.extend_from_slice(&basis[..block_len]); // matching block
        input.extend_from_slice(b"end"); // 3 more literal bytes

        let script = generate_delta(&input[..], &index).expect("script");
        assert!(script.tokens().len() >= 2);
        assert_eq!(script.literal_bytes(), 6);
    }

    // Convenience function test
    #[test]
    fn generate_delta_convenience_function_works() {
        let basis = vec![0u8; 2048];
        let index = build_index(&basis);
        let input = b"hello";

        let script = generate_delta(&input[..], &index).expect("script");
        assert!(script.total_bytes() > 0);
    }

    #[test]
    fn delta_script_round_trip_identical_data() {
        let basis: Vec<u8> = (0..10_000).map(|b| (b % 251) as u8).collect();
        let index = build_index(&basis);
        // Use the same data as basis
        let input = basis.clone();

        let script = generate_delta(&input[..], &index).expect("script");
        // Should be mostly or all copy tokens
        assert!(script.literal_bytes() < script.total_bytes());

        let mut basis_cursor = Cursor::new(basis);
        let mut output = Vec::new();
        apply_delta(&mut basis_cursor, &mut output, &index, &script).expect("apply");
        assert_eq!(output, input);
    }
}
