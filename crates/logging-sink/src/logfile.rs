//! Upstream-compatible log-file line formatting.
//!
//! Upstream rsync writes every log-file line through one function:
//! `logit()` prefixes the message with `timestring(time(NULL))` and the
//! writer's pid - `"%s [%d] %s"` (upstream: log.c:122-132). `timestring()`
//! renders local time as `%4d/%02d/%02d %02d:%02d:%02d`
//! (upstream: util1.c:1456-1473). Both the client `--log-file` sink and the
//! daemon `log file` sink share that formatter; this module is its single
//! oc-rsync counterpart.

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::escape::{EscapeStyle, escape_for_output};

/// Maximum epoch seconds accepted for timestamp formatting.
///
/// Corresponds to 9999-12-31 23:59:59 UTC - the last representable date in a
/// 4-digit-year `YYYY/MM/DD HH:MM:SS` layout. upstream: util1.c:1463-1466
/// `timestring()` falls back to a placeholder when `localtime_r()` fails.
const MAX_TIMESTAMP_EPOCH_SECS: i64 = 253_402_300_799;

/// Formats an instant as `YYYY/MM/DD HH:MM:SS` in the local timezone.
///
/// upstream: util1.c:1456-1473 `timestring()` - `localtime_r()` followed by
/// `"%4d/%02d/%02d %02d:%02d:%02d"`. The local offset is computed per instant
/// (DST-correct) via `platform::local_time`, matching `localtime_r`.
/// Out-of-range instants render the placeholder `0000/00/00 00:00:00`,
/// mirroring the upstream NULL-`localtime_r` fallback.
#[must_use]
pub fn format_log_timestamp(instant: SystemTime) -> String {
    let unix_secs = match instant.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(before_epoch) => -i64::try_from(before_epoch.duration().as_secs()).unwrap_or(i64::MAX),
    };
    let offset = i64::from(platform::local_time::local_utc_offset_seconds(unix_secs));
    let local_secs = unix_secs.saturating_add(offset);
    if !(0..=MAX_TIMESTAMP_EPOCH_SECS).contains(&local_secs) {
        // upstream: util1.c:1463-1466 timestring() NULL-check equivalent.
        return "0000/00/00 00:00:00".to_owned();
    }

    let day_seconds = (local_secs % 86_400) as u32;
    let hours = day_seconds / 3_600;
    let minutes = (day_seconds % 3_600) / 60;
    let seconds = day_seconds % 60;
    let (year, month, day) = civil_from_days(local_secs / 86_400);
    format!("{year:04}/{month:02}/{day:02} {hours:02}:{minutes:02}:{seconds:02}")
}

/// Converts a day count (days since 1970-01-01) to a civil `(year, month, day)`.
///
/// Algorithm from Howard Hinnant's date library (public domain).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = i64::from(yoe) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Returns the prefix upstream stamps on every log-file line, for `instant`
/// and `pid`: `"YYYY/MM/DD HH:MM:SS [pid] "`.
///
/// upstream: log.c:127 `fprintf(logfile_fp, "%s [%d] %s", timestring(time(NULL)),
/// (int)getpid(), buf)`.
#[must_use]
pub fn format_log_line_prefix(instant: SystemTime, pid: u32) -> String {
    format!("{} [{pid}] ", format_log_timestamp(instant))
}

/// Returns the log-file line prefix for the current instant and process.
#[must_use]
pub fn log_line_prefix_now() -> String {
    format_log_line_prefix(SystemTime::now(), std::process::id())
}

/// Writer adapter that stamps the upstream log-file prefix on every line.
///
/// Wraps the log-file handle so that each written line starts with
/// `"YYYY/MM/DD HH:MM:SS [pid] "` (upstream: log.c:122-132 `logit()`), with
/// the timestamp taken when the line starts. Lines that would be empty are
/// dropped: upstream emits cosmetic blank separators as `FCLIENT` messages
/// (e.g. main.c:427/461 `rprintf(FCLIENT, "\n")`), and `rwrite()` converts
/// `FCLIENT` to `FINFO` *after* skipping the log-file branch
/// (upstream: log.c:288-289), so a blank line never reaches the log file.
#[derive(Debug)]
pub struct LogFileWriter<W: Write> {
    inner: W,
    at_line_start: bool,
}

impl<W: Write> LogFileWriter<W> {
    /// Wraps `inner` so every line it receives is stamped with the upstream
    /// log-file prefix.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            at_line_start: true,
        }
    }

    /// Returns a shared reference to the wrapped writer.
    pub const fn get_ref(&self) -> &W {
        &self.inner
    }

    /// Consumes the adapter, returning the wrapped writer.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> LogFileWriter<W> {
    /// Writes one line body through the log-file escape filter.
    ///
    /// upstream: log.c:126 `logit()` is the only function that writes to
    /// `logfile_fp`, and it hands every line to `filtered_fwrite(logfile_fp,
    /// buf, len, 0, 1, trailing)` (log.c:132). Escaping there rather than in
    /// each producer is what makes it unbypassable - a caller cannot forget it,
    /// because there is no other way to reach the file.
    ///
    /// The timestamp/pid prefix and the line terminator are written raw, as
    /// upstream does: `logit()` `fprintf`s the prefix before the call and
    /// passes the stripped trailing newline to `filtered_fwrite` as `end_char`.
    fn write_escaped(&mut self, body: &[u8]) -> io::Result<()> {
        if body.is_empty() {
            return Ok(());
        }
        self.inner
            .write_all(&escape_for_output(body, EscapeStyle::log_file()))
    }
}

impl<W: Write> Write for LogFileWriter<W> {
    /// upstream: log.c:128-129 - `logit()` strips exactly ONE trailing newline
    /// and hands everything before it to `filtered_fwrite`. A newline *inside*
    /// the message is therefore escaped as `\#012` and stays on one log line;
    /// only the newline that ends the message closes the record. That is the
    /// whole CWE-117 defence: were an embedded newline to start a fresh line,
    /// it would be stamped with a genuine timestamp/pid prefix and a filename
    /// could forge an authentic-looking log entry.
    ///
    /// The message boundary is the `write` call, so a producer must deliver one
    /// message per call - which both `MessageSink` and the CLI's `writeln!`
    /// sites do. A partial write (a message split across calls) is carried by
    /// `at_line_start`, so only the first fragment is prefixed.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let (body, terminated) = match buf.split_last() {
            Some((b'\n', head)) => (head, true),
            _ => (buf, false),
        };
        // log.c:288-289 - FCLIENT blank separators never reach the log file, so
        // an empty message must vanish rather than emit a bare prefix.
        if body.is_empty() && self.at_line_start {
            return Ok(buf.len());
        }
        if self.at_line_start {
            self.inner.write_all(log_line_prefix_now().as_bytes())?;
            self.at_line_start = false;
        }
        self.write_escaped(body)?;
        if terminated {
            self.inner.write_all(b"\n")?;
            self.at_line_start = true;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix_pattern_matches(line: &str) -> bool {
        // ^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} \[\d+\]
        let bytes = line.as_bytes();
        if bytes.len() < 23 {
            return false;
        }
        let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
        digits(0..4)
            && bytes[4] == b'/'
            && digits(5..7)
            && bytes[7] == b'/'
            && digits(8..10)
            && bytes[10] == b' '
            && digits(11..13)
            && bytes[13] == b':'
            && digits(14..16)
            && bytes[16] == b':'
            && digits(17..19)
            && bytes[19] == b' '
            && bytes[20] == b'['
            && line[21..]
                .split_once("] ")
                .is_some_and(|(pid, _)| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
    }

    /// upstream: log.c:127 - every log-file line starts with
    /// `YYYY/MM/DD HH:MM:SS [pid] `.
    #[test]
    fn prefix_has_upstream_shape() {
        let prefix = log_line_prefix_now();
        assert!(
            prefix_pattern_matches(&prefix),
            "prefix {prefix:?} does not match upstream logit() shape"
        );
        assert!(prefix.ends_with("] "));
        assert!(prefix.contains(&format!("[{}] ", std::process::id())));
    }

    /// upstream: util1.c:1467-1469 - zero-padded `%4d/%02d/%02d %02d:%02d:%02d`.
    #[test]
    fn timestamp_is_zero_padded() {
        let rendered = format_log_timestamp(UNIX_EPOCH + std::time::Duration::from_secs(3_723));
        // 1970-01-01 01:02:03 UTC; the local offset may shift it, but the
        // shape must remain fixed-width.
        assert_eq!(rendered.len(), 19);
        assert_eq!(&rendered[4..5], "/");
        assert_eq!(&rendered[7..8], "/");
        assert_eq!(&rendered[10..11], " ");
    }

    /// upstream: util1.c:1461 - `localtime_r`, not UTC: the rendered hour
    /// must reflect the host offset for the instant.
    #[test]
    fn timestamp_applies_local_offset() {
        let instant = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let offset = i64::from(platform::local_time::local_utc_offset_seconds(
            1_700_000_000,
        ));
        let shifted = 1_700_000_000_i64 + offset;
        let expected_hour = (shifted % 86_400) / 3_600;
        let rendered = format_log_timestamp(instant);
        assert_eq!(&rendered[11..13], format!("{expected_hour:02}").as_str());
    }

    /// upstream: util1.c:1463-1466 - out-of-range instants render the
    /// placeholder instead of overflowing the fixed-width year.
    #[test]
    fn timestamp_out_of_range_renders_placeholder() {
        let far = UNIX_EPOCH + std::time::Duration::from_secs(u64::MAX / 2);
        assert_eq!(format_log_timestamp(far), "0000/00/00 00:00:00");
    }

    /// upstream: log.c:122-132 - each line written through the sink gains
    /// exactly one prefix, and multi-line writes prefix every line.
    #[test]
    fn writer_prefixes_every_line() {
        let mut writer = LogFileWriter::new(Vec::new());
        writer.write_all(b"building file list\n").unwrap();
        // One message per call: that is the boundary `logit()` works on, and
        // the shape every real producer (MessageSink, `writeln!`) delivers.
        writer.write_all(b"first\n").unwrap();
        writer.write_all(b"second\n").unwrap();
        let output = String::from_utf8(writer.into_inner()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(
                prefix_pattern_matches(line),
                "line {line:?} lacks the upstream prefix"
            );
        }
        assert!(lines[0].ends_with("building file list"));
        assert!(lines[1].ends_with("first"));
        assert!(lines[2].ends_with("second"));
    }

    /// A line split across separate `write` calls must receive one prefix,
    /// stamped when the line starts (upstream logs one prefix per message).
    #[test]
    fn writer_stamps_split_line_once() {
        let mut writer = LogFileWriter::new(Vec::new());
        writer.write_all(b"sent 42 bytes").unwrap();
        writer.write_all(b"  total size 7\n").unwrap();
        let output = String::from_utf8(writer.into_inner()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(prefix_pattern_matches(lines[0]));
        assert!(lines[0].ends_with("sent 42 bytes  total size 7"));
    }

    /// upstream: main.c:427/461 emit blank separators as FCLIENT, which never
    /// reaches the log file (log.c:288-289): an empty line must vanish rather
    /// than appear as a bare prefix.
    #[test]
    fn writer_drops_blank_lines() {
        let mut writer = LogFileWriter::new(Vec::new());
        writer.write_all(b"\n").unwrap();
        writer.write_all(b"totals\n").unwrap();
        writer.write_all(b"\n").unwrap();
        let output = String::from_utf8(writer.into_inner()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "blank lines must not reach the log: {output:?}"
        );
        assert!(lines[0].ends_with("totals"));
    }

    /// Returns the body of the single logged line, prefix stripped.
    fn single_line_body(raw: &[u8]) -> Vec<u8> {
        assert_eq!(
            raw.iter().filter(|&&byte| byte == b'\n').count(),
            1,
            "expected exactly one line: {raw:?}"
        );
        let body_start = raw
            .windows(2)
            .position(|pair| pair == b"] ")
            .expect("prefix ends with `] `")
            + 2;
        assert_eq!(*raw.last().expect("non-empty"), b'\n', "newline is raw");
        raw[body_start..raw.len() - 1].to_vec()
    }

    /// upstream: log.c:132 - `logit()` hands every line to `filtered_fwrite`,
    /// so a control byte that reached the log through *any* producer is
    /// escaped. Escaping in the sink rather than in a renderer is what makes
    /// this unbypassable: the daemon's log writers never touch the CLI's
    /// out-format renderer, yet must not be able to emit a raw `ESC` that
    /// forges a log line on the operator's terminal (CWE-117).
    #[test]
    fn writer_escapes_control_bytes_in_the_body() {
        let mut writer = LogFileWriter::new(Vec::new());
        writer.write_all(b"recv nope\x1b[31mX\n").unwrap();
        let body = single_line_body(&writer.into_inner());
        assert_eq!(
            body,
            b"recv nope\\#033[31mX".to_vec(),
            "ESC must be escaped exactly once, as \\#033"
        );
    }

    /// A line with nothing escapable must reach the file byte-identical. This
    /// is the non-vacuity control for the test above: without it, an escaper
    /// that mangled every line would still satisfy "the ESC is gone".
    #[test]
    fn writer_leaves_printable_lines_byte_identical() {
        let mut writer = LogFileWriter::new(Vec::new());
        writer.write_all(b"recv plain/name.txt\tok\n").unwrap();
        let body = single_line_body(&writer.into_inner());
        assert_eq!(body, b"recv plain/name.txt\tok".to_vec());
    }

    /// A newline inside the message must not open a second log record.
    ///
    /// This is the injection itself, not a cosmetic detail: every line the
    /// sink emits is stamped with a real timestamp and pid, so a filename
    /// carrying `\n` would otherwise produce a second, entirely authentic-
    /// looking entry under the attacker's control. upstream: log.c:128-129
    /// strips only the message's own trailing newline.
    #[test]
    fn writer_escapes_a_newline_inside_the_message() {
        let mut writer = LogFileWriter::new(Vec::new());
        writer
            .write_all(b"recv a\n2026/01/01 00:00:00 [1] forged\n")
            .unwrap();
        let body = single_line_body(&writer.into_inner());
        assert_eq!(
            body,
            b"recv a\\#0122026/01/01 00:00:00 [1] forged".to_vec(),
            "an embedded newline must stay on one line as \\#012"
        );
    }

    /// upstream: log.c:132 passes `use_isprint = 0` and `escape_c1 = 1`, so a
    /// UTF-8 filename survives verbatim while the 8-bit CSI (0x9B) - which can
    /// drive a terminal just as an `ESC [` pair does - is escaped.
    #[test]
    fn writer_escapes_c1_but_passes_utf8_through() {
        let mut writer = LogFileWriter::new(Vec::new());
        writer.write_all(b"recv caf\xc3\xa9\x9b31m\n").unwrap();
        let body = single_line_body(&writer.into_inner());
        assert_eq!(body, b"recv caf\xc3\xa9\\#23331m".to_vec());
    }
}
