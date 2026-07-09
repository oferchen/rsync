//! Diagnostics for filenames skipped because they cannot be transcoded.
//!
//! When `--iconv` is active and a filename (or symlink target) cannot be
//! strictly converted to the peer charset, upstream rsync skips the entry,
//! sets `io_error |= IOERR_GENERAL`, and prints a `cannot convert filename`
//! diagnostic. This module renders that message identically across the local
//! copy, the flist sender, and the flist receiver so all three paths emit the
//! same bytes.
//!
//! # Upstream Reference
//!
//! - `flist.c:1631` `send_file1()` - sender: `rprintf(FERROR_XFER, "[%s]
//!   cannot convert filename: %s (%s)\n", who_am_i(), f_name(file, fbuf),
//!   strerror(errno))`.
//! - `flist.c:757` `recv_file_entry()` - receiver: same message via
//!   `rprintf(FERROR_UTF8, ...)`.
//! - `log.c:239` `filtered_fwrite()` - non-printable bytes render as `\#%03o`.

/// Formats the upstream `cannot convert filename` diagnostic for `name_bytes`.
///
/// `role` is the `who_am_i()` token (`"sender"` or `"receiver"`). The name is
/// escaped the way upstream renders non-printable bytes on the terminal, and
/// the parenthetical is `strerror(EILSEQ)` - the errno `iconv(3)` sets when a
/// byte sequence has no representation in the target charset.
#[must_use]
pub fn cannot_convert_filename_message(role: &str, name_bytes: &[u8]) -> String {
    format!(
        "[{}] cannot convert filename: {} ({})",
        role,
        escape_filename(name_bytes),
        eilseq_strerror()
    )
}

/// Escapes non-printable bytes in a filename for terminal output, matching
/// upstream rsync's `filtered_fwrite()` octal escape (`log.c:239` `"\#%03o"`).
///
/// Printable ASCII (`0x20..=0x7e`) and horizontal tab pass through verbatim;
/// every other byte is rendered as a three-digit octal escape.
#[must_use]
pub fn escape_filename(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b == b'\t' || (0x20..=0x7e).contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("\\#{b:03o}"));
        }
    }
    out
}

/// Returns the platform `strerror(EILSEQ)` text used in the parenthetical of
/// the `cannot convert filename` diagnostic.
///
/// `iconv(3)` sets `errno` to `EILSEQ` when an input byte sequence has no
/// representation in the target charset; upstream prints `strerror(errno)`.
/// The errno value differs per platform, so it is resolved from the OS.
#[must_use]
pub fn eilseq_strerror() -> String {
    #[cfg(target_os = "linux")]
    const EILSEQ: i32 = 84;
    #[cfg(target_os = "macos")]
    const EILSEQ: i32 = 92;
    #[cfg(windows)]
    const EILSEQ: i32 = 42;
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    const EILSEQ: i32 = 84;

    let err = std::io::Error::from_raw_os_error(EILSEQ);
    let full = err.to_string();
    full.strip_suffix(&format!(" (os error {EILSEQ})"))
        .map(str::to_owned)
        .unwrap_or(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_matches_upstream_octal_form() {
        // Byte 0xe9 (0o351) renders as "\#351"; printable ASCII is verbatim.
        assert_eq!(escape_filename(b"caf\xe9.txt"), "caf\\#351.txt");
        assert_eq!(escape_filename(b"plain.txt"), "plain.txt");
        // Control byte 0x0a escapes; tab passes through unescaped.
        assert_eq!(escape_filename(b"a\nb\tc"), "a\\#012b\tc");
    }

    #[test]
    fn strerror_is_non_empty_without_os_error_suffix() {
        let s = eilseq_strerror();
        assert!(!s.is_empty());
        assert!(!s.contains("os error"), "{s}");
    }

    #[test]
    fn message_shape_matches_upstream() {
        let msg = cannot_convert_filename_message("sender", b"caf\xe9.txt");
        assert!(
            msg.starts_with("[sender] cannot convert filename: caf\\#351.txt ("),
            "{msg}"
        );
        assert!(msg.ends_with(')'), "{msg}");
    }
}
