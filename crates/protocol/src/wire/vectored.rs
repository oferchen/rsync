//! Vectored I/O (scatter/gather) for efficient multi-buffer writes.
//!
//! Uses `write_vectored` (writev on Unix) to send multiple buffers
//! in a single syscall. Falls back to sequential writes when the
//! writer doesn't support vectored I/O or when there are few buffers.

use std::io::{self, IoSlice, Write};

/// Minimum number of buffers to use vectored writes.
/// Below this, sequential writes have less overhead.
const VECTORED_THRESHOLD: usize = 2;

/// Maximum number of IoSlice entries per writev call.
/// Linux IOV_MAX is typically 1024.
const MAX_IOV: usize = 1024;

/// Writes multiple buffers using vectored I/O when beneficial,
/// falling back to sequential writes for few buffers.
///
/// # Arguments
/// * `writer` - The destination writer
/// * `buffers` - Slice of byte slices to write
///
/// # Returns
/// Total bytes written across all buffers.
pub fn write_vectored_all<W: Write>(writer: &mut W, buffers: &[&[u8]]) -> io::Result<u64> {
    if buffers.is_empty() {
        return Ok(0);
    }

    if buffers.len() < VECTORED_THRESHOLD {
        return write_sequential(writer, buffers);
    }

    write_vectored_impl(writer, buffers)
}

/// Vectored write implementation using IoSlice.
fn write_vectored_impl<W: Write>(writer: &mut W, buffers: &[&[u8]]) -> io::Result<u64> {
    let mut total: u64 = 0;

    // Process in chunks of MAX_IOV
    for chunk in buffers.chunks(MAX_IOV) {
        let io_slices: Vec<IoSlice<'_>> = chunk.iter().map(|b| IoSlice::new(b)).collect();

        // write_vectored may not write all data in one call
        let mut slices = &io_slices[..];

        while !slices.is_empty() {
            match writer.write_vectored(slices) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "write_vectored returned 0",
                    ));
                }
                Ok(n) => {
                    total += n as u64;
                    // Advance past fully written slices
                    let mut remaining = n;
                    while !slices.is_empty() && remaining >= slices[0].len() {
                        remaining -= slices[0].len();
                        slices = &slices[1..];
                    }
                    if remaining > 0 && !slices.is_empty() {
                        // Partial write of a slice — fall back to sequential for remainder
                        let partial_buf = &chunk[chunk.len() - slices.len()][remaining..];
                        writer.write_all(partial_buf)?;
                        total += partial_buf.len() as u64;
                        slices = &slices[1..];
                        // Write remaining slices sequentially
                        for s in slices {
                            writer.write_all(s)?;
                            total += s.len() as u64;
                        }
                        break;
                    }
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    Ok(total)
}

/// Sequential write fallback.
fn write_sequential<W: Write>(writer: &mut W, buffers: &[&[u8]]) -> io::Result<u64> {
    let mut total: u64 = 0;
    for buf in buffers {
        writer.write_all(buf)?;
        total += buf.len() as u64;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_vectored_empty() {
        let mut writer = Vec::new();
        let buffers: &[&[u8]] = &[];
        let written = write_vectored_all(&mut writer, buffers).expect("empty write succeeds");
        assert_eq!(written, 0);
        assert!(writer.is_empty());
    }

    #[test]
    fn write_vectored_single_buffer() {
        let mut writer = Vec::new();
        let data = b"hello";
        let buffers = &[&data[..]];
        let written = write_vectored_all(&mut writer, buffers).expect("single buffer write succeeds");
        assert_eq!(written, 5);
        assert_eq!(&writer, b"hello");
    }

    #[test]
    fn write_vectored_multiple_buffers() {
        let mut writer = Vec::new();
        let buf1 = b"hello";
        let buf2 = b" ";
        let buf3 = b"world";
        let buffers = &[&buf1[..], &buf2[..], &buf3[..]];
        let written = write_vectored_all(&mut writer, buffers).expect("multiple buffer write succeeds");
        assert_eq!(written, 11);
        assert_eq!(&writer, b"hello world");
    }

    #[test]
    fn write_vectored_data_integrity() {
        let mut writer = Vec::new();
        let buf1 = b"The quick brown fox";
        let buf2 = b" jumps over";
        let buf3 = b" the lazy dog";
        let buffers = &[&buf1[..], &buf2[..], &buf3[..]];

        let written = write_vectored_all(&mut writer, buffers).expect("write succeeds");

        let expected = b"The quick brown fox jumps over the lazy dog";
        assert_eq!(written, expected.len() as u64);
        assert_eq!(&writer, expected);
    }

    #[test]
    fn write_vectored_large_payload() {
        let mut writer = Vec::new();

        // Create 100 small buffers
        let data: Vec<Vec<u8>> = (0..100)
            .map(|i| format!("buffer-{:03}", i).into_bytes())
            .collect();

        let buffer_refs: Vec<&[u8]> = data.iter().map(|v| v.as_slice()).collect();

        let written = write_vectored_all(&mut writer, &buffer_refs).expect("large write succeeds");

        // Verify all data was written
        let mut expected = Vec::new();
        for d in &data {
            expected.extend_from_slice(d);
        }

        assert_eq!(written, expected.len() as u64);
        assert_eq!(&writer, &expected);
    }

    #[test]
    fn write_sequential_fallback() {
        let mut writer = Vec::new();
        let buf1 = b"test";
        let buf2 = b"data";
        let buffers = &[&buf1[..], &buf2[..]];

        // Call the sequential function directly
        let written = write_sequential(&mut writer, buffers).expect("sequential write succeeds");

        assert_eq!(written, 8);
        assert_eq!(&writer, b"testdata");
    }

    #[test]
    fn threshold_is_reasonable() {
        // Threshold should be at least 2 to make vectored I/O worthwhile
        assert!(VECTORED_THRESHOLD >= 2, "VECTORED_THRESHOLD must be >= 2");
    }

    #[test]
    fn write_vectored_handles_partial_writes() {
        struct PartialWriter {
            data: Vec<u8>,
            chunk_size: usize,
        }

        impl Write for PartialWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let take = buf.len().min(self.chunk_size);
                self.data.extend_from_slice(&buf[..take]);
                Ok(take)
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                let mut remaining = self.chunk_size;
                let mut written = 0;

                for buf in bufs {
                    if buf.is_empty() || remaining == 0 {
                        break;
                    }

                    let take = remaining.min(buf.len());
                    self.data.extend_from_slice(&buf[..take]);
                    remaining -= take;
                    written += take;

                    if take < buf.len() {
                        break;
                    }
                }

                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = PartialWriter {
            data: Vec::new(),
            chunk_size: 3,
        };

        let buf1 = b"hello";
        let buf2 = b"world";
        let buffers = &[&buf1[..], &buf2[..]];

        let written = write_vectored_all(&mut writer, buffers).expect("partial write succeeds");

        assert_eq!(written, 10);
        assert_eq!(&writer.data, b"helloworld");
    }

    #[test]
    fn write_vectored_handles_interrupted() {
        struct InterruptOnceWriter {
            data: Vec<u8>,
            interrupted: bool,
        }

        impl Write for InterruptOnceWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.data.extend_from_slice(buf);
                Ok(buf.len())
            }

            fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "EINTR"));
                }

                let mut written = 0;
                for buf in bufs {
                    self.data.extend_from_slice(buf);
                    written += buf.len();
                }
                Ok(written)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = InterruptOnceWriter {
            data: Vec::new(),
            interrupted: false,
        };

        let buf1 = b"hello";
        let buf2 = b"world";
        let buffers = &[&buf1[..], &buf2[..]];

        let written = write_vectored_all(&mut writer, buffers).expect("interrupted write retries and succeeds");

        assert!(writer.interrupted, "should have been interrupted once");
        assert_eq!(written, 10);
        assert_eq!(&writer.data, b"helloworld");
    }

    #[test]
    fn write_vectored_returns_error_on_write_zero() {
        struct ZeroWriter;

        impl Write for ZeroWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Ok(0)
            }

            fn write_vectored(&mut self, _bufs: &[IoSlice<'_>]) -> io::Result<usize> {
                Ok(0)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = ZeroWriter;
        let buf1 = b"hello";
        let buf2 = b"world";
        let buffers = &[&buf1[..], &buf2[..]];

        let result = write_vectored_all(&mut writer, buffers);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn write_vectored_exceeds_max_iov() {
        let mut writer = Vec::new();

        // Create more buffers than MAX_IOV
        let data: Vec<Vec<u8>> = (0..2000)
            .map(|i| format!("{}", i % 10).into_bytes())
            .collect();

        let buffer_refs: Vec<&[u8]> = data.iter().map(|v| v.as_slice()).collect();

        let written = write_vectored_all(&mut writer, &buffer_refs).expect("large IOV write succeeds");

        // Verify all data was written
        let mut expected = Vec::new();
        for d in &data {
            expected.extend_from_slice(d);
        }

        assert_eq!(written, expected.len() as u64);
        assert_eq!(&writer, &expected);
    }
}
