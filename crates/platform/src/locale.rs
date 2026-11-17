//! Process locale initialisation.
//!
//! upstream: main.c:1910-1911 calls `setlocale(LC_CTYPE, "")` before it parses
//! any option, and rsync.c:75-83 `default_charset()` then names the active
//! codeset with `nl_langinfo(CODESET)`. The codeset decides which name bytes
//! the client's terminal output treats as printable (rsync.c:95-98, log.c:476).

/// Adopts the environment's `LC_CTYPE` locale and returns its codeset name.
///
/// Mirrors upstream's `setlocale(LC_CTYPE, "")` followed by
/// `nl_langinfo(CODESET)`. When the environment names a locale the system
/// does not have, `setlocale` leaves the `C` locale in place, exactly as it
/// does for upstream, and the `C` codeset is returned.
///
/// Must be called once, before any thread exists: `setlocale` is not
/// thread-safe. Returns `None` when the codeset cannot be read.
#[cfg(unix)]
#[allow(unsafe_code)]
#[must_use]
pub fn init_ctype_locale() -> Option<String> {
    // SAFETY: `setlocale` takes a valid category and a NUL-terminated string
    // that outlives the call; the caller guarantees no other thread exists, so
    // no concurrent locale query can observe the switch. Neither `nix` nor
    // `rustix` wraps `setlocale`.
    unsafe { libc::setlocale(libc::LC_CTYPE, c"".as_ptr()) };
    // SAFETY: `nl_langinfo` returns a pointer to a NUL-terminated string owned
    // by the C library that stays valid until the next locale change; it is
    // copied out before this function returns. A NULL return is handled.
    // Neither `nix` nor `rustix` wraps `nl_langinfo`.
    let codeset = unsafe { libc::nl_langinfo(libc::CODESET) };
    if codeset.is_null() {
        return None;
    }
    // SAFETY: `codeset` is non-null and NUL-terminated (see above).
    let name = unsafe { std::ffi::CStr::from_ptr(codeset) };
    Some(name.to_string_lossy().into_owned())
}

/// Windows has no C-library `LC_CTYPE` locale to adopt, so no codeset is
/// reported and callers keep the conservative escaping of an unknown codeset.
#[cfg(not(unix))]
#[must_use]
pub fn init_ctype_locale() -> Option<String> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::init_ctype_locale;

    /// WHY: the codeset drives which name bytes reach the terminal raw, so the
    /// call must report one on every Unix this crate builds for.
    #[test]
    fn reports_a_codeset() {
        let codeset = init_ctype_locale().expect("nl_langinfo(CODESET)");
        assert!(!codeset.is_empty());
    }
}
