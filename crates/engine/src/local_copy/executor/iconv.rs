//! Filename transcoding for the local-copy executor.
//!
//! Applies the `--iconv=LOCAL,REMOTE` converter attached to
//! [`LocalCopyOptions`](crate::local_copy::LocalCopyOptions) to source
//! filename components before they become destination filesystem entries.
//!
//! In upstream rsync's local-copy mode the sender and receiver share an
//! address space but each opens its own `iconv_t` context (`rsync.c:118-140`).
//! The destination filename is the composition of `ic_send` (LOCAL -> UTF-8
//! wire) and `ic_recv` (UTF-8 wire -> REMOTE), which collapses to a single
//! LOCAL -> REMOTE transcoding. The bridge from `--iconv` to the
//! [`FilenameConverter`](protocol::iconv::FilenameConverter) used here lives
//! in
//! [`IconvSetting::resolve_local_copy_converter`](core::client::config::IconvSetting::resolve_local_copy_converter).
//!
//! # Upstream Reference
//!
//! - `flist.c:1579-1603` `send_file_name()` - `iconvbufs(ic_send, ...)`
//!   transcodes the filename before it leaves the sender.
//! - `flist.c:738-754` `recv_file_entry()` - `iconvbufs(ic_recv, ...)`
//!   transcodes the filename before the receiver hits the filesystem.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};

use protocol::iconv::FilenameConverter;

/// Returns the byte representation of a filename component for iconv input.
///
/// Unix `OsStr` values are arbitrary byte strings (`OsStrExt::as_bytes`).
/// On other platforms `OsStr::as_encoded_bytes` returns the WTF-8 encoding
/// used by Rust internally. Either representation is valid input to
/// [`encoding_rs`](https://docs.rs/encoding_rs/), which `FilenameConverter`
/// wraps.
#[inline]
fn os_str_to_bytes(name: &OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        name.as_bytes()
    }
    #[cfg(not(unix))]
    {
        name.as_encoded_bytes()
    }
}

/// Reconstructs an `OsString` from converted iconv output bytes.
///
/// On Unix the bytes are passed through verbatim because `OsStr` can hold
/// any byte sequence. On non-Unix platforms `OsString` requires WTF-8 input
/// when constructed via `OsString::from_encoded_bytes_unchecked`. Since the
/// iconv result is encoded in REMOTE charset (which may not be UTF-8 / WTF-8
/// at all), we route non-Unix platforms through `String::from_utf8_lossy`,
/// matching the established receiver-side pattern in
/// `protocol::flist::read::extras` for symlink targets. This trades
/// REMOTE-charset fidelity for safe `OsString` construction on Windows;
/// no behavioural regression vs. the prior implementation because the
/// prior implementation never transcoded local-copy filenames at all.
#[inline]
fn bytes_to_os_string(bytes: Vec<u8>) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(bytes)
    }
    #[cfg(not(unix))]
    {
        let lossy = String::from_utf8_lossy(&bytes);
        OsString::from(lossy.into_owned())
    }
}

/// Transcodes a single filename component using the configured converter.
///
/// Returns `Cow::Borrowed` when the converter is absent, is an identity
/// converter, or the round-trip produced bytes equal to the input. This
/// preserves the no-allocation hot path for transfers without `--iconv`.
///
/// When the converter encounters bytes that cannot be represented in the
/// target encoding, unconvertible characters are replaced with `?` rather
/// than aborting the transfer. This mirrors upstream rsync's
/// `ICB_INCLUDE_BAD` behaviour (rsync.c:229-231) where invalid bytes are
/// passed through verbatim. A warning is emitted via
/// [`trace_conversion_warning`](protocol::iconv::trace_conversion_warning)
/// when replacement occurs.
#[must_use]
pub(crate) fn transcode_filename_component<'a>(
    name: &'a OsStr,
    converter: Option<&FilenameConverter>,
) -> Cow<'a, OsStr> {
    let Some(converter) = converter else {
        return Cow::Borrowed(name);
    };
    if converter.is_identity() {
        return Cow::Borrowed(name);
    }

    let bytes = os_str_to_bytes(name);
    let outcome = converter.local_to_remote_lossy(bytes);
    if outcome.had_replacements {
        protocol::iconv::trace_conversion_warning(
            protocol::iconv::IconvRole::Client,
            &String::from_utf8_lossy(bytes),
            converter.local_encoding_name(),
            converter.remote_encoding_name(),
        );
    }
    match outcome.output {
        Cow::Borrowed(b) if std::ptr::eq(b, bytes) => Cow::Borrowed(name),
        output => Cow::Owned(bytes_to_os_string(output.into_owned())),
    }
}

/// Reports whether `name` can be strictly transcoded LOCAL -> REMOTE under
/// the configured converter.
///
/// Returns `true` when no converter is configured, the converter is an
/// identity mapping, or the round-trip succeeds without loss. Returns `false`
/// only when the converter is active and the name contains bytes that cannot
/// be represented in the remote charset - the case upstream rsync skips.
///
/// # Upstream Reference
///
/// - `flist.c:1614-1638` `send_file1()` - `iconvbufs(ic_send, ..., ICB_INIT)`
///   uses the strict (non-`ICB_INCLUDE_BAD`) mode; a `< 0` return skips the
///   file rather than substituting replacement bytes.
#[must_use]
pub(crate) fn name_is_convertible(name: &OsStr, converter: Option<&FilenameConverter>) -> bool {
    match converter {
        Some(conv) if !conv.is_identity() => conv.local_to_remote(os_str_to_bytes(name)).is_ok(),
        _ => true,
    }
}

/// Emits the upstream `cannot convert filename` diagnostic to stderr.
///
/// The `who_am_i()` role for the sending half of a local copy is `sender`, so
/// the message is unconditionally prefixed `[sender]`. Message formatting is
/// shared with the flist sender/receiver via
/// [`protocol::iconv::cannot_convert_filename_message`].
///
/// # Upstream Reference
///
/// - `flist.c:1631` `send_file1()` - `rprintf(FERROR_XFER, "[%s] cannot convert
///   filename: %s (%s)\n", who_am_i(), f_name(file, fbuf), strerror(errno))`.
pub(crate) fn emit_cannot_convert_filename(display: &OsStr) {
    eprintln!(
        "{}",
        protocol::iconv::cannot_convert_filename_message("sender", os_str_to_bytes(display))
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_converter_returns_borrowed() {
        let name = OsStr::new("hello.txt");
        let out = transcode_filename_component(name, None);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(&*out, OsStr::new("hello.txt"));
    }

    #[test]
    fn identity_converter_returns_borrowed() {
        let name = OsStr::new("hello.txt");
        let conv = FilenameConverter::identity();
        let out = transcode_filename_component(name, Some(&conv));
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(&*out, OsStr::new("hello.txt"));
    }

    #[cfg(all(unix, feature = "iconv"))]
    #[test]
    fn utf8_to_latin1_emits_latin1_bytes() {
        use std::os::unix::ffi::OsStrExt;

        // Source "café.txt" in UTF-8: c3 a9 for "é".
        let utf8 = OsStr::from_bytes(b"caf\xc3\xa9.txt");
        let conv = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("ctor");
        let out = transcode_filename_component(utf8, Some(&conv));
        // Latin-1 "café.txt" replaces "é" with the single byte 0xe9.
        assert_eq!(out.as_bytes(), b"caf\xe9.txt");
    }

    #[cfg(all(unix, feature = "iconv"))]
    #[test]
    fn latin1_to_utf8_emits_utf8_bytes() {
        use std::os::unix::ffi::OsStrExt;

        // Source "café.txt" in Latin-1: e9 for "é".
        let latin1 = OsStr::from_bytes(b"caf\xe9.txt");
        let conv = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("ctor");
        let out = transcode_filename_component(latin1, Some(&conv));
        // UTF-8 "café.txt" expands "é" to c3 a9.
        assert_eq!(out.as_bytes(), b"caf\xc3\xa9.txt");
    }

    #[cfg(all(unix, feature = "iconv"))]
    #[test]
    fn invalid_bytes_replaced_with_lossy_conversion() {
        use std::os::unix::ffi::OsStrExt;

        // Lone continuation byte 0x80 is not valid UTF-8. encoding_rs
        // decodes it as U+FFFD, which ISO-8859-1 cannot represent, so
        // the lossy encoder replaces it with '?'.
        let bad = OsStr::from_bytes(b"bad\x80name");
        let conv = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("ctor");
        let out = transcode_filename_component(bad, Some(&conv));
        assert_eq!(out.as_bytes(), b"bad?name");
    }

    #[test]
    fn convertible_without_converter_is_true() {
        assert!(name_is_convertible(OsStr::new("caf\u{e9}.txt"), None));
    }

    #[cfg(all(unix, feature = "iconv"))]
    #[test]
    fn unconvertible_name_is_rejected_by_strict_check() {
        use std::os::unix::ffi::OsStrExt;

        // Byte 0xe9 alone is not valid UTF-8, so LOCAL=UTF-8 -> REMOTE=Latin-1
        // strict conversion must fail: this is the entry upstream skips.
        let bad = OsStr::from_bytes(b"caf\xe9.txt");
        let conv = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("ctor");
        assert!(!name_is_convertible(bad, Some(&conv)));

        // A clean name still round-trips.
        let good = OsStr::from_bytes(b"caf\xc3\xa9.txt");
        assert!(name_is_convertible(good, Some(&conv)));
    }
}
