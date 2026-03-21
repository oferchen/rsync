//! Sequential (non-pipelined) checksum computation.

use std::io::{self, Read};

use crate::strong::StrongDigest;

use super::types::{ChecksumInput, ChecksumResult, PipelineConfig};

/// Computes checksums sequentially without thread parallelism.
///
/// Processes each input one at a time. Lower overhead than the pipelined
/// path for small workloads.
///
/// # Errors
///
/// Returns an error if reading from any input fails.
pub fn sequential_checksum<D, R>(
    inputs: Vec<ChecksumInput<R>>,
    config: PipelineConfig,
) -> io::Result<Vec<ChecksumResult<D::Digest>>>
where
    D: StrongDigest,
    D::Seed: Default,
    R: Read,
{
    let mut results = Vec::with_capacity(inputs.len());

    for input in inputs {
        let mut reader = input.reader;
        let mut hasher = D::new();
        let mut total_bytes = 0u64;
        let mut buffer = vec![0u8; config.buffer_size];

        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
            total_bytes += bytes_read as u64;
        }

        results.push(ChecksumResult {
            digest: hasher.finalize(),
            bytes_processed: total_bytes,
        });
    }

    Ok(results)
}
