use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::local_copy::LocalCopyError;

#[derive(Default)]
pub(crate) struct SparseWriteState {
    pending_zero_run: u64,
}

impl SparseWriteState {
    fn accumulate(&mut self, additional: usize) {
        self.pending_zero_run = self.pending_zero_run.saturating_add(additional as u64);
    }

    fn flush(&mut self, writer: &mut fs::File, destination: &Path) -> Result<(), LocalCopyError> {
        if self.pending_zero_run == 0 {
            return Ok(());
        }

        let mut remaining = self.pending_zero_run;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            writer
                .seek(SeekFrom::Current(step as i64))
                .map_err(|error| {
                    LocalCopyError::io("seek in destination file", destination.to_path_buf(), error)
                })?;
            remaining -= step;
        }

        self.pending_zero_run = 0;
        Ok(())
    }

    fn replace(&mut self, next_run: usize) {
        self.pending_zero_run = next_run as u64;
    }

    pub(crate) fn finish(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        self.flush(writer, destination)
    }
}

pub(crate) fn write_sparse_chunk(
    writer: &mut fs::File,
    state: &mut SparseWriteState,
    chunk: &[u8],
    destination: &Path,
) -> Result<usize, LocalCopyError> {
    if chunk.is_empty() {
        return Ok(0);
    }

    let leading = leading_zero_run(chunk);
    state.accumulate(leading);

    if leading == chunk.len() {
        return Ok(chunk.len());
    }

    let trailing = trailing_zero_run(&chunk[leading..]);
    let data_start = leading;
    let data_end = chunk.len() - trailing;

    if data_end > data_start {
        state.flush(writer, destination)?;
        writer
            .write_all(&chunk[data_start..data_end])
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?;
    }

    state.replace(trailing);

    Ok(chunk.len())
}

#[inline]
fn leading_zero_run(bytes: &[u8]) -> usize {
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

    offset + leading_zero_run_scalar(iter.remainder())
}

#[inline]
fn leading_zero_run_scalar(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|&&byte| byte == 0).count()
}

#[inline]
fn trailing_zero_run(bytes: &[u8]) -> usize {
    let mut offset = 0usize;
    let mut buffer = [0u8; 16];
    let mut iter = bytes.rchunks_exact(16);

    for chunk in &mut iter {
        buffer.copy_from_slice(chunk);
        if u128::from_ne_bytes(buffer) == 0 {
            offset += 16;
            continue;
        }

        let trailing = chunk.iter().rev().take_while(|&&byte| byte == 0).count();
        return offset + trailing;
    }

    offset + trailing_zero_run_scalar(iter.remainder())
}

#[inline]
fn trailing_zero_run_scalar(bytes: &[u8]) -> usize {
    bytes.iter().rev().take_while(|&&byte| byte == 0).count()
}

#[cfg(test)]
mod tests {
    use super::{
        SparseWriteState, leading_zero_run, leading_zero_run_scalar, trailing_zero_run,
        trailing_zero_run_scalar, write_sparse_chunk,
    };
    use std::io::{Read, Seek, SeekFrom};
    use tempfile::NamedTempFile;

    #[test]
    fn leading_zero_run_matches_scalar_reference() {
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
                leading_zero_run(case),
                leading_zero_run_scalar(case),
                "leading zero-run length mismatch for {case:?}"
            );
        }

        let mut long = vec![0u8; 512];
        assert_eq!(leading_zero_run(&long), long.len());
        long[511] = 42;
        assert_eq!(leading_zero_run(&long), 511);
        long.push(0);
        assert_eq!(leading_zero_run(&long[511..]), 0);
        assert_eq!(leading_zero_run(&long[512..]), 1);
    }

    #[test]
    fn trailing_zero_run_matches_scalar_reference() {
        let cases: &[&[u8]] = &[
            &[],
            &[0],
            &[0, 0, 0],
            &[0, 0, 1, 0, 0],
            &[0, 7, 0, 0, 0],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &[0, 1],
            &[1, 0],
            &[1, 2, 3, 0, 0, 0],
        ];

        for case in cases {
            assert_eq!(
                trailing_zero_run(case),
                trailing_zero_run_scalar(case),
                "trailing zero-run length mismatch for {case:?}"
            );
        }

        let mut long = vec![0u8; 512];
        assert_eq!(trailing_zero_run(&long), long.len());
        long[0] = 42;
        assert_eq!(trailing_zero_run(&long), 511);
        long.insert(0, 0);
        assert_eq!(trailing_zero_run(&long[..512]), 510);
    }

    #[test]
    fn sparse_writer_accumulates_zero_runs_across_chunks() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let first = [b'A', b'B', 0, 0, 0];
        write_sparse_chunk(file.as_file_mut(), &mut state, &first, path.as_path())
            .expect("write first chunk");

        let second = [0, 0, b'C', b'D'];
        write_sparse_chunk(file.as_file_mut(), &mut state, &second, path.as_path())
            .expect("write second chunk");

        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("finalise sparse writer");

        let total = (first.len() + second.len()) as u64;
        file.as_file_mut()
            .set_len(total)
            .expect("truncate file to final length");
        file.as_file_mut()
            .seek(SeekFrom::Start(0))
            .expect("rewind for verification");

        let mut buffer = vec![0u8; total as usize];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back contents");

        assert_eq!(&buffer[0..2], b"AB");
        assert!(buffer[2..7].iter().all(|&byte| byte == 0));
        assert_eq!(&buffer[7..9], b"CD");
    }

    #[test]
    fn sparse_writer_flushes_trailing_zero_run() {
        let mut file = NamedTempFile::new().expect("temp file");
        let path = file.path().to_path_buf();
        let mut state = SparseWriteState::default();

        let chunk = [b'Z', 0, 0, 0, 0];
        write_sparse_chunk(file.as_file_mut(), &mut state, &chunk, path.as_path())
            .expect("write chunk");
        state
            .finish(file.as_file_mut(), path.as_path())
            .expect("flush trailing zeros");

        file.as_file_mut()
            .set_len(chunk.len() as u64)
            .expect("truncate file");
        file.as_file_mut().seek(SeekFrom::Start(0)).expect("rewind");

        let mut buffer = vec![0u8; chunk.len()];
        file.as_file_mut()
            .read_exact(&mut buffer)
            .expect("read back data");

        assert_eq!(buffer[0], b'Z');
        assert!(buffer[1..].iter().all(|&byte| byte == 0));
    }
}
