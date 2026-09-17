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

/// Appends the decimal representation of a `u32` to a string.
fn push_u32(buf: &mut String, value: u32) {
    use std::fmt::Write as _;
    let _ = write!(buf, "{value}");
}

/// Upper bound on the modifier run scanned before an escape letter.
///
/// upstream: `log.c:568` bounds the width-digit scan with
/// `c - fmt < sizeof fmt - 8` and `LOG_FMT_SIZE == 32` (log.c:532); the
/// matching `log_format_has()` scan repeats it (log.c:847). The CLI
/// `--out-format` parser applies the same `LOG_FMT_SIZE - 8` cap, so the
/// daemon and client agree on where the escape letter falls.
const LOG_FMT_MODIFIER_RUN: usize = 32 - 8;

/// Maximum field width honoured by a log-format escape.
///
/// Matches the CLI `--out-format` renderer's `MAX_PLACEHOLDER_WIDTH` so the two
/// expanders pad identically.
const LOG_FMT_MAX_WIDTH: usize = 4096;

/// Humanization selected by apostrophes in a log-format escape.
///
/// upstream: `log.c:562-573` counts the `'` characters before and after the
/// width digits and passes the total to `do_big_num()` as its `human_flag`
/// (log.c:596/713). `lib/compat.c:170` reads that flag: one apostrophe groups
/// the digits with a separator, two request decimal (K/M/G/T/P, base 1000)
/// units, and three or more request binary (base 1024) units.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LogFmtHumanize {
    /// Plain decimal digits (`do_big_num` `human_flag == 0`).
    None,
    /// Thousands separator only (`human_flag == 1`).
    Separator,
    /// Decimal unit suffixes, base 1000 (`human_flag == 2`).
    DecimalUnits,
    /// Binary unit suffixes, base 1024 (`human_flag >= 3`).
    BinaryUnits,
}

/// Width, alignment, and humanize modifiers preceding a log-format escape.
///
/// upstream: `log.c:558-576` -- the `'`/`-`/digit run that fills the `fmt`
/// scratch buffer before the escape letter is read.
struct LogFmtSpec {
    /// Minimum field width, capped at [`LOG_FMT_MAX_WIDTH`]; `None` when absent.
    width: Option<usize>,
    /// Left-justify the value (upstream `-` flag) instead of right-justifying.
    left_align: bool,
    /// Humanization mode for numeric escapes.
    humanize: LogFmtHumanize,
}

/// Scans the optional `'`/`-`/digit modifiers preceding an escape letter.
///
/// Every consumed character is appended to `raw` so the caller can replay an
/// unrecognized escape verbatim, which is how upstream leaves an unknown `%`
/// code in the output buffer untouched (log.c:791 `if (!n) continue;`).
///
/// upstream: `log.c:562-573` -- leading apostrophes, an optional `-`, a
/// bounded digit run, then trailing apostrophes.
fn parse_log_fmt_spec(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    raw: &mut String,
) -> LogFmtSpec {
    let mut apostrophes = 0usize;
    while chars.peek() == Some(&'\'') {
        apostrophes += 1;
        if let Some(c) = chars.next() {
            raw.push(c);
        }
    }

    // Mirrors upstream's `c = fmt + 1`: the escape letter occupies the first
    // scratch slot, so the modifier budget starts at one.
    let mut consumed = 1usize;

    let mut left_align = false;
    if chars.peek() == Some(&'-') {
        left_align = true;
        if let Some(c) = chars.next() {
            raw.push(c);
        }
        consumed += 1;
    }

    let mut width_value = 0usize;
    let mut saw_width = false;
    while consumed < LOG_FMT_MODIFIER_RUN {
        let Some(&peeked) = chars.peek() else { break };
        let Some(digit) = peeked.to_digit(10) else {
            break;
        };
        saw_width = true;
        width_value = width_value
            .saturating_mul(10)
            .saturating_add(digit as usize);
        chars.next();
        raw.push(peeked);
        consumed += 1;
    }

    while chars.peek() == Some(&'\'') {
        apostrophes += 1;
        if let Some(c) = chars.next() {
            raw.push(c);
        }
    }

    let width = saw_width.then(|| width_value.min(LOG_FMT_MAX_WIDTH));
    let humanize = match apostrophes {
        0 => LogFmtHumanize::None,
        1 => LogFmtHumanize::Separator,
        2 => LogFmtHumanize::DecimalUnits,
        _ => LogFmtHumanize::BinaryUnits,
    };

    LogFmtSpec {
        width,
        left_align,
        humanize,
    }
}

/// Appends `value` to `out`, applying the spec's width and alignment.
///
/// Width is measured in characters (the escaping sink later renders any
/// non-printable byte as `\#ooo`, so the values reaching here are effectively
/// ASCII). upstream: `log.c:793-797` runs the value through `snprintf` with the
/// scratch `%[-][width]s` only when a modifier is present, so a bare escape is
/// emitted unpadded.
fn pad_field(out: &mut String, value: &str, spec: &LogFmtSpec) {
    let Some(width) = spec.width else {
        out.push_str(value);
        return;
    };
    let len = value.chars().count();
    if len >= width {
        out.push_str(value);
    } else if spec.left_align {
        out.push_str(value);
        out.extend(std::iter::repeat_n(' ', width - len));
    } else {
        out.extend(std::iter::repeat_n(' ', width - len));
        out.push_str(value);
    }
}

/// Renders a numeric escape value under the humanize mode.
///
/// upstream: `log.c:596/713` pass the value to `do_big_num()`
/// (`lib/compat.c:170`). The daemon fields are non-negative counts, so the
/// negative-number handling upstream carries for signed contexts never applies.
fn render_num(value: u64, humanize: LogFmtHumanize) -> String {
    match humanize {
        LogFmtHumanize::None => value.to_string(),
        LogFmtHumanize::Separator => group_thousands(value),
        LogFmtHumanize::DecimalUnits => {
            humanize_units(value, 1000).unwrap_or_else(|| group_thousands(value))
        }
        LogFmtHumanize::BinaryUnits => {
            humanize_units(value, 1024).unwrap_or_else(|| group_thousands(value))
        }
    }
}

/// Groups a value's digits in threes with a comma separator.
///
/// upstream: `lib/compat.c:230-238` inserts `number_separator` every third
/// digit; the CLI renderer hard-codes `,`, so the daemon matches it.
fn group_thousands(value: u64) -> String {
    use std::fmt::Write as _;

    if value == 0 {
        return "0".to_owned();
    }

    let mut groups = Vec::new();
    let mut remaining = value;
    while remaining > 0 {
        groups.push((remaining % 1000) as u16);
        remaining /= 1000;
    }

    let mut rendered = String::new();
    if let Some(most_significant) = groups.pop() {
        rendered.push_str(&most_significant.to_string());
    }
    for group in groups.iter().rev() {
        rendered.push(',');
        let _ = write!(rendered, "{group:03}");
    }
    rendered
}

/// Renders a value with K/M/G/T/P unit suffixes, or `None` below `base`.
///
/// upstream: `lib/compat.c:181-204` divides by the multiplier until the
/// magnitude fits, formatting with two fractional digits.
fn humanize_units(value: u64, base: u64) -> Option<String> {
    if value < base {
        return None;
    }
    let base_f = base as f64;
    let mut magnitude = value as f64 / base_f;
    const UNITS: [char; 5] = ['K', 'M', 'G', 'T', 'P'];
    let mut units = 'P';
    for (index, candidate) in UNITS.iter().enumerate() {
        units = *candidate;
        if magnitude < base_f || index == UNITS.len() - 1 {
            break;
        }
        magnitude /= base_f;
    }
    Some(format!("{magnitude:.2}{units}"))
}

/// Expands a log format string using the provided context.
///
/// Processes each `%X` escape by substituting the corresponding field from
/// `ctx`, honouring the optional `'` (humanize), `-` (left-align), and width
/// modifiers that may precede the escape letter. Unknown escapes are passed
/// through verbatim, together with any modifiers scanned before them. A literal
/// `%%` produces a single `%` in the output.
///
/// Upstream: `log.c:log_formatted()` -- iterates over the format string,
/// scanning the `'`/`-`/digit modifier run (log.c:558-576) before expanding
/// each percent-escape from global and per-file state.
fn expand_log_format(format: &str, ctx: &LogFormatContext<'_>) -> String {
    let mut result = String::with_capacity(format.len() * 2);
    let mut chars = format.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '%' {
            result.push(ch);
            continue;
        }

        // Remember the raw characters (`%` plus modifiers) so an unrecognized
        // escape - or a `%` with no following letter - is replayed verbatim.
        let mut raw = String::from("%");
        let spec = parse_log_fmt_spec(&mut chars, &mut raw);

        let Some(letter) = chars.next() else {
            result.push_str(&raw);
            break;
        };
        raw.push(letter);

        match letter {
            'o' => pad_field(&mut result, ctx.operation.as_str(), &spec),
            'h' => pad_field(&mut result, ctx.hostname, &spec),
            'a' => pad_field(&mut result, ctx.remote_addr, &spec),
            'm' => pad_field(&mut result, ctx.module_name, &spec),
            'u' => pad_field(&mut result, ctx.username, &spec),
            'f' => pad_field(&mut result, ctx.filename, &spec),
            'P' => pad_field(&mut result, ctx.module_path, &spec),
            't' => pad_field(&mut result, ctx.timestamp, &spec),
            'i' => pad_field(&mut result, ctx.itemize_string, &spec),
            'l' => pad_field(
                &mut result,
                &render_num(ctx.file_length, spec.humanize),
                &spec,
            ),
            'b' => pad_field(
                &mut result,
                &render_num(ctx.bytes_transferred, spec.humanize),
                &spec,
            ),
            'c' => pad_field(
                &mut result,
                &render_num(ctx.bytes_checksumed, spec.humanize),
                &spec,
            ),
            // upstream renders `%p` with a plain `%d` (log.c:617), so the pid is
            // width/alignment-formatted but never unit-humanized.
            'p' => pad_field(&mut result, &ctx.pid.to_string(), &spec),
            '%' => result.push('%'),
            _ => result.push_str(&raw),
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

    #[test]
    fn default_log_format_matches_upstream() {
        assert_eq!(DEFAULT_LOG_FORMAT, "%o %h [%a] %m (%u) %f %l");
    }

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

    #[test]
    fn expand_default_format() {
        let ctx = sample_context();
        let result = expand_log_format(DEFAULT_LOG_FORMAT, &ctx);
        assert_eq!(
            result,
            "send client.example.com [192.168.1.100] backup (alice) docs/report.pdf 1048576"
        );
    }

    #[test]
    fn expand_recv_operation() {
        let mut ctx = sample_context();
        ctx.operation = TransferOperation::Recv;
        let result = expand_log_format("%o", &ctx);
        assert_eq!(result, "recv");
    }

    #[test]
    fn expand_empty_format() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("", &ctx), "");
    }

    #[test]
    fn expand_no_escapes() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("plain text", &ctx), "plain text");
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
        assert_eq!(expand_log_format("%l", &ctx), u64::MAX.to_string());
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

    // --- Modifier scan (upstream log.c:558-576) --------------------------
    //
    // These pin WHY the daemon must scan the `'`/`-`/width run: upstream's
    // `log_formatted()` renders `%-15m` as a left-justified field, so a daemon
    // that treated the modifiers as literal text would write a byte-divergent
    // `log file` line for any admin who configures a padded `log format`.

    #[test]
    fn width_right_justifies_a_string_escape() {
        let ctx = sample_context();
        // upstream: log.c:793-797 snprintf("%15s", "backup") -> 9 leading spaces.
        assert_eq!(expand_log_format("%15m", &ctx), "         backup");
    }

    #[test]
    fn dash_left_justifies_a_string_escape() {
        let ctx = sample_context();
        // upstream: log.c:566 records the `-`; snprintf("%-15s", "backup").
        assert_eq!(expand_log_format("%-15m", &ctx), "backup         ");
    }

    #[test]
    fn width_right_justifies_a_numeric_escape() {
        let mut ctx = sample_context();
        ctx.file_length = 42;
        assert_eq!(expand_log_format("%8l", &ctx), "      42");
    }

    #[test]
    fn value_wider_than_field_is_left_intact() {
        let ctx = sample_context();
        // "backup" is 6 chars; a width of 3 cannot shrink it (upstream pads a
        // minimum width, it never truncates).
        assert_eq!(expand_log_format("%3m", &ctx), "backup");
    }

    // Non-vacuity control: with NO modifiers the escape must expand exactly as
    // before, so the modifier scan cannot be a no-op that "passes" by never
    // padding anything.
    #[test]
    fn no_modifier_leaves_the_value_unpadded() {
        let ctx = sample_context();
        assert_eq!(expand_log_format("%m", &ctx), "backup");
    }

    // --- Humanize scan (upstream log.c:562-573 -> do_big_num) -------------

    #[test]
    fn single_apostrophe_groups_thousands() {
        let mut ctx = sample_context();
        ctx.file_length = 1_234_567;
        // upstream: do_big_num(n, 1, NULL) -> comma-grouped digits.
        assert_eq!(expand_log_format("%'l", &ctx), "1,234,567");
    }

    #[test]
    fn double_apostrophe_uses_decimal_units() {
        let mut ctx = sample_context();
        ctx.file_length = 1_500_000;
        // upstream: do_big_num(n, 2, NULL) -> base-1000 K/M/G suffixes.
        assert_eq!(expand_log_format("%''l", &ctx), "1.50M");
    }

    #[test]
    fn triple_apostrophe_uses_binary_units() {
        let mut ctx = sample_context();
        ctx.file_length = 1_048_576;
        // upstream: do_big_num(n, 3, NULL) -> base-1024 K/M/G suffixes.
        assert_eq!(expand_log_format("%'''l", &ctx), "1.00M");
    }

    #[test]
    fn humanize_combines_with_width_and_alignment() {
        let mut ctx = sample_context();
        ctx.file_length = 1_234_567;
        assert_eq!(expand_log_format("%-12'l", &ctx), "1,234,567   ");
    }

    // Non-vacuity control: a numeric escape WITHOUT apostrophes must stay a
    // plain decimal (and full u64 range, since these fields are unsigned).
    #[test]
    fn numeric_without_apostrophe_stays_plain_decimal() {
        let mut ctx = sample_context();
        ctx.file_length = 1_234_567;
        assert_eq!(expand_log_format("%l", &ctx), "1234567");
    }

    #[test]
    fn pid_is_width_formatted_but_never_unit_humanized() {
        let mut ctx = sample_context();
        ctx.pid = 12345;
        // upstream renders %p with `%d` (log.c:617): width applies, units do not.
        assert_eq!(expand_log_format("%8p", &ctx), "   12345");
        assert_eq!(expand_log_format("%''p", &ctx), "12345");
    }

    #[test]
    fn unknown_escape_replays_scanned_modifiers_verbatim() {
        let ctx = sample_context();
        // upstream leaves an unknown `%` code (with its modifiers) in place.
        assert_eq!(expand_log_format("%-15Z", &ctx), "%-15Z");
    }

    #[test]
    fn group_thousands_boundaries() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn humanize_units_below_base_is_none() {
        assert_eq!(humanize_units(999, 1000), None);
        assert_eq!(humanize_units(1023, 1024), None);
    }
}
