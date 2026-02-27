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
//! # Upstream Reference
//!
//! - `io.c:forward_filesfrom_data()` — reads from local fd and writes to socket
//! - `io.c:start_filesfrom_forwarding()` — initializes forwarding state
//! - `flist.c:send_file_list()` — sender reads filenames from `filesfrom_fd`
//! - `options.c:server_options()` — sends `--files-from` args to server

use std::io::{self, Read, Write};

/// Reads filenames from a local source and writes them to a socket/writer
/// as NUL-separated entries, matching upstream rsync's wire format.
///
/// If `eol_nulls` is true, the input is already NUL-delimited (from `--from0`).
/// Otherwise, CR and LF characters are converted to NUL bytes. Multiple
/// consecutive NUL bytes are collapsed. The stream is terminated with a
/// double-NUL sentinel.
///
/// # Upstream Reference
///
/// - `io.c:forward_filesfrom_data()` — the core forwarding loop
pub fn forward_files_from<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    eol_nulls: bool,
) -> io::Result<()> {
    let mut buf = vec![0u8; 4096];
    let mut last_char_was_nul = false;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }

        let chunk = &mut buf[..n];

        // Convert CR/LF to NUL if not already NUL-delimited.
        // upstream: io.c:397-403 — transform CR and/or LF into '\0'
        if !eol_nulls {
            for byte in chunk.iter_mut() {
                if *byte == b'\n' || *byte == b'\r' {
                    *byte = b'\0';
                }
            }
        }

        // Write the chunk, collapsing runs of consecutive NULs.
        // upstream: io.c:456-482 — eliminate multi-'\0' runs
        for &byte in chunk.iter() {
            if byte == b'\0' {
                if !last_char_was_nul {
                    writer.write_all(b"\0")?;
                    last_char_was_nul = true;
                }
            } else {
                writer.write_all(&[byte])?;
                last_char_was_nul = false;
            }
        }
    }

    // Send end-of-file marker: double-NUL if last char was not NUL,
    // single NUL if last char was already NUL.
    // upstream: io.c:379 — write_buf(iobuf.out_fd, "\0\0", ff_lastchar ? 2 : 1)
    if last_char_was_nul {
        writer.write_all(b"\0")?;
    } else {
        writer.write_all(b"\0\0")?;
    }
    writer.flush()?;

    Ok(())
}

/// Reads NUL-separated filenames from a remote source (wire protocol).
///
/// Returns a vector of filenames. The stream is terminated by a double-NUL
/// sentinel (an empty filename after a NUL). Empty strings between NULs
/// are skipped.
///
/// This is used by the sender process to receive the file list from the
/// client when `--files-from=-` was passed (stdin/socket forwarding).
///
/// # Upstream Reference
///
/// - `flist.c:2262` — `read_line(filesfrom_fd, fbuf, sizeof fbuf, rl_flags)`
///   with `RL_EOL_NULLS` set when `reading_remotely`
pub fn read_files_from_stream<R: Read>(reader: &mut R) -> io::Result<Vec<String>> {
    let mut filenames = Vec::new();
    let mut current = Vec::new();

    let mut byte_buf = [0u8; 1];
    loop {
        let n = reader.read(&mut byte_buf)?;
        if n == 0 {
            // Unexpected EOF — return what we have.
            if !current.is_empty() {
                if let Ok(s) = String::from_utf8(std::mem::take(&mut current)) {
                    if !s.is_empty() {
                        filenames.push(s);
                    }
                }
            }
            break;
        }

        let byte = byte_buf[0];
        if byte == b'\0' {
            if current.is_empty() {
                // Double-NUL: end of stream
                break;
            }
            if let Ok(s) = String::from_utf8(std::mem::take(&mut current)) {
                if !s.is_empty() {
                    filenames.push(s);
                }
            } else {
                current.clear();
            }
        } else {
            current.push(byte);
        }
    }

    Ok(filenames)
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

        forward_files_from(&mut reader, &mut output, false).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0file3.txt\0\0");
    }

    #[test]
    fn forward_null_delimited_file() {
        let input = b"file1.txt\0file2.txt\0file3.txt\0";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, true).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0file3.txt\0\0");
    }

    #[test]
    fn forward_crlf_endings() {
        let input = b"file1.txt\r\nfile2.txt\r\n";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0\0");
    }

    #[test]
    fn forward_no_trailing_newline() {
        let input = b"file1.txt\nfile2.txt";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0\0");
    }

    #[test]
    fn forward_empty_input() {
        let input = b"";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false).unwrap();

        assert_eq!(output, b"\0\0");
    }

    #[test]
    fn forward_single_file() {
        let input = b"only.txt\n";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false).unwrap();

        assert_eq!(output, b"only.txt\0\0");
    }

    #[test]
    fn forward_blank_lines_collapsed() {
        let input = b"file1.txt\n\n\nfile2.txt\n";
        let mut reader = Cursor::new(input);
        let mut output = Vec::new();

        forward_files_from(&mut reader, &mut output, false).unwrap();

        assert_eq!(output, b"file1.txt\0file2.txt\0\0");
    }

    #[test]
    fn read_nul_terminated_stream() {
        let input = b"file1.txt\0file2.txt\0file3.txt\0\0";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader).unwrap();

        assert_eq!(files, vec!["file1.txt", "file2.txt", "file3.txt"]);
    }

    #[test]
    fn read_empty_stream() {
        let input = b"\0";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader).unwrap();

        assert!(files.is_empty());
    }

    #[test]
    fn read_single_file_stream() {
        let input = b"only.txt\0\0";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader).unwrap();

        assert_eq!(files, vec!["only.txt"]);
    }

    #[test]
    fn read_unexpected_eof() {
        let input = b"partial.txt";
        let mut reader = Cursor::new(input);

        let files = read_files_from_stream(&mut reader).unwrap();

        assert_eq!(files, vec!["partial.txt"]);
    }

    #[test]
    fn roundtrip_newline_delimited() {
        let input = b"alpha.txt\nbeta.txt\ngamma.txt\n";
        let mut reader = Cursor::new(input);
        let mut wire = Vec::new();

        forward_files_from(&mut reader, &mut wire, false).unwrap();

        let mut wire_reader = Cursor::new(&wire);
        let files = read_files_from_stream(&mut wire_reader).unwrap();

        assert_eq!(files, vec!["alpha.txt", "beta.txt", "gamma.txt"]);
    }

    #[test]
    fn roundtrip_null_delimited() {
        let input = b"one.txt\0two.txt\0three.txt\0";
        let mut reader = Cursor::new(input);
        let mut wire = Vec::new();

        forward_files_from(&mut reader, &mut wire, true).unwrap();

        let mut wire_reader = Cursor::new(&wire);
        let files = read_files_from_stream(&mut wire_reader).unwrap();

        assert_eq!(files, vec!["one.txt", "two.txt", "three.txt"]);
    }
}
