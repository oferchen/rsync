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
//! An empty argument is escaped as the literal `.\0` (2 bytes) to avoid
//! collision with the terminator NUL, matching upstream's
//! `send_protected_args()` at `rsync.c:299-300`.
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
///
/// An empty argument is encoded as `.\0` (literal dot + NUL), matching
/// upstream `rsync.c:299-300 send_protected_args()`:
///
/// ```c
/// if (!args[i][0])
///     write_buf(fd, ".", 2);
/// ```
///
/// Without this escape an empty arg would emit a lone `\0`, which collides
/// with the terminator NUL the receiver uses to detect end-of-list. The
/// peer's `recv_secluded_args` would treat the empty arg as the terminator
/// and stop reading mid-list, leaving the SSH/lsh.sh receiver blocked on
/// `building file list ...` because the trailing positional args (the `.`
/// separator and the source/destination paths) never arrive. This is the
/// terminator-collision class behind the upstream `files-from.test`
/// 4th-invocation PUSH hang surface.
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
    if bytes.is_empty() {
        // upstream: rsync.c:299-300 send_protected_args() encodes an empty
        // arg as `.\0` so the terminator NUL can never be confused with a
        // legitimately empty mid-list arg.
        writer.write_all(b".\0")?;
    } else {
        writer.write_all(&bytes)?;
        writer.write_all(b"\0")?;
    }
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

    // Empty-arg encoding regression tests for UTS files-from 4th-invocation
    // PUSH hang. Upstream `rsync.c:299-300 send_protected_args()` encodes an
    // empty argument as the literal `.\0` (2 bytes) rather than a bare `\0`
    // so the terminator can never be confused with a legitimately empty
    // mid-list arg. Without this escape, an empty arg would emit `\0`, the
    // receiver's `recv_secluded_args` would interpret it as the terminator,
    // and every subsequent positional arg (the `.` separator and the path
    // operands) would stall in the SSH pipe. The lsh.sh server then sits in
    // `building file list ...` until the upstream testsuite kills it at
    // 300s, surfacing as the 4th-invocation hang.

    #[test]
    fn send_empty_mid_list_arg_encodes_as_dot_to_avoid_terminator_collision() {
        // The middle arg is empty. Without the dot escape this would emit
        // `arg1\0\0arg3\0\0`, where the second `\0` collides with the
        // recv-side terminator detection and truncates the list to one arg.
        let args = vec!["arg1", "", "arg3"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");

        // upstream wire encoding: empty -> `.\0`, terminator -> trailing `\0`.
        assert_eq!(buf, b"arg1\0.\0arg3\0\0");
    }

    #[test]
    fn send_empty_first_arg_encodes_as_dot() {
        let args = vec!["", "second"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");
        assert_eq!(buf, b".\0second\0\0");
    }

    #[test]
    fn send_empty_last_arg_encodes_as_dot_before_terminator() {
        let args = vec!["first", ""];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");
        // Without the escape this would emit `first\0\0\0` which the
        // receiver truncates at the first `\0\0` boundary, dropping the
        // trailing args slot. With the escape the wire reads `first\0.\0\0`.
        assert_eq!(buf, b"first\0.\0\0");
    }

    #[test]
    fn round_trip_empty_mid_list_arg_recovers_dot_sentinel() {
        // After the dot-escape the receiver decodes the empty arg as `.`.
        // Upstream `read_args` treats the resulting `.` as the dot-separator
        // it expects at this position in the argv stream, so the protocol
        // contract holds even though the empty-string information is lost
        // on the wire. This is the upstream-rsync contract verbatim.
        let args = vec!["--server", "", "."];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args, None).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert_eq!(
            received,
            vec!["--server", ".", "."],
            "empty arg must round-trip as `.` to avoid terminator collision; \
             losing this contract reproduces the files-from 4th-invocation hang"
        );
    }

    #[test]
    fn empty_arg_round_trip_does_not_leak_into_next_phase() {
        // Lock the post-fix invariant: after recv_secluded_args returns, the
        // reader cursor must be positioned past the terminator NUL even when
        // the args list contained empty entries. Any residual byte would
        // poison the subsequent protocol greeting exchange.
        let args = vec!["alpha", "", "omega"];
        let mut wire = Vec::new();
        send_secluded_args(&mut wire, &args, None).expect("send should succeed");
        // Append a sentinel that simulates the next protocol byte.
        wire.extend_from_slice(b"@RSYNCD");

        let mut cursor = io::Cursor::new(&wire);
        let received = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert_eq!(received, vec!["alpha", ".", "omega"]);

        let mut leftover = Vec::new();
        cursor
            .read_to_end(&mut leftover)
            .expect("trailing bytes must remain readable");
        assert_eq!(
            leftover, b"@RSYNCD",
            "recv_secluded_args must stop exactly at the terminator NUL even \
             with empty-arg escapes, leaving the greeting bytes for the next \
             reader"
        );
    }

    // Drain-invariant tests for the secluded-args terminator.
    //
    // Upstream `io.c:1308-1367` `read_args()` consumes the input stream one
    // line at a time via `read_line()`; the loop terminates when an empty
    // line (the lone `\0` terminator) is read. After termination, the input
    // file descriptor is positioned immediately after the terminator NUL, so
    // the next read - the protocol greeting handshake - sees a clean stream.
    //
    // These tests lock the same invariant in `recv_secluded_args`: every byte
    // of the wire payload up to and including the terminator NUL must be
    // consumed. If even one residual byte leaks, it corrupts the subsequent
    // `@RSYNCD:` greeting exchange and surfaces as a confusing handshake
    // failure rather than a clear argument-protocol error.

    #[test]
    fn recv_secluded_args_consumes_terminator_completely() {
        // Wire format: 5 args followed by the empty-arg terminator.
        let wire: &[u8] = b"--server\0--sender\0-logDtpr\0.\0/path\0\0";
        let mut cursor = io::Cursor::new(wire);

        let args = recv_secluded_args(&mut cursor, None).expect("recv should succeed");

        assert_eq!(args.len(), 5);
        assert_eq!(args[0], "--server");
        assert_eq!(args[1], "--sender");
        assert_eq!(args[2], "-logDtpr");
        assert_eq!(args[3], ".");
        assert_eq!(args[4], "/path");

        // Critical invariant: the cursor must be positioned past the
        // terminator NUL. Any residual byte would leak into the next reader
        // (the protocol greeting), corrupting the handshake.
        assert_eq!(
            cursor.position(),
            wire.len() as u64,
            "recv_secluded_args must consume the terminator NUL; otherwise residual bytes leak \
             into the protocol greeting stream"
        );
    }

    #[test]
    fn recv_secluded_args_handles_empty_arg_list() {
        // Wire format: terminator only - an empty arg list still drains 1 byte.
        let wire: &[u8] = b"\0";
        let mut cursor = io::Cursor::new(wire);

        let args = recv_secluded_args(&mut cursor, None).expect("recv should succeed");

        assert!(args.is_empty());
        assert_eq!(
            cursor.position(),
            1,
            "empty arg list still consumes the terminator NUL"
        );
    }

    #[test]
    fn recv_secluded_args_unexpected_eof_before_terminator() {
        // Wire format: trailing `\0\0` terminator missing - premature EOF.
        let wire: &[u8] = b"arg1\0arg2";
        let mut cursor = io::Cursor::new(wire);

        let err = recv_secluded_args(&mut cursor, None)
            .expect_err("recv should fail when terminator is missing");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn recv_secluded_args_stops_exactly_after_terminator_byte() {
        // Wire format: args, terminator NUL, then a trailing greeting byte
        // that belongs to the next protocol phase. recv_secluded_args must
        // consume exactly through the terminator NUL and leave the trailing
        // byte intact for the next reader.
        //
        // This locks the boundary more strictly than the buffer-end check:
        // a function that read one byte too few would leave the cursor
        // before the terminator; a function that read one byte too many
        // would swallow the greeting byte. Only consuming exactly through
        // the terminator NUL passes.
        let wire: &[u8] = b"--server\0--sender\0.\0/path\0\0@";
        let terminator_end = wire.len() - 1;
        let mut cursor = io::Cursor::new(wire);

        let args = recv_secluded_args(&mut cursor, None).expect("recv should succeed");

        assert_eq!(args, vec!["--server", "--sender", ".", "/path"]);
        assert_eq!(
            cursor.position(),
            terminator_end as u64,
            "cursor must rest on the trailing byte after the terminator NUL"
        );

        // The trailing byte must still be readable verbatim from the cursor;
        // otherwise the subsequent protocol greeting reader sees corrupted
        // input and the handshake fails with a confusing error.
        let mut leftover = [0u8; 1];
        cursor
            .read_exact(&mut leftover)
            .expect("trailing byte must remain in the stream");
        assert_eq!(leftover, [b'@']);
    }

    // Multi-byte-trailer drain tests for UTS-1.d.followup.
    //
    // The single-trailing-byte test above proves the terminator is consumed
    // exactly. These tests strengthen the boundary check with a multi-byte
    // trailer: if recv_secluded_args left any portion of the terminator NUL
    // unread, the leftover read would see a stray `\0` instead of the
    // expected bytes. Reading the entire trailer back via read_to_end and
    // asserting byte-equality locks the drain invariant against off-by-one
    // regressions that a single-byte sentinel cannot detect (e.g. consuming
    // the first byte of the trailer instead of the terminator).

    #[test]
    fn recv_secluded_args_consumes_terminator_nul() {
        // Wire format: 4 server-mode args, terminator NUL, then a multi-byte
        // sentinel that simulates the protocol greeting bytes the next
        // reader would consume. If the terminator NUL is not fully drained,
        // the leftover slice would carry a stray leading `\0`.
        let wire: &[u8] = b"--server\0-vlogDtpre.iLsfxCIvu\0.\0src/\0\0EXTRA";
        let mut cursor = io::Cursor::new(wire);

        let args = recv_secluded_args(&mut cursor, None).expect("recv should succeed");

        assert_eq!(args, vec!["--server", "-vlogDtpre.iLsfxCIvu", ".", "src/"]);

        let mut leftover = Vec::new();
        cursor
            .read_to_end(&mut leftover)
            .expect("trailing bytes must remain readable");
        assert_eq!(
            leftover, b"EXTRA",
            "recv_secluded_args must consume the terminator NUL exactly; any \
             residual byte leaks into the next protocol phase as a stray NUL"
        );
    }

    #[test]
    fn recv_secluded_args_empty_arg_list() {
        // Wire format: an empty arg list is a single terminator NUL. The
        // trailing sentinel bytes must remain readable verbatim - the
        // terminator must be drained without swallowing any of the trailer.
        let wire: &[u8] = b"\0NEXT";
        let mut cursor = io::Cursor::new(wire);

        let args = recv_secluded_args(&mut cursor, None).expect("recv should succeed");
        assert!(args.is_empty());

        let mut leftover = Vec::new();
        cursor
            .read_to_end(&mut leftover)
            .expect("trailing bytes must remain readable");
        assert_eq!(
            leftover, b"NEXT",
            "empty arg list must consume only the terminator NUL"
        );
    }
}
