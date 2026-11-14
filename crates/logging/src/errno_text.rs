//! Upstream's `rsyserr()`: an OS error rendered as `strerror(errno) (errno)`.
//!
//! upstream: log.c:480-500 `rsyserr(code, errcode, format, ...)` writes
//! `RSYNC_NAME ": [" who_am_i() "] "`, the formatted message, then
//! `": %s (%d)\n"` with `strerror(errcode)` and `errcode`. Every site that
//! reports a failed system call goes through it, so the errno text is built at
//! the call site from the raw errno, never recovered from a rendered string.
//!
//! On Windows the reference is 3.5.1 built under Cygwin: a Win32 or Winsock
//! code becomes Cygwin's errno, and the text is Cygwin's `strerror`.

use std::io;

/// The errno upstream would see for `error`, if it carries an OS error.
///
/// A tagged failure, `io::Error::new(kind, payload)`, reports no
/// `raw_os_error()` even when its payload is, or wraps, a real OS error; the
/// errno is recovered from the payload so tagging an error does not drop it.
#[must_use]
pub fn upstream_errno(error: &io::Error) -> Option<i32> {
    raw_code(error).map(errno_from_raw)
}

/// Upstream's `strerror(errno)`.
#[must_use]
pub fn strerror(errno: i32) -> String {
    platform_strerror(errno)
}

/// Upstream's `rsyserr()` line, without the trailing newline:
/// `PROGRAM: [ROLE] MESSAGE: strerror (errno)`.
///
/// `program` is the name upstream prints as `RSYNC_NAME`; callers pass their
/// brand's program name.
#[must_use]
pub fn rsyserr(program: &str, role: &str, message: impl std::fmt::Display, errno: i32) -> String {
    format!(
        "{program}: [{role}] {message}: {} ({errno})",
        strerror(errno)
    )
}

/// [`rsyserr`] for an `io::Error`. An error that carries no errno, which
/// upstream's call sites never see, keeps its own text in place of the
/// `strerror (errno)` tail.
#[must_use]
pub fn rsyserr_io(
    program: &str,
    role: &str,
    message: impl std::fmt::Display,
    error: &io::Error,
) -> String {
    match upstream_errno(error) {
        Some(errno) => rsyserr(program, role, message, errno),
        None => format!("{program}: [{role}] {message}: {error}"),
    }
}

/// The `strerror (errno)` tail `rsyserr()` appends for `error`.
///
/// A non-OS error has no errno to print and renders verbatim.
#[must_use]
pub fn upstream_errno_text(error: &io::Error) -> String {
    match upstream_errno(error) {
        Some(errno) => format!("{} ({errno})", strerror(errno)),
        None => error.to_string(),
    }
}

/// `strerror(errno)` alone, for the upstream sites that print the text without
/// the number (e.g. main.c:1071 `Failed to stat temp-dir %s: %s`).
#[must_use]
pub fn upstream_strerror(error: &io::Error) -> String {
    match upstream_errno(error) {
        Some(errno) => strerror(errno),
        None => error.to_string(),
    }
}

fn raw_code(error: &io::Error) -> Option<i32> {
    if let Some(code) = error.raw_os_error() {
        return Some(code);
    }
    let mut current: Option<&(dyn std::error::Error + 'static)> = error
        .get_ref()
        .map(|e| e as &(dyn std::error::Error + 'static));
    while let Some(err) = current {
        if let Some(io_err) = err.downcast_ref::<io::Error>()
            && let Some(code) = io_err.raw_os_error()
        {
            return Some(code);
        }
        current = err.source();
    }
    None
}

#[cfg(not(windows))]
fn errno_from_raw(code: i32) -> i32 {
    code
}

#[cfg(windows)]
fn errno_from_raw(code: i32) -> i32 {
    crate::cygwin_errno::errno_from_win_error(code as u32)
}

#[cfg(not(windows))]
fn platform_strerror(errno: i32) -> String {
    let display = io::Error::from_raw_os_error(errno).to_string();
    let suffix = format!(" (os error {errno})");
    match display.strip_suffix(&suffix) {
        Some(text) => text.to_owned(),
        None => display,
    }
}

#[cfg(windows)]
fn platform_strerror(errno: i32) -> String {
    crate::cygwin_errno::strerror(errno)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_os_error_renders_verbatim() {
        let error = io::Error::other("short read");
        assert_eq!(upstream_errno(&error), None);
        assert_eq!(upstream_errno_text(&error), "short read");
    }

    #[cfg(unix)]
    #[test]
    fn rsyserr_matches_log_c_layout() {
        assert_eq!(
            rsyserr("rsync", "sender", "link_stat \"/x\" failed", 2),
            "rsync: [sender] link_stat \"/x\" failed: No such file or directory (2)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_os_error_renders_as_strerror_and_errno() {
        let error = io::Error::from_raw_os_error(2);
        assert_eq!(upstream_errno_text(&error), "No such file or directory (2)");
        assert_eq!(upstream_strerror(&error), "No such file or directory");
    }

    #[cfg(unix)]
    #[test]
    fn a_tagged_os_error_keeps_its_errno() {
        let error = io::Error::new(io::ErrorKind::NotFound, io::Error::from_raw_os_error(13));
        assert_eq!(upstream_errno(&error), Some(13));
        assert_eq!(upstream_errno_text(&error), "Permission denied (13)");
    }

    #[cfg(windows)]
    #[test]
    fn a_win32_error_renders_as_cygwin_strerror_and_errno() {
        // ERROR_FILE_NOT_FOUND -> ENOENT, ERROR_ACCESS_DENIED -> EACCES.
        assert_eq!(
            upstream_errno_text(&io::Error::from_raw_os_error(2)),
            "No such file or directory (2)"
        );
        assert_eq!(
            upstream_errno_text(&io::Error::from_raw_os_error(5)),
            "Permission denied (13)"
        );
        // WSAECONNREFUSED -> ECONNREFUSED, which Cygwin numbers 111.
        assert_eq!(
            upstream_errno_text(&io::Error::from_raw_os_error(10061)),
            "Connection refused (111)"
        );
    }
}
