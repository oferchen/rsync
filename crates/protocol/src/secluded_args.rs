#![deny(unsafe_code)]
//! Secluded-args (protect-args) stdin argument transmission protocol.
//!
//! When `--protect-args` / `--secluded-args` / `-s` is active, rsync avoids
//! passing file paths and transfer arguments on the SSH command line. Instead,
//! after the remote rsync process starts, the client sends the full argument
//! list over stdin as null-separated strings, terminated by an empty string
//! (a lone `\0`).
//!
//! This prevents the remote shell from expanding wildcards or misinterpreting
//! special characters in file paths.
//!
//! # Wire Format
//!
//! Each argument is encoded as a UTF-8 (or raw byte) string followed by a
//! null byte (`\0`). The list is terminated by an additional null byte (an
//! empty string), producing the sequence:
//!
//! ```text
//! arg1\0arg2\0arg3\0\0
//! ```
//!
//! # Charset Conversion
//!
//! When `--iconv` is configured, upstream rsync transcodes each argument
//! before writing or after reading: the writer converts local-charset bytes
//! to the wire encoding (`iconvbufs(ic_send, ...)` in `rsync.c:283-320`)
//! and the reader converts wire bytes back to local-charset
//! (`read_line(RL_CONVERT)` in `io.c:1240-1289`). The conversion is applied
//! per argument so the NUL delimiters and terminator stay verbatim.
//!
//! # Upstream Reference
//!
//! - `rsync.c:283-320`: `send_protected_args()` per-arg `iconvbufs(ic_send, ...)`
//! - `io.c:1240-1289`: `read_line()` with `RL_CONVERT` -> `iconvbufs(ic_recv, ...)`
//! - `compat.c:799-806`: `filesfrom_convert` / protect-args iconv gating

use std::borrow::Cow;
use std::io::{self, Read, Write};

use crate::iconv::FilenameConverter;

/// Serializes arguments as null-separated strings with an empty terminator.
///
/// Writes each argument followed by a `\0` byte, then writes a final `\0`
/// to signal end-of-arguments. The writer is flushed after all arguments
/// are written.
///
/// When `iconv` is `Some`, each argument is transcoded with
/// [`FilenameConverter::local_to_remote`] before being written, mirroring
/// upstream `send_protected_args()`'s `iconvbufs(ic_send, ...)` call.
/// When `iconv` is `None`, bytes are forwarded verbatim - equivalent to
/// upstream's `ic_send == (iconv_t)-1` case.
///
/// # Upstream Reference
///
/// Mirrors `send_protected_args()` in upstream `rsync.c:283-320`.
pub fn send_secluded_args<W: Write>(
    writer: &mut W,
    args: &[&str],
    iconv: Option<&FilenameConverter>,
) -> io::Result<()> {
    for arg in args {
        write_secluded_arg(writer, arg.as_bytes(), iconv)?;
    }
    // Empty string terminator
    writer.write_all(b"\0")?;
    writer.flush()
}

/// Writes a single secluded arg, applying iconv when configured.
fn write_secluded_arg<W: Write>(
    writer: &mut W,
    arg: &[u8],
    iconv: Option<&FilenameConverter>,
) -> io::Result<()> {
    let bytes: Cow<'_, [u8]> = match iconv {
        Some(converter) => match converter.local_to_remote(arg) {
            Ok(cow) => cow,
            // upstream rsync.c:308 uses ICB_INCLUDE_BAD: bad bytes pass
            // through verbatim rather than aborting the args exchange.
            Err(_) => Cow::Borrowed(arg),
        },
        None => Cow::Borrowed(arg),
    };
    writer.write_all(&bytes)?;
    writer.write_all(b"\0")?;
    Ok(())
}

/// Deserializes a null-separated argument list from a reader.
///
/// Reads bytes one at a time, collecting characters into arguments separated
/// by `\0` bytes. An empty argument (two consecutive `\0` bytes or a `\0`
/// at the start) signals the end of the argument list.
///
/// When `iconv` is `Some`, each argument's wire bytes are transcoded with
/// [`FilenameConverter::remote_to_local`] before UTF-8 decoding, mirroring
/// upstream `read_line(RL_CONVERT)`'s `iconvbufs(ic_recv, ...)` call. When
/// `iconv` is `None`, wire bytes are decoded verbatim - equivalent to
/// upstream's `ic_recv == (iconv_t)-1` case.
///
/// Returns `Ok(args)` with the parsed argument vector on success.
///
/// # Upstream Reference
///
/// Mirrors the protected-args reading logic in upstream `io.c:1240-1289`
/// `read_line()` with the `RL_CONVERT` flag.
///
/// # Errors
///
/// Returns an error if the reader encounters an I/O error or reaches EOF
/// before the terminating empty argument.
pub fn recv_secluded_args<R: Read>(
    reader: &mut R,
    iconv: Option<&FilenameConverter>,
) -> io::Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = Vec::new();
    let mut byte = [0u8; 1];

    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading secluded args",
                ));
            }
            Err(e) => return Err(e),
        }

        if byte[0] == 0 {
            if current.is_empty() {
                // Empty string = terminator
                break;
            }
            let raw = std::mem::take(&mut current);
            let bytes: Vec<u8> = match iconv {
                Some(converter) => match converter.remote_to_local(&raw) {
                    Ok(cow) => cow.into_owned(),
                    // upstream io.c uses ICB_INCLUDE_BAD: bad bytes pass
                    // through verbatim rather than aborting.
                    Err(_) => raw,
                },
                None => raw,
            };
            let arg = String::from_utf8(bytes).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid UTF-8 in secluded arg: {e}"),
                )
            })?;
            args.push(arg);
        } else {
            current.push(byte[0]);
        }
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_simple_args() {
        let args = vec!["--server", "--sender", "-logDtpr", ".", "/path/to/files"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert_eq!(received, args);
    }

    #[test]
    fn round_trip_empty_args() {
        let args: Vec<&str> = vec![];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert!(received.is_empty());
    }

    #[test]
    fn round_trip_special_characters() {
        let args = vec![
            "file with spaces",
            "file'with'quotes",
            "file\"double\"quotes",
            "file\twith\ttabs",
            "path/to/$pecial",
            "wildcard*pattern",
            "file\nwith\nnewlines",
        ];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert_eq!(received, args);
    }

    #[test]
    fn wire_format_matches_expected() {
        let args = vec!["arg1", "arg2"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");

        // Expected: "arg1\0arg2\0\0"
        assert_eq!(buf, b"arg1\0arg2\0\0");
    }

    #[test]
    fn single_arg_wire_format() {
        let args = vec!["hello"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");
        assert_eq!(buf, b"hello\0\0");
    }

    #[test]
    fn empty_args_produces_single_null() {
        let args: Vec<&str> = vec![];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");
        assert_eq!(buf, b"\0");
    }

    #[test]
    fn recv_from_truncated_stream_returns_error() {
        // Stream ends without terminator
        let buf = b"arg1\0arg2";
        let mut cursor = io::Cursor::new(&buf[..]);
        let result = recv_secluded_args(&mut cursor, None);
        assert!(result.is_err());
    }

    #[test]
    fn recv_from_empty_stream_returns_error() {
        let buf = b"";
        let mut cursor = io::Cursor::new(&buf[..]);
        let result = recv_secluded_args(&mut cursor, None);
        assert!(result.is_err());
    }

    #[test]
    fn round_trip_unicode_paths() {
        let args = vec![
            "/home/user/Documents/日本語",
            "/tmp/Ñoño/café",
            "/data/файлы",
        ];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert_eq!(received, args);
    }

    #[test]
    fn round_trip_long_arg_list() {
        let args: Vec<String> = (0..1000).map(|i| format!("/path/to/file_{i}")).collect();
        let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args_refs, None).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert_eq!(received, args);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn send_with_identity_converter_matches_no_iconv() {
        let identity = FilenameConverter::identity();
        let args = vec!["--server", "alpha.txt", "beta.txt"];
        let mut buf_with = Vec::new();
        let mut buf_without = Vec::new();

        send_secluded_args(&mut buf_with, &args, Some(&identity)).expect("send with iconv");
        send_secluded_args(&mut buf_without, &args, None).expect("send without iconv");

        assert_eq!(buf_with, buf_without);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn send_transcodes_each_arg_separately() {
        // Local UTF-8 -> wire Latin-1. Each arg is transcoded in isolation,
        // so the NUL delimiters and terminator stay verbatim.
        let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");
        // UTF-8: é = C3 A9, ï = C3 AF.
        let args = vec!["café", "naïve"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, Some(&converter)).expect("send with iconv");

        // Latin-1: é = E9, ï = EF.
        assert_eq!(buf, b"caf\xE9\0na\xEFve\0\0");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn roundtrip_with_iconv_local_utf8_wire_latin1() {
        // Writer: local=UTF-8, remote=Latin-1.
        // Reader: local=UTF-8, remote=Latin-1 (same direction inverted).
        let writer_iconv = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("writer");
        let reader_iconv = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("reader");

        let args = vec!["café", "naïve", "/data/files"];
        let mut wire = Vec::new();
        send_secluded_args(&mut wire, &args, Some(&writer_iconv)).expect("send");

        let mut cursor = io::Cursor::new(&wire);
        let received =
            recv_secluded_args(&mut cursor, Some(&reader_iconv)).expect("recv with iconv");
        assert_eq!(received, args);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn recv_with_identity_converter_matches_no_iconv() {
        let identity = FilenameConverter::identity();
        let wire: &[u8] = b"alpha\0beta\0gamma\0\0";

        let mut cursor_with = io::Cursor::new(wire);
        let with = recv_secluded_args(&mut cursor_with, Some(&identity)).expect("recv with iconv");

        let mut cursor_without = io::Cursor::new(wire);
        let without = recv_secluded_args(&mut cursor_without, None).expect("recv without iconv");

        assert_eq!(with, without);
        assert_eq!(with, vec!["alpha", "beta", "gamma"]);
    }
}
