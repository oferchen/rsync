//! Filename reading, compression, validation, and encoding conversion.
//!
//! Handles the rsync wire format for filenames, including common-prefix
//! compression between consecutive entries, iconv encoding conversion,
//! and security validation (dot-dot rejection, absolute path handling).
//!
//! # Upstream Reference
//!
//! - `flist.c:recv_file_entry()` lines 760-800 for name reading
//! - `util1.c:943`: `clean_fname()` with `CFN_REFUSE_DOT_DOT_DIRS`
//! - `flist.c:756-760`: pathname safety check

use std::io::{self, Read};

use logging::debug_log;

use crate::codec::ProtocolCodec;
use crate::flist::flags::FileFlags;

use super::FileListReader;

impl FileListReader {
    /// Reads the file name with path compression.
    ///
    /// The rsync wire format compresses file names by sharing a common prefix
    /// with the previous entry. If `XMIT_SAME_NAME` is set, a `same_len` byte
    /// indicates how many bytes to reuse from the previous name.
    ///
    /// # Wire Format
    ///
    /// - If `XMIT_SAME_NAME`: read u8 as `same_len`
    /// - If `XMIT_LONG_NAME`: read via codec (4-byte LE for proto < 30, varint for >= 30)
    /// - Read `suffix_len` bytes as the name suffix
    /// - Concatenate: `prev_name[..same_len] + suffix`
    pub(super) fn read_name<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<Vec<u8>> {
        let same_len = if flags.same_name() {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        } else {
            0
        };

        // upstream: flist.c:719-722 - XMIT_LONG_NAME uses read_varint30()
        // which dispatches to read_int (4-byte LE) for protocol < 30,
        // read_varint for protocol >= 30
        let suffix_len = if flags.long_name() {
            self.codec.read_long_name_len(reader)?
        } else {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            byte[0] as usize
        };

        debug_log!(
            Flist,
            4,
            "read_name: same_len={} suffix_len={} long_name={}",
            same_len,
            suffix_len,
            flags.long_name()
        );

        if same_len > self.state.prev_name().len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "same_len {} exceeds previous name length {}",
                    same_len,
                    self.state.prev_name().len()
                ),
            ));
        }

        let mut name = Vec::with_capacity(same_len + suffix_len);
        name.extend_from_slice(&self.state.prev_name()[..same_len]);

        if suffix_len > 0 {
            let start = name.len();
            name.resize(start + suffix_len, 0);
            reader.read_exact(&mut name[start..])?;
        }

        debug_log!(
            Flist,
            3,
            "read_name: total_len={} name_bytes={:?}",
            name.len(),
            &name[..name.len().min(64)]
        );

        self.state.update_name(&name);

        Ok(name)
    }

    /// Applies iconv encoding conversion to a filename.
    ///
    /// When `--iconv` is used, filenames are converted from the remote encoding
    /// to the local encoding. This enables interoperability between systems
    /// with different character encodings.
    ///
    /// # Upstream Reference
    ///
    /// `flist.c:738-754` `recv_file_entry()` runs the freshly-read filename
    /// through `iconvbufs(ic_recv, ...)` before `clean_fname()`. The
    /// prefix-compression buffer (`lastname` / `state.prev_name()`) intentionally
    /// retains the wire bytes so subsequent entries can share the prefix
    /// before any conversion is applied.
    // upstream: flist.c recv_file_entry() iconv_buf(ic_recv, ...)
    pub(super) fn apply_encoding_conversion(&self, name: Vec<u8>) -> io::Result<Vec<u8>> {
        if let Some(ref converter) = self.iconv {
            match converter.remote_to_local(&name) {
                Ok(converted) => Ok(converted.into_owned()),
                Err(e) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("filename encoding conversion failed: {e}"),
                )),
            }
        } else {
            Ok(name)
        }
    }

    /// Cleans and validates a filename received from the sender.
    ///
    /// Mirrors upstream `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)` followed
    /// by the leading-slash check at flist.c:756-760. Performs in-place on a byte
    /// buffer to avoid allocations on the common (clean) path.
    ///
    /// Normalization:
    /// - Collapses duplicate slashes (`a//b` -> `a/b`)
    /// - Removes interior `.` components (`a/./b` -> `a/b`)
    /// - Strips trailing slashes (`a/b/` -> `a/b`)
    /// - Replaces empty result with `.`
    ///
    /// Validation:
    /// - Rejects any `..` path component (always, regardless of mode)
    /// - Rejects leading `/` when `relative_paths` is false
    /// - Strips leading slashes when `relative_paths` is true
    ///
    /// # Upstream Reference
    ///
    /// - `util1.c:943`: `clean_fname()` with `CFN_REFUSE_DOT_DOT_DIRS`
    /// - `flist.c:756-760`: pathname safety check after `clean_fname`
    pub(super) fn clean_and_validate_name(&self, name: Vec<u8>) -> io::Result<Vec<u8>> {
        if name.is_empty() {
            return Ok(name);
        }

        // Fast path: most names from a well-behaved sender need no cleaning.
        if !needs_cleaning(&name) {
            if !self.relative_paths && name[0] == b'/' {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "ABORTING due to unsafe pathname from sender: {}",
                        String::from_utf8_lossy(&name)
                    ),
                ));
            }
            return Ok(name);
        }

        // Slow path: normalize and validate.
        let mut out = Vec::with_capacity(name.len());
        let anchored = name[0] == b'/';

        // upstream: flist.c:757 - reject absolute paths when not --relative
        if anchored && !self.relative_paths {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "ABORTING due to unsafe pathname from sender: {}",
                    String::from_utf8_lossy(&name)
                ),
            ));
        }

        // Skip all leading slashes for --relative mode.
        // Non-relative absolute paths were rejected above.
        let start = if anchored {
            name.iter().position(|&b| b != b'/').unwrap_or(name.len())
        } else {
            0
        };

        let mut i = start;
        while i < name.len() {
            // Skip duplicate slashes
            if name[i] == b'/' {
                i += 1;
                continue;
            }

            // Check for `.` or `..` components
            if name[i] == b'.' {
                let next = name.get(i + 1).copied();
                // Single `.` component: skip it
                if next == Some(b'/') || next.is_none() {
                    i += if next.is_some() { 2 } else { 1 };
                    continue;
                }
                // `..` component: always reject
                // upstream: util1.c:982-985 CFN_REFUSE_DOT_DOT_DIRS
                if next == Some(b'.') {
                    let after = name.get(i + 2).copied();
                    if after == Some(b'/') || after.is_none() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "ABORTING due to unsafe pathname from sender: {}",
                                String::from_utf8_lossy(&name)
                            ),
                        ));
                    }
                }
            }

            // Copy this path component
            if !out.is_empty() {
                out.push(b'/');
            }
            while i < name.len() && name[i] != b'/' {
                out.push(name[i]);
                i += 1;
            }
            if i < name.len() {
                i += 1; // skip the slash
            }
        }

        // upstream: util1.c:1004-1005 - empty result becomes "."
        if out.is_empty() {
            out.push(b'.');
        }

        Ok(out)
    }
}

/// Returns true if a filename needs normalization or contains unsafe components.
///
/// Checks for patterns that `clean_and_validate_name` would modify:
/// leading slashes, duplicate slashes, `.` or `..` path components.
/// This allows a fast bypass for the common case of well-formed names.
fn needs_cleaning(name: &[u8]) -> bool {
    if name.is_empty() {
        return false;
    }

    // Leading slash requires stripping or rejection
    if name[0] == b'/' {
        return true;
    }

    let mut i = 0;
    while i < name.len() {
        // Duplicate slashes
        if name[i] == b'/' {
            if i + 1 < name.len() && name[i + 1] == b'/' {
                return true;
            }
            i += 1;
            continue;
        }

        // Check for `.` or `..` at component start
        if name[i] == b'.' {
            let at_start = i == 0 || name[i - 1] == b'/';
            if at_start {
                let next = name.get(i + 1).copied();
                // "." component
                if next == Some(b'/') || next.is_none() {
                    return true;
                }
                // ".." component
                if next == Some(b'.') {
                    let after = name.get(i + 2).copied();
                    if after == Some(b'/') || after.is_none() {
                        return true;
                    }
                }
            }
        }

        i += 1;
    }

    // Trailing slash
    if name[name.len() - 1] == b'/' {
        return true;
    }

    false
}
