//! Filename octal escaping matching upstream rsync's `filtered_fwrite`
//! in `log.c:225-246`.
//!
//! Upstream rsync escapes non-printable bytes in filenames as `\#ooo`
//! (backslash, hash, three octal digits). The `--8-bit-output` / `-8`
//! flag disables escaping for high-bit characters (0x80-0xFF), leaving
//! only control characters below 0x20 (except tab) escaped.

use std::fmt::Write;
use std::path::Path;

/// Returns `true` when a byte is printable in the C locale.
///
/// upstream: itypes.h:isPrint() wraps libc `isprint()`, which in the C
/// locale returns true for bytes 0x20 through 0x7E.
#[inline]
fn is_c_print(byte: u8) -> bool {
    (0x20..=0x7E).contains(&byte)
}

/// Escapes non-printable bytes for display output, matching upstream
/// rsync's `filtered_fwrite` in `log.c:225-246`.
///
/// When `allow_8bit` is false (the default), bytes outside ASCII
/// printable range (0x20-0x7E) are escaped as `\#ooo` (backslash, hash,
/// three octal digits). Tab (0x09) is passed through unescaped.
///
/// When `allow_8bit` is true (`--8-bit-output`), only control characters
/// below 0x20 (except tab) are escaped. High-bit bytes (0x80-0xFF) and
/// DEL (0x7F) pass through for terminal rendering.
///
/// Literal `\#ddd` sequences (where each `d` is an ASCII digit) in the
/// input are also escaped to prevent ambiguity with the escape notation.
pub(crate) fn escape_for_output(input: &[u8], allow_8bit: bool) -> String {
    // Fast path: if all bytes are ASCII printable or tab, return as-is.
    if !allow_8bit {
        let all_safe = input.iter().all(|&b| is_c_print(b) || b == b'\t');
        if all_safe && !has_literal_escape_sequence(input) {
            // SAFETY: all bytes are in 0x09 or 0x20-0x7E, which is valid ASCII/UTF-8
            return String::from_utf8(input.to_vec())
                .unwrap_or_else(|_| escape_bytes_slow(input, allow_8bit));
        }
    } else {
        let all_safe = input.iter().all(|&b| b >= b' ' || b == b'\t');
        if all_safe && !has_literal_escape_sequence(input) {
            return String::from_utf8_lossy(input).into_owned();
        }
    }

    escape_bytes_slow(input, allow_8bit)
}

/// Returns `true` when the input contains a literal `\#ddd` sequence.
fn has_literal_escape_sequence(input: &[u8]) -> bool {
    let len = input.len();
    if len < 5 {
        return false;
    }
    for i in 0..len - 4 {
        if input[i] == b'\\'
            && input[i + 1] == b'#'
            && input[i + 2].is_ascii_digit()
            && input[i + 3].is_ascii_digit()
            && input[i + 4].is_ascii_digit()
        {
            return true;
        }
    }
    false
}

/// Slow path: escapes bytes one at a time.
fn escape_bytes_slow(input: &[u8], allow_8bit: bool) -> String {
    let mut output = String::with_capacity(input.len() + input.len() / 4);
    let len = input.len();
    let mut i = 0;

    while i < len {
        let byte = input[i];

        // upstream: log.c:235-236 - escape literal \#ddd sequences to prevent
        // ambiguity with the escape notation. If the input contains \#001
        // literally, escape the backslash so the output reads \#134#001.
        if i + 4 < len
            && byte == b'\\'
            && input[i + 1] == b'#'
            && input[i + 2].is_ascii_digit()
            && input[i + 3].is_ascii_digit()
            && input[i + 4].is_ascii_digit()
        {
            let _ = write!(output, "\\#{byte:03o}");
            i += 1;
            continue;
        }

        // upstream: log.c:237 - the escaping condition is:
        //   *in_buf != '\t'
        //   && ((use_isprint && !isPrint(in_buf)) || *(uchar*)in_buf < ' ')
        //
        // use_isprint = !allow_8bit_chars, so:
        //   not tab AND ((!allow_8bit AND not printable) OR byte < space)
        let needs_escape = byte != b'\t' && ((!allow_8bit && !is_c_print(byte)) || byte < b' ');

        if needs_escape {
            let _ = write!(output, "\\#{byte:03o}");
        } else if byte.is_ascii() {
            output.push(byte as char);
        } else {
            // High-bit byte with allow_8bit=true. Emit as the Latin-1
            // code point - this is the closest Rust equivalent to
            // upstream's raw byte pass-through.
            output.push(byte as char);
        }
        i += 1;
    }

    output
}

/// Escapes a path for display output.
///
/// On Unix, operates on the raw bytes of the path to faithfully represent
/// non-UTF-8 filenames. On other platforms, falls back to `to_string_lossy()`
/// before escaping.
pub(crate) fn escape_path(path: &Path, allow_8bit: bool) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        escape_for_output(path.as_os_str().as_bytes(), allow_8bit)
    }
    #[cfg(not(unix))]
    {
        let lossy = path.to_string_lossy();
        escape_for_output(lossy.as_bytes(), allow_8bit)
    }
}

/// Escapes a string for display output.
///
/// Convenience wrapper for already-converted strings (e.g. from
/// `to_string_lossy()`). Escapes non-printable bytes in the UTF-8
/// representation.
#[cfg(test)]
fn escape_str(s: &str, allow_8bit: bool) -> String {
    escape_for_output(s.as_bytes(), allow_8bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- escape_for_output: default mode (allow_8bit = false) --

    #[test]
    fn ascii_printable_passes_through() {
        let input = b"hello_world.txt";
        assert_eq!(escape_for_output(input, false), "hello_world.txt");
    }

    #[test]
    fn space_passes_through() {
        let input = b"hello world.txt";
        assert_eq!(escape_for_output(input, false), "hello world.txt");
    }

    #[test]
    fn tab_passes_through() {
        let input = b"before\tafter";
        assert_eq!(escape_for_output(input, false), "before\tafter");
    }

    #[test]
    fn control_char_0x01_escaped() {
        assert_eq!(escape_for_output(&[0x01], false), "\\#001");
    }

    #[test]
    fn del_0x7f_escaped() {
        assert_eq!(escape_for_output(&[0x7F], false), "\\#177");
    }

    #[test]
    fn high_bit_0x80_escaped_in_default_mode() {
        assert_eq!(escape_for_output(&[0x80], false), "\\#200");
    }

    #[test]
    fn high_bit_0xff_escaped_in_default_mode() {
        assert_eq!(escape_for_output(&[0xFF], false), "\\#377");
    }

    #[test]
    fn null_byte_escaped() {
        assert_eq!(escape_for_output(&[0x00], false), "\\#000");
    }

    #[test]
    fn mixed_printable_and_control() {
        let input = b"file\x01name\x7f.txt";
        assert_eq!(escape_for_output(input, false), "file\\#001name\\#177.txt");
    }

    #[test]
    fn all_specified_bytes_escaped_correctly() {
        // Verify the exact values from the task description.
        assert_eq!(escape_for_output(&[0x01], false), "\\#001");
        assert_eq!(escape_for_output(&[0x7F], false), "\\#177");
        assert_eq!(escape_for_output(&[0x80], false), "\\#200");
        assert_eq!(escape_for_output(&[0xFF], false), "\\#377");
    }

    // -- escape_for_output: 8-bit mode (allow_8bit = true) --

    #[test]
    fn control_char_escaped_in_8bit_mode() {
        assert_eq!(escape_for_output(&[0x01], true), "\\#001");
    }

    #[test]
    fn del_0x7f_passes_through_in_8bit_mode() {
        // upstream: with use_isprint=0 (allow_8bit=1), the condition is
        // byte != '\t' && byte < ' ', which excludes 0x7F.
        assert_eq!(escape_for_output(&[0x7F], true), "\x7F");
    }

    #[test]
    fn high_bit_0x80_passes_through_in_8bit_mode() {
        assert_eq!(escape_for_output(&[0x80], true), "\u{80}");
    }

    #[test]
    fn high_bit_0xff_passes_through_in_8bit_mode() {
        assert_eq!(escape_for_output(&[0xFF], true), "\u{FF}");
    }

    #[test]
    fn tab_passes_through_in_8bit_mode() {
        assert_eq!(escape_for_output(b"\t", true), "\t");
    }

    #[test]
    fn only_control_chars_escaped_in_8bit_mode() {
        // 0x01 and 0x7F: only 0x01 is escaped (< 0x20)
        let input = &[0x01, 0x7F];
        assert_eq!(escape_for_output(input, true), "\\#001\x7F");
    }

    // -- Literal \#ddd sequence escaping --

    #[test]
    fn literal_escape_sequence_is_escaped() {
        // A filename containing literal \#001 should escape the backslash.
        let input = b"file\\#001.txt";
        assert_eq!(escape_for_output(input, false), "file\\#134#001.txt");
    }

    #[test]
    fn literal_escape_sequence_escaped_in_8bit_mode() {
        let input = b"file\\#999.txt";
        assert_eq!(escape_for_output(input, true), "file\\#134#999.txt");
    }

    #[test]
    fn non_digit_after_hash_not_escaped() {
        // \#abc is not a valid escape sequence - leave it alone.
        let input = b"file\\#abc.txt";
        assert_eq!(escape_for_output(input, false), "file\\#abc.txt");
    }

    #[test]
    fn partial_escape_sequence_at_end_not_escaped() {
        // \#12 at end (only 2 digits) - not an escape sequence.
        let input = b"file\\#12";
        assert_eq!(escape_for_output(input, false), "file\\#12");
    }

    // -- escape_path --

    #[test]
    fn escape_path_ascii() {
        let path = Path::new("src/main.rs");
        assert_eq!(escape_path(path, false), "src/main.rs");
    }

    #[test]
    fn escape_path_with_directory_separator() {
        let path = Path::new("foo/bar/baz.txt");
        assert_eq!(escape_path(path, false), "foo/bar/baz.txt");
    }

    // -- escape_str --

    #[test]
    fn escape_str_passes_printable() {
        assert_eq!(escape_str("hello.txt", false), "hello.txt");
    }

    #[test]
    fn escape_str_escapes_control() {
        assert_eq!(escape_str("a\x01b", false), "a\\#001b");
    }

    // -- Fast path coverage --

    #[test]
    fn fast_path_all_printable() {
        let input = b"abcdefghijklmnopqrstuvwxyz/0123456789";
        assert_eq!(
            escape_for_output(input, false),
            "abcdefghijklmnopqrstuvwxyz/0123456789"
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(escape_for_output(b"", false), "");
        assert_eq!(escape_for_output(b"", true), "");
    }
}
