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

impl<W: Write> Write for LogFileWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Copy line segments through in batches, stamping the prefix whenever
        // a non-empty line starts and dropping FCLIENT-style blank separators
        // (log.c:288-289 - FCLIENT never reaches the log file).
        let mut rest = buf;
        while !rest.is_empty() {
            if self.at_line_start && rest[0] == b'\n' {
                rest = &rest[1..];
                continue;
            }
            if self.at_line_start {
                self.inner.write_all(log_line_prefix_now().as_bytes())?;
                self.at_line_start = false;
            }
            match rest.iter().position(|&byte| byte == b'\n') {
                Some(newline) => {
                    self.inner.write_all(&rest[..=newline])?;
                    self.at_line_start = true;
                    rest = &rest[newline + 1..];
                }
                None => {
                    self.inner.write_all(rest)?;
                    rest = &[];
                }
            }
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
        writer.write_all(b"first\nsecond\n").unwrap();
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
        writer.write_all(b"totals\n\n").unwrap();
        let output = String::from_utf8(writer.into_inner()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "blank lines must not reach the log: {output:?}"
        );
        assert!(lines[0].ends_with("totals"));
    }
}
