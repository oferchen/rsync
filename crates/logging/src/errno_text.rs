//! Rendering an `io::Error` the way upstream's `rsyserr()` does.

use std::io;

/// Renders an error the way upstream's `rsyserr` does: `strerror(errno)`
/// followed by the numeric errno in parentheses.
///
/// `io::Error`'s own `Display` appends `" (os error N)"`, so the suffix is
/// stripped and re-emitted in upstream's shape. A non-OS error renders
/// verbatim, with no parenthesised number to invent.
///
/// upstream: `rsync-3.5.0/log.c` `rsyserr()` - `"%s: %s (%d)"`.
pub fn upstream_errno_text(error: &io::Error) -> String {
    let display = error.to_string();
    // A tagged failure is an `io::Error::new(kind, payload)`, i.e. the `Custom`
    // variant, whose `raw_os_error()` is `None` even though the payload wraps a
    // real OS error. Recover the errno from the source chain so tagging an
    // error does not silently drop the number from its own message.
    let errno = error.raw_os_error().or_else(|| {
        std::error::Error::source(error.get_ref()?)?
            .downcast_ref::<io::Error>()?
            .raw_os_error()
    });
    match errno {
        Some(errno) => {
            let suffix = format!(" (os error {errno})");
            let text = display.strip_suffix(&suffix).unwrap_or(&display);
            format!("{text} ({errno})")
        }
        None => display,
    }
}
