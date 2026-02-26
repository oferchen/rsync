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
//! # Upstream Reference
//!
//! - `main.c`: `send_protected_args()` / `read_args()`
//! - `options.c`: `--protect-args` flag handling

use std::io::{self, Read, Write};

/// Serializes arguments as null-separated strings with an empty terminator.
///
/// Writes each argument followed by a `\0` byte, then writes a final `\0`
/// to signal end-of-arguments. The writer is flushed after all arguments
/// are written.
///
/// # Upstream Reference
///
/// Mirrors `send_protected_args()` in upstream `main.c`.
pub fn send_secluded_args<W: Write>(writer: &mut W, args: &[&str]) -> io::Result<()> {
    for arg in args {
        writer.write_all(arg.as_bytes())?;
        writer.write_all(b"\0")?;
    }
    // Empty string terminator
    writer.write_all(b"\0")?;
    writer.flush()
}

/// Deserializes a null-separated argument list from a reader.
///
/// Reads bytes one at a time, collecting characters into arguments separated
/// by `\0` bytes. An empty argument (two consecutive `\0` bytes or a `\0`
/// at the start) signals the end of the argument list.
///
/// Returns `Ok(args)` with the parsed argument vector on success.
///
/// # Upstream Reference
///
/// Mirrors the protected-args reading logic in upstream `main.c:read_args()`.
///
/// # Errors
///
/// Returns an error if the reader encounters an I/O error or reaches EOF
/// before the terminating empty argument.
pub fn recv_secluded_args<R: Read>(reader: &mut R) -> io::Result<Vec<String>> {
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
            let arg = String::from_utf8(std::mem::take(&mut current)).map_err(|e| {
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
        send_secluded_args(&mut buf, &args).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor).expect("recv should succeed");
        assert_eq!(received, args);
    }

    #[test]
    fn round_trip_empty_args() {
        let args: Vec<&str> = vec![];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor).expect("recv should succeed");
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
        send_secluded_args(&mut buf, &args).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor).expect("recv should succeed");
        assert_eq!(received, args);
    }

    #[test]
    fn wire_format_matches_expected() {
        let args = vec!["arg1", "arg2"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args).expect("send should succeed");

        // Expected: "arg1\0arg2\0\0"
        assert_eq!(buf, b"arg1\0arg2\0\0");
    }

    #[test]
    fn single_arg_wire_format() {
        let args = vec!["hello"];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args).expect("send should succeed");
        assert_eq!(buf, b"hello\0\0");
    }

    #[test]
    fn empty_args_produces_single_null() {
        let args: Vec<&str> = vec![];
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args).expect("send should succeed");
        assert_eq!(buf, b"\0");
    }

    #[test]
    fn recv_from_truncated_stream_returns_error() {
        // Stream ends without terminator
        let buf = b"arg1\0arg2";
        let mut cursor = io::Cursor::new(&buf[..]);
        let result = recv_secluded_args(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn recv_from_empty_stream_returns_error() {
        let buf = b"";
        let mut cursor = io::Cursor::new(&buf[..]);
        let result = recv_secluded_args(&mut cursor);
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
        send_secluded_args(&mut buf, &args).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor).expect("recv should succeed");
        assert_eq!(received, args);
    }

    #[test]
    fn round_trip_long_arg_list() {
        let args: Vec<String> = (0..1000).map(|i| format!("/path/to/file_{i}")).collect();
        let args_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut buf = Vec::new();
        send_secluded_args(&mut buf, &args_refs).expect("send should succeed");

        let mut cursor = io::Cursor::new(buf);
        let received = recv_secluded_args(&mut cursor).expect("recv should succeed");
        assert_eq!(received, args);
    }
}
