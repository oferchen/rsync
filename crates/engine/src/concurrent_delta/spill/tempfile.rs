//! Tempfile-backed storage primitives for the spill layer.
//!
//! Encapsulates the two flavours of disk backing used by
//! [`SpillableReorderBuffer`](super::SpillableReorderBuffer):
//!
//! - `Spooled` - the default. Wraps `tempfile::SpooledTempFile`, which keeps
//!   small spills in memory and rolls over to disk past a threshold.
//! - `Directory` - opens a single anonymous tempfile inside a caller-provided
//!   directory.
//!
//! The abstraction is intentionally narrow: the reorder buffer only needs a
//! single read/write/seek handle plus a way to open one. Keeping the tempfile
//! plumbing isolated here lets the buffer module focus on reordering and
//! serialization logic.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::Path;

/// Backing storage for spilled bytes.
///
/// Two flavours are supported:
///
/// - `Spooled` - the default. Wraps `tempfile::SpooledTempFile`, which keeps
///   small spills in memory and rolls over to disk past a threshold. The OS
///   deletes the file when the buffer is dropped.
/// - `Directory` - opens a single anonymous tempfile inside a caller-provided
///   directory. If the directory vanishes mid-transfer (operator cleanup,
///   container restart) the buffer performs one `create_dir_all` retry
///   before surfacing the error.
pub(super) enum SpillBackend {
    Spooled(::tempfile::SpooledTempFile),
    Directory(File),
}

impl SpillBackend {
    pub(super) fn file(&mut self) -> &mut dyn ReadWriteSeek {
        match self {
            SpillBackend::Spooled(f) => f,
            SpillBackend::Directory(f) => f,
        }
    }
}

/// Trait object alias to keep the [`SpillBackend::file`] accessor honest.
pub(super) trait ReadWriteSeek: Read + Write + Seek {}
impl<T: Read + Write + Seek + ?Sized> ReadWriteSeek for T {}

/// Opens a fresh tempfile backend.
///
/// When `dir` is `Some`, an anonymous tempfile is created inside that
/// directory (caller is responsible for ensuring the directory exists).
/// When `dir` is `None`, a `SpooledTempFile` is used, which keeps small
/// spills in memory (up to 1 MB) and rolls over to disk for larger volumes,
/// avoiding disk I/O for transient pressure spikes.
pub(super) fn open_backend(dir: Option<&Path>) -> io::Result<SpillBackend> {
    match dir {
        Some(dir) => Ok(SpillBackend::Directory(::tempfile::tempfile_in(dir)?)),
        None => Ok(SpillBackend::Spooled(::tempfile::SpooledTempFile::new(
            1024 * 1024,
        ))),
    }
}
