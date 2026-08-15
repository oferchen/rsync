//! Filename octal escaping matching upstream rsync's `filtered_fwrite`
//! in `log.c:242-263`.
//!
//! Upstream rsync escapes non-printable bytes in filenames as `\#ooo`
//! (backslash, hash, three octal digits). Which bytes qualify depends on the
//! destination, so `filtered_fwrite` takes two independent switches - see
//! [`EscapeStyle`].

use std::io::Write;
use std::path::Path;

/// The two `filtered_fwrite` escape switches, resolved per destination.
///
/// upstream: log.c:242 `filtered_fwrite(FILE *f, const char *in_buf, int
/// in_len, int use_isprint, int escape_c1, char end_char)`. Upstream passes a
/// different pair for each sink, so oc-rsync carries the pair rather than the
/// `--8-bit-output` flag alone:
///   - the terminal sink gets `use_isprint = !allow_8bit_chars, escape_c1 = 0`
///     (log.c:425);
///   - the log-file sink gets `use_isprint = 0, escape_c1 = 1` (log.c:132),
///     which is deliberately *not* gated on `--8-bit-output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscapeStyle {
    /// Escape every byte libc `isprint()` rejects (upstream `use_isprint`).
    use_isprint: bool,
    /// Escape the C1 control range 0x80-0x9F (upstream `escape_c1`).
    escape_c1: bool,
    /// Escape the C0 controls below 0x20, tab excepted.
    ///
    /// Upstream has no switch for this: every `filtered_fwrite` call site
    /// escapes them (log.c:250 `*fp < ' '`). oc-rsync needs the switch because
    /// it has a sink upstream does not - [`EscapeStyle::passthrough`], for a
    /// producer whose own sink escapes.
    escape_controls: bool,
}

impl Default for EscapeStyle {
    /// The terminal style without `-8` - oc-rsync's default output sink.
    fn default() -> Self {
        Self::terminal(false)
    }
}

impl EscapeStyle {
    /// Escaping for stdout/stderr, honouring `--8-bit-output` / `-8`.
    ///
    /// upstream: log.c:425 `filtered_fwrite(f, buf, len, !allow_8bit_chars, 0,
    /// trailing_CR_or_NL)`. Without `-8` every non-`isprint` byte is escaped,
    /// which covers DEL (0x7F) and the whole high range 0x80-0xFF; with `-8`
    /// only the sub-0x20 controls (tab excepted) are.
    #[must_use]
    pub const fn terminal(allow_8bit: bool) -> Self {
        Self {
            use_isprint: !allow_8bit,
            escape_c1: false,
            escape_controls: true,
        }
    }

    /// Escaping for the log file (CWE-117 log injection defence).
    ///
    /// upstream: log.c:132 `filtered_fwrite(logfile_fp, buf, len, 0, 1,
    /// trailing)`. The `isprint()` filter is off, so DEL and 0xA0-0xFF reach
    /// the log verbatim and a non-UTF-8 name stays readable; in exchange the
    /// C1 range is escaped unconditionally, so neither a 7-bit `ESC` nor an
    /// 8-bit `CSI` can forge a log line or drive the operator's terminal. The
    /// pair does not depend on `--8-bit-output`.
    #[must_use]
    pub const fn log_file() -> Self {
        Self {
            use_isprint: false,
            escape_c1: true,
            escape_controls: true,
        }
    }

    /// Leaves every byte alone, for a producer whose sink already escapes.
    ///
    /// upstream has no `filtered_fwrite` call in `log_formatted()` at all: the
    /// renderer builds the line and escaping happens later, once, in the
    /// writer - `log.c:132 logit()` for the log file, `log.c:425 rwrite()` for
    /// the terminal. [`LogFileWriter`](crate::logfile::LogFileWriter) now owns
    /// the log-file half, so a renderer feeding it must select this style;
    /// applying [`EscapeStyle::log_file`] there too would escape the backslash
    /// of an already-emitted `\#033` and corrupt the line.
    #[must_use]
    pub const fn passthrough() -> Self {
        Self {
            use_isprint: false,
            escape_c1: false,
            escape_controls: false,
        }
    }
}

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
/// upstream: log.c:253-255 `filtered_fwrite` escape condition:
///   `*in_buf != '\t' && ((use_isprint && !isPrint(in_buf)) || *(uchar*)in_buf < ' '
///     || (escape_c1 && *(uchar*)in_buf >= 0x80 && *(uchar*)in_buf <= 0x9f))`
#[inline]
fn should_escape_byte(byte: u8, style: EscapeStyle) -> bool {
    byte != b'\t'
        && ((style.use_isprint && !is_c_print(byte))
            || (style.escape_controls && byte < b' ')
            || (style.escape_c1 && (0x80..=0x9F).contains(&byte)))
}

/// Escapes non-printable bytes for display output, matching upstream
/// rsync's `filtered_fwrite` in `log.c:242-263`.
///
/// Which bytes qualify is decided by `style` - see [`EscapeStyle`]. Tab
/// (0x09) is always passed through unescaped.
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
pub fn escape_for_output(input: &[u8], style: EscapeStyle) -> Vec<u8> {
    // A producer feeding a sink that escapes must not pre-escape anything: the
    // `\#ddd` guard below would otherwise fire on the sink's own output the
    // moment it re-entered this function, turning `\#033` into `\#134#033`.
    if style == EscapeStyle::passthrough() {
        return input.to_vec();
    }

    // Fast path: all bytes ASCII-printable or tab (0x09, 0x20-0x7E) -> no byte
    // needs escaping under any style and there is no literal `\#ddd` to guard,
    // so return the bytes verbatim.
    //
    // DEL (0x7F) and high bytes (0x80-0xFF) are deliberately excluded here so
    // they always take the slow path, which applies the style's own rule.
    let all_safe = input.iter().all(|&b| is_c_print(b) || b == b'\t');
    if all_safe && !has_literal_escape_sequence(input) {
        return input.to_vec();
    }

    escape_bytes_slow(input, style)
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
/// input byte verbatim). This matters whenever a style lets high bytes through
/// (`-8` on the terminal, or any log-file line): a multi-byte UTF-8 filename
/// such as `café` (bytes `63 61 66 c3 a9`) passes through as the original bytes
/// `c3 a9`, and a lone invalid byte such as `0x80` passes through as the single
/// byte `80` - neither is re-encoded nor replaced with U+FFFD. Escaped bytes
/// are written as their ASCII `\#ooo` form.
fn escape_bytes_slow(input: &[u8], style: EscapeStyle) -> Vec<u8> {
    let mut output: Vec<u8> = Vec::with_capacity(input.len() + input.len() / 4);
    let len = input.len();
    let mut i = 0;

    while i < len {
        let byte = input[i];

        // upstream: log.c:252-254 - escape literal \#ddd sequences to prevent
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

        if should_escape_byte(byte, style) {
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
/// non-UTF-8 filenames: a lone invalid byte such as `0x80` reaches the escape
/// layer verbatim, matching upstream `filtered_fwrite` (log.c:242-263), which
/// copies filename bytes to the output fd unmodified.
///
/// On non-Unix hosts (chiefly Windows) a path is an `OsStr` whose internal
/// WTF-8 bytes are not exposed by stable std. For every well-formed Unicode
/// path, `to_str()` yields the exact UTF-8 bytes with no substitution, so
/// escaping them stays byte-faithful and mirrors upstream. Only a path that is
/// ill-formed UTF-16 (a lone surrogate, which cannot round-trip through `&str`)
/// falls back to `to_string_lossy()`, replacing the surrogate with U+FFFD. That
/// single lossy case is a documented platform limitation, not an accidental
/// transcode of otherwise-representable filenames: stable std offers no API to
/// recover the raw WTF-8 bytes, so full byte-fidelity for lone surrogates is
/// unreachable on Windows today.
pub fn escape_path(path: &Path, style: EscapeStyle) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        escape_for_output(path.as_os_str().as_bytes(), style)
    }
    #[cfg(not(unix))]
    {
        match path.as_os_str().to_str() {
            Some(valid) => escape_for_output(valid.as_bytes(), style),
            None => escape_for_output(path.to_string_lossy().as_bytes(), style),
        }
    }
}

/// Escapes a string for display output, returning raw bytes.
///
/// Convenience wrapper for already-converted strings (e.g. from
/// `to_string_lossy()`). Escapes non-printable bytes in the UTF-8
/// representation.
#[cfg(test)]
fn escape_str(s: &str, style: EscapeStyle) -> Vec<u8> {
    escape_for_output(s.as_bytes(), style)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// stdout/stderr without `-8` (upstream log.c:425 `!allow_8bit_chars`).
    const TERMINAL: EscapeStyle = EscapeStyle::terminal(false);
    /// stdout/stderr with `-8` (upstream log.c:425 `allow_8bit_chars`).
    const TERMINAL_8BIT: EscapeStyle = EscapeStyle::terminal(true);
    /// The log-file sink (upstream log.c:132 `logit`).
    const LOGFILE: EscapeStyle = EscapeStyle::log_file();

    // -- escape_for_output: terminal style, no -8 (use_isprint=1, escape_c1=0) --

    #[test]
    fn ascii_printable_passes_through() {
        let input = b"hello_world.txt";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"hello_world.txt".to_vec()
        );
    }

    #[test]
    fn space_passes_through() {
        let input = b"hello world.txt";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"hello world.txt".to_vec()
        );
    }

    #[test]
    fn tab_passes_through() {
        let input = b"before\tafter";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"before\tafter".to_vec()
        );
    }

    #[test]
    fn control_char_0x01_escaped() {
        assert_eq!(escape_for_output(&[0x01], TERMINAL), b"\\#001".to_vec());
    }

    #[test]
    fn del_0x7f_escaped() {
        assert_eq!(escape_for_output(&[0x7F], TERMINAL), b"\\#177".to_vec());
    }

    #[test]
    fn high_bit_0x80_escaped_in_default_mode() {
        // upstream: default mode (use_isprint=1) octal-escapes a lone high byte.
        assert_eq!(escape_for_output(&[0x80], TERMINAL), b"\\#200".to_vec());
    }

    #[test]
    fn high_bit_0xff_escaped_in_default_mode() {
        assert_eq!(escape_for_output(&[0xFF], TERMINAL), b"\\#377".to_vec());
    }

    #[test]
    fn null_byte_escaped() {
        assert_eq!(escape_for_output(&[0x00], TERMINAL), b"\\#000".to_vec());
    }

    #[test]
    fn mixed_printable_and_control() {
        let input = b"file\x01name\x7f.txt";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"file\\#001name\\#177.txt".to_vec()
        );
    }

    #[test]
    fn all_specified_bytes_escaped_correctly() {
        // Verify the exact values from the task description.
        assert_eq!(escape_for_output(&[0x01], TERMINAL), b"\\#001".to_vec());
        assert_eq!(escape_for_output(&[0x7F], TERMINAL), b"\\#177".to_vec());
        assert_eq!(escape_for_output(&[0x80], TERMINAL), b"\\#200".to_vec());
        assert_eq!(escape_for_output(&[0xFF], TERMINAL), b"\\#377".to_vec());
    }

    // -- escape_for_output: terminal style under -8 (use_isprint=0, escape_c1=0) --

    #[test]
    fn control_char_escaped_in_8bit_mode() {
        assert_eq!(
            escape_for_output(&[0x01], TERMINAL_8BIT),
            b"\\#001".to_vec()
        );
    }

    #[test]
    fn del_0x7f_passes_through_in_8bit_mode() {
        // upstream: with use_isprint=0 (allow_8bit=1), the condition is
        // byte != '\t' && byte < ' ', which excludes 0x7F.
        assert_eq!(escape_for_output(&[0x7F], TERMINAL_8BIT), vec![0x7F]);
    }

    #[test]
    fn lone_high_byte_0x80_is_raw_in_8bit_mode() {
        // upstream: `filtered_fwrite` writes the raw byte to the output fd when
        // allow_8bit_chars is set (log.c:242-263). A lone 0x80 is invalid UTF-8
        // and cannot live in a Rust String, so the escape layer returns raw
        // bytes and the writer emits exactly one byte, matching `rsync -8`
        // byte-for-byte. Previously the `from_utf8_lossy` return type yielded
        // U+FFFD here.
        assert_eq!(escape_for_output(&[0x80], TERMINAL_8BIT), vec![0x80]);
        // Exactly one byte reaches the sink - no U+FFFD (ef bf bd) expansion.
        assert_eq!(escape_for_output(&[0x80], TERMINAL_8BIT).len(), 1);
    }

    #[test]
    fn lone_high_byte_0xff_is_raw_in_8bit_mode() {
        assert_eq!(escape_for_output(&[0xFF], TERMINAL_8BIT), vec![0xFF]);
        assert_eq!(escape_for_output(&[0xFF], TERMINAL_8BIT).len(), 1);
    }

    #[test]
    fn tab_passes_through_in_8bit_mode() {
        assert_eq!(escape_for_output(b"\t", TERMINAL_8BIT), b"\t".to_vec());
    }

    #[test]
    fn multibyte_utf8_passes_raw_in_8bit_mode() {
        // WHY: upstream `filtered_fwrite` copies non-escaped bytes verbatim, so
        // `-8` on `café` (bytes 63 61 66 c3 a9) must yield the original bytes
        // c3 a9, matching `rsync -v -8` byte-for-byte. A byte-to-code-point cast
        // would re-encode to c3 83 c2 a9 (mojibake `Ã©`).
        let input = b"caf\xc3\xa9";
        let out = escape_for_output(input, TERMINAL_8BIT);
        assert_eq!(out, b"caf\xc3\xa9".to_vec());
    }

    #[test]
    fn multibyte_utf8_escaped_octal_in_default_mode() {
        // WHY: default mode (use_isprint=1) escapes every non-printable byte,
        // so each UTF-8 continuation byte of `café` becomes its own \#ooo.
        let input = b"caf\xc3\xa9";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"caf\\#303\\#251".to_vec()
        );
    }

    #[test]
    fn only_control_chars_escaped_in_8bit_mode() {
        // 0x01 and 0x7F: only 0x01 is escaped (< 0x20)
        let input = &[0x01, 0x7F];
        assert_eq!(
            escape_for_output(input, TERMINAL_8BIT),
            b"\\#001\x7F".to_vec()
        );
    }

    // -- Literal \#ddd sequence escaping --

    #[test]
    fn literal_escape_sequence_is_escaped() {
        // A filename containing literal \#001 should escape the backslash.
        let input = b"file\\#001.txt";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"file\\#134#001.txt".to_vec()
        );
    }

    #[test]
    fn literal_escape_sequence_escaped_in_8bit_mode() {
        let input = b"file\\#999.txt";
        assert_eq!(
            escape_for_output(input, TERMINAL_8BIT),
            b"file\\#134#999.txt".to_vec()
        );
    }

    #[test]
    fn non_digit_after_hash_not_escaped() {
        // \#abc is not a valid escape sequence - leave it alone.
        let input = b"file\\#abc.txt";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"file\\#abc.txt".to_vec()
        );
    }

    #[test]
    fn partial_escape_sequence_at_end_not_escaped() {
        // \#12 at end (only 2 digits) - not an escape sequence.
        let input = b"file\\#12";
        assert_eq!(escape_for_output(input, TERMINAL), b"file\\#12".to_vec());
    }

    // -- escape_path --

    #[test]
    fn escape_path_ascii() {
        let path = Path::new("src/main.rs");
        assert_eq!(escape_path(path, TERMINAL), b"src/main.rs".to_vec());
    }

    #[test]
    fn escape_path_with_directory_separator() {
        let path = Path::new("foo/bar/baz.txt");
        assert_eq!(escape_path(path, TERMINAL), b"foo/bar/baz.txt".to_vec());
    }

    #[cfg(unix)]
    #[test]
    fn escape_path_non_utf8_bytes_survive_under_8bit() {
        // WHY: a lone 0x80 is invalid UTF-8. `escape_path` must not route the
        // path through a `String` (which would replace it with U+FFFD): under
        // `-8` the raw byte passes verbatim to the byte sink, matching upstream
        // `filtered_fwrite`, which copies filename bytes unmodified
        // (log.c:242-263). This is the round-trip that `--8-bit-output` parity
        // depends on.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(OsStr::from_bytes(b"a\x80b"));
        let out = escape_path(path, TERMINAL_8BIT);
        assert_eq!(out, b"a\x80b".to_vec());
        // Exactly three bytes reach the sink - no U+FFFD (ef bf bd) expansion.
        assert_eq!(out.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn escape_path_non_utf8_byte_octal_escaped_in_default_mode() {
        // WHY: default mode (use_isprint=1) octal-escapes the raw high byte
        // rather than lossily transcoding it, so the escape is reversible.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(OsStr::from_bytes(b"a\x80b"));
        assert_eq!(escape_path(path, TERMINAL), b"a\\#200b".to_vec());
    }

    #[cfg(not(unix))]
    #[test]
    fn escape_path_valid_unicode_is_byte_faithful() {
        // WHY: `to_str()` yields exact UTF-8 for a well-formed path, so `-8`
        // keeps the original bytes (no U+FFFD), matching upstream
        // `filtered_fwrite` byte-for-byte. `café` is bytes 63 61 66 c3 a9.
        let path = Path::new("caf\u{e9}");
        assert_eq!(escape_path(path, TERMINAL_8BIT), b"caf\xc3\xa9".to_vec());
        assert_eq!(escape_path(path, TERMINAL), b"caf\\#303\\#251".to_vec());
    }

    #[cfg(windows)]
    #[test]
    fn escape_path_lone_surrogate_pins_documented_limitation() {
        // Pinned behavior: a lone UTF-16 surrogate is ill-formed and cannot
        // round-trip through `&str`, so `to_str()` returns None and the path
        // falls back to `to_string_lossy()`, replacing the surrogate with
        // U+FFFD (ef bf bd). Stable std exposes no API to recover the raw WTF-8
        // bytes, so full byte-fidelity is unreachable here; this asserts the
        // fallback is deliberate, not an accidental transcode of a
        // representable filename.
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        // 'a', lone high surrogate 0xD800, 'b'.
        let os = OsString::from_wide(&[0x0061, 0xD800, 0x0062]);
        let path = Path::new(&os);
        // Under -8 the U+FFFD bytes (all high) pass through raw.
        assert_eq!(escape_path(path, TERMINAL_8BIT), b"a\xef\xbf\xbdb".to_vec());
    }

    // -- escape_str --

    #[test]
    fn escape_str_passes_printable() {
        assert_eq!(escape_str("hello.txt", TERMINAL), b"hello.txt".to_vec());
    }

    #[test]
    fn escape_str_escapes_control() {
        assert_eq!(escape_str("a\x01b", TERMINAL), b"a\\#001b".to_vec());
    }

    // -- Fast path coverage --

    #[test]
    fn fast_path_all_printable() {
        let input = b"abcdefghijklmnopqrstuvwxyz/0123456789";
        assert_eq!(
            escape_for_output(input, TERMINAL),
            b"abcdefghijklmnopqrstuvwxyz/0123456789".to_vec()
        );
    }

    #[test]
    fn empty_input() {
        assert_eq!(escape_for_output(b"", TERMINAL), Vec::<u8>::new());
        assert_eq!(escape_for_output(b"", TERMINAL_8BIT), Vec::<u8>::new());
    }

    // -- log-file style (upstream log.c:132: use_isprint=0, escape_c1=1) --

    /// WHY: a filename carrying `\n` or `ESC` must not be able to forge a log
    /// line or drive the operator's terminal when they later `cat` the log
    /// (CWE-117). upstream: log.c:126-131 escapes the message before it is
    /// written to `logfile_fp`.
    #[test]
    fn log_file_escapes_sub_0x20_controls() {
        assert_eq!(escape_for_output(b"a\nb", LOGFILE), b"a\\#012b".to_vec());
        assert_eq!(escape_for_output(b"a\x1bb", LOGFILE), b"a\\#033b".to_vec());
        assert_eq!(escape_for_output(b"a\rb", LOGFILE), b"a\\#015b".to_vec());
        assert_eq!(escape_for_output(&[0x00], LOGFILE), b"\\#000".to_vec());
    }

    /// WHY: 0x9B is the 8-bit CSI - on a terminal that decodes C1 it starts a
    /// control sequence exactly as `ESC [` does, so the log sink escapes the
    /// whole C1 range whether or not `--8-bit-output` is in play.
    /// upstream: log.c:255 `escape_c1 && *(uchar*)in_buf >= 0x80 && <= 0x9f`.
    #[test]
    fn log_file_escapes_the_c1_range() {
        assert_eq!(escape_for_output(&[0x80], LOGFILE), b"\\#200".to_vec());
        assert_eq!(escape_for_output(&[0x9B], LOGFILE), b"\\#233".to_vec());
        assert_eq!(escape_for_output(&[0x9F], LOGFILE), b"\\#237".to_vec());
    }

    /// WHY: the log sink runs with `use_isprint = 0`, so DEL and everything
    /// above the C1 range reaches the log verbatim - a non-UTF-8 filename stays
    /// readable there. Escaping them would be as much a divergence as failing
    /// to escape a control byte. MEASURED against rsync 3.5.0 with
    /// `--log-file`: `\x7f` and `\xff` appear raw in the log while `\x1b`,
    /// `\n` and `\x9b` appear as `\#033`, `\#012` and `\#233`.
    #[test]
    fn log_file_passes_del_and_high_bytes_raw() {
        assert_eq!(escape_for_output(&[0x7F], LOGFILE), vec![0x7F]);
        assert_eq!(escape_for_output(&[0xA0], LOGFILE), vec![0xA0]);
        assert_eq!(escape_for_output(&[0xFF], LOGFILE), vec![0xFF]);
        assert_eq!(
            escape_for_output(b"caf\xc3\xa9", LOGFILE),
            b"caf\xc3\xa9".to_vec()
        );
    }

    /// upstream: log.c:253-254 - tab survives in every style.
    #[test]
    fn log_file_passes_tab_raw() {
        assert_eq!(escape_for_output(b"a\tb", LOGFILE), b"a\tb".to_vec());
    }

    /// upstream: log.c:252-254 - a literal `\#ddd` in a name is disambiguated
    /// from a real escape in every style, including the log sink.
    #[test]
    fn log_file_disambiguates_literal_escape_sequence() {
        assert_eq!(
            escape_for_output(b"file\\#001.txt", LOGFILE),
            b"file\\#134#001.txt".to_vec()
        );
    }

    /// CLASS GUARD: for every byte value, the log style must escape exactly the
    /// set upstream's `filtered_fwrite(.., use_isprint=0, escape_c1=1, ..)`
    /// escapes - no more, no less - and must never be gated on `-8`.
    #[test]
    fn log_file_style_matches_upstream_predicate_for_every_byte() {
        for byte in 0u8..=255 {
            let expected_escape = byte != b'\t' && (byte < 0x20 || (0x80..=0x9F).contains(&byte));
            let out = escape_for_output(&[byte], LOGFILE);
            let escaped = out != vec![byte];
            assert_eq!(
                escaped, expected_escape,
                "byte {byte:#04x} escaped={escaped} expected={expected_escape}"
            );
            if expected_escape {
                assert_eq!(out, format!("\\#{byte:03o}").into_bytes());
            }
        }
    }

    /// The log style is a fixed pair, so it cannot be reached from the `-8`
    /// flag: constructing it twice must yield the same rule either way.
    #[test]
    fn log_file_style_is_independent_of_eight_bit_output() {
        assert_ne!(EscapeStyle::log_file(), EscapeStyle::terminal(false));
        assert_ne!(EscapeStyle::log_file(), EscapeStyle::terminal(true));
    }
}
