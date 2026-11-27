//! Filename octal escaping matching upstream rsync's `filtered_fwrite`
//! in `log.c:295-344`.
//!
//! Upstream rsync escapes non-printable bytes in filenames as `\#ooo`
//! (backslash, hash, three octal digits). Which bytes qualify depends on the
//! destination and on the process locale - see
//! [`EscapeStyle`](crate::escape::EscapeStyle).
//!
//! The path is spelled out because `lib.rs` puts a `///` comment on
//! `pub mod escape;`: rustdoc concatenates that with this `//!` block and
//! resolves the result in the CRATE ROOT's scope, where a bare `EscapeStyle`
//! does not exist.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Process-wide `--8-bit-output` / `-8` state.
///
/// upstream: `allow_8bit_chars` is a single file-scope global (options.c:105),
/// consulted only where a buffer is written to the terminal - `rwrite()` ->
/// `filtered_fwrite(f, buf, len, !allow_8bit_chars, ..)` (log.c:499). A
/// process global (not a thread-local) mirrors that exactly: the flag is one
/// CLI decision that every output boundary on every transfer thread must read.
/// Defaults to `false`, upstream's default (every non-`isprint` byte escaped).
static ALLOW_8BIT_CHARS: AtomicBool = AtomicBool::new(false);

/// Records the resolved `--8-bit-output` / `-8` setting for the whole process.
///
/// The CLI calls this once it has parsed the flag, before any transfer thread
/// writes peer-controlled bytes to the terminal.
pub fn set_eight_bit_output(value: bool) {
    ALLOW_8BIT_CHARS.store(value, Ordering::Relaxed);
}

/// Reads the process-wide `--8-bit-output` / `-8` setting.
#[must_use]
pub fn eight_bit_output() -> bool {
    ALLOW_8BIT_CHARS.load(Ordering::Relaxed)
}

/// How the process `LC_CTYPE` codeset classifies name bytes.
///
/// upstream: main.c:1911 `setlocale(LC_CTYPE, "")` adopts the environment's
/// locale, and every multibyte decision afterwards - `mbrtowc()` in
/// log.c:251-274 `locale_char_len()`, and the `iconv` validation of
/// rsync.c:96-98 `ic_chck` - is made in that locale's codeset. oc-rsync decodes
/// only the codesets it can decode without `iconv`; any other codeset keeps the
/// byte-at-a-time escaping, which escapes every byte at or above 0x80 on the
/// terminal (see [`set_locale_codeset`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocaleCharset {
    /// 7-bit ASCII - the `C`/`POSIX` locale. No byte at or above 0x80 starts a
    /// character, so `mbrtowc()` rejects each one and `iconv` reports `EILSEQ`.
    Ascii,
    /// UTF-8.
    Utf8,
    /// Any other codeset (ISO-8859-x, GBK, EUC-JP, ...). Not decoded.
    Other,
}

/// The process codeset class; `Ascii` until [`set_locale_codeset`] runs, which
/// is the `C` locale a process starts in before `setlocale()`.
static LOCALE_CHARSET: AtomicU8 = AtomicU8::new(LocaleCharset::Ascii as u8);

/// Records the codeset `nl_langinfo(CODESET)` reported after the process
/// adopted its `LC_CTYPE` locale.
///
/// `None` (no codeset could be read) keeps the `C` locale's ASCII codeset.
/// Codeset names are compared ignoring case, `-` and `_`, so glibc's `UTF-8`
/// and `ANSI_X3.4-1968` and the BSDs' `US-ASCII` are all recognised.
pub fn set_locale_codeset(codeset: Option<&str>) {
    LOCALE_CHARSET.store(classify_codeset(codeset) as u8, Ordering::Relaxed);
}

/// Reads the process codeset class recorded by [`set_locale_codeset`].
#[must_use]
pub fn locale_charset() -> LocaleCharset {
    match LOCALE_CHARSET.load(Ordering::Relaxed) {
        x if x == LocaleCharset::Utf8 as u8 => LocaleCharset::Utf8,
        x if x == LocaleCharset::Other as u8 => LocaleCharset::Other,
        _ => LocaleCharset::Ascii,
    }
}

fn classify_codeset(codeset: Option<&str>) -> LocaleCharset {
    let Some(codeset) = codeset else {
        return LocaleCharset::Ascii;
    };
    let folded: String = codeset
        .chars()
        .filter(|c| !matches!(c, '-' | '_'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match folded.as_str() {
        "utf8" => LocaleCharset::Utf8,
        "ansix3.41968" | "usascii" | "ascii" | "646" | "" => LocaleCharset::Ascii,
        _ => LocaleCharset::Other,
    }
}

/// Process-wide "check messages against the locale charset" state - upstream's
/// `ic_chck`.
///
/// upstream: rsync.c:96-98 opens `ic_chck = iconv_open(defset, defset)` only
/// for a process that is not a server and runs without `-8`. While it is open,
/// log.c:463-496 `rwrite()` validates every terminal line in the locale
/// codeset: a byte that does not start a valid character is written as
/// `\#ooo`, and the valid characters go through `filtered_fwrite(.., 0, ..)`
/// (log.c:476), which keeps printable multibyte characters raw. A server or a
/// `-8` client never opens it and takes `filtered_fwrite(.., !allow_8bit_chars,
/// ..)` (log.c:499) instead.
static MESSAGE_CHARSET_CHECK: AtomicBool = AtomicBool::new(false);

/// Records whether this process checks terminal output against the locale
/// charset (upstream `ic_chck`): true for a client without `-8`.
pub fn set_message_charset_check(value: bool) {
    MESSAGE_CHARSET_CHECK.store(value, Ordering::Relaxed);
}

/// Whether a `use_isprint` terminal line takes upstream's `ic_chck` branch.
///
/// The branch is exact only for the codesets oc-rsync can validate without
/// `iconv` - ASCII and UTF-8. Under any other codeset the line keeps the
/// `isprint()` branch, which escapes every byte at or above 0x80.
fn message_charset_checked() -> bool {
    MESSAGE_CHARSET_CHECK.load(Ordering::Relaxed)
        && matches!(locale_charset(), LocaleCharset::Ascii | LocaleCharset::Utf8)
}

/// The terminal escape style honouring the process-wide `-8` setting.
///
/// upstream: log.c:499 `rwrite()` selects `filtered_fwrite(.., !allow_8bit_chars,
/// 0, ..)`, so a terminal-bound line escapes every non-`isprint` byte unless
/// `-8` is in effect. This is the style every terminal sink should use.
#[must_use]
pub fn terminal_style() -> EscapeStyle {
    EscapeStyle::terminal(eight_bit_output())
}

/// The `filtered_fwrite` escape switch, resolved per destination.
///
/// upstream: log.c:295 `filtered_fwrite(FILE *f, const char *in_buf, int
/// in_len, int use_isprint, char end_char)`. Every call site escapes C0 (tab
/// excepted), DEL and 0x80-0x9F (log.c:288-291); `use_isprint` additionally
/// escapes every byte `isprint()` rejects. The sinks differ only in that flag:
///   - the terminal sink of a server or a `-8` client passes
///     `!allow_8bit_chars` (log.c:499);
///   - a client without `-8` validates the line against the locale charset
///     first and passes 0 (log.c:463-496, see [`set_message_charset_check`]);
///   - the log-file sink passes 0 (log.c:143), which is deliberately *not*
///     gated on `--8-bit-output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscapeStyle {
    /// Escape every byte libc `isprint()` rejects (upstream `use_isprint`).
    use_isprint: bool,
    /// Escape C0 (tab excepted), DEL and 0x80-0x9F.
    ///
    /// Upstream has no switch for this: every `filtered_fwrite` call escapes
    /// them (log.c:288-291). oc-rsync needs the switch because it has a sink
    /// upstream does not - [`EscapeStyle::passthrough`], for a producer whose
    /// own sink escapes.
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
    /// upstream: log.c:499 `filtered_fwrite(f, buf, len, !allow_8bit_chars,
    /// trailing_NL)`. Without `-8` every non-`isprint` byte is escaped, which
    /// covers DEL (0x7F) and the whole high range 0x80-0xFF; with `-8` a valid
    /// printable locale character and a high byte outside 0x80-0x9F pass. A
    /// client without `-8` takes the locale-checked branch instead (log.c:476),
    /// selected in the escaper by [`set_message_charset_check`].
    #[must_use]
    pub const fn terminal(allow_8bit: bool) -> Self {
        Self {
            use_isprint: !allow_8bit,
            escape_controls: true,
        }
    }

    /// Escaping for the log file (CWE-117 log injection defence).
    ///
    /// upstream: log.c:143 `filtered_fwrite(logfile_fp, buf, len, 0,
    /// trailing)`. The `isprint()` filter is off, so 0xA0-0xFF reach the log
    /// verbatim and a non-UTF-8 name stays readable, while C0, DEL and the C1
    /// range are still escaped, so neither a 7-bit `ESC` nor an 8-bit `CSI` can
    /// forge a log line or drive the operator's terminal. The style does not
    /// depend on `--8-bit-output`.
    #[must_use]
    pub const fn log_file() -> Self {
        Self {
            use_isprint: false,
            escape_controls: true,
        }
    }

    /// Leaves every byte alone, for a producer whose sink already escapes.
    ///
    /// upstream has no `filtered_fwrite` call in `log_formatted()` at all: the
    /// renderer builds the line and escaping happens later, once, in the
    /// writer - `log.c:143 logit()` for the log file, `log.c:499 rwrite()` for
    /// the terminal. `LogFileWriter` (in the `logging-sink` crate) now owns
    /// the log-file half, so a renderer feeding it must select this style;
    /// applying [`EscapeStyle::log_file`] there too would escape the backslash
    /// of an already-emitted `\#033` and corrupt the line.
    #[must_use]
    pub const fn passthrough() -> Self {
        Self {
            use_isprint: false,
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
/// upstream: log.c:289-291 `filtered_char_len()` escape condition:
///   `*buf != '\t' && ((use_isprint && !isPrint(buf)) || byte < ' '
///     || byte == 0x7f || (byte >= 0x80 && byte <= 0x9f))`
#[inline]
fn should_escape_byte(byte: u8, style: EscapeStyle) -> bool {
    byte != b'\t'
        && ((style.use_isprint && !is_c_print(byte))
            || (style.escape_controls
                && (byte < b' ' || byte == 0x7F || (0x80..=0x9F).contains(&byte))))
}

/// Decodes one multibyte character in the process `LC_CTYPE` codeset,
/// returning `(byte_len, is_control)`. Returns `None` when the codeset is not
/// UTF-8 or on an invalid/incomplete sequence, so callers fall back to
/// byte-by-byte filtering.
///
/// upstream: log.c:251-274 `locale_char_len()` - `mbrtowc()` in the locale the
/// process adopted at main.c:1911. In the `C` locale it rejects every byte at or
/// above 0x80.
fn locale_char_len(buf: &[u8]) -> Option<(usize, bool)> {
    if locale_charset() != LocaleCharset::Utf8 {
        return None;
    }
    let slice = &buf[..buf.len().min(4)];
    let valid = match std::str::from_utf8(slice) {
        Ok(s) => s,
        Err(err) if err.valid_up_to() > 0 => {
            std::str::from_utf8(&slice[..err.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    let ch = valid.chars().next()?;
    let is_control = ch != '\t' && ch.is_control();
    Some((ch.len_utf8(), is_control))
}

/// Escapes non-printable bytes for display output, matching upstream
/// rsync's `filtered_fwrite` in `log.c:295-344`.
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
    if !may_need_escaping(input, style) {
        return input.to_vec();
    }

    escape_bytes_slow(input, style)
}

/// Whether [`escape_for_output`] could change `input` under `style`.
///
/// `false` guarantees the escaped output equals `input`, so a sink can write
/// the bytes it already holds without building an escaped copy. `true` is
/// conservative: a byte at or above 0x80 always answers `true` because only
/// the slow path applies the style's and the locale's rule to it.
#[must_use]
pub fn may_need_escaping(input: &[u8], style: EscapeStyle) -> bool {
    // A producer feeding a sink that escapes must not pre-escape anything: the
    // `\#ddd` guard would otherwise fire on the sink's own output the moment
    // it re-entered the escaper, turning `\#033` into `\#134#033`.
    if style == EscapeStyle::passthrough() {
        return false;
    }

    // All bytes ASCII-printable or tab (0x09, 0x20-0x7E) -> no byte needs
    // escaping under any style and there is no literal `\#ddd` to guard.
    // DEL (0x7F) and high bytes (0x80-0xFF) are deliberately excluded so they
    // always take the slow path, which applies the style's own rule.
    let all_safe = input.iter().all(|&b| is_c_print(b) || b == b'\t');
    !all_safe || has_literal_escape_sequence(input)
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
    // upstream: log.c:463-496 - with `ic_chck` open, `iconvbufs()` passes the
    // valid characters to `filtered_fwrite(f, convbuf, len, 0, ..)` (log.c:476)
    // and writes each byte it rejects as `\#ooo` (log.c:484-486).
    let charset_checked = style.use_isprint && style.escape_controls && message_charset_checked();
    let style = if charset_checked {
        EscapeStyle {
            use_isprint: false,
            escape_controls: true,
        }
    } else {
        style
    };

    while i < len {
        let byte = input[i];

        if !style.use_isprint
            && style.escape_controls
            && byte >= 0x80
            && let Some((char_len, is_control)) = locale_char_len(&input[i..])
        {
            if is_control {
                for &b in &input[i..i + char_len] {
                    let _ = write!(output, "\\#{b:03o}");
                }
            } else {
                output.extend_from_slice(&input[i..i + char_len]);
            }
            i += char_len;
            continue;
        }

        if charset_checked && byte >= 0x80 {
            let _ = write!(output, "\\#{byte:03o}");
            i += 1;
            continue;
        }

        // upstream: log.c:318-319 - escape literal \#ddd sequences to prevent
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
/// layer verbatim, matching upstream `filtered_fwrite` (log.c:249-344), which
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
/// Writes `line` and a newline to `out` as one terminal line, escaped with the
/// process [`terminal_style`].
///
/// upstream: log.c:463-499 `rwrite()` escapes every diagnostic it prints, and
/// log.c:460 writes the trailing newline raw. A line with nothing to escape is
/// written without building a copy.
///
/// # Errors
///
/// Any error writing to `out`.
pub fn write_terminal_line<W: Write>(out: &mut W, line: &str) -> std::io::Result<()> {
    let style = terminal_style();
    if !may_need_escaping(line.as_bytes(), style) {
        return writeln!(out, "{line}");
    }
    let mut escaped = escape_for_output(line.as_bytes(), style);
    escaped.push(b'\n');
    out.write_all(&escaped)
}

/// Prints `line` on stderr as one escaped terminal line; see
/// [`write_terminal_line`]. A failed write to stderr is ignored, as
/// `eprintln!` would panic and upstream's `rwrite()` has no one to report to.
pub fn eprint_terminal_line(line: &str) {
    let _ = write_terminal_line(&mut std::io::stderr().lock(), line);
}

#[cfg(test)]
fn escape_str(s: &str, style: EscapeStyle) -> Vec<u8> {
    escape_for_output(s.as_bytes(), style)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// stdout/stderr without `-8` (upstream log.c:499 `!allow_8bit_chars`).
    const TERMINAL: EscapeStyle = EscapeStyle::terminal(false);
    /// stdout/stderr with `-8` (upstream log.c:499 `allow_8bit_chars`).
    const TERMINAL_8BIT: EscapeStyle = EscapeStyle::terminal(true);
    /// The log-file sink (upstream log.c:143 `logit`).
    const LOGFILE: EscapeStyle = EscapeStyle::log_file();

    // -- escape_for_output: terminal style, no -8 (use_isprint=1) --

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

    // -- escape_for_output: terminal style under -8 (use_isprint=0) --

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
        assert_eq!(
            escape_for_output(&[0x7F], TERMINAL_8BIT),
            b"\\#177".to_vec()
        );
    }

    #[test]
    fn lone_high_byte_0x80_is_raw_in_8bit_mode() {
        // upstream: `filtered_fwrite` writes the raw byte to the output fd when
        // allow_8bit_chars is set (log.c:242-264). A lone 0x80 is invalid UTF-8
        // and cannot live in a Rust String, so the escape layer returns raw
        // bytes and the writer emits exactly one byte, matching `rsync -8`
        // byte-for-byte. Previously the `from_utf8_lossy` return type yielded
        // U+FFFD here.
        // In upstream 3.5.2 (log.c:290-291), C1 bytes (0x80..=0x9F) are escaped even with -8.
        assert_eq!(
            escape_for_output(&[0x80], TERMINAL_8BIT),
            b"\\#200".to_vec()
        );
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
            b"\\#001\\#177".to_vec()
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
        // (log.c:242-264). This is the round-trip that `--8-bit-output` parity
        // depends on.
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        // 0xA0 is non-UTF-8 and above the C1 range (0x80..=0x9F), so it passes verbatim under -8.
        let path = Path::new(OsStr::from_bytes(b"a\xa0b"));
        let out = escape_path(path, TERMINAL_8BIT);
        assert_eq!(out, b"a\xa0b".to_vec());
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

    // -- log-file style (upstream log.c:143: use_isprint=0) --

    /// WHY: a filename carrying `\n` or `ESC` must not be able to forge a log
    /// line or drive the operator's terminal when they later `cat` the log
    /// (CWE-117). upstream: log.c:137-143 escapes the message before it is
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
    /// upstream: log.c:291 `byte >= 0x80 && byte <= 0x9f`.
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
        assert_eq!(escape_for_output(&[0x7F], LOGFILE), b"\\#177".to_vec());
        assert_eq!(escape_for_output(&[0xA0], LOGFILE), vec![0xA0]);
        assert_eq!(escape_for_output(&[0xFF], LOGFILE), vec![0xFF]);
        assert_eq!(
            escape_for_output(b"caf\xc3\xa9", LOGFILE),
            b"caf\xc3\xa9".to_vec()
        );
    }

    /// upstream: log.c:320 - tab survives in every style.
    #[test]
    fn log_file_passes_tab_raw() {
        assert_eq!(escape_for_output(b"a\tb", LOGFILE), b"a\tb".to_vec());
    }

    /// upstream: log.c:318-319 - a literal `\#ddd` in a name is disambiguated
    /// from a real escape in every style, including the log sink.
    #[test]
    fn log_file_disambiguates_literal_escape_sequence() {
        assert_eq!(
            escape_for_output(b"file\\#001.txt", LOGFILE),
            b"file\\#134#001.txt".to_vec()
        );
    }

    /// CLASS GUARD: for every byte value, the log style must escape exactly the
    /// set upstream's `filtered_fwrite(.., use_isprint=0, ..)`
    /// escapes - no more, no less - and must never be gated on `-8`.
    #[test]
    fn log_file_style_matches_upstream_predicate_for_every_byte() {
        for byte in 0u8..=255 {
            let expected_escape =
                byte != b'\t' && (byte < 0x20 || byte == 0x7F || (0x80..=0x9F).contains(&byte));
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

    /// The log style is fixed: log.c:143 always passes `use_isprint = 0`, so
    /// flipping the process `-8` flag must not change what it escapes. (In
    /// 3.5.2 it equals the `-8` terminal rule; only the non-`-8` terminal
    /// differs.)
    #[test]
    fn log_file_style_is_independent_of_eight_bit_output() {
        let sample = b"a\x1b\x7f\x85\xc3\xa9\xff";
        set_eight_bit_output(false);
        let without = escape_for_output(sample, EscapeStyle::log_file());
        set_eight_bit_output(true);
        let with = escape_for_output(sample, EscapeStyle::log_file());
        assert_eq!(without, with);
        assert_ne!(EscapeStyle::log_file(), EscapeStyle::terminal(false));
    }
}

/// Terminal escaping under the process locale and `ic_chck` (rsync 3.5.2
/// rsync.c:96-98, log.c:251-293, log.c:463-499).
///
/// Each test sets the process-wide codeset and `ic_chck` state; nextest runs
/// every test in its own process, so the settings cannot leak into a peer.
#[cfg(test)]
mod locale_checked_tests {
    use super::*;

    /// A client without `-8` - upstream opens `ic_chck`.
    fn client_in(codeset: &str) {
        set_locale_codeset(Some(codeset));
        set_message_charset_check(true);
    }

    fn terminal(input: &[u8]) -> Vec<u8> {
        escape_for_output(input, EscapeStyle::terminal(false))
    }

    #[test]
    fn codeset_names_classify_like_nl_langinfo_reports_them() {
        assert_eq!(classify_codeset(Some("UTF-8")), LocaleCharset::Utf8);
        assert_eq!(classify_codeset(Some("utf8")), LocaleCharset::Utf8);
        assert_eq!(
            classify_codeset(Some("ANSI_X3.4-1968")),
            LocaleCharset::Ascii
        );
        assert_eq!(classify_codeset(Some("US-ASCII")), LocaleCharset::Ascii);
        assert_eq!(classify_codeset(Some("ISO-8859-1")), LocaleCharset::Other);
        assert_eq!(classify_codeset(Some("GBK")), LocaleCharset::Other);
        assert_eq!(classify_codeset(None), LocaleCharset::Ascii);
    }

    /// WHY: under a UTF-8 locale upstream prints a printable multibyte name
    /// raw (log.c:476 -> filtered_fwrite(.., 0, ..)); escaping it was the bug.
    #[test]
    fn utf8_client_keeps_printable_multibyte_raw() {
        client_in("UTF-8");
        assert_eq!(terminal(b"caf\xc3\xa9"), b"caf\xc3\xa9".to_vec());
        assert_eq!(terminal(b"nb\xc2\xa0x"), b"nb\xc2\xa0x".to_vec());
    }

    /// WHY: a valid C1 character is a control (log.c:264-265), so all of its
    /// bytes are escaped even though the sequence is valid UTF-8.
    #[test]
    fn utf8_client_escapes_c1_character_bytewise() {
        client_in("UTF-8");
        assert_eq!(terminal(b"c1\xc2\x85x"), b"c1\\#302\\#205x".to_vec());
    }

    /// WHY: `iconv` rejects a byte that does not start a valid character and
    /// rwrite() writes it as `\#ooo` (log.c:484-486), whatever its value.
    #[test]
    fn utf8_client_escapes_each_invalid_byte() {
        client_in("UTF-8");
        assert_eq!(terminal(b"bad\xff"), b"bad\\#377".to_vec());
        assert_eq!(terminal(b"trunc\xc3"), b"trunc\\#303".to_vec());
        assert_eq!(
            terminal(b"sur\xed\xa0\x80"),
            b"sur\\#355\\#240\\#200".to_vec()
        );
        assert_eq!(terminal(b"lone\x85x"), b"lone\\#205x".to_vec());
        assert_eq!(terminal(b"lat\xe9x"), b"lat\\#351x".to_vec());
    }

    /// WHY: C0 (tab excepted) and DEL are escaped on every path (log.c:288-291).
    #[test]
    fn utf8_client_escapes_c0_and_del_but_not_tab() {
        client_in("UTF-8");
        assert_eq!(terminal(b"esc\x1bx"), b"esc\\#033x".to_vec());
        assert_eq!(terminal(b"cr\rx"), b"cr\\#015x".to_vec());
        assert_eq!(terminal(b"del\x7f"), b"del\\#177".to_vec());
        assert_eq!(terminal(b"tab\tx"), b"tab\tx".to_vec());
    }

    /// WHY: a literal `\#ddd` would read as an escape, so its backslash is
    /// escaped (log.c:287-288) under every locale.
    #[test]
    fn utf8_client_escapes_literal_escape_sequence() {
        client_in("UTF-8");
        assert_eq!(terminal(b"lit\\#123"), b"lit\\#134#123".to_vec());
        assert_eq!(
            terminal(b"caf\xc3\xa9\\#123"),
            b"caf\xc3\xa9\\#134#123".to_vec()
        );
    }

    /// WHY: the `C` codeset is ASCII, so every byte at or above 0x80 is invalid
    /// and escaped - the same bytes `isprint()` rejects.
    #[test]
    fn c_locale_client_escapes_every_high_byte() {
        client_in("ANSI_X3.4-1968");
        assert_eq!(terminal(b"caf\xc3\xa9"), b"caf\\#303\\#251".to_vec());
        assert_eq!(terminal(b"c1\xc2\x85x"), b"c1\\#302\\#205x".to_vec());
        assert_eq!(terminal(b"bad\xff"), b"bad\\#377".to_vec());
        assert_eq!(terminal(b"del\x7f\tx"), b"del\\#177\tx".to_vec());
        assert_eq!(terminal(b"lit\\#123"), b"lit\\#134#123".to_vec());
    }

    /// WHY: a server never opens `ic_chck` (rsync.c:96 `!am_server`), so it
    /// keeps the `isprint()` branch (log.c:499) even under a UTF-8 locale.
    #[test]
    fn server_keeps_isprint_escaping_under_utf8() {
        set_locale_codeset(Some("UTF-8"));
        set_message_charset_check(false);
        assert_eq!(terminal(b"caf\xc3\xa9"), b"caf\\#303\\#251".to_vec());
    }

    /// WHY: `-8` closes `ic_chck`; filtered_fwrite(.., 0, ..) then keeps a
    /// valid printable character and an invalid byte outside 0x80-0x9F raw.
    #[test]
    fn eight_bit_client_under_utf8_passes_invalid_high_bytes() {
        set_locale_codeset(Some("UTF-8"));
        set_message_charset_check(false);
        let style = EscapeStyle::terminal(true);
        assert_eq!(
            escape_for_output(b"caf\xc3\xa9", style),
            b"caf\xc3\xa9".to_vec()
        );
        assert_eq!(escape_for_output(b"bad\xff", style), b"bad\xff".to_vec());
        assert_eq!(
            escape_for_output(b"lone\x85x", style),
            b"lone\\#205x".to_vec()
        );
        assert_eq!(
            escape_for_output(b"c1\xc2\x85x", style),
            b"c1\\#302\\#205x".to_vec()
        );
    }

    /// WHY: oc-rsync cannot validate other codesets without `iconv`, so a
    /// client there keeps the conservative `isprint()` escaping. This pins the
    /// documented gap: upstream would print a printable ISO-8859-1 byte raw.
    #[test]
    fn unsupported_codeset_client_keeps_conservative_escaping() {
        client_in("ISO-8859-1");
        assert_eq!(terminal(b"caf\xe9"), b"caf\\#351".to_vec());
    }

    /// WHY: the log file never takes the `ic_chck` branch (log.c:143), so the
    /// client's check must not change it.
    #[test]
    fn log_file_is_unaffected_by_the_client_check() {
        client_in("UTF-8");
        let style = EscapeStyle::log_file();
        assert_eq!(escape_for_output(b"bad\xff", style), b"bad\xff".to_vec());
        assert_eq!(
            escape_for_output(b"c1\xc2\x85x", style),
            b"c1\\#302\\#205x".to_vec()
        );
    }

    /// upstream: log.c:288-291 - a C0 byte in a diagnostic is escaped, a tab
    /// is not, and the trailing newline stays raw.
    #[test]
    fn write_terminal_line_escapes_c0_and_keeps_a_tab() {
        let mut out = Vec::new();
        write_terminal_line(&mut out, "link_stat \"no\u{1}p\te\" failed").unwrap();
        assert_eq!(out, b"link_stat \"no\\#001p\te\" failed\n".to_vec());
    }
}
