//! Anonymous temporary file creation via `O_TMPFILE` and finalization via `linkat`.
//!
//! This module provides both low-level syscall wrappers ([`open_anonymous_tmpfile`],
//! [`link_anonymous_tmpfile`]) and a higher-level [`AnonymousTempFile`] guard type
//! that owns the anonymous fd and exposes a safe `link_to` finalization method.
//!
//! The [`open_temp_file`] convenience function probes the filesystem once and
//! returns either an [`AnonymousTempFile`] or [`TempFileResult::Unavailable`],
//! letting callers fall back to named temp files without error handling.

mod low_level;
mod types;

pub use low_level::{link_anonymous_tmpfile, o_tmpfile_available, open_anonymous_tmpfile};
pub use types::{AnonymousTempFile, OTmpfileSupport, TempFileResult, o_tmpfile_probe, open_temp_file};
