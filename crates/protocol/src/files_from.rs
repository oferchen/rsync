//! Wire protocol for `--files-from` file list forwarding.
//!
//! When `--files-from` specifies a local file and the transfer is remote,
//! the client reads the file locally and forwards its contents over the
//! protocol connection as NUL-separated filenames. The sender process on
//! the remote end reads these filenames to build the file list.
//!
//! When `--files-from` specifies a remote file (`:path` prefix), the server
//! opens the file directly and reads it. No forwarding is needed.
//!
//! # Wire Format
//!
//! Filenames are sent as NUL-separated (`\0`) byte strings. CR and LF
//! characters in the source are converted to NUL. Multiple consecutive NUL
//! bytes are collapsed to a single NUL. The stream ends with a double-NUL
//! (`\0\0`) sentinel, or a single NUL if the last filename already ends
//! with NUL.
//!
//! # Charset Conversion
//!
//! When `--iconv` is configured and `--protect-args`/`--secluded-args` is in
//! effect, upstream rsync transcodes each NUL-separated entry: the writer
//! converts local-charset bytes to UTF-8 wire bytes, and the reader converts
//! UTF-8 wire bytes back to local-charset bytes. The conversion is applied
//! per entry so the NUL delimiters and double-NUL terminator stay verbatim.
//!
//! # Upstream Reference
//!
//! - `io.c:forward_filesfrom_data()` — reads from local fd and writes to socket
//! - `io.c:start_filesfrom_forwarding()` — initializes forwarding state
//! - `io.c:read_line()` (RL_CONVERT branch) — reader-side iconv
//! - `compat.c:799-806` — `filesfrom_convert` gating (`protect_args && files_from`)
//! - `flist.c:send_file_list()` — sender reads filenames from `filesfrom_fd`
//! - `options.c:server_options()` — sends `--files-from` args to server

use std::borrow::Cow;
use std::io::{self, Read, Write};

use crate::iconv::FilenameConverter;

/// Reads filenames from a local source and writes them to a socket/writer
/// as NUL-separated entries, matching upstream rsync's wire format.
///
/// If `eol_nulls` is true, the input is already NUL-delimited (from `--from0`).
/// Otherwise, CR and LF characters are converted to NUL bytes. Multiple
/// consecutive NUL bytes are collapsed. The stream is terminated with a
/// double-NUL sentinel.
///
/// When `iconv` is `Some`, each NUL-separated entry is transcoded with
/// [`FilenameConverter::local_to_remote`] before being written to the wire,
/// mirroring upstream `forward_filesfrom_data`'s `iconvbufs(ic_send, ...)`
/// call. When `iconv` is `None`, bytes are forwarded verbatim — equivalent
/// to upstream's `ic_send == (iconv_t)-1` case.
///
/// # Upstream Reference
///
/// - `io.c:forward_filesfrom_data()` — the core forwarding loop
/// - `compat.c:799-806` — `filesfrom_convert` gating predicate
pub fn forward_files_from<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    eol_nulls: bool,
    iconv: Option<&FilenameConverter>,
) -> io::Result<()> {
    let mut buf = vec![0u8; 4096];
    let mut current_entry: Vec<u8> = Vec::new();
    // Tracks whether the last byte written to the wire was a NUL, so the
    // EOF terminator length matches upstream's `ff_lastchar` semantics.
    let mut last_emitted_nul = false;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }

        let chunk = &mut buf[..n];

        // upstream: io.c:397-403 — transform CR and/or LF into '\0'.
        if !eol_nulls {
            for byte in chunk.iter_mut() {
                if *byte == b'\n' || *byte == b'\r' {
                    *byte = b'\0';
                }
            }
        }

        for &byte in chunk.iter() {
            if byte == b'\0' {
                if !current_entry.is_empty() {
                    write_filesfrom_entry(writer, &current_entry, iconv)?;
                    current_entry.clear();
                    last_emitted_nul = true;
                }
                // upstream: io.c:456-482 — collapse runs of consecutive '\0'.
            } else {
                current_entry.push(byte);
            }
        }
    }

    // Flush a trailing entry that lacks a terminating NUL.
    if !current_entry.is_empty() {
        write_filesfrom_entry(writer, &current_entry, iconv)?;
        current_entry.clear();
        last_emitted_nul = true;
    }

    // upstream: io.c:379 — write_buf(iobuf.out_fd, "\0\0", ff_lastchar ? 2 : 1).
    if last_emitted_nul {
        writer.write_all(b"\0")?;
    } else {
        writer.write_all(b"\0\0")?;
    }
    writer.flush()?;

    Ok(())
}

/// Writes a single `--files-from` entry, applying iconv when configured.
fn write_filesfrom_entry<W: Write>(
    writer: &mut W,
    entry: &[u8],
    iconv: Option<&FilenameConverter>,
) -> io::Result<()> {
    let bytes: Cow<'_, [u8]> = match iconv {
        Some(converter) => match converter.local_to_remote(entry) {
            Ok(cow) => cow,
            // upstream io.c uses ICB_INCLUDE_BAD: bad bytes are passed through
            // verbatim rather than aborting the forward.
            Err(_) => Cow::Borrowed(entry),
        },
        None => Cow::Borrowed(entry),
    };
    writer.write_all(&bytes)?;
    writer.write_all(b"\0")?;
    Ok(())
}

/// Reads NUL-separated filenames from a remote source (wire protocol).
///
/// Returns a vector of filenames. The stream is terminated by a double-NUL
/// sentinel (an empty filename after a NUL). Empty strings between NULs
/// are skipped.
///
/// When `iconv` is `Some`, each NUL-separated entry is transcoded with
/// [`FilenameConverter::remote_to_local`] before being decoded into a
/// `String`, mirroring upstream `read_line(RL_CONVERT)`'s
/// `iconvbufs(ic_recv, ...)` call. When `iconv` is `None`, wire bytes are
/// decoded verbatim — equivalent to upstream's `ic_recv == (iconv_t)-1`
/// case.
///
/// This is used by the sender process to receive the file list from the
/// client when `--files-from=-` was passed (stdin/socket forwarding).
///
/// # Upstream Reference
///
/// - `flist.c:2262` — `read_line(filesfrom_fd, fbuf, sizeof fbuf, rl_flags)`
///   with `RL_EOL_NULLS` set when `reading_remotely`
/// - `io.c:read_line()` (RL_CONVERT branch) — `iconvbufs(ic_recv, ...)`
/// - `compat.c:799-806` — `filesfrom_convert` gating predicate
pub fn read_files_from_stream<R: Read>(
    reader: &mut R,
    iconv: Option<&FilenameConverter>,
) -> io::Result<Vec<String>> {
    let mut filenames = Vec::new();
    let mut current = Vec::new();

    let mut byte_buf = [0u8; 1];
    loop {
        let n = reader.read(&mut byte_buf)?;
        if n == 0 {
            // Unexpected EOF — flush any pending entry then return.
            if !current.is_empty() {
                push_decoded_filename(&mut filenames, &current, iconv);
                current.clear();
            }
            break;
        }

        let byte = byte_buf[0];
        if byte == b'\0' {
            if current.is_empty() {
                // Double-NUL: end of stream.
                break;
            }
            push_decoded_filename(&mut filenames, &current, iconv);
            current.clear();
        } else {
            current.push(byte);
        }
    }

    Ok(filenames)
}

/// Decodes a single wire entry into a `String`, applying iconv when configured.
///
/// Non-UTF-8 results are silently dropped to preserve the existing String-based
/// representation. Upstream's `ICB_INCLUDE_BAD` flag (pass through bad bytes)
/// is honored only at the iconv layer; the final UTF-8 check is a downstream
/// limitation of the `Vec<String>` API.
fn push_decoded_filename(out: &mut Vec<String>, raw: &[u8], iconv: Option<&FilenameConverter>) {
    let bytes: Cow<'_, [u8]> = match iconv {
        Some(converter) => match converter.remote_to_local(raw) {
            Ok(cow) => cow,
            // upstream: ICB_INCLUDE_BAD — keep the bad bytes rather than abort.
            Err(_) => Cow::Borrowed(raw),
        },
        None => Cow::Borrowed(raw),
    };
    if let Ok(s) = std::str::from_utf8(&bytes)
        && !s.is_empty()
    {
        out.push(s.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn forward_newline_delimited_file() {
        let input = b"file1.txt\nfile2.txt\nfile3.txt\n";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false, None).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0file3.txt\0\0");
    }

    #[test]
    fn forward_null_delimited_file() {
        let input = b"file1.txt\0file2.txt\0file3.txt\0";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, true, None).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0file3.txt\0\0");
    }

    #[test]
    fn forward_crlf_endings() {
        let input = b"file1.txt\r\nfile2.txt\r\n";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false, None).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0\0");
    }

    #[test]
    fn forward_no_trailing_newline() {
        let input = b"file1.txt\nfile2.txt";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false, None).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0\0");
    }

    #[test]
    fn forward_empty_input() {
        let input = b"";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false, None).unwrap();

        assert_eq!(output, b"\0\0");
    }

    #[test]
    fn forward_single_file() {
        let input = b"only.txt\n";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false, None).unwrap();

        assert_eq!(output, b"only.txt\0\0");
    }

    #[test]
    fn forward_blank_lines_collapsed() {
        let input = b"file1.txt\n\n\nfile2.txt\n";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false, None).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0\0");
    }

    #[test]
    fn read_nul_terminated_stream() {
        let input = b"file1.txt\0file2.txt\0file3.txt\0\0";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader, None).unwrap();

        assert_eq!(files, vec!["file1.txt", "file2.txt", "file3.txt"]);
    }

    #[test]
    fn read_empty_stream() {
        let input = b"\0";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader, None).unwrap();

        assert!(files.is_empty());
    }

    #[test]
    fn read_single_file_stream() {
        let input = b"only.txt\0\0";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader, None).unwrap();

        assert_eq!(files, vec!["only.txt"]);
    }

    #[test]
    fn read_unexpected_eof() {
        let input = b"partial.txt";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader, None).unwrap();

        assert_eq!(files, vec!["partial.txt"]);
    }

    #[test]
    fn roundtrip_newline_delimited() {
        let input = b"alpha.txt\nbeta.txt\ngamma.txt\n";
        let mut reader = Cursor::new(input);
        let mut wire = Vec::new();

        forward_files_from(&mut reader, &mut wire, false, None).unwrap();

        let mut wire_reader = Cursor::new(&wire);
        let files = read_files_from_stream(&mut wire_reader, None).unwrap();

        assert_eq!(files, vec!["alpha.txt", "beta.txt", "gamma.txt"]);
    }

    #[test]
    fn roundtrip_null_delimited() {
        let input = b"one.txt\0two.txt\0three.txt\0";
        let mut reader = Cursor::new(input);
        let mut wire = Vec::new();

        forward_files_from(&mut reader, &mut wire, true, None).unwrap();

        let mut wire_reader = Cursor::new(&wire);
        let files = read_files_from_stream(&mut wire_reader, None).unwrap();

        assert_eq!(files, vec!["one.txt", "two.txt", "three.txt"]);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn forward_with_identity_converter_matches_no_iconv() {
        let identity = FilenameConverter::identity();
        let input = b"alpha.txt\nbeta.txt\n";
        let mut wire_with = Vec::new();
        let mut wire_without = Vec::new();

        forward_files_from(
            &mut Cursor::new(input),
            &mut wire_with,
            false,
            Some(&identity),
        )
        .unwrap();
        forward_files_from(&mut Cursor::new(input), &mut wire_without, false, None).unwrap();

        assert_eq!(wire_with, wire_without);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn forward_transcodes_each_entry_separately() {
        // Latin-1 local → UTF-8 wire. NUL delimiters must remain verbatim
        // and each entry is converted in isolation.
        let converter = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("converter");
        // Latin-1 bytes for "café" (0xE9 = é) and "naïve" (0xEF = ï).
        let input: &[u8] = b"caf\xE9\nna\xEFve\n";
        let mut wire = Vec::new();

        forward_files_from(&mut Cursor::new(input), &mut wire, false, Some(&converter)).unwrap();

        // UTF-8 encoding: é = C3 A9, ï = C3 AF.
        assert_eq!(wire, b"caf\xC3\xA9\0na\xC3\xAFve\0\0");
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn read_transcodes_each_entry_separately() {
        // UTF-8 wire → Latin-1 local. Each entry is converted in isolation,
        // so NUL delimiters and the double-NUL terminator stay verbatim.
        // We verify the round-trip in the next test rather than reading
        // raw Latin-1 bytes through the String API (which requires UTF-8).
        let converter = FilenameConverter::new("UTF-8", "ISO-8859-1").expect("converter");
        // Wire bytes are Latin-1 (remote=ISO-8859-1) for "café" / "naïve".
        let wire: &[u8] = b"caf\xE9\0na\xEFve\0\0";

        let files = read_files_from_stream(&mut Cursor::new(wire), Some(&converter)).unwrap();

        assert_eq!(files, vec!["café", "naïve"]);
    }

    #[cfg(feature = "iconv")]
    #[test]
    fn roundtrip_with_iconv_latin1_local_utf8_wire() {
        // Local is Latin-1 (writer side uses local_to_remote = Latin-1 → UTF-8).
        // Reader side uses remote_to_local = UTF-8 → UTF-8 (no-op when local
        // matches wire), so we instead read with a UTF-8/UTF-8 identity to
        // observe the post-write wire bytes as a String.
        let writer_iconv = FilenameConverter::new("ISO-8859-1", "UTF-8").expect("writer converter");
        let reader_iconv = FilenameConverter::identity();

        let input: &[u8] = b"caf\xE9\nna\xEFve\n";
        let mut wire = Vec::new();
        forward_files_from(
            &mut Cursor::new(input),
            &mut wire,
            false,
            Some(&writer_iconv),
        )
        .unwrap();

        let files = read_files_from_stream(&mut Cursor::new(&wire), Some(&reader_iconv)).unwrap();
        assert_eq!(files, vec!["café", "naïve"]);
    }
}
