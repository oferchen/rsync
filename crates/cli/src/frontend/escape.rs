//! Filename octal escaping matching upstream rsync's `filtered_fwrite`
//! in `log.c:225-246`.
//!
//! Upstream rsync escapes non-printable bytes in filenames as `\#ooo`
//! (backslash, hash, three octal digits). The `--8-bit-output` / `-8`
//! flag disables escaping for high-bit characters (0x80-0xFF), leaving
//! only control characters below 0x20 (except tab) escaped.

use std::io::Write;
use std::path::Path;

/// Returns `true` when a byte is printable in the C locale.
///
/// upstream: itypes.h:isPrint() wraps libc `isprint()`, which in the C
/// locale returns true for bytes 0x20 through 0x7E.
#[inline]
fn is_c_print(byte: u8) -> bool {
    (0x20..=0x7E).contains(&byte)
}

/// Decides whether a single byte must be octal-escaped as `\#ooo`.
///
/// upstream: log.c:237 `filtered_fwrite` escape condition:
///   `*in_buf != '\t' && ((use_isprint && !isPrint(in_buf)) || *(uchar*)in_buf < ' ')`
/// where `use_isprint = !allow_8bit_chars` (log.c:398). Strategy on the
/// `--8-bit-output` flag:
///   - default (`eight_bit` false): escape tab-excluded non-printable bytes,
///     which includes DEL (0x7F) and the high range 0x80-0xFF.
///   - `-8` (`eight_bit` true): escape only control bytes below 0x20 (except
///     tab); DEL and high bytes pass through raw for terminal rendering.
#[inline]
fn should_escape_byte(byte: u8, eight_bit: bool) -> bool {
    byte != b'\t' && ((!eight_bit && !is_c_print(byte)) || byte < b' ')
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
///
/// The return value is a raw byte buffer, not a `String`: upstream
/// `filtered_fwrite` writes filename bytes to the output fd unmodified, so a
/// lone invalid-UTF-8 byte (e.g. `0x80`) passed through under `--8-bit-output`
/// must survive verbatim. A `String` cannot hold arbitrary invalid UTF-8, so
/// returning `Vec<u8>` and writing it directly to a byte sink is the only way
/// to reach byte-for-byte parity with upstream on that edge case.
pub(crate) fn escape_for_output(input: &[u8], allow_8bit: bool) -> Vec<u8> {
    // Fast path: all bytes ASCII-printable or tab (0x09, 0x20-0x7E) -> no byte
    // needs escaping in either mode and there is no literal `\#ddd` to guard,
    // so return the bytes verbatim.
    //
    // DEL (0x7F) and high bytes (0x80-0xFF) are deliberately excluded here so
    // they always take the slow path, which applies the correct per-mode
    // handling: escape as \#ooo when !allow_8bit, or pass through raw when
    // allow_8bit.
    let all_safe = input.iter().all(|&b| is_c_print(b) || b == b'\t');
    if all_safe && !has_literal_escape_sequence(input) {
        return input.to_vec();
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

/// Slow path: escapes bytes one at a time into a raw byte buffer.
///
/// Non-escaped bytes are emitted raw (upstream `filtered_fwrite` copies the
/// input byte verbatim). This matters under `-8`: a multi-byte UTF-8 filename
/// such as `café` (bytes `63 61 66 c3 a9`) passes through as the original bytes
/// `c3 a9`, and a lone invalid byte such as `0x80` passes through as the single
/// byte `80` - neither is re-encoded nor replaced with U+FFFD. Escaped bytes
/// are written as their ASCII `\#ooo` form.
fn escape_bytes_slow(input: &[u8], allow_8bit: bool) -> Vec<u8> {
    let mut output: Vec<u8> = Vec::with_capacity(input.len() + input.len() / 4);
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

        if should_escape_byte(byte, allow_8bit) {
            let _ = write!(output, "\\#{byte:03o}");
        } else {
            output.push(byte);
        }
        i += 1;
    }

    output
}

/// Escapes a path for display output, returning raw bytes.
///
/// On Unix, operates on the raw bytes of the path to faithfully represent
/// non-UTF-8 filenames. On other platforms, falls back to `to_string_lossy()`
/// before escaping. The bytes are meant to be written directly to a byte sink
/// (stdout/stderr); interpolating them through a `String` would replace lone
/// invalid bytes with U+FFFD, diverging from upstream `filtered_fwrite`.
pub(crate) fn escape_path(path: &Path, allow_8bit: bool) -> Vec<u8> {
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

/// Escapes a string for display output, returning raw bytes.
///
/// Convenience wrapper for already-converted strings (e.g. from
/// `to_string_lossy()`). Escapes non-printable bytes in the UTF-8
/// representation.
#[cfg(test)]
fn escape_str(s: &str, allow_8bit: bool) -> Vec<u8> {
    escape_for_output(s.as_bytes(), allow_8bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- escape_for_output: default mode (allow_8bit = false) --

    #[test]
    fn ascii_printable_passes_through() {
        let input = b"hello_world.txt";
        assert_eq!(escape_for_output(input, false), b"hello_world.txt".to_vec());
    }

    #[test]
    fn space_passes_through() {
        let input = b"hello world.txt";
        assert_eq!(escape_for_output(input, false), b"hello world.txt".to_vec());
    }

    #[test]
    fn tab_passes_through() {
        let input = b"before\tafter";
        assert_eq!(escape_for_output(input, false), b"before\tafter".to_vec());
    }

    #[test]
    fn control_char_0x01_escaped() {
        assert_eq!(escape_for_output(&[0x01], false), b"\\#001".to_vec());
    }

    #[test]
    fn del_0x7f_escaped() {
        assert_eq!(escape_for_output(&[0x7F], false), b"\\#177".to_vec());
    }

    #[test]
    fn high_bit_0x80_escaped_in_default_mode() {
        // upstream: default mode (use_isprint=1) octal-escapes a lone high byte.
        assert_eq!(escape_for_output(&[0x80], false), b"\\#200".to_vec());
    }

    #[test]
    fn high_bit_0xff_escaped_in_default_mode() {
        assert_eq!(escape_for_output(&[0xFF], false), b"\\#377".to_vec());
    }

    #[test]
    fn null_byte_escaped() {
        assert_eq!(escape_for_output(&[0x00], false), b"\\#000".to_vec());
    }

    #[test]
    fn mixed_printable_and_control() {
        let input = b"file\x01name\x7f.txt";
        assert_eq!(
            escape_for_output(input, false),
            b"file\\#001name\\#177.txt".to_vec()
        );
    }

    #[test]
    fn all_specified_bytes_escaped_correctly() {
        // Verify the exact values from the task description.
        assert_eq!(escape_for_output(&[0x01], false), b"\\#001".to_vec());
        assert_eq!(escape_for_output(&[0x7F], false), b"\\#177".to_vec());
        assert_eq!(escape_for_output(&[0x80], false), b"\\#200".to_vec());
        assert_eq!(escape_for_output(&[0xFF], false), b"\\#377".to_vec());
    }

    // -- escape_for_output: 8-bit mode (allow_8bit = true) --

    #[test]
    fn control_char_escaped_in_8bit_mode() {
        assert_eq!(escape_for_output(&[0x01], true), b"\\#001".to_vec());
    }

    #[test]
    fn del_0x7f_passes_through_in_8bit_mode() {
        // upstream: with use_isprint=0 (allow_8bit=1), the condition is
        // byte != '\t' && byte < ' ', which excludes 0x7F.
        assert_eq!(escape_for_output(&[0x7F], true), vec![0x7F]);
    }

    #[test]
    fn lone_high_byte_0x80_is_raw_in_8bit_mode() {
        // upstream: `filtered_fwrite` writes the raw byte to the output fd when
        // allow_8bit_chars is set (log.c:225-246). A lone 0x80 is invalid UTF-8
        // and cannot live in a Rust String, so the escape layer returns raw
        // bytes and the writer emits exactly one byte, matching `rsync -8`
        // byte-for-byte. Previously the `from_utf8_lossy` return type yielded
        // U+FFFD here.
        assert_eq!(escape_for_output(&[0x80], true), vec![0x80]);
        // Exactly one byte reaches the sink - no U+FFFD (ef bf bd) expansion.
        assert_eq!(escape_for_output(&[0x80], true).len(), 1);
    }

    #[test]
    fn lone_high_byte_0xff_is_raw_in_8bit_mode() {
        assert_eq!(escape_for_output(&[0xFF], true), vec![0xFF]);
        assert_eq!(escape_for_output(&[0xFF], true).len(), 1);
    }

    #[test]
    fn tab_passes_through_in_8bit_mode() {
        assert_eq!(escape_for_output(b"\t", true), b"\t".to_vec());
    }

    #[test]
    fn multibyte_utf8_passes_raw_in_8bit_mode() {
        // WHY: upstream `filtered_fwrite` copies non-escaped bytes verbatim, so
        // `-8` on `café` (bytes 63 61 66 c3 a9) must yield the original bytes
        // c3 a9, matching `rsync -v -8` byte-for-byte. A byte-to-code-point cast
        // would re-encode to c3 83 c2 a9 (mojibake `Ã©`).
        let input = b"caf\xc3\xa9";
        let out = escape_for_output(input, true);
        assert_eq!(out, b"caf\xc3\xa9".to_vec());
    }

    #[test]
    fn multibyte_utf8_escaped_octal_in_default_mode() {
        // WHY: default mode (use_isprint=1) escapes every non-printable byte,
        // so each UTF-8 continuation byte of `café` becomes its own \#ooo.
        let input = b"caf\xc3\xa9";
        assert_eq!(escape_for_output(input, false), b"caf\\#303\\#251".to_vec());
    }

    #[test]
    fn only_control_chars_escaped_in_8bit_mode() {
        // 0x01 and 0x7F: only 0x01 is escaped (< 0x20)
        let input = &[0x01, 0x7F];
        assert_eq!(escape_for_output(input, true), b"\\#001\x7F".to_vec());
    }

    // -- Literal \#ddd sequence escaping --

    #[test]
    fn literal_escape_sequence_is_escaped() {
        // A filename containing literal \#001 should escape the backslash.
        let input = b"file\\#001.txt";
        assert_eq!(
            escape_for_output(input, false),
            b"file\\#134#001.txt".to_vec()
        );
    }

    #[test]
    fn literal_escape_sequence_escaped_in_8bit_mode() {
        let input = b"file\\#999.txt";
        assert_eq!(
            escape_for_output(input, true),
            b"file\\#134#999.txt".to_vec()
        );
    }

    #[test]
    fn non_digit_after_hash_not_escaped() {
        // \#abc is not a valid escape sequence - leave it alone.
        let input = b"file\\#abc.txt";
        assert_eq!(escape_for_output(input, false), b"file\\#abc.txt".to_vec());
    }

    #[test]
    fn partial_escape_sequence_at_end_not_escaped() {
        // \#12 at end (only 2 digits) - not an escape sequence.
        let input = b"file\\#12";
        assert_eq!(escape_for_output(input, false), b"file\\#12".to_vec());
    }

    // -- escape_path --

    #[test]
    fn escape_path_ascii() {
        let path = Path::new("src/main.rs");
        assert_eq!(escape_path(path, false), b"src/main.rs".to_vec());
    }

    #[test]
    fn escape_path_with_directory_separator() {
        let path = Path::new("foo/bar/baz.txt");
        assert_eq!(escape_path(path, false), b"foo/bar/baz.txt".to_vec());
    }

    // -- escape_str --

    #[test]
    fn escape_str_passes_printable() {
        assert_eq!(escape_str("hello.txt", false), b"hello.txt".to_vec());
    }

    #[test]
    fn escape_str_escapes_control() {
        assert_eq!(escape_str("a\x01b", false), b"a\\#001b".to_vec());
    }

    // -- Fast path coverage --

    #[test]
    fn fast_path_all_printable() {
        let input = b"abcdefghijklmnopqrstuvwxyz/0123456789";
        assert_eq!(
            escape_for_output(input, false),
            b"abcdefghijklmnopqrstuvwxyz/0123456789".to_vec()
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(escape_for_output(b"", false), Vec::<u8>::new());
        assert_eq!(escape_for_output(b"", true), Vec::<u8>::new());
    }
}
