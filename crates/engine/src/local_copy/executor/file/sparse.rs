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
    let mut offset = 0usize;
    let mut written = 0usize;

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
                    written = written.saturating_add(rel_zero);
                }

                let mut zero_end = zero_index + 1;
                while zero_end < chunk.len() && chunk[zero_end] == 0 {
                    zero_end += 1;
                }

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
                written = written.saturating_add(chunk.len() - offset);
                break;
            }
        }
    }

    Ok(written)
}
