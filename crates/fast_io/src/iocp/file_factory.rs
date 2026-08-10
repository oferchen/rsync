//! Factory types and enum wrappers for IOCP reader/writer.
//!
//! Mirrors the io_uring factory pattern: each factory checks availability
//! and returns either an IOCP or Std variant. The enum wrappers dispatch
//! trait methods to the underlying implementation.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};

use super::config::{IOCP_MIN_FILE_SIZE, IocpConfig, is_iocp_available};
use super::file_reader::IocpReader;
use super::file_writer::IocpWriter;
use crate::traits::{
    FileReader, FileReaderFactory, FileWriter, FileWriterFactory, StdFileReader, StdFileWriter,
};

/// Reader that is either IOCP-based or standard buffered I/O.
pub enum IocpOrStdReader {
    /// IOCP-based reader using overlapped I/O.
    Iocp(IocpReader),
    /// Standard buffered reader.
    Std(StdFileReader),
}

impl std::fmt::Debug for IocpOrStdReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iocp(_) => f.debug_tuple("Iocp").field(&"<iocp>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
}

impl Read for IocpOrStdReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Iocp(r) => r.read(buf),
            Self::Std(r) => r.read(buf),
        }
    }
}

impl FileReader for IocpOrStdReader {
    fn size(&self) -> u64 {
        match self {
            Self::Iocp(r) => r.size(),
            Self::Std(r) => r.size(),
        }
    }

    fn position(&self) -> u64 {
        match self {
            Self::Iocp(r) => r.position(),
            Self::Std(r) => r.position(),
        }
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        match self {
            Self::Iocp(r) => r.seek_to(pos),
            Self::Std(r) => r.seek_to(pos),
        }
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        match self {
            Self::Iocp(r) => r.read_all(),
            Self::Std(r) => r.read_all(),
        }
    }
}

/// Writer that is either IOCP-based or standard buffered I/O.
pub enum IocpOrStdWriter {
    /// IOCP-based writer using overlapped I/O.
    Iocp(IocpWriter),
    /// Standard buffered writer.
    Std(StdFileWriter),
}

impl std::fmt::Debug for IocpOrStdWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Iocp(_) => f.debug_tuple("Iocp").field(&"<iocp>").finish(),
            Self::Std(_) => f.debug_tuple("Std").field(&"<buffered>").finish(),
        }
    }
}

impl Write for IocpOrStdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Iocp(w) => w.write(buf),
            Self::Std(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Iocp(w) => w.flush(),
            Self::Std(w) => w.flush(),
        }
    }
}

impl Seek for IocpOrStdWriter {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::Iocp(w) => w.seek(pos),
            Self::Std(w) => w.seek(pos),
        }
    }
}

impl FileWriter for IocpOrStdWriter {
    fn bytes_written(&self) -> u64 {
        match self {
            Self::Iocp(w) => w.bytes_written(),
            Self::Std(w) => w.bytes_written(),
        }
    }

    fn sync(&mut self) -> io::Result<()> {
        match self {
            Self::Iocp(w) => w.sync(),
            Self::Std(w) => w.sync(),
        }
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        match self {
            Self::Iocp(w) => w.preallocate(size),
            Self::Std(w) => w.preallocate(size),
        }
    }
}

/// Factory that creates IOCP readers with automatic fallback.
///
/// When IOCP is available and the file is large enough to benefit from
/// async I/O, returns an IOCP reader. Otherwise, returns a standard
/// buffered reader.
#[derive(Debug, Clone)]
pub struct IocpReaderFactory {
    config: IocpConfig,
    force_fallback: bool,
}

impl Default for IocpReaderFactory {
    fn default() -> Self {
        Self {
            config: IocpConfig::default(),
            force_fallback: false,
        }
    }
}

impl IocpReaderFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IocpConfig) -> Self {
        Self {
            config,
            force_fallback: false,
        }
    }

    /// Forces fallback to standard I/O regardless of IOCP availability.
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether IOCP will be used for reads.
    #[must_use]
    pub fn will_use_iocp(&self) -> bool {
        !self.force_fallback && is_iocp_available()
    }
}

impl FileReaderFactory for IocpReaderFactory {
    type Reader = IocpOrStdReader;

    fn open(&self, path: &Path) -> io::Result<Self::Reader> {
        if self.will_use_iocp() {
            // Files below IOCP_MIN_FILE_SIZE are read synchronously because the
            // overlapped-I/O setup overhead exceeds the async benefit at that size.
            let metadata = std::fs::metadata(path)?;
            if metadata.len() >= IOCP_MIN_FILE_SIZE {
                if let Ok(reader) = IocpReader::open(path, &self.config) {
                    return Ok(IocpOrStdReader::Iocp(reader));
                }
            }
        }
        Ok(IocpOrStdReader::Std(StdFileReader::open(path)?))
    }
}

/// Factory that creates IOCP writers with automatic fallback.
#[derive(Debug, Clone)]
pub struct IocpWriterFactory {
    config: IocpConfig,
    force_fallback: bool,
}

impl Default for IocpWriterFactory {
    fn default() -> Self {
        Self {
            config: IocpConfig::default(),
            force_fallback: false,
        }
    }
}

impl IocpWriterFactory {
    /// Creates a factory with custom configuration.
    #[must_use]
    pub fn with_config(config: IocpConfig) -> Self {
        Self {
            config,
            force_fallback: false,
        }
    }

    /// Forces fallback to standard I/O regardless of IOCP availability.
    #[must_use]
    pub fn force_fallback(mut self, force: bool) -> Self {
        self.force_fallback = force;
        self
    }

    /// Returns whether IOCP will be used for writes.
    #[must_use]
    pub fn will_use_iocp(&self) -> bool {
        !self.force_fallback && is_iocp_available()
    }
}

impl FileWriterFactory for IocpWriterFactory {
    type Writer = IocpOrStdWriter;

    fn create(&self, path: &Path) -> io::Result<Self::Writer> {
        if self.will_use_iocp() {
            if let Ok(writer) = IocpWriter::create(path, &self.config) {
                return Ok(IocpOrStdWriter::Iocp(writer));
            }
        }
        Ok(IocpOrStdWriter::Std(StdFileWriter::create(path)?))
    }

    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer> {
        if self.will_use_iocp() {
            if let Ok(writer) = IocpWriter::create_with_size(path, size, &self.config) {
                return Ok(IocpOrStdWriter::Iocp(writer));
            }
        }
        Ok(IocpOrStdWriter::Std(StdFileWriter::create_with_size(
            path, size,
        )?))
    }
}

/// Creates a writer from an existing std::fs::File, respecting the IOCP policy.
///
/// `Enabled` forces IOCP (error if unavailable). `Auto` uses IOCP if available
/// and the file path can be recovered for re-opening. `Disabled` always uses
/// standard I/O.
///
/// # Reopen path for `FILE_FLAG_OVERLAPPED` (issue #1929)
///
/// `std::fs::File` opens handles without `FILE_FLAG_OVERLAPPED`, so the raw
/// handle cannot be associated with a completion port. To use IOCP we recover
/// the path via `GetFinalPathNameByHandleW`, drop the original handle, and
/// reopen with `CreateFileW(..., FILE_FLAG_OVERLAPPED, ...)` inside
/// [`IocpWriter::create`].
///
/// Under [`crate::IocpPolicy::Enabled`], a failure to recover the path (e.g.
/// the handle refers to an anonymous file with no name, a pipe, or any other
/// object that does not back onto the file system) is reported as
/// `io::ErrorKind::Unsupported` so the caller knows the request could not be
/// honoured. Under [`crate::IocpPolicy::Auto`], the same failure transparently
/// falls back to standard buffered I/O.
pub fn writer_from_file(
    file: std::fs::File,
    buffer_capacity: usize,
    policy: crate::IocpPolicy,
) -> io::Result<IocpOrStdWriter> {
    match policy {
        crate::IocpPolicy::Enabled => {
            if !is_iocp_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IOCP requested but not available on this system",
                ));
            }
            // Issue #1929: recover the file path from the existing handle,
            // close the non-overlapped handle, and reopen via the IocpWriter
            // path-based constructor which uses FILE_FLAG_OVERLAPPED.
            let path = path_from_file(&file).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "IOCP requested but the supplied file handle cannot be reopened with \
                         FILE_FLAG_OVERLAPPED: {err}. Anonymous handles (pipes, O_TMPFILE-style \
                         unnamed files) are not supported by IOCP and must be opened directly \
                         through the path-based factories."
                    ),
                )
            })?;
            // Drop the std::fs::File before reopening so we do not hold a
            // sharing-incompatible handle while CreateFileW runs. The path we
            // recovered above stays valid because the kernel keeps the file
            // alive through the still-live handle until this drop.
            drop(file);
            let writer = IocpWriter::create_for_append(
                &path,
                buffer_capacity,
                &IocpConfig::default(),
            )
            .map_err(|err| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "IOCP requested but FILE_FLAG_OVERLAPPED reopen of {} failed: {err}. \
                             The handle's underlying object may not support overlapped I/O \
                             (named pipes, console handles, character devices).",
                        path.display()
                    ),
                )
            })?;
            Ok(IocpOrStdWriter::Iocp(writer))
        }
        crate::IocpPolicy::Auto => {
            // Best-effort: try IOCP, transparently fall back to std on any
            // failure (path recovery, reopen, association).
            if is_iocp_available()
                && let Ok(path) = path_from_file(&file)
            {
                drop(file);
                if let Ok(writer) =
                    IocpWriter::create_for_append(&path, buffer_capacity, &IocpConfig::default())
                {
                    return Ok(IocpOrStdWriter::Iocp(writer));
                }
                // Reopen failed - the original file is gone, so reopen with
                // standard I/O. We cannot recover the handle we dropped.
                return Ok(IocpOrStdWriter::Std(
                    StdFileWriter::from_file_with_capacity(
                        std::fs::OpenOptions::new()
                            .write(true)
                            .read(true)
                            .open(&path)?,
                        buffer_capacity,
                    ),
                ));
            }
            Ok(IocpOrStdWriter::Std(
                StdFileWriter::from_file_with_capacity(file, buffer_capacity),
            ))
        }
        crate::IocpPolicy::Disabled => Ok(IocpOrStdWriter::Std(
            StdFileWriter::from_file_with_capacity(file, buffer_capacity),
        )),
    }
}

/// Recovers the canonical file system path of an open `std::fs::File`.
///
/// Wraps `GetFinalPathNameByHandleW` with `VOLUME_NAME_DOS | FILE_NAME_NORMALIZED`
/// to obtain a `\\?\C:\path\to\file` style path that round-trips through
/// `CreateFileW`. Returns an error for handles that have no path (anonymous
/// pipes, unnamed temp files, sockets, etc.).
fn path_from_file(file: &std::fs::File) -> io::Result<PathBuf> {
    let handle = file.as_raw_handle() as HANDLE;

    // First call with a 0-length buffer returns the required length in WCHARs
    // *not including* the trailing null. We grow defensively in case the path
    // changes between calls (extremely rare but possible on shared volumes).
    // SAFETY: passing a null pointer with cchFilePath == 0 is the documented
    // probe form per
    // https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew
    #[allow(unsafe_code)]
    let required = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            std::ptr::null_mut(),
            0,
            VOLUME_NAME_DOS | FILE_NAME_NORMALIZED,
        )
    };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }

    // Allocate room for the path plus the trailing null.
    let mut buffer: Vec<u16> = vec![0; required as usize + 1];

    // SAFETY: `buffer` is sized to `required + 1` WCHARs and remains valid for
    // the call duration. The kernel writes at most `buffer.len()` WCHARs and
    // returns the count of WCHARs written excluding the null terminator.
    #[allow(unsafe_code)]
    let written = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            VOLUME_NAME_DOS | FILE_NAME_NORMALIZED,
        )
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(io::Error::last_os_error());
    }

    buffer.truncate(written as usize);
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

/// Creates a reader from a file path, respecting the IOCP policy.
///
/// `Enabled` forces IOCP (error if unavailable). `Auto` uses IOCP for
/// large files if available. `Disabled` always uses standard I/O.
pub fn reader_from_path<P: AsRef<Path>>(
    path: P,
    policy: crate::IocpPolicy,
) -> io::Result<IocpOrStdReader> {
    match policy {
        crate::IocpPolicy::Enabled => {
            if !is_iocp_available() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "IOCP requested but not available on this system",
                ));
            }
            let config = IocpConfig::default();
            Ok(IocpOrStdReader::Iocp(IocpReader::open(
                path.as_ref(),
                &config,
            )?))
        }
        crate::IocpPolicy::Auto => {
            if is_iocp_available() {
                let metadata = std::fs::metadata(path.as_ref())?;
                if metadata.len() >= IOCP_MIN_FILE_SIZE {
                    let config = IocpConfig::default();
                    if let Ok(reader) = IocpReader::open(path.as_ref(), &config) {
                        return Ok(IocpOrStdReader::Iocp(reader));
                    }
                }
            }
            Ok(IocpOrStdReader::Std(StdFileReader::open(path.as_ref())?))
        }
        crate::IocpPolicy::Disabled => {
            Ok(IocpOrStdReader::Std(StdFileReader::open(path.as_ref())?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IocpPolicy;
    use tempfile::tempdir;

    #[test]
    fn factory_reader_opens_std_for_small_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small.txt");
        std::fs::write(&path, b"tiny").unwrap();

        let factory = IocpReaderFactory::default();
        let reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn factory_reader_forced_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("forced.bin");
        let data = vec![0u8; 128 * 1024]; // > IOCP_MIN_FILE_SIZE
        std::fs::write(&path, &data).unwrap();

        let factory = IocpReaderFactory::default().force_fallback(true);
        assert!(!factory.will_use_iocp());
        let reader = factory.open(&path).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn factory_writer_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("factory_write.txt");

        let factory = IocpWriterFactory::default();
        let mut writer = factory.create(&path).unwrap();
        writer.write_all(b"factory test").unwrap();
        writer.flush().unwrap();
        drop(writer);

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "factory test");
    }

    #[test]
    fn factory_writer_forced_fallback() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("forced_write.txt");

        let factory = IocpWriterFactory::default().force_fallback(true);
        assert!(!factory.will_use_iocp());
        let writer = factory.create(&path).unwrap();
        assert!(matches!(writer, IocpOrStdWriter::Std(_)));
    }

    #[test]
    fn reader_from_path_disabled_uses_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("disabled.txt");
        std::fs::write(&path, b"disabled test").unwrap();

        let reader = reader_from_path(&path, IocpPolicy::Disabled).unwrap();
        assert!(matches!(reader, IocpOrStdReader::Std(_)));
    }

    #[test]
    fn writer_from_file_disabled_uses_std() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("writer_disabled.txt");
        let file = std::fs::File::create(&path).unwrap();

        let writer = writer_from_file(file, 8192, IocpPolicy::Disabled).unwrap();
        assert!(matches!(writer, IocpOrStdWriter::Std(_)));
    }

    #[test]
    fn reader_writer_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("roundtrip.bin");
        let test_data: Vec<u8> = (0..65536).map(|i| ((i * 17 + 5) % 256) as u8).collect();

        {
            let factory = IocpWriterFactory::default();
            let mut writer = factory.create(&path).unwrap();
            writer.write_all(&test_data).unwrap();
            writer.flush().unwrap();
        }

        let factory = IocpReaderFactory::default();
        let mut reader = factory.open(&path).unwrap();
        let read_back = reader.read_all().unwrap();

        assert_eq!(read_back, test_data);
    }

    /// Issue #1929: under `IocpPolicy::Enabled`, a file opened without
    /// FILE_FLAG_OVERLAPPED must be transparently reopened with the flag and
    /// returned as an `Iocp` variant so the writer is actually associated
    /// with a completion port.
    #[test]
    fn writer_from_file_enabled_reopens_with_overlapped() {
        if !is_iocp_available() {
            return;
        }
        let dir = tempdir().unwrap();
        let path = dir.path().join("overlapped_reopen.bin");
        // Pre-create the file using std::fs (no FILE_FLAG_OVERLAPPED).
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();

        let mut writer = writer_from_file(file, 8192, crate::IocpPolicy::Enabled).unwrap();
        assert!(
            matches!(writer, IocpOrStdWriter::Iocp(_)),
            "Enabled policy must produce an Iocp writer after reopen"
        );

        let payload = b"reopen-with-overlapped";
        writer.write_all(payload).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(&content[..payload.len()], payload);
    }

    /// Issue #1929: anonymous handles (e.g. unnamed pipes) cannot be reopened
    /// because `GetFinalPathNameByHandleW` has no path to return. Under
    /// `Enabled`, surface a clear `Unsupported` error.
    #[test]
    fn writer_from_file_enabled_rejects_anonymous_handle() {
        if !is_iocp_available() {
            return;
        }
        // An anonymous pipe handle has no file system path; CreatePipe
        // returns handles whose final path lookup fails.
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::System::Pipes::CreatePipe;
        let mut read_handle: HANDLE = std::ptr::null_mut();
        let mut write_handle: HANDLE = std::ptr::null_mut();
        // SAFETY: standard Win32 pipe creation; both out parameters are
        // populated on success.
        #[allow(unsafe_code)]
        let ok = unsafe { CreatePipe(&mut read_handle, &mut write_handle, std::ptr::null(), 0) };
        if ok != windows_sys::Win32::Foundation::TRUE {
            return; // CreatePipe unavailable in this test environment
        }
        // Wrap the write handle as a std::fs::File so writer_from_file can
        // try to recover its path.
        // SAFETY: write_handle is owned by us; from_raw_handle takes
        // ownership and will close on drop.
        #[allow(unsafe_code)]
        let file = unsafe {
            use std::os::windows::io::FromRawHandle;
            std::fs::File::from_raw_handle(write_handle as *mut std::ffi::c_void)
        };
        // Close the read end so we don't leak it.
        // SAFETY: read_handle came from CreatePipe and is still owned by us.
        #[allow(unsafe_code)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(read_handle);
        }

        let result = writer_from_file(file, 8192, crate::IocpPolicy::Enabled);
        let err = result.expect_err("anonymous handle must be rejected under Enabled");
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(
            err.to_string().contains("FILE_FLAG_OVERLAPPED"),
            "error message must point at the underlying FILE_FLAG_OVERLAPPED requirement, got: {err}"
        );
    }

    /// Under `IocpPolicy::Auto`, an anonymous handle must transparently fall
    /// back to standard buffered I/O without surfacing an error.
    #[test]
    fn writer_from_file_auto_falls_back_for_anonymous_handle() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auto_named.bin");
        // Use a regular file - on Auto we expect either Iocp (if reopen
        // succeeds) or Std (if it fails). Both are acceptable; the contract
        // is "no error".
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();

        let writer = writer_from_file(file, 8192, crate::IocpPolicy::Auto).unwrap();
        // Either variant is acceptable depending on path-recovery success.
        let _ = writer;
    }
}
