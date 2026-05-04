//! IOCP-based async file writer for Windows.
//!
//! Uses overlapped WriteFile with an I/O completion port to perform async
//! writes. Data is buffered internally and flushed via overlapped I/O when
//! the buffer is full or flush() is called.

use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use windows_sys::Win32::Foundation::{HANDLE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_ALWAYS, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_GENERIC_WRITE,
    FILE_SHARE_READ, FlushFileBuffers, OPEN_EXISTING, SetEndOfFile, SetFilePointerEx, WriteFile,
};
use windows_sys::Win32::System::IO::GetQueuedCompletionStatus;

use super::completion_port::CompletionPort;
use super::config::IocpConfig;
use super::error::classify_overlapped_error;
use super::overlapped::OverlappedOp;
use crate::traits::FileWriter;

/// IOCP-based file writer.
///
/// Opens the file with `FILE_FLAG_OVERLAPPED` and associates it with a
/// per-writer completion port. Write operations are buffered and submitted
/// as overlapped I/O on flush.
pub struct IocpWriter {
    handle: HANDLE,
    port: CompletionPort,
    config: IocpConfig,
    buffer: Vec<u8>,
    file_offset: u64,
    bytes_written: u64,
}

impl IocpWriter {
    /// Creates a file for overlapped writing via IOCP.
    pub fn create<P: AsRef<Path>>(path: P, config: &IocpConfig) -> io::Result<Self> {
        Self::open_with_disposition(path.as_ref(), config, CREATE_ALWAYS)
    }

    /// Reopens an existing file for overlapped writing via IOCP.
    ///
    /// Used by [`super::file_factory::writer_from_file`] when the caller hands
    /// us a `std::fs::File` opened without `FILE_FLAG_OVERLAPPED` (issue #1929).
    /// Unlike [`Self::create`], this preserves the existing file contents and
    /// positions the writer at offset 0 - callers that need to append must
    /// seek to the desired offset before writing.
    ///
    /// `_buffer_capacity` is currently informational; the IOCP writer uses
    /// `config.buffer_size` for its internal buffer. The argument is kept for
    /// API symmetry with `StdFileWriter::from_file_with_capacity`.
    pub fn create_for_append<P: AsRef<Path>>(
        path: P,
        _buffer_capacity: usize,
        config: &IocpConfig,
    ) -> io::Result<Self> {
        Self::open_with_disposition(path.as_ref(), config, OPEN_EXISTING)
    }

    /// Shared open implementation that varies only the creation disposition.
    ///
    /// `disposition` matches the `dwCreationDisposition` argument of
    /// `CreateFileW`: pass `CREATE_ALWAYS` to truncate or `OPEN_EXISTING` to
    /// reopen the file in place. Other values (`OPEN_ALWAYS`, `CREATE_NEW`,
    /// `TRUNCATE_EXISTING`) work as documented but are not exercised by the
    /// crate today.
    fn open_with_disposition(path: &Path, config: &IocpConfig, disposition: u32) -> io::Result<Self> {
        let wide_path = super::file_reader::to_wide_path(path)?;

        // SAFETY: CreateFileW with valid path and standard write flags.
        // FILE_FLAG_OVERLAPPED enables async I/O. The caller-controlled
        // disposition selects between truncation (CREATE_ALWAYS) and reopening
        // an existing file (OPEN_EXISTING).
        #[allow(unsafe_code)]
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                FILE_GENERIC_WRITE,
                FILE_SHARE_READ,
                std::ptr::null(),
                disposition,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };

        if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let port = CompletionPort::new(1)?;
        port.associate(handle, 0)?;

        Ok(Self {
            handle,
            port,
            config: config.clone(),
            buffer: Vec::with_capacity(config.buffer_size),
            file_offset: 0,
            bytes_written: 0,
        })
    }

    /// Creates a file with pre-allocated space.
    pub fn create_with_size<P: AsRef<Path>>(
        path: P,
        size: u64,
        config: &IocpConfig,
    ) -> io::Result<Self> {
        let writer = Self::create(path, config)?;

        // SAFETY: handle is valid; the SetFilePointerEx/SetEndOfFile pair is
        // the documented Win32 sequence to preallocate disk blocks for the
        // file before any overlapped write is submitted.
        #[allow(unsafe_code)]
        unsafe {
            let mut new_pos: i64 = 0;
            if SetFilePointerEx(writer.handle, size as i64, &mut new_pos, 0) != TRUE {
                let err = io::Error::last_os_error();
                windows_sys::Win32::Foundation::CloseHandle(writer.handle);
                return Err(err);
            }
            if SetEndOfFile(writer.handle) != TRUE {
                let err = io::Error::last_os_error();
                windows_sys::Win32::Foundation::CloseHandle(writer.handle);
                return Err(err);
            }
        }

        Ok(writer)
    }

    /// Writes data at the specified offset using overlapped I/O.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        let mut op = OverlappedOp::new_write(offset, data);
        let overlapped_ptr = op.as_overlapped_ptr();

        let mut bytes_written: u32 = 0;

        // SAFETY: handle valid, overlapped_ptr and data pinned and valid.
        #[allow(unsafe_code)]
        let success = unsafe {
            WriteFile(
                self.handle,
                op.buffer.as_ptr().cast(),
                data.len() as u32,
                &mut bytes_written,
                overlapped_ptr,
            )
        };

        if success == TRUE {
            return Ok(bytes_written as usize);
        }

        let err = io::Error::last_os_error();
        // ERROR_IO_PENDING (997) means the write is queued; any other error is fatal.
        if err.raw_os_error() != Some(997) {
            // Issue #1930: upgrade ERROR_INVALID_PARAMETER to a typed error
            // pointing at the most likely cause - handle not opened with
            // FILE_FLAG_OVERLAPPED. Other errors pass through unchanged.
            return Err(classify_overlapped_error(err, "WriteFile"));
        }

        let mut transferred: u32 = 0;
        let mut key: usize = 0;
        let mut overlapped_out: *mut windows_sys::Win32::System::IO::OVERLAPPED =
            std::ptr::null_mut();

        // SAFETY: port handle valid, waiting for submitted operation.
        #[allow(unsafe_code)]
        let wait_ok = unsafe {
            GetQueuedCompletionStatus(
                self.port.handle(),
                &mut transferred,
                &mut key,
                &mut overlapped_out,
                u32::MAX,
            )
        };

        if wait_ok != TRUE {
            return Err(classify_overlapped_error(
                io::Error::last_os_error(),
                "GetQueuedCompletionStatus(WriteFile)",
            ));
        }

        Ok(transferred as usize)
    }

    /// Flushes the internal buffer via overlapped write.
    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        let data = std::mem::take(&mut self.buffer);
        let mut written = 0;

        while written < data.len() {
            let chunk_size = std::cmp::min(self.config.buffer_size, data.len() - written);
            let n = self.write_at(self.file_offset, &data[written..written + chunk_size])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "overlapped write returned zero bytes",
                ));
            }
            written += n;
            self.file_offset += n as u64;
        }

        self.buffer = Vec::with_capacity(self.config.buffer_size);
        Ok(())
    }
}

impl Write for IocpWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let available = self.config.buffer_size - self.buffer.len();

        if buf.len() <= available {
            self.buffer.extend_from_slice(buf);
            self.bytes_written += buf.len() as u64;
            Ok(buf.len())
        } else if self.buffer.is_empty() && buf.len() >= self.config.buffer_size {
            // Bypass the internal buffer when the caller already provided at
            // least one full chunk: an overlapped write directly from `buf`
            // saves a copy.
            let n = self.write_at(self.file_offset, buf)?;
            self.file_offset += n as u64;
            self.bytes_written += n as u64;
            Ok(n)
        } else {
            self.buffer.extend_from_slice(&buf[..available]);
            self.flush_buffer()?;
            let remaining = &buf[available..];
            if !remaining.is_empty() {
                self.buffer.extend_from_slice(remaining);
            }
            self.bytes_written += buf.len() as u64;
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()
    }
}

impl Seek for IocpWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.flush_buffer()?;
        match pos {
            SeekFrom::Start(p) => {
                self.file_offset = p;
            }
            SeekFrom::Current(delta) => {
                self.file_offset = if delta >= 0 {
                    self.file_offset + delta as u64
                } else {
                    self.file_offset
                        .checked_sub((-delta) as u64)
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidInput, "seek before start of file")
                        })?
                };
            }
            SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "SeekFrom::End not supported for IOCP writer",
                ));
            }
        }
        Ok(self.file_offset)
    }
}

impl FileWriter for IocpWriter {
    fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    fn sync(&mut self) -> io::Result<()> {
        self.flush_buffer()?;
        // SAFETY: handle is valid.
        #[allow(unsafe_code)]
        let ok = unsafe { FlushFileBuffers(self.handle) };
        if ok != TRUE {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        // SAFETY: handle is valid.
        #[allow(unsafe_code)]
        unsafe {
            let mut new_pos: i64 = 0;
            if SetFilePointerEx(self.handle, size as i64, &mut new_pos, 0) != TRUE {
                return Err(io::Error::last_os_error());
            }
            if SetEndOfFile(self.handle) != TRUE {
                return Err(io::Error::last_os_error());
            }
            // SetEndOfFile leaves the pointer at `size`; restore it so that the
            // next overlapped write resumes at the caller's logical offset.
            if SetFilePointerEx(self.handle, self.file_offset as i64, &mut new_pos, 0) != TRUE {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

impl Drop for IocpWriter {
    fn drop(&mut self) {
        let _ = self.flush_buffer();
        // SAFETY: self.handle is valid and owned by this struct.
        #[allow(unsafe_code)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

// SAFETY: IocpWriter owns its HANDLE and CompletionPort. Windows file handles
// and completion ports are kernel objects safe to use from any thread.
// The writer is used single-threaded but must be Send for trait bounds.
#[allow(unsafe_code)]
unsafe impl Send for IocpWriter {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn create_and_write_small_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small.txt");

        let config = IocpConfig::default();
        {
            let mut writer = IocpWriter::create(&path, &config).unwrap();
            writer.write_all(b"hello iocp").unwrap();
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), 10);
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, b"hello iocp");
    }

    #[test]
    fn write_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.bin");

        let config = IocpConfig::default();
        {
            let mut writer = IocpWriter::create(&path, &config).unwrap();
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), 0);
        }

        let content = std::fs::read(&path).unwrap();
        assert!(content.is_empty());
    }

    #[test]
    fn write_large_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.bin");
        let data: Vec<u8> = (0..256 * 1024).map(|i| (i % 256) as u8).collect();

        let config = IocpConfig::default();
        {
            let mut writer = IocpWriter::create(&path, &config).unwrap();
            writer.write_all(&data).unwrap();
            writer.flush().unwrap();
        }

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, data);
    }

    #[test]
    fn write_with_multiple_flushes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multi_flush.txt");

        let config = IocpConfig::default();
        {
            let mut writer = IocpWriter::create(&path, &config).unwrap();
            writer.write_all(b"first").unwrap();
            writer.flush().unwrap();
            writer.write_all(b" second").unwrap();
            writer.flush().unwrap();
            assert_eq!(writer.bytes_written(), 12);
        }

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "first second");
    }

    #[test]
    fn sync_persists_data() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sync.txt");

        let config = IocpConfig::default();
        {
            let mut writer = IocpWriter::create(&path, &config).unwrap();
            writer.write_all(b"sync test").unwrap();
            writer.sync().unwrap();
        }

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "sync test");
    }

    #[test]
    fn create_with_size_preallocates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("prealloc.bin");

        let config = IocpConfig::default();
        {
            let mut writer = IocpWriter::create_with_size(&path, 1024, &config).unwrap();
            writer.write_all(b"data").unwrap();
            writer.flush().unwrap();
        }

        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() >= 1024);
    }
}
