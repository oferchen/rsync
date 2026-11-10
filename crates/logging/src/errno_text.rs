//! Rendering an `io::Error` the way upstream's `rsyserr()` does.

use std::io;

/// Renders an error the way upstream's `rsyserr` does: `strerror(errno)`
/// followed by the numeric errno in parentheses.
///
/// `io::Error`'s own `Display` appends `" (os error N)"`, so the OS error's
/// text is replaced by upstream's shape. A non-OS error renders verbatim, with
/// no parenthesised number to invent.
///
/// On Windows the reference is 3.5.1 under Cygwin: the Win32 or Winsock code
/// is mapped to Cygwin's errno and rendered with Cygwin's `strerror` text, so
/// `ERROR_FILE_NOT_FOUND` prints `No such file or directory (2)`.
///
/// upstream: log.c:499-500 `rsyserr()` - `": %s (%d)"`.
pub fn upstream_errno_text(error: &io::Error) -> String {
    let display = error.to_string();
    let Some((os_error, code)) = os_error_of(error) else {
        return display;
    };
    let (text, errno) = errno_and_text(os_error, code);
    match display.strip_suffix(&os_error.to_string()) {
        Some(prefix) => format!("{prefix}{text} ({errno})"),
        // A wrapping payload with its own wording: keep that wording and only
        // re-shape the trailing errno.
        None => {
            let suffix = format!(" (os error {code})");
            let wording = display.strip_suffix(&suffix).unwrap_or(&display);
            format!("{wording} ({errno})")
        }
    }
}

/// The OS error behind `error` and its raw code.
///
/// A tagged failure is an `io::Error::new(kind, payload)`, i.e. the `Custom`
/// variant, whose `raw_os_error()` is `None` even though the payload is, or
/// wraps, a real OS error. Recover it so tagging an error does not silently
/// drop the number from its own message.
fn os_error_of(error: &io::Error) -> Option<(&io::Error, i32)> {
    if let Some(code) = error.raw_os_error() {
        return Some((error, code));
    }
    let payload = error.get_ref()?;
    let inner = match payload.downcast_ref::<io::Error>() {
        Some(inner) => inner,
        None => payload.source()?.downcast_ref::<io::Error>()?,
    };
    inner.raw_os_error().map(|code| (inner, code))
}

/// The strerror text and errno upstream prints for an OS error.
#[cfg(not(windows))]
fn errno_and_text(error: &io::Error, errno: i32) -> (String, i32) {
    let display = error.to_string();
    let suffix = format!(" (os error {errno})");
    let text = display.strip_suffix(&suffix).unwrap_or(&display).to_owned();
    (text, errno)
}

/// The strerror text and errno 3.5.1 under Cygwin prints for a Win32 or
/// Winsock error code.
#[cfg(windows)]
fn errno_and_text(_error: &io::Error, code: i32) -> (String, i32) {
    let errno = crate::cygwin_errno::errno_from_win_error(code as u32);
    (crate::cygwin_errno::strerror(errno), errno)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_os_error_renders_verbatim() {
        let error = io::Error::other("short read");
        assert_eq!(upstream_errno_text(&error), "short read");
    }

    #[cfg(unix)]
    #[test]
    fn an_os_error_renders_as_strerror_and_errno() {
        let error = io::Error::from_raw_os_error(2);
        assert_eq!(upstream_errno_text(&error), "No such file or directory (2)");
    }

    #[cfg(unix)]
    #[test]
    fn a_tagged_os_error_keeps_its_errno() {
        let error = io::Error::new(io::ErrorKind::NotFound, io::Error::from_raw_os_error(13));
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
        // WSAECONNREFUSED -> ECONNREFUSED (Cygwin numbers it 111).
        assert_eq!(
            upstream_errno_text(&io::Error::from_raw_os_error(10061)),
            "Connection refused (111)"
        );
    }
}
