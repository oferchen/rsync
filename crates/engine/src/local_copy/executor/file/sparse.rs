use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::local_copy::LocalCopyError;
use memchr::memchr;

pub(crate) fn write_sparse_chunk(
    writer: &mut fs::File,
    chunk: &[u8],
    destination: &Path,
) -> Result<usize, LocalCopyError> {
    if chunk.is_empty() {
        return Ok(0);
    }

    let mut offset = 0usize;

    while offset < chunk.len() {
        match memchr(0, &chunk[offset..]) {
            Some(rel_zero) => {
                let zero_index = offset + rel_zero;

                if rel_zero > 0 {
                    writer
                        .write_all(&chunk[offset..zero_index])
                        .map_err(|error| {
                            LocalCopyError::io("copy file", destination.to_path_buf(), error)
                        })?;
                }

                let zero_run = zero_run_length(&chunk[zero_index..]);
                let zero_end = zero_index + zero_run;

                let span = zero_end - zero_index;
                if span > 0 {
                    writer
                        .seek(SeekFrom::Current(span as i64))
                        .map_err(|error| {
                            LocalCopyError::io(
                                "seek in destination file",
                                destination.to_path_buf(),
                                error,
                            )
                        })?;
                }

                offset = zero_end;
            }
            None => {
                writer.write_all(&chunk[offset..]).map_err(|error| {
                    LocalCopyError::io("copy file", destination.to_path_buf(), error)
                })?;
                break;
            }
        }
    }

    Ok(chunk.len())
}

#[inline]
fn zero_run_length(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut buffer = [0u8; 16];
    let mut iter = bytes.chunks_exact(16);

    for chunk in &mut iter {
        buffer.copy_from_slice(chunk);
        if u128::from_ne_bytes(buffer) == 0 {
            offset += 16;
            continue;
        }

        let position = chunk.iter().position(|&byte| byte != 0).unwrap_or(16);
        return offset + position;
    }

    offset + zero_run_length_scalar(iter.remainder())
}

#[inline]
fn zero_run_length_scalar(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|&&byte| byte == 0).count()
}

#[cfg(test)]
mod tests {
    use super::{zero_run_length, zero_run_length_scalar};

    #[test]
    fn zero_run_length_matches_scalar_reference() {
        let cases: &[&[u8]] = &[
            &[],
            &[0],
            &[0, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 7, 0, 0, 0],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1],
        ];

        for case in cases {
            assert_eq!(
                zero_run_length(case),
                zero_run_length_scalar(case),
                "zero-run length mismatch for {case:?}"
            );
        }

        let mut long = vec![0u8; 512];
        assert_eq!(zero_run_length(&long), long.len());
        long[511] = 42;
        assert_eq!(zero_run_length(&long), 511);
        long.push(0);
        assert_eq!(zero_run_length(&long[511..]), 0);
        assert_eq!(zero_run_length(&long[512..]), 1);
    }
}
