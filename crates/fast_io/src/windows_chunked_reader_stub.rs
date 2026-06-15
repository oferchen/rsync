//! Portable `WindowsChunkedReader` alias for non-Windows platforms.
//!
//! The real `WindowsChunkedReader` (compiled only on Windows) is a bounded-RSS
//! chunked file reader; it replaces the `mmap_reader_stub` `Vec<u8>`-per-file
//! allocation that would otherwise dominate peak RSS on large basis files. See
//! the Windows module for the full design.
//!
//! On every other target the stub simply re-exposes [`std::fs::File`] under
//! the `WindowsChunkedReader` name so cross-platform callers compile
//! unconditionally. `std::fs::File` already implements [`Read`](std::io::Read)
//! and [`Seek`](std::io::Seek), which are the only traits the chunked reader's
//! consumers require, so swapping the alias in is a zero-cost shim on Unix.
//!
//! This module is compiled when the target OS is not Windows. The Windows
//! build pulls in the real implementation under the same module path so the
//! type name is always resolvable regardless of platform.

#![cfg(not(windows))]

/// Cross-platform alias for the Windows bounded-RSS chunked reader.
///
/// On non-Windows targets this is simply [`std::fs::File`]: the standard
/// library's buffered file handle already provides the `Read + Seek` surface
/// that the Windows reader's call sites rely on, and Unix mmap callers go
/// through `mmap_reader::MmapReader` rather than the chunked path. Exposing
/// the alias here lets cross-platform code reference `WindowsChunkedReader`
/// without `#[cfg(windows)]` plumbing.
pub type WindowsChunkedReader = std::fs::File;

#[cfg(test)]
mod tests {
    use super::WindowsChunkedReader;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// Cross-platform smoke test: `WindowsChunkedReader` is nameable and
    /// openable on the current platform. On non-Windows targets this
    /// exercises the [`std::fs::File`] alias; on Windows CI the equivalent
    /// test in `windows_chunked_reader::tests` exercises the real reader.
    #[test]
    fn nameable_and_openable() {
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(b"hello").expect("write temp file");
        f.flush().expect("flush temp file");
        let _reader: WindowsChunkedReader =
            std::fs::File::open(f.path()).expect("open through alias");
    }

    /// Cross-platform parity test: the file length reported via the alias
    /// agrees with the fixture size. On Windows the reader exposes
    /// `len()`/`size()` accessors that report the same value; on
    /// non-Windows targets the alias resolves to [`std::fs::File`], so the
    /// equivalent length is read through `metadata().len()`.
    #[test]
    fn len_matches_fixture_size() {
        let payload = b"0123456789abcdef";
        let mut f = NamedTempFile::new().expect("create temp file");
        f.write_all(payload).expect("write temp file");
        f.flush().expect("flush temp file");
        let reader: WindowsChunkedReader =
            std::fs::File::open(f.path()).expect("open through alias");
        let reported = reader.metadata().expect("stat alias file").len();
        assert_eq!(reported, payload.len() as u64);
    }
}
