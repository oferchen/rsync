// Daemon transfer log format expansion engine.
//
// Implements the `log format` directive from rsyncd.conf, expanding format
// specifiers in the style of upstream rsync's `log_formatted()` in log.c.
// Each transferred file produces a log line by substituting specifiers like
// `%o`, `%h`, `%f` with runtime values from the transfer context.

/// Default log format used when `transfer logging` is enabled but no
/// explicit `log format` directive is set.
///
/// Mirrors upstream rsync's default: `%o %h [%a] %m (%u) %f %l`.
// upstream: loadparm.c — lp_log_format() default
pub(crate) const DEFAULT_LOG_FORMAT: &str = "%o %h [%a] %m (%u) %f %l";

/// Operation direction for a daemon transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransferOperation {
    /// Files are being sent from the daemon to the client.
    Send,
    /// Files are being received by the daemon from the client.
    Recv,
}

impl TransferOperation {
    /// Returns the short label used in log output.
    ///
    /// Matches upstream rsync's `am_sender ? "send" : "recv"` convention.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Recv => "recv",
        }
    }
}

/// Runtime values available for log format specifier expansion.
///
/// Populated once per transferred file and passed to [`expand_log_format`] to
/// produce the final log line. Fields mirror upstream rsync's `log_formatted()`
/// parameter set and the format specifiers documented in `rsyncd.conf(5)`.
#[derive(Clone, Debug)]
pub(crate) struct LogFormatContext<'a> {
    /// Transfer direction (`send` or `recv`). Specifier: `%o`.
    pub(crate) operation: TransferOperation,
    /// Remote hostname (or `"undetermined"` when reverse lookup is disabled). Specifier: `%h`.
    pub(crate) hostname: &'a str,
    /// Remote IP address. Specifier: `%a`.
    pub(crate) remote_address: IpAddr,
    /// Module name. Specifier: `%m`.
    pub(crate) module_name: &'a str,
    /// Authenticated username (empty string if no auth). Specifier: `%u`.
    pub(crate) auth_user: &'a str,
    /// File path relative to the module root. Specifier: `%f`.
    pub(crate) filename: &'a str,
    /// File length in bytes. Specifier: `%l`.
    pub(crate) file_length: u64,
    /// PID of the daemon process. Specifier: `%p`.
    pub(crate) daemon_pid: u32,
    /// Module path on disk. Specifier: `%P`.
    pub(crate) module_path: &'a Path,
    /// Current date/time string. Specifier: `%t`.
    pub(crate) timestamp: &'a str,
    /// Bytes actually transferred over the wire. Specifier: `%b`.
    pub(crate) bytes_transferred: u64,
    /// Total bytes that were checksum-verified. Specifier: `%c`.
    pub(crate) checksum_bytes: u64,
    /// Itemize change string (like `-i` output, e.g. `>f..t......`). Specifier: `%i`.
    pub(crate) itemize: &'a str,
}

/// Expands a log format string by substituting `%`-specifiers with values from
/// the provided context.
///
/// Implements the specifiers documented in `rsyncd.conf(5)`:
///
/// | Specifier | Expansion                                    |
/// |-----------|----------------------------------------------|
/// | `%o`      | Operation (`send` / `recv`)                  |
/// | `%h`      | Remote hostname                              |
/// | `%a`      | Remote IP address                            |
/// | `%m`      | Module name                                  |
/// | `%u`      | Authenticated username                       |
/// | `%f`      | Filename (relative to module root)            |
/// | `%l`      | File length in bytes                         |
/// | `%p`      | Daemon PID                                   |
/// | `%P`      | Module path                                  |
/// | `%t`      | Current date/time                            |
/// | `%b`      | Bytes transferred                            |
/// | `%c`      | Total checksum bytes                         |
/// | `%i`      | Itemize change string                        |
/// | `%%`      | Literal `%`                                  |
///
/// Unknown specifiers are silently dropped, matching upstream behaviour.
// upstream: log.c:log_formatted()
pub(crate) fn expand_log_format(format: &str, ctx: &LogFormatContext<'_>) -> String {
    let mut output = String::with_capacity(format.len() * 2);
    let mut chars = format.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('o') => output.push_str(ctx.operation.as_str()),
            Some('h') => output.push_str(ctx.hostname),
            Some('a') => output.push_str(&ctx.remote_address.to_string()),
            Some('m') => output.push_str(ctx.module_name),
            Some('u') => output.push_str(ctx.auth_user),
            Some('f') => output.push_str(ctx.filename),
            Some('l') => output.push_str(&ctx.file_length.to_string()),
            Some('p') => output.push_str(&ctx.daemon_pid.to_string()),
            Some('P') => output.push_str(&ctx.module_path.display().to_string()),
            Some('t') => output.push_str(ctx.timestamp),
            Some('b') => output.push_str(&ctx.bytes_transferred.to_string()),
            Some('c') => output.push_str(&ctx.checksum_bytes.to_string()),
            Some('i') => output.push_str(ctx.itemize),
            Some('%') => output.push('%'),
            // Unknown specifier: silently skip (upstream drops unrecognized codes)
            Some(_) => {}
            // Trailing `%` at end of format string: drop it
            None => {}
        }
    }

    output
}

/// Checks whether a log format string contains a given specifier character.
///
/// This is used to determine ahead of time whether expensive fields (like `%i`
/// or `%b`) need to be computed for the transfer context.
// upstream: log.c:log_format_has()
#[cfg(test)]
pub(crate) fn log_format_has(format: &str, specifier: char) -> bool {
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(&next) = chars.peek() {
                chars.next();
                if next == specifier {
                    return true;
                }
            }
        }
    }
    false
}

/// Writes a transfer log line to the provided log sink.
///
/// Expands the format string using the context and writes it as an informational
/// log message. This is the main entry point called from the daemon transfer
/// path when `transfer_logging` is enabled on a module.
pub(crate) fn log_transfer(
    log_sink: &SharedLogSink,
    format: &str,
    ctx: &LogFormatContext<'_>,
) {
    let expanded = expand_log_format(format, ctx);
    let message = rsync_info!(expanded).with_role(Role::Daemon);
    log_message(log_sink, &message);
}

#[cfg(test)]
mod log_format_tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::path::Path;

    fn make_context() -> LogFormatContext<'static> {
        LogFormatContext {
            operation: TransferOperation::Send,
            hostname: "client.example.com",
            remote_address: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            module_name: "data",
            auth_user: "alice",
            filename: "docs/readme.txt",
            file_length: 4096,
            daemon_pid: 12345,
            module_path: Path::new("/srv/data"),
            timestamp: "2024/01/15 10:30:00",
            bytes_transferred: 2048,
            checksum_bytes: 4096,
            itemize: ">f..t......",
        }
    }

    // ==================== TransferOperation tests ====================

    #[test]
    fn transfer_operation_send_str() {
        assert_eq!(TransferOperation::Send.as_str(), "send");
    }

    #[test]
    fn transfer_operation_recv_str() {
        assert_eq!(TransferOperation::Recv.as_str(), "recv");
    }

    #[test]
    fn transfer_operation_eq() {
        assert_eq!(TransferOperation::Send, TransferOperation::Send);
        assert_ne!(TransferOperation::Send, TransferOperation::Recv);
    }

    #[test]
    fn transfer_operation_clone() {
        let op = TransferOperation::Recv;
        let cloned = op;
        assert_eq!(op, cloned);
    }

    #[test]
    fn transfer_operation_debug() {
        let debug = format!("{:?}", TransferOperation::Send);
        assert!(debug.contains("Send"));
    }

    // ==================== expand_log_format basic specifier tests ====================

    #[test]
    fn expand_operation_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%o", &ctx), "send");
    }

    #[test]
    fn expand_hostname_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%h", &ctx), "client.example.com");
    }

    #[test]
    fn expand_address_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%a", &ctx), "192.168.1.100");
    }

    #[test]
    fn expand_module_name_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%m", &ctx), "data");
    }

    #[test]
    fn expand_auth_user_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%u", &ctx), "alice");
    }

    #[test]
    fn expand_filename_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%f", &ctx), "docs/readme.txt");
    }

    #[test]
    fn expand_file_length_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%l", &ctx), "4096");
    }

    #[test]
    fn expand_pid_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%p", &ctx), "12345");
    }

    #[test]
    fn expand_module_path_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%P", &ctx), "/srv/data");
    }

    #[test]
    fn expand_timestamp_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%t", &ctx), "2024/01/15 10:30:00");
    }

    #[test]
    fn expand_bytes_transferred_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%b", &ctx), "2048");
    }

    #[test]
    fn expand_checksum_bytes_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%c", &ctx), "4096");
    }

    #[test]
    fn expand_itemize_specifier() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%i", &ctx), ">f..t......");
    }

    #[test]
    fn expand_literal_percent() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%%", &ctx), "%");
    }

    // ==================== expand_log_format combined tests ====================

    #[test]
    fn expand_default_format() {
        let ctx = make_context();
        let result = expand_log_format(DEFAULT_LOG_FORMAT, &ctx);
        assert_eq!(
            result,
            "send client.example.com [192.168.1.100] data (alice) docs/readme.txt 4096"
        );
    }

    #[test]
    fn expand_multiple_specifiers_mixed_with_text() {
        let ctx = make_context();
        assert_eq!(
            expand_log_format("[%p] %o %h %m/%f (%l bytes)", &ctx),
            "[12345] send client.example.com data/docs/readme.txt (4096 bytes)"
        );
    }

    #[test]
    fn expand_no_specifiers_returns_literal() {
        let ctx = make_context();
        assert_eq!(
            expand_log_format("plain text, no specifiers", &ctx),
            "plain text, no specifiers"
        );
    }

    #[test]
    fn expand_empty_format() {
        let ctx = make_context();
        assert_eq!(expand_log_format("", &ctx), "");
    }

    #[test]
    fn expand_unknown_specifier_dropped() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%z", &ctx), "");
    }

    #[test]
    fn expand_trailing_percent_dropped() {
        let ctx = make_context();
        assert_eq!(expand_log_format("end%", &ctx), "end");
    }

    #[test]
    fn expand_consecutive_specifiers() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%o%o%o", &ctx), "sendsendsend");
    }

    #[test]
    fn expand_all_specifiers() {
        let ctx = make_context();
        let result = expand_log_format("%o|%h|%a|%m|%u|%f|%l|%p|%P|%t|%b|%c|%i|%%", &ctx);
        assert_eq!(
            result,
            "send|client.example.com|192.168.1.100|data|alice|docs/readme.txt|4096|12345|/srv/data|2024/01/15 10:30:00|2048|4096|>f..t......|%"
        );
    }

    #[test]
    fn expand_recv_operation() {
        let mut ctx = make_context();
        ctx.operation = TransferOperation::Recv;
        assert_eq!(expand_log_format("%o", &ctx), "recv");
    }

    #[test]
    fn expand_empty_auth_user() {
        let mut ctx = make_context();
        ctx.auth_user = "";
        assert_eq!(expand_log_format("(%u)", &ctx), "()");
    }

    #[test]
    fn expand_ipv6_address() {
        let mut ctx = make_context();
        ctx.remote_address = IpAddr::V6("::1".parse().unwrap());
        assert_eq!(expand_log_format("%a", &ctx), "::1");
    }

    #[test]
    fn expand_zero_length_file() {
        let mut ctx = make_context();
        ctx.file_length = 0;
        assert_eq!(expand_log_format("%l", &ctx), "0");
    }

    #[test]
    fn expand_large_file_length() {
        let mut ctx = make_context();
        ctx.file_length = 1_099_511_627_776; // 1 TiB
        assert_eq!(expand_log_format("%l", &ctx), "1099511627776");
    }

    #[test]
    fn expand_percent_in_middle_of_text() {
        let ctx = make_context();
        assert_eq!(expand_log_format("100%% done", &ctx), "100% done");
    }

    #[test]
    fn expand_adjacent_percents() {
        let ctx = make_context();
        assert_eq!(expand_log_format("%%%%", &ctx), "%%");
    }

    // ==================== log_format_has tests ====================

    #[test]
    fn log_format_has_finds_specifier() {
        assert!(log_format_has("%o %h %f", 'o'));
        assert!(log_format_has("%o %h %f", 'h'));
        assert!(log_format_has("%o %h %f", 'f'));
    }

    #[test]
    fn log_format_has_rejects_absent() {
        assert!(!log_format_has("%o %h %f", 'i'));
        assert!(!log_format_has("%o %h %f", 'b'));
    }

    #[test]
    fn log_format_has_empty_format() {
        assert!(!log_format_has("", 'o'));
    }

    #[test]
    fn log_format_has_no_specifiers() {
        assert!(!log_format_has("plain text", 'o'));
    }

    #[test]
    fn log_format_has_literal_percent() {
        // `%%` should not match 'o' even if followed by 'o'
        assert!(!log_format_has("%%o", 'o'));
    }

    #[test]
    fn log_format_has_finds_percent_specifier() {
        assert!(log_format_has("%%", '%'));
    }

    #[test]
    fn log_format_has_trailing_percent() {
        assert!(!log_format_has("test%", 'o'));
    }

    // ==================== DEFAULT_LOG_FORMAT tests ====================

    #[test]
    fn default_log_format_contains_expected_specifiers() {
        assert!(log_format_has(DEFAULT_LOG_FORMAT, 'o'));
        assert!(log_format_has(DEFAULT_LOG_FORMAT, 'h'));
        assert!(log_format_has(DEFAULT_LOG_FORMAT, 'a'));
        assert!(log_format_has(DEFAULT_LOG_FORMAT, 'm'));
        assert!(log_format_has(DEFAULT_LOG_FORMAT, 'u'));
        assert!(log_format_has(DEFAULT_LOG_FORMAT, 'f'));
        assert!(log_format_has(DEFAULT_LOG_FORMAT, 'l'));
    }

    // ==================== LogFormatContext tests ====================

    #[test]
    fn log_format_context_debug() {
        let ctx = make_context();
        let debug = format!("{ctx:?}");
        assert!(debug.contains("LogFormatContext"));
        assert!(debug.contains("client.example.com"));
    }

    #[test]
    fn log_format_context_clone() {
        let ctx = make_context();
        let cloned = ctx.clone();
        assert_eq!(ctx.operation, cloned.operation);
        assert_eq!(ctx.hostname, cloned.hostname);
        assert_eq!(ctx.module_name, cloned.module_name);
        assert_eq!(ctx.filename, cloned.filename);
        assert_eq!(ctx.file_length, cloned.file_length);
    }
}
