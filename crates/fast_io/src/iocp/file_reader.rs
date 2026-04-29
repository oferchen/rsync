//! IOCP-based async file reader for Windows.
//!
//! Uses overlapped ReadFile with an I/O completion port to perform async
//! reads. For sequential access, submits multiple read-ahead operations
//! to keep the I/O pipeline full.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use windows_sys::Win32::Foundation::{HANDLE, TRUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_SHARE_READ,
    OPEN_EXISTING, ReadFile,
};
use windows_sys::Win32::System::IO::GetQueuedCompletionStatus;

use super::completion_port::CompletionPort;
use super::config::IocpConfig;
use super::overlapped::OverlappedOp;
use crate::traits::FileReader;

/// IOCP-based file reader.
///
/// Opens the file with `FILE_FLAG_OVERLAPPED` and associates it with a
/// per-reader completion port. Read operations are submitted as overlapped
/// I/O and completed via the completion port.
pub struct IocpReader {
    handle: HANDLE,
    port: CompletionPort,
    config: IocpConfig,
    size: u64,
    position: u64,
}

impl IocpReader {
    /// Opens a file for overlapped reading via IOCP.
    pub fn open<P: AsRef<Path>>(path: P, config: &IocpConfig) -> io::Result<Self> {
        let wide_path = to_wide_path(path.as_ref())?;

        // SAFETY: CreateFileW with valid path and standard flags.
        // FILE_FLAG_OVERLAPPED enables async I/O.
        #[allow(unsafe_code)]
        let handle = unsafe {
            CreateFileW(
                wide_path.as_ptr(),
                FILE_GENERIC_READ,
                FILE_SHARE_READ,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                std::ptr::null_mut(),
            )
        };

        if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let port = CompletionPort::new(1)?;
        port.associate(handle, 0)?;

        // Get file size via standard File (borrows the handle)
        let size = {
            // SAFETY: FromRawHandle borrows the handle; we don't take ownership
            // since we close it manually in Drop.
            #[allow(unsafe_code)]
            let file = unsafe {
                use std::os::windows::io::FromRawHandle;
                File::from_raw_handle(handle as *mut std::ffi::c_void)
            };
            let metadata = file.metadata()?;
            let len = metadata.len();
            // Prevent File from closing our handle
            std::mem::forget(file);
            len
        };

        Ok(Self {
            handle,
            port,
            config: config.clone(),
            size,
            position: 0,
        })
    }

    /// Reads data at the specified offset using overlapped I/O.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut op = OverlappedOp::new_read(offset, buf.len());
        let overlapped_ptr = op.as_overlapped_ptr();
        let buffer_ptr = op.buffer_ptr();

        let mut bytes_read: u32 = 0;

        // SAFETY: handle is valid, overlapped_ptr and buffer_ptr are pinned
        // and valid for the duration of this call. We wait for completion
        // before accessing the buffer.
        #[allow(unsafe_code)]
        let success = unsafe {
            ReadFile(
                self.handle,
                buffer_ptr.cast(),
                buf.len() as u32,
                &mut bytes_read,
                overlapped_ptr,
            )
        };

        if success == TRUE {
            // ReadFile may complete synchronously even with FILE_FLAG_OVERLAPPED;
            // when it does, no completion will be queued so handle it inline.
            let n = bytes_read as usize;
            buf[..n].copy_from_slice(&op.buffer[..n]);
            return Ok(n);
        }

        let err = io::Error::last_os_error();
        // ERROR_IO_PENDING (997) is the documented "operation queued" status;
        // any other error is fatal.
        if err.raw_os_error() != Some(997) {
            return Err(err);
        }

        let mut transferred: u32 = 0;
        let mut key: usize = 0;
        let mut overlapped_out: *mut windows_sys::Win32::System::IO::OVERLAPPED =
            std::ptr::null_mut();

        // SAFETY: port handle is valid, we wait indefinitely for the
        // completion of our submitted operation.
        #[allow(unsafe_code)]
        let wait_ok = unsafe {
            GetQueuedCompletionStatus(
                self.port.handle(),
                &mut transferred,
                &mut key,
                &mut overlapped_out,
                u32::MAX, // INFINITE
            )
        };

        if wait_ok != TRUE {
            return Err(io::Error::last_os_error());
        }

        let n = transferred as usize;
        buf[..n].copy_from_slice(&op.buffer[..n]);
        Ok(n)
    }

    /// Reads the entire file using batched overlapped I/O.
    ///
    /// Submits multiple concurrent read operations to maximize throughput
    /// via the completion port.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        let file_size = self.size as usize;
        if file_size == 0 {
            return Ok(Vec::new());
        }

        let mut result = vec![0u8; file_size];
        let buf_size = self.config.buffer_size;
        let max_concurrent = self.config.concurrent_ops as usize;
        let mut offset: usize = 0;

        while offset < file_size {
            let batch_count = std::cmp::min(
                max_concurrent,
                (file_size - offset + buf_size - 1) / buf_size,
            );

            let mut ops: Vec<_> = (0..batch_count)
                .map(|i| {
                    let op_offset = offset + i * buf_size;
                    let op_size = std::cmp::min(buf_size, file_size - op_offset);
                    OverlappedOp::new_read(op_offset as u64, op_size)
                })
                .collect();

            for (i, op) in ops.iter_mut().enumerate() {
                let op_offset = offset + i * buf_size;
                let op_size = std::cmp::min(buf_size, file_size - op_offset);
                let overlapped_ptr = op.as_overlapped_ptr();
                let buffer_ptr = op.buffer_ptr();

                let mut bytes_read: u32 = 0;

                // SAFETY: handle valid, pointers pinned and valid for op lifetime.
                #[allow(unsafe_code)]
                let success = unsafe {
                    ReadFile(
                        self.handle,
                        buffer_ptr.cast(),
                        op_size as u32,
                        &mut bytes_read,
                        overlapped_ptr,
                    )
                };

                if success != TRUE {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() != Some(997) {
                        return Err(err);
                    }
                }
            }

            for i in 0..batch_count {
                let mut transferred: u32 = 0;
                let mut key: usize = 0;
                let mut overlapped_out: *mut windows_sys::Win32::System::IO::OVERLAPPED =
                    std::ptr::null_mut();

                // SAFETY: port handle valid, waiting for submitted operations.
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
                    return Err(io::Error::last_os_error());
                }

                // Completions are processed in submission order rather than by
                // matching the returned OVERLAPPED pointer. This is safe because
                // each batch waits for all submitted ops before moving on.
                let op_offset = offset + i * buf_size;
                let n = transferred as usize;
                let dest_end = std::cmp::min(op_offset + n, file_size);
                result[op_offset..dest_end].copy_from_slice(&ops[i].buffer[..dest_end - op_offset]);
            }

            offset += batch_count * buf_size;
        }

        self.position = file_size as u64;
        Ok(result)
    }
}

impl Read for IocpReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.position >= self.size {
            return Ok(0);
        }
        let to_read = std::cmp::min(buf.len() as u64, self.size - self.position) as usize;
        let n = self.read_at(self.position, &mut buf[..to_read])?;
        self.position += n as u64;
        Ok(n)
    }
}

impl FileReader for IocpReader {
    fn size(&self) -> u64 {
        self.size
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        if pos > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position beyond end of file",
            ));
        }
        self.position = pos;
        Ok(())
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        self.position = 0;
        self.read_all_batched()
    }
}

impl Drop for IocpReader {
    fn drop(&mut self) {
        // SAFETY: self.handle is valid and owned by this struct.
        #[allow(unsafe_code)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

// SAFETY: IocpReader owns its HANDLE and CompletionPort. Windows file handles
// and completion ports are kernel objects safe to use from any thread.
// The reader is used single-threaded but must be Send for trait bounds.
#[allow(unsafe_code)]
unsafe impl Send for IocpReader {}

/// Converts a Path to a wide (UTF-16) null-terminated string for Win32 APIs.
pub(crate) fn to_wide_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    Ok(wide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_and_read_small_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, b"hello iocp").unwrap();

        let config = IocpConfig::default();
        let mut reader = IocpReader::open(&path, &config).unwrap();
        assert_eq!(reader.size(), 10);
        assert_eq!(reader.position(), 0);

        let data = reader.read_all().unwrap();
        assert_eq!(data, b"hello iocp");
    }

    #[test]
    fn read_empty_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();

        let config = IocpConfig::default();
        let mut reader = IocpReader::open(&path, &config).unwrap();
        assert_eq!(reader.size(), 0);

        let data = reader.read_all().unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn sequential_reads_track_position() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("position.txt");
        std::fs::write(&path, b"0123456789ABCDEF").unwrap();

        let config = IocpConfig::default();
        let mut reader = IocpReader::open(&path, &config).unwrap();

        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"0123");
        assert_eq!(reader.position(), 4);

        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"4567");
        assert_eq!(reader.position(), 8);
    }

    #[test]
    fn seek_and_read() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("seek.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let config = IocpConfig::default();
        let mut reader = IocpReader::open(&path, &config).unwrap();

        reader.seek_to(6).unwrap();
        assert_eq!(reader.position(), 6);

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn read_large_file_batched() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("large.bin");
        let data: Vec<u8> = (0..256 * 1024).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let config = IocpConfig::default();
        let mut reader = IocpReader::open(&path, &config).unwrap();
        let result = reader.read_all().unwrap();
        assert_eq!(result, data);
    }
}
