/// Default transfer log format matching upstream rsync's `rsyncd.conf(5)`.
///
/// Upstream: `log.c` -- `lp_log_format()` returns `"%o %h [%a] %m (%u) %f %l"` when
/// `transfer logging` is enabled but no explicit `log format` is configured.
const DEFAULT_LOG_FORMAT: &str = "%o %h [%a] %m (%u) %f %l";

/// Direction of a daemon transfer operation.
///
/// Maps to the `%o` escape in the log format string. Upstream rsync uses
/// "send" when the daemon sends files to the client and "recv" when
/// receiving files from the client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TransferOperation {
    /// Daemon is sending files to the client.
    Send,
    /// Daemon is receiving files from the client.
    Recv,
}

impl TransferOperation {
    /// Returns the upstream-compatible string representation.
    ///
    /// Upstream: `log.c` -- `am_sender ? "send" : "recv"`.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Recv => "recv",
        }
    }
}

impl fmt::Display for TransferOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Contextual data for expanding a daemon transfer log format string.
///
/// Each field corresponds to an upstream rsync log format escape. Fields are
/// populated from the active module definition and connection state at the
/// time of the transfer.
///
/// Upstream: `log.c:log_formatted()` -- walks the format string and expands
/// escapes using global state and function-local parameters.
struct LogFormatContext<'a> {
    /// Transfer direction (`%o`).
    operation: TransferOperation,
    /// Resolved peer hostname or IP display string (`%h`).
    hostname: &'a str,
    /// Peer IP address string (`%a`).
    remote_addr: &'a str,
    /// Module name from the daemon config (`%m`).
    module_name: &'a str,
    /// Authenticated username, or empty if anonymous (`%u`).
    username: &'a str,
    /// Relative path of the transferred file (`%f`).
    filename: &'a str,
    /// File size in bytes (`%l`).
    file_length: u64,
    /// Daemon process ID (`%p`).
    pid: u32,
    /// Filesystem path of the module root (`%P`).
    module_path: &'a str,
    /// Formatted timestamp string (`%t`).
    timestamp: &'a str,
    /// Number of bytes transferred over the wire (`%b`).
    bytes_transferred: u64,
    /// Number of bytes that were checksumed (`%c`).
    bytes_checksumed: u64,
    /// Itemize-changes string for the file (`%i`).
    itemize_string: &'a str,
}

/// Appends the decimal representation of a `u64` to a string.
fn push_u64(buf: &mut String, value: u64) {
    use std::fmt::Write as _;
    let _ = write!(buf, "{value}");
}

/// Appends the decimal representation of a `u32` to a string.
fn push_u32(buf: &mut String, value: u32) {
    use std::fmt::Write as _;
    let _ = write!(buf, "{value}");
}

/// Expands a log format string using the provided context.
///
/// Processes each `%X` escape by substituting the corresponding field from
/// `ctx`. Unknown escapes are passed through verbatim. A literal `%%`
/// produces a single `%` in the output.
///
/// Upstream: `log.c:log_formatted()` -- iterates over the format string
/// one character at a time, expanding percent-escapes from global and
/// per-file state.
fn expand_log_format(format: &str, ctx: &LogFormatContext<'_>) -> String {
    let mut result = String::with_capacity(format.len() * 2);
    let mut chars = format.chars();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            result.push(ch);
            continue;
        }

        match chars.next() {
            Some('o') => result.push_str(ctx.operation.as_str()),
            Some('h') => result.push_str(ctx.hostname),
            Some('a') => result.push_str(ctx.remote_addr),
            Some('m') => result.push_str(ctx.module_name),
            Some('u') => result.push_str(ctx.username),
            Some('f') => result.push_str(ctx.filename),
            Some('l') => push_u64(&mut result, ctx.file_length),
            Some('p') => push_u32(&mut result, ctx.pid),
            Some('P') => result.push_str(ctx.module_path),
            Some('t') => result.push_str(ctx.timestamp),
            Some('b') => push_u64(&mut result, ctx.bytes_transferred),
            Some('c') => push_u64(&mut result, ctx.bytes_checksumed),
            Some('i') => result.push_str(ctx.itemize_string),
            Some('%') => result.push('%'),
            Some(other) => {
                // Unknown escape: pass through verbatim
                result.push('%');
                result.push(other);
            }
            None => {
                // Trailing percent with no escape character
                result.push('%');
            }
        }
    }

    result
}

/// Expands the transfer log format and writes the result to the log sink.
///
/// Uses the module's configured `log_format`, falling back to
/// `DEFAULT_LOG_FORMAT` when none is specified.
fn log_transfer(format: &str, ctx: &LogFormatContext<'_>, log_sink: &SharedLogSink) {
    let expanded = expand_log_format(format, ctx);
    let message = rsync_info!(expanded).with_role(Role::Daemon);
    log_message(log_sink, &message);
}

/// Formats a Unix epoch timestamp as `YYYY/MM/DD HH:MM:SS`.
///
/// Upstream: `log.c` uses `strftime("%Y/%m/%d %H:%M:%S", ...)` via `timestring()`.
/// This implementation performs the conversion manually to avoid external crate
/// dependencies while matching the upstream output format.
fn format_daemon_timestamp(epoch_secs: u64) -> String {
    // Days from 1970-01-01 to the given epoch seconds.
    let total_days = epoch_secs / 86400;
    let day_seconds = (epoch_secs % 86400) as u32;
    let hours = day_seconds / 3600;
    let minutes = (day_seconds % 3600) / 60;
    let seconds = day_seconds % 60;

    // Civil date from day count using the algorithm from
    // Howard Hinnant's `chrono`-compatible date conversion.
    let (year, month, day) = civil_from_days(total_days as i64);

    format!("{year:04}/{month:02}/{day:02} {hours:02}:{minutes:02}:{seconds:02}")
}

/// Converts a day count (days since 1970-01-01) to a civil date (year, month, day).
///
/// Algorithm from Howard Hinnant's date library (public domain).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

/// Returns the effective log format string for a module.
///
/// Falls back to `DEFAULT_LOG_FORMAT` when the module does not specify a
/// custom `log_format` directive.
fn effective_log_format(module: &ModuleDefinition) -> &str {
    module.log_format.as_deref().unwrap_or(DEFAULT_LOG_FORMAT)
}

#[cfg(test)]
mod log_format_tests {
    use super::*;

    fn sample_context<'a>() -> LogFormatContext<'a> {
        LogFormatContext {
            operation: TransferOperation::Send,
            hostname: "client.example.com",
            remote_addr: "192.168.1.100",
            module_name: "backup",
            username: "alice",
            filename: "docs/report.pdf",
            file_length: 1048576,
            pid: 42,
            module_path: "/srv/backup",
            timestamp: "2026/02/21 14:30:00",
            bytes_transferred: 524288,
            bytes_checksumed: 1048576,
            itemize_string: ">f+++++++++",
        }
    }

    // --- TransferOperation tests ---

    #[test]
    fn transfer_operation_send_str() {
        assert_eq!(TransferOperation::Send.as_str(), "send");
    }

    #[test]
    fn transfer_operation_recv_str() {
        assert_eq!(TransferOperation::Recv.as_str(), "recv");
    }

    #[test]
    fn transfer_operation_display_send() {
        let op = TransferOperation::Send;
        assert_eq!(format!("{op}"), "send");
    }

    #[test]
    fn transfer_operation_display_recv() {
        let op = TransferOperation::Recv;
        assert_eq!(format!("{op}"), "recv");
    }

    #[test]
    fn transfer_operation_eq() {
        assert_eq!(TransferOperation::Send, TransferOperation::Send);
        assert_eq!(TransferOperation::Recv, TransferOperation::Recv);
        assert_ne!(TransferOperation::Send, TransferOperation::Recv);
    }

    #[test]
    fn transfer_operation_clone() {
        let op = TransferOperation::Send;
        let cloned = op;
        assert_eq!(op, cloned);
    }

    #[test]
    fn transfer_operation_debug() {
        let debug = format!("{:?}", TransferOperation::Send);
        assert!(debug.contains("Send"));
    }

    // --- DEFAULT_LOG_FORMAT tests ---

    #[test]
    fn default_log_format_matches_upstream() {
        assert_eq!(DEFAULT_LOG_FORMAT, "%o %h [%a] %m (%u) %f %l");
    }

    // --- expand_log_format: individual escape tests ---

    #[test]
    fn expand_operation() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%o", &ctx), "send");
    }

    #[test]
    fn expand_hostname() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%h", &ctx), "client.example.com");
    }

    #[test]
    fn expand_remote_addr() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%a", &ctx), "192.168.1.100");
    }

    #[test]
    fn expand_module_name() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%m", &ctx), "backup");
    }

    #[test]
    fn expand_username() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%u", &ctx), "alice");
    }

    #[test]
    fn expand_filename() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%f", &ctx), "docs/report.pdf");
    }

    #[test]
    fn expand_file_length() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%l", &ctx), "1048576");
    }

    #[test]
    fn expand_pid() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%p", &ctx), "42");
    }

    #[test]
    fn expand_module_path() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%P", &ctx), "/srv/backup");
    }

    #[test]
    fn expand_timestamp() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%t", &ctx), "2026/02/21 14:30:00");
    }

    #[test]
    fn expand_bytes_transferred() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%b", &ctx), "524288");
    }

    #[test]
    fn expand_bytes_checksumed() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%c", &ctx), "1048576");
    }

    #[test]
    fn expand_itemize_string() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%i", &ctx), ">f+++++++++");
    }

    #[test]
    fn expand_literal_percent() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%%", &ctx), "%");
    }

    // --- expand_log_format: default format test ---

    #[test]
    fn expand_default_format() {
        let ctx = sample_context();
        let result = expand_log_format(DEFAULT_LOG_FORMAT, &ctx);
        assert_eq!(
            result,
            "send client.example.com [192.168.1.100] backup (alice) docs/report.pdf 1048576"
        );
    }

    // --- expand_log_format: recv operation ---

    #[test]
    fn expand_recv_operation() {
        let mut ctx = sample_context();
        ctx.operation = TransferOperation::Recv;
        let result = expand_log_format("%o", &ctx);
        assert_eq!(result, "recv");
    }

    // --- expand_log_format: edge cases ---

    #[test]
    fn expand_empty_format() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("", &ctx), "");
    }

    #[test]
    fn expand_no_escapes() {
        let ctx = sample_context();
        assert_eq!(
            expand_log_format("plain text", &ctx),
            "plain text"
        );
    }

    #[test]
    fn expand_unknown_escape_passthrough() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%Z", &ctx), "%Z");
    }

    #[test]
    fn expand_trailing_percent() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("end%", &ctx), "end%");
    }

    #[test]
    fn expand_multiple_escapes() {
        let ctx = sample_context();
        let result = expand_log_format("%o %h %a", &ctx);
        assert_eq!(result, "send client.example.com 192.168.1.100");
    }

    #[test]
    fn expand_adjacent_escapes() {
        let ctx = sample_context();
        let result = expand_log_format("%o%h%a", &ctx);
        assert_eq!(result, "sendclient.example.com192.168.1.100");
    }

    #[test]
    fn expand_double_percent_with_escape() {
        let ctx = sample_context();
        let result = expand_log_format("100%% complete: %f", &ctx);
        assert_eq!(result, "100% complete: docs/report.pdf");
    }

    #[test]
    fn expand_zero_file_length() {
        let mut ctx = sample_context();
        ctx.file_length = 0;
        assert_eq!(expand_log_format("%l", &ctx), "0");
    }

    #[test]
    fn expand_large_file_length() {
        let mut ctx = sample_context();
        ctx.file_length = u64::MAX;
        assert_eq!(
            expand_log_format("%l", &ctx),
            u64::MAX.to_string()
        );
    }

    #[test]
    fn expand_empty_username() {
        let mut ctx = sample_context();
        ctx.username = "";
        let result = expand_log_format("(%u)", &ctx);
        assert_eq!(result, "()");
    }

    #[test]
    fn expand_custom_format() {
        let ctx = sample_context();
        let result = expand_log_format("%i %o %f %l %b", &ctx);
        assert_eq!(result, ">f+++++++++ send docs/report.pdf 1048576 524288");
    }

    // --- effective_log_format tests ---

    #[test]
    fn effective_log_format_uses_module_setting() {
        let module = ModuleDefinition {
            transfer_logging: true,
            log_format: Some("%o %f %l".to_owned()),
            ..Default::default()
        };
        assert_eq!(effective_log_format(&module), "%o %f %l");
    }

    #[test]
    fn effective_log_format_falls_back_to_default() {
        let module = ModuleDefinition {
            transfer_logging: true,
            log_format: None,
            ..Default::default()
        };
        assert_eq!(effective_log_format(&module), DEFAULT_LOG_FORMAT);
    }

    // --- format_daemon_timestamp tests ---

    #[test]
    fn timestamp_unix_epoch() {
        assert_eq!(format_daemon_timestamp(0), "1970/01/01 00:00:00");
    }

    #[test]
    fn timestamp_known_date() {
        // 2026-02-21 14:30:00 UTC = 1771684200 epoch seconds
        // (verified via `datetime.datetime(2026,2,21,14,30,0,tzinfo=utc).timestamp()`)
        let ts = format_daemon_timestamp(1_771_684_200);
        assert_eq!(ts, "2026/02/21 14:30:00");
    }

    #[test]
    fn timestamp_end_of_day() {
        // 1970-01-01 23:59:59 = 86399
        assert_eq!(format_daemon_timestamp(86399), "1970/01/01 23:59:59");
    }

    #[test]
    fn timestamp_start_of_second_day() {
        // 1970-01-02 00:00:00 = 86400
        assert_eq!(format_daemon_timestamp(86400), "1970/01/02 00:00:00");
    }

    #[test]
    fn timestamp_leap_year_date() {
        // 2024-02-29 12:00:00 UTC = 1709208000
        let ts = format_daemon_timestamp(1_709_208_000);
        assert_eq!(ts, "2024/02/29 12:00:00");
    }

    // --- push_u64 / push_u32 tests ---

    #[test]
    fn push_u64_zero() {
        let mut buf = String::new();
        push_u64(&mut buf, 0);
        assert_eq!(buf, "0");
    }

    #[test]
    fn push_u64_max() {
        let mut buf = String::new();
        push_u64(&mut buf, u64::MAX);
        assert_eq!(buf, u64::MAX.to_string());
    }

    #[test]
    fn push_u32_value() {
        let mut buf = String::new();
        push_u32(&mut buf, 12345);
        assert_eq!(buf, "12345");
    }

    // --- civil_from_days tests ---

    #[test]
    fn civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn civil_from_days_known_date() {
        // 2026-02-21 is day 20505 from epoch
        assert_eq!(civil_from_days(20505), (2026, 2, 21));
    }
}
