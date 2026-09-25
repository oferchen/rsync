//! Batch mode support for client transfers.
//!
//! Handles both writing batch files during a transfer and replaying
//! previously recorded batch files. Mirrors upstream `main.c:read_batch()`
//! for replay and, for a local copy only, `main.c:374-383` for the stats
//! trailer.

use std::sync::{Arc, Mutex};

use engine::batch::{BatchConfig, BatchStats, BatchWriter};

use crate::message::Role;
use crate::rsync_error;

use super::super::config::{ClientConfig, FilterRuleKind, FilterRuleSpec};
use super::super::error::ClientError;
use super::super::remote;
use super::super::summary::ClientSummary;

/// Validates that `--read-batch` is not combined with remote destinations
/// and dispatches to [`replay_batch`] when in read mode.
///
/// Returns `Some(Ok(...))` for replay, `Some(Err(...))` for validation
/// failure, or `None` when the config is in write mode (caller should
/// proceed with the normal transfer).
pub(crate) fn handle_batch_read(
    batch_cfg: &BatchConfig,
    config: &ClientConfig,
) -> Option<Result<ClientSummary, ClientError>> {
    if !batch_cfg.is_read_mode() {
        return None;
    }

    // upstream: main.c:1500-1509 - reject remote destinations with --read-batch
    let has_remote_dest = config.transfer_args().iter().any(|arg| {
        let s = arg.to_string_lossy();
        s.starts_with("rsync://") || s.contains("::") || remote::operand_is_remote(arg)
    });
    if has_remote_dest {
        return Some(Err(ClientError::new(
            super::super::FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                super::super::FEATURE_UNAVAILABLE_EXIT_CODE,
                "remote destination is not allowed with --read-batch"
            )
            .with_role(Role::Client),
        )));
    }

    Some(replay_batch(batch_cfg, config))
}

/// Creates a [`BatchWriter`] for recording a transfer to a batch file.
pub(crate) fn create_batch_writer(
    batch_cfg: &BatchConfig,
) -> Result<Arc<Mutex<BatchWriter>>, ClientError> {
    match BatchWriter::new((*batch_cfg).clone()) {
        Ok(writer) => Ok(Arc::new(Mutex::new(writer))),
        Err(e) => {
            let msg = format!(
                "failed to create batch file '{}': {}",
                batch_cfg.batch_file_path().display(),
                e
            );
            Err(ClientError::new(
                1,
                rsync_error!(1, "{}", msg).with_role(Role::Client),
            ))
        }
    }
}

/// Builds the data-stream-affecting [`engine::batch::BatchFlags`] from the
/// active config.
///
/// The same flag set is recorded on `--write-batch` and reconciled on
/// `--read-batch`, so both paths derive it identically from the current
/// options. Mirrors upstream `batch.c:97-113 write_stream_flags()`.
fn config_batch_flags(config: &ClientConfig) -> engine::batch::BatchFlags {
    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = config.preserve_xattrs();
    #[cfg(not(all(unix, feature = "xattr")))]
    let preserve_xattrs = false;

    #[cfg(all(any(unix, windows), feature = "acl"))]
    let preserve_acls = config.preserve_acls();
    #[cfg(not(all(any(unix, windows), feature = "acl")))]
    let preserve_acls = false;

    engine::batch::BatchFlags {
        recurse: config.recursive(),
        preserve_uid: config.preserve_owner(),
        preserve_gid: config.preserve_group(),
        preserve_links: config.links(),
        preserve_devices: config.preserve_devices(),
        preserve_hard_links: config.preserve_hard_links(),
        always_checksum: config.checksum(),
        xfer_dirs: config.dirs(),
        // upstream: batch.c:68 - do_compression is bit 8 in stream flags, set
        // whenever compression is active (batch.c:96-113 write_stream_flags()
        // gates the bit on protocol >= 29, which BatchFlags::to_bitmap applies).
        // Upstream tees the raw wire bytes to batch_fd via
        // write_batch_monitor_in in io.c:read_buf(), so a batch recorded under
        // -z carries token.c:send_deflated_token() framing and the header must
        // advertise it. The codec is always zlib: compat.c:414 getenv_nstr()
        // pins the compression list to "zlib" while write_batch is set.
        do_compression: config.compress(),
        // upstream: batch.c:69,101-103 - bit 9 records tweaked_iconv
        // (iconv_opt != NULL). --no-iconv and an unset --iconv both leave
        // iconv_opt NULL, so only an explicit charset request sets the bit.
        iconv: !config.iconv().is_unspecified() && !config.iconv().is_disabled(),
        preserve_acls,
        preserve_xattrs,
        inplace: config.inplace(),
        append: config.append(),
        append_verify: config.append_verify(),
    }
}

/// Writes the batch header containing stream flags before the transfer begins.
pub(crate) fn write_batch_header(
    writer: &Arc<Mutex<BatchWriter>>,
    config: &ClientConfig,
) -> Result<(), ClientError> {
    let batch_flags = config_batch_flags(config);

    let mut w = writer.lock().map_err(|_| {
        ClientError::new(
            1,
            rsync_error!(1, "batch writer lock poisoned").with_role(Role::Client),
        )
    })?;
    if let Err(e) = w.write_header(batch_flags) {
        let msg = format!("failed to write batch header: {e}");
        return Err(ClientError::new(
            1,
            rsync_error!(1, "{}", msg).with_role(Role::Client),
        ));
    }

    Ok(())
}

/// Flushes the batch file and generates the replay script, optionally
/// appending the stats trailer first.
///
/// When filter rules are active in `config`, the replay script embeds them
/// using the same heredoc format as upstream `batch.c:write_filter_rules()`,
/// ensuring the replay applies identical filters.
///
/// `write_trailer` says whether this call owns the batch trailer - the five
/// varlong30 stats of upstream `main.c:374-383` plus the goodbye `NDX_DONE`.
/// Only the local-copy path does, because it has no protocol stream to record
/// from. Every remote transfer produces the trailer inside the transfer layer,
/// where upstream produces it too:
///
/// - PUSH: `handle_stats(-1)` writes the stats to `batch_fd` before
///   `read_final_goodbye()` tees the goodbye `NDX_DONE` (`main.c:1363-1365`).
/// - PULL: both the stats (`main.c:364-373`) and the goodbye `NDX_DONE`
///   (`main.c:917`) arrive over the wire, so the read tee records them.
///
/// Appending a second copy here would leave the batch with a trailer upstream
/// `--read-batch` cannot parse: its `read_final_goodbye()` reads one `NDX_DONE`
/// too many and aborts with `RERR_PROTOCOL`.
pub(crate) fn finalize_batch(
    writer_arc: &Arc<Mutex<BatchWriter>>,
    batch_cfg: &BatchConfig,
    config: &ClientConfig,
    summary: &ClientSummary,
    write_trailer: bool,
) -> Result<(), ClientError> {
    {
        let mut writer = writer_arc.lock().map_err(|_| {
            ClientError::new(
                1,
                rsync_error!(1, "batch writer lock poisoned").with_role(Role::Client),
            )
        })?;

        if write_trailer {
            // upstream: main.c:374-383 - write_varlong30(batch_fd, stats.total_read, 3)
            let proto = batch_cfg.protocol_version;
            let stats = BatchStats {
                total_read: summary.bytes_received() as i64,
                total_written: summary.bytes_sent() as i64,
                total_size: summary.total_source_bytes() as i64,
                flist_buildtime: if proto >= 29 {
                    Some(summary.file_list_generation_time().as_millis() as i64)
                } else {
                    None
                },
                flist_xfertime: if proto >= 29 {
                    Some(summary.file_list_transfer_time().as_millis() as i64)
                } else {
                    None
                },
            };
            if let Err(e) = writer.write_stats(&stats) {
                let msg = format!("failed to write batch stats: {e}");
                return Err(ClientError::new(
                    1,
                    rsync_error!(1, "{}", msg).with_role(Role::Client),
                ));
            }

            // upstream: main.c:920 - write_ndx(f_out, NDX_DONE) inside
            // read_final_goodbye() is the last thing a sender records, after
            // the stats. For protocol >= 30, NDX_DONE = 0x00 (single byte);
            // for protocol < 30 it is 0xFFFFFFFF (4 bytes).
            let goodbye_bytes: &[u8] = if proto >= 30 {
                &[0x00]
            } else {
                &[0xFF, 0xFF, 0xFF, 0xFF]
            };
            if let Err(e) = writer.write_data(goodbye_bytes) {
                let msg = format!("failed to write batch goodbye NDX_DONE: {e}");
                return Err(ClientError::new(
                    1,
                    rsync_error!(1, "{}", msg).with_role(Role::Client),
                ));
            }
        }

        if let Err(e) = writer.flush() {
            let msg = format!("failed to flush batch file: {e}");
            return Err(ClientError::new(
                1,
                rsync_error!(1, "{}", msg).with_role(Role::Client),
            ));
        }
    }

    // upstream: batch.c:305-306 - embed filter rules in the replay script
    let filter_text = serialize_filter_rules(config.filter_rules())?;
    let filter_opt = if filter_text.is_empty() {
        None
    } else {
        Some(filter_text.as_str())
    };

    // upstream: batch.c:300-304 - embed the destination operand as the
    // `${1:-<dest>}` fallback so `./BATCH.sh` (with no argument) writes to
    // the same destination used when the batch was captured. The destination
    // is the last positional operand on the original command line.
    let dest_operand = config
        .transfer_args()
        .last()
        .map(|s| s.to_string_lossy().into_owned());

    // upstream: batch.c:217,219-220 - the filter heredoc honors eol_nulls
    // (--from0), NUL-terminating rules and appending ";\n".
    let script_cfg = batch_cfg.clone().with_eol_nulls(config.from0());
    if let Err(e) = engine::batch::script::generate_script_with_filters(
        &script_cfg,
        filter_opt,
        dest_operand.as_deref(),
    ) {
        let msg = format!("failed to generate batch script: {e}");
        return Err(ClientError::new(
            1,
            rsync_error!(1, "{}", msg).with_role(Role::Client),
        ));
    }

    Ok(())
}

/// Serializes filter rules into the text format used by batch script heredocs.
///
/// Each rule is formatted as a single line matching upstream rsync's
/// `batch.c:write_filter_rules()` / `exclude.c:get_rule_prefix()` output:
///
/// ```text
/// {prefix} {pattern}[/]\n
/// ```
///
/// The prefix encodes the rule action (`+`/`-`/`P`/`R`/`:`) and modifier
/// flags (`s`/`r`/`p`/`x`/`!`). A trailing `/` is appended for
/// directory-only patterns. Returns an empty string when no rules are present.
///
/// A pattern containing a newline is refused rather than written. Each rule is
/// one here-doc line, so an embedded newline lets a crafted pattern - from a
/// dir-merge or `--exclude-from` file in an untrusted tree - forge the `#E#`
/// terminator on a line of its own and inject shell commands into the
/// generated replay script. Such a pattern also cannot round-trip a
/// line-delimited here-doc, so upstream fails closed and so does this.
///
/// This is the last layer that still sees rules individually; the script
/// emitter receives one flattened string, where a rule-internal newline is
/// indistinguishable from the separator between rules.
///
/// # Upstream Reference
///
/// - `batch.c:213-240`: `write_filter_rules()` iterates filter_list and, per
///   rule, `if (ent->pattern && strchr(ent->pattern, '\n'))` reports the error
///   below and calls `exit_cleanup(RERR_SYNTAX)`.
/// - `exclude.c:1525-1587`: `get_rule_prefix()` builds the prefix string
fn serialize_filter_rules(rules: &[FilterRuleSpec]) -> Result<String, ClientError> {
    if rules.is_empty() {
        return Ok(String::new());
    }

    let mut output = String::new();
    for rule in rules {
        // upstream: batch.c:222-231 - refuse a newline-bearing pattern before
        // any of it reaches the script.
        if rule.pattern().contains('\n') {
            let msg = "cannot write a filter rule containing a newline to the batch replay script";
            return Err(ClientError::new(
                1,
                rsync_error!(1, "{}", msg).with_role(Role::Client),
            ));
        }
        // upstream: exclude.c:1532-1541 - action prefix
        let action_char = match rule.kind() {
            FilterRuleKind::Include => '+',
            FilterRuleKind::Exclude | FilterRuleKind::ExcludeIfPresent => '-',
            FilterRuleKind::Protect => 'P',
            FilterRuleKind::Risk => 'R',
            FilterRuleKind::DirMerge => ':',
            FilterRuleKind::Clear => '!',
        };
        output.push(action_char);

        // upstream: exclude.c:1546-1547 - negate modifier
        if rule.is_negated() {
            output.push('!');
        }

        // upstream: exclude.c:1564-1565 - xattr modifier
        if rule.is_xattr_only() {
            output.push('x');
        }

        // upstream: exclude.c:1566-1572 - sender/receiver side modifiers
        if rule.applies_to_sender() && !rule.applies_to_receiver() {
            output.push('s');
        }
        if rule.applies_to_receiver() && !rule.applies_to_sender() {
            output.push('r');
        }

        // upstream: exclude.c:1573-1578 - perishable modifier
        if rule.is_perishable() {
            output.push('p');
        }

        // upstream: exclude.c:1581-1582 - space separator before pattern
        output.push(' ');

        // upstream: batch.c:213-214 - pattern text
        let pattern = rule.pattern();
        output.push_str(pattern);

        // upstream: batch.c:215-216 - trailing '/' for directory-only rules.
        // FilterRuleSpec stores the trailing '/' as part of the pattern text,
        // so we do not append an extra one.

        // upstream: batch.c:217 - newline terminator (non-null-terminated mode)
        output.push('\n');
    }

    Ok(output)
}

/// Replay a batch file by driving the real receiver pipeline over the
/// recorded stream.
///
/// Mirrors upstream `--read-batch`: the batch file becomes the receiving
/// client's `f_in` (`main.c:652-664`) and the ordinary receiver decodes the
/// recorded file list and delta stream (`main.c:1405 do_recv()`), with the
/// generator's consumer-less `f_out` swallowed by a discard sink. The
/// receiver's negotiated state (protocol, compat flags, checksum seed) is
/// pinned from the batch header instead of a live handshake
/// (`compat.c:604-613 setup_protocol()` under `read_batch`).
fn replay_batch(
    batch_cfg: &BatchConfig,
    config: &ClientConfig,
) -> Result<ClientSummary, ClientError> {
    // upstream: main.c:1538-1541 - with --read-batch no source is specified;
    // the destination is the last (and only counted) operand.
    let dest_root = config
        .transfer_args()
        .last()
        .cloned()
        .unwrap_or_else(|| std::ffi::OsString::from("."));

    // upstream: batch.c:120 check_batch_flags() reconciles the active options
    // against the batch header during replay, so carry the current flag state
    // into the reader.
    let active_flags = config_batch_flags(config);
    let replay_cfg = batch_cfg.clone().with_active_flags(active_flags);

    let mut reader = engine::batch::BatchReader::new(replay_cfg).map_err(map_batch_error)?;
    // upstream: main.c:1941-1942 read_stream_flags(batch_fd), then
    // compat.c:604-613 setup_protocol() reads protocol/compat/seed back from
    // the batch fd; the reader's single header parse covers both, and rejects
    // a too-new batch protocol with RERR_PROTOCOL (compat.c:609-613).
    let stream_flags = reader.read_header().map_err(map_batch_error)?;

    // upstream: compat.c:641-642 - setup_protocol() calls check_batch_flags()
    // under read_batch. Non-iconv mismatches are forced to the batch's value
    // and mentioned at --info=misc level; an --iconv mismatch is fatal
    // (RERR_SYNTAX, via the FlagMismatch arm of map_batch_error).
    let notices = engine::batch::check_batch_flags(
        stream_flags,
        active_flags,
        reader.config().protocol_version,
    )
    .map_err(map_batch_error)?;
    if config.verbosity() > 0 {
        for message in &notices {
            println!("{message}");
        }
    }

    let header = reader.header().cloned().ok_or_else(|| {
        ClientError::new(
            1,
            rsync_error!(1, "batch header missing after read").with_role(Role::Client),
        )
    })?;
    let body = reader.into_body().map_err(map_batch_error)?;

    let compat_flags = header
        .compat_flags
        .map(|bits| protocol::CompatibilityFlags::from_bits(bits as u32));
    let server_config = build_replay_server_config(config, dest_root, &stream_flags, compat_flags)?;

    let mut ctx = crate::server::ReceiverContext::for_batch_replay(&header, server_config)
        .map_err(|e| {
            // upstream: compat.c:625-638 - a protocol outside the supported
            // bounds aborts with exit_cleanup(RERR_PROTOCOL).
            ClientError::new(2, rsync_error!(2, "{}", e).with_role(Role::Client))
        })?;

    let start = std::time::Instant::now();
    let stats = ctx.run_local_replay(body, None).map_err(|e| {
        let msg = format!("batch replay failed: {e}");
        ClientError::new(1, rsync_error!(1, "{}", msg).with_role(Role::Client))
    })?;

    // upstream: main.c:362-373 - the --read-batch side surfaces the replayed
    // totals through output_summary(). The receiver ran for real, so reuse the
    // pull receiver's stats conversion (including the io_error -> exit-code
    // mapping of cleanup.c:210-218).
    let mut summary = remote::daemon_transfer::convert_server_stats_to_summary(
        crate::server::ServerStats::Receiver(stats),
        start.elapsed(),
    );

    // A custom `--out-format` made the replay receiver buffer one
    // metadata-bearing itemize event per transferred row instead of printing
    // its own line; hand each to the CLI's out-format renderer, exactly as the
    // daemon/SSH pull drivers drain their itemize sink. Empty for plain
    // `-v`/`-i` (those printed their own output during the replay).
    let mut itemize_sink = remote::itemize_sink::ItemizeEventSink::new(true);
    for row in ctx.drain_event_rows() {
        use crate::server::ItemizeCallback as _;
        itemize_sink.on_itemize_row(&row.as_row());
    }
    let events = itemize_sink.take_events();
    if !events.is_empty() {
        summary = summary.with_events(events);
    }

    Ok(summary)
}

/// Maps a batch open/header/reconcile error onto upstream's exit codes.
fn map_batch_error(e: engine::batch::BatchError) -> ClientError {
    match e {
        // upstream: batch.c:137-142 - an --iconv mismatch aborts with
        // RERR_SYNTAX (exit 1) printing the bare reconcile message.
        engine::batch::BatchError::FlagMismatch(msg) => {
            ClientError::new(1, rsync_error!(1, "{}", msg).with_role(Role::Client))
        }
        // upstream: compat.c:609-613 setup_protocol() - a batch recorded with
        // a protocol newer than this build supports aborts with
        // exit_cleanup(RERR_PROTOCOL) (exit 2), printing the bare "too new"
        // diagnostic rather than the generic replay-failure message. The
        // reader tags this case with protocol::ProtocolViolation; detect it
        // and mirror the exit code and message exactly.
        engine::batch::BatchError::Io(ref io_err)
            if io_err
                .get_ref()
                .is_some_and(|inner| inner.is::<protocol::ProtocolViolation>()) =>
        {
            ClientError::new(2, rsync_error!(2, "{}", io_err).with_role(Role::Client))
        }
        // upstream: batch.c:271,280 - a batch file that cannot be opened, or
        // that resolves to a non-regular node, aborts with
        // `exit_cleanup(RERR_FILEIO)` (exit 11) and prints the bare
        // `Batch file ...` line. The generic arm below would report exit 1
        // under a "batch replay failed" prefix, which names the wrong phase:
        // nothing was replayed, the input was refused.
        engine::batch::BatchError::BatchFileUnusable(ref msg) => {
            ClientError::new(11, rsync_error!(11, "{}", msg).with_role(Role::Client))
        }
        other => {
            let msg = format!("batch replay failed: {other}");
            ClientError::new(1, rsync_error!(1, "{}", msg).with_role(Role::Client))
        }
    }
}

/// Builds the receiver `ServerConfig` that drives a `--read-batch` replay.
///
/// Reuses the daemon-pull receiver builder - on a pull the local client IS
/// the receiver, exactly the position a batch replay is in - then overrides
/// what a recorded batch dictates:
///
/// - no daemon connection is involved (upstream: main.c:1538 read_batch
///   forces `local_server = 1`), and
/// - every data-stream-affecting option is forced to the batch's recorded
///   stream flag, mirroring upstream `batch.c:126-135` where
///   `check_batch_flags()` writes each recorded bit back onto the option
///   global before `do_recv()` decodes the stream.
fn build_replay_server_config(
    config: &ClientConfig,
    dest_root: std::ffi::OsString,
    stream_flags: &engine::batch::BatchFlags,
    compat_flags: Option<protocol::CompatibilityFlags>,
) -> Result<crate::server::ServerConfig, ClientError> {
    // upstream: main.c:1426 send_filter_list(read_batch ? -1 : f_out) - the
    // replay parses the local filter rules without a peer to send them to.
    let filter_rules =
        remote::flags::build_wire_format_rules(config.filter_rules(), config.delete_excluded())?;
    let mut server_config = remote::daemon_transfer::build_server_config_for_receiver(
        config,
        &[dest_root],
        filter_rules,
    )?;
    server_config.connection.is_daemon_connection = false;

    // upstream: main.c:652-654 - read_batch calls set_allow_inc_recurse() on the
    // invocation's own options, before compat.c:641 check_batch_flags() forces
    // them to the batch's, so evaluate before the stream-flag overrides below.
    if let Some(flags) = compat_flags {
        crate::server::setup::refuse_incompatible_inc_recurse(
            server_config.allows_inc_recurse(),
            flags,
            true,
        )
        .map_err(|e| {
            let code = crate::server::error::rerr_for_io_error(&e);
            ClientError::new(code, rsync_error!(code, "{}", e).with_role(Role::Client))
        })?;
    }

    // A custom `--out-format` makes the replay receiver buffer one
    // metadata-bearing itemize event per transferred row (it suppresses its own
    // stdout); `handle_batch_read` drains and renders these, mirroring the
    // daemon/SSH pull drivers. Plain `-v`/`-i` keep printing their own default
    // output because `render_out_format_locally()` is false for them.
    server_config.flags.info_flags.out_format_active = config.render_out_format_locally();

    // upstream: batch.c:126-135 - the recorded stream flags win over the
    // invocation's options; the stream was encoded under them.
    server_config.flags.recursive = stream_flags.recurse;
    server_config.flags.owner = stream_flags.preserve_uid;
    server_config.flags.group = stream_flags.preserve_gid;
    server_config.flags.links = stream_flags.preserve_links;
    server_config.flags.devices = stream_flags.preserve_devices;
    server_config.flags.hard_links = stream_flags.preserve_hard_links;
    server_config.flags.checksum = stream_flags.always_checksum;
    server_config.flags.dirs = stream_flags.xfer_dirs;
    server_config.flags.acls = stream_flags.preserve_acls;
    server_config.flags.xattrs = stream_flags.preserve_xattrs;
    server_config.write.inplace = stream_flags.inplace;
    server_config.flags.append = stream_flags.append;
    server_config.flags.append_verify = stream_flags.append_verify;
    // upstream: compat.c:414 getenv_nstr() pins a batch to zlib, which is
    // exactly the compact `-z` codec path; explicit choices do not apply.
    server_config.flags.compress = stream_flags.do_compression;
    server_config.connection.compress_choice = None;
    if server_config.flags.compress && server_config.connection.compression_level.is_none() {
        // upstream: options.c:2765-2768 - compress_level defaults to 6.
        server_config.connection.compression_level =
            Some(compress::zlib::CompressionLevel::Default);
    }
    Ok(server_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::batch::BatchMode;

    fn read_batch_config(proto: i32) -> BatchConfig {
        BatchConfig::new(BatchMode::Read, "test_batch".to_owned(), proto)
    }

    fn config_with_compress(compress: bool) -> ClientConfig {
        ClientConfig::builder().compress(compress).build()
    }

    #[test]
    fn read_batch_rejects_remote_destination() {
        let batch_cfg = read_batch_config(30);
        let config = ClientConfig::builder()
            .compress(false)
            .transfer_args(["rsync://host/mod/dest"])
            .build();
        let result = handle_batch_read(&batch_cfg, &config);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    /// A `--read-batch` file recorded with a protocol newer than this build
    /// supports must abort with `RERR_PROTOCOL` (exit 2), not the generic
    /// exit 1 replay-failure code.
    ///
    /// WHY: upstream compat.c:609-612 setup_protocol() prints "The protocol
    /// version in the batch file is too new (%d > %d)." and calls
    /// exit_cleanup(RERR_PROTOCOL). The reader tags this case as a
    /// `protocol::ProtocolViolation`; if the dispatch map_err collapsed it to
    /// exit 1 a caller would mistake a fundamental protocol incompatibility for
    /// a mere usage error. This pins the RERR_PROTOCOL mapping and the bare
    /// upstream diagnostic text.
    #[test]
    fn read_batch_from_newer_protocol_exits_rerr_protocol() {
        let temp = tempfile::TempDir::new().unwrap();
        let batch_path = temp.path().join("too_new.batch");
        let dest = temp.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        // Record a header stamped with protocol 33, one past the supported max.
        let write_cfg = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().into_owned(),
            33,
        );
        let mut writer = BatchWriter::new(write_cfg).unwrap();
        writer
            .write_header(engine::batch::BatchFlags::default())
            .unwrap();
        writer.finalize().unwrap();

        let read_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let config = ClientConfig::builder()
            .compress(false)
            .transfer_args([dest.to_string_lossy().to_string()])
            .build();

        let err = handle_batch_read(&read_cfg, &config)
            .expect("read mode handled")
            .expect_err("too-new batch must be rejected");
        assert_eq!(
            err.exit_code(),
            2,
            "too-new batch must exit RERR_PROTOCOL (2), got {}",
            err.exit_code()
        );
        assert!(
            err.to_string().contains("too new"),
            "expected upstream 'too new' diagnostic, got: {err}"
        );
    }

    /// Writes a header-only protocol-32 batch whose compat flags carry
    /// CF_INC_RECURSE.
    fn inc_recurse_batch(dir: &std::path::Path) -> BatchConfig {
        let path = dir.join("inc_recurse.batch").to_string_lossy().into_owned();
        let compat = protocol::CompatibilityFlags::INC_RECURSE
            | protocol::CompatibilityFlags::VARINT_FLIST_FLAGS;
        let write_cfg = BatchConfig::new(BatchMode::Write, path.clone(), 32)
            .with_compat_flags(compat.bits() as i32)
            .with_checksum_seed(1);
        let mut writer = BatchWriter::new(write_cfg).unwrap();
        writer
            .write_header(engine::batch::BatchFlags {
                recurse: true,
                ..Default::default()
            })
            .unwrap();
        writer.finalize().unwrap();
        BatchConfig::new(BatchMode::Read, path, 32)
    }

    /// upstream main.c:652-654 + compat.c:780-785: --read-batch evaluates
    /// set_allow_inc_recurse() on the invocation's own options, so replaying an
    /// inc-recursive batch with options that need the whole list (or without
    /// -r, which the batch's recurse bit does not rescue) aborts RERR_SYNTAX
    /// instead of mis-replaying the stream.
    #[test]
    fn read_batch_refuses_inc_recurse_its_options_disallow() {
        let temp = tempfile::TempDir::new().unwrap();
        let read_cfg = inc_recurse_batch(temp.path());
        let dest = temp.path().join("dest").to_string_lossy().into_owned();
        for (name, config) in [
            (
                "--delete-after",
                ClientConfig::builder()
                    .recursive(true)
                    .delete_after(true)
                    .transfer_args([dest.clone()])
                    .build(),
            ),
            (
                "no -r",
                ClientConfig::builder()
                    .recursive(false)
                    .transfer_args([dest.clone()])
                    .build(),
            ),
        ] {
            let err = handle_batch_read(&read_cfg, &config)
                .expect("read mode handled")
                .expect_err(name);
            assert_eq!(err.exit_code(), 1, "{name}: RERR_SYNTAX");
            assert!(
                err.to_string()
                    .contains("Incompatible options specified for inc-recursive batch file."),
                "{name}: {err}"
            );
        }

        // Opposed control: plain -r with --delete is allowed, so whatever the
        // header-only replay does next, it is not this refusal.
        let config = ClientConfig::builder()
            .recursive(true)
            .delete(true)
            .transfer_args([dest])
            .build();
        if let Err(err) = handle_batch_read(&read_cfg, &config).expect("read mode handled") {
            assert!(!err.to_string().contains("Incompatible options"), "{err}");
        }
    }

    /// Control: an in-range batch (protocol 32) round-trips through the same
    /// `handle_batch_read` dispatch and succeeds. This anchors the too-new
    /// rejection above - the RERR_PROTOCOL gate must fire ONLY when the
    /// recorded protocol exceeds the supported maximum, never for a batch this
    /// build can actually replay.
    #[test]
    fn read_batch_in_range_protocol_replays_successfully() {
        use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
        use protocol::CompatibilityFlags;

        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("src");
        let batch_path = temp.path().join("in_range.batch");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("hello.txt"), b"in-range batch payload").unwrap();

        // Record a valid protocol-32 batch via the --only-write-batch path,
        // mirroring the production compat flags the CLI assembles.
        let compat = CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::INPLACE_PARTIAL_DIR
            | CompatibilityFlags::VARINT_FLIST_FLAGS;
        let write_cfg = BatchConfig::new(
            BatchMode::OnlyWrite,
            batch_path.to_string_lossy().into_owned(),
            32,
        )
        .with_compat_flags(compat.bits() as i32)
        .with_checksum_seed(1);
        let writer = Arc::new(Mutex::new(BatchWriter::new(write_cfg).unwrap()));
        writer
            .lock()
            .unwrap()
            .write_header(engine::batch::BatchFlags {
                recurse: true,
                ..Default::default()
            })
            .unwrap();

        let options = LocalCopyOptions::default()
            .recursive(true)
            .batch_writer(Some(Arc::clone(&writer)));
        let mut src_os = source.clone().into_os_string();
        src_os.push("/");
        let operands = vec![src_os, temp.path().join("write_dest").into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        plan.execute_with_options(LocalCopyExecution::DryRun, options)
            .unwrap();
        {
            // Mirror finalize_batch's trailer: the five varlong30 stats
            // (upstream main.c:374-383) plus the goodbye NDX_DONE that
            // read_final_goodbye() consumes - the real receiver reads both.
            let mut w = writer.lock().unwrap();
            w.write_stats(&BatchStats {
                total_read: 0,
                total_written: 0,
                total_size: 22,
                flist_buildtime: Some(0),
                flist_xfertime: Some(0),
            })
            .unwrap();
            w.write_data(&[0x00]).unwrap();
        }
        Arc::try_unwrap(writer)
            .expect("writer uniquely owned")
            .into_inner()
            .unwrap()
            .finalize()
            .unwrap();

        // Replay through the production dispatch entry point.
        let replay_dest = temp.path().join("replay");
        std::fs::create_dir_all(&replay_dest).unwrap();
        let read_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let config = ClientConfig::builder()
            .compress(false)
            .transfer_args([replay_dest.to_string_lossy().to_string()])
            .build();
        handle_batch_read(&read_cfg, &config)
            .expect("read mode handled")
            .expect("in-range batch must replay successfully");
        assert_eq!(
            std::fs::read(replay_dest.join("hello.txt")).unwrap(),
            b"in-range batch payload",
            "in-range replay must materialise the source file"
        );
    }

    /// An `--iconv` mismatch between the batch header and the replay
    /// invocation must abort with RERR_SYNTAX (exit 1), printing upstream's
    /// bare reconcile message.
    ///
    /// WHY: upstream batch.c:137-142 - every other stream-flag mismatch is
    /// forced to the batch's value, but iconv changes the byte encoding of
    /// the recorded names themselves, so `check_batch_flags()` calls
    /// `exit_cleanup(RERR_SYNTAX)`. The dispatch (compat.c:641-642 runs the
    /// check inside setup_protocol under read_batch) now owns this call; a
    /// dispatch that skipped the reconcile would feed mis-encoded names to
    /// the receiver instead of failing loudly.
    #[test]
    fn read_batch_iconv_mismatch_exits_rerr_syntax() {
        let temp = tempfile::TempDir::new().unwrap();
        let batch_path = temp.path().join("iconv.batch");
        let dest = temp.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let write_cfg = BatchConfig::new(
            BatchMode::Write,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let mut writer = BatchWriter::new(write_cfg).unwrap();
        writer
            .write_header(engine::batch::BatchFlags {
                iconv: true,
                ..Default::default()
            })
            .unwrap();
        writer.finalize().unwrap();

        let read_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let config = ClientConfig::builder()
            .compress(false)
            .transfer_args([dest.to_string_lossy().to_string()])
            .build();

        let err = handle_batch_read(&read_cfg, &config)
            .expect("read mode handled")
            .expect_err("an --iconv mismatch must be fatal");
        assert_eq!(
            err.exit_code(),
            1,
            "iconv mismatch must exit RERR_SYNTAX (1), got {}",
            err.exit_code()
        );
        assert!(
            err.to_string().contains("--iconv"),
            "expected upstream's bare --iconv reconcile message, got: {err}"
        );
    }

    /// The replay summary reflects the REAL receiver's accounting: only the
    /// regular files the recorded stream transferred count as copied, never
    /// every flist entry.
    ///
    /// WHY: upstream counts `stats.xferred_files` per recorded transfer row
    /// (receiver.c:977) - directories and the root entry are itemize rows,
    /// not transfers. The native replay fork counted every flist entry as a
    /// transferred regular file; driving dispatch through the receiver
    /// pipeline is what fixes the breakdown, so this pins the dispatch route
    /// itself: re-routing to the native replay inflates the count and fails
    /// here.
    #[test]
    fn read_batch_summary_counts_only_transferred_files() {
        use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
        use protocol::CompatibilityFlags;

        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("src");
        let batch_path = temp.path().join("counts.batch");
        std::fs::create_dir_all(source.join("sub")).unwrap();
        std::fs::write(source.join("top.txt"), b"top payload").unwrap();
        std::fs::write(source.join("sub").join("inner.txt"), b"inner payload").unwrap();

        let compat = CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::INPLACE_PARTIAL_DIR
            | CompatibilityFlags::VARINT_FLIST_FLAGS;
        let write_cfg = BatchConfig::new(
            BatchMode::OnlyWrite,
            batch_path.to_string_lossy().into_owned(),
            32,
        )
        .with_compat_flags(compat.bits() as i32)
        .with_checksum_seed(1);
        let writer = Arc::new(Mutex::new(BatchWriter::new(write_cfg).unwrap()));
        writer
            .lock()
            .unwrap()
            .write_header(engine::batch::BatchFlags {
                recurse: true,
                ..Default::default()
            })
            .unwrap();

        let options = LocalCopyOptions::default()
            .recursive(true)
            .batch_writer(Some(Arc::clone(&writer)));
        let mut src_os = source.clone().into_os_string();
        src_os.push("/");
        let operands = vec![src_os, temp.path().join("write_dest").into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        plan.execute_with_options(LocalCopyExecution::DryRun, options)
            .unwrap();
        {
            let mut w = writer.lock().unwrap();
            w.write_stats(&BatchStats {
                total_read: 0,
                total_written: 0,
                total_size: 24,
                flist_buildtime: Some(0),
                flist_xfertime: Some(0),
            })
            .unwrap();
            w.write_data(&[0x00]).unwrap();
        }
        Arc::try_unwrap(writer)
            .expect("writer uniquely owned")
            .into_inner()
            .unwrap()
            .finalize()
            .unwrap();

        let replay_dest = temp.path().join("replay");
        std::fs::create_dir_all(&replay_dest).unwrap();
        let read_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let config = ClientConfig::builder()
            .compress(false)
            .transfer_args([replay_dest.to_string_lossy().to_string()])
            .build();
        let summary = handle_batch_read(&read_cfg, &config)
            .expect("read mode handled")
            .expect("batch must replay successfully");

        assert_eq!(
            std::fs::read(replay_dest.join("top.txt")).unwrap(),
            b"top payload"
        );
        assert_eq!(
            std::fs::read(replay_dest.join("sub").join("inner.txt")).unwrap(),
            b"inner payload"
        );
        // Flist carries ".", "sub", "top.txt", "sub/inner.txt" - only the two
        // regular files are recorded transfers (receiver.c:977).
        assert_eq!(
            summary.files_copied(),
            2,
            "only the recorded transfers count as copied, not every flist entry"
        );
    }

    /// Records a recursive protocol-32 batch for `source` into `batch_path`
    /// via the `--only-write-batch` local-copy path, then appends the stats
    /// trailer + goodbye the real replay receiver reads. Shared by the replay
    /// parity tests below so each varies only the replay-side config.
    fn record_recurse_batch(
        source: &std::path::Path,
        batch_path: &std::path::Path,
        dest: &std::path::Path,
    ) {
        use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
        use protocol::CompatibilityFlags;

        let compat = CompatibilityFlags::SAFE_FILE_LIST
            | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
            | CompatibilityFlags::CHECKSUM_SEED_FIX
            | CompatibilityFlags::INPLACE_PARTIAL_DIR
            | CompatibilityFlags::VARINT_FLIST_FLAGS;
        let write_cfg = BatchConfig::new(
            BatchMode::OnlyWrite,
            batch_path.to_string_lossy().into_owned(),
            32,
        )
        .with_compat_flags(compat.bits() as i32)
        .with_checksum_seed(1);
        let writer = Arc::new(Mutex::new(BatchWriter::new(write_cfg).unwrap()));
        writer
            .lock()
            .unwrap()
            .write_header(engine::batch::BatchFlags {
                recurse: true,
                ..Default::default()
            })
            .unwrap();

        let options = LocalCopyOptions::default()
            .recursive(true)
            .batch_writer(Some(Arc::clone(&writer)));
        let mut src_os = source.to_path_buf().into_os_string();
        src_os.push("/");
        let operands = vec![src_os, dest.to_path_buf().into_os_string()];
        let plan = LocalCopyPlan::from_operands(&operands).unwrap();
        plan.execute_with_options(LocalCopyExecution::DryRun, options)
            .unwrap();
        {
            let mut w = writer.lock().unwrap();
            w.write_stats(&BatchStats {
                total_read: 0,
                total_written: 0,
                total_size: 0,
                flist_buildtime: Some(0),
                flist_xfertime: Some(0),
            })
            .unwrap();
            w.write_data(&[0x00]).unwrap();
        }
        Arc::try_unwrap(writer)
            .expect("writer uniquely owned")
            .into_inner()
            .unwrap()
            .finalize()
            .unwrap();
    }

    /// `--read-batch --delete` must run the real receiver's delete pass, so an
    /// extraneous destination file is removed and the summary counts it.
    ///
    /// WHY: upstream generator.c:2753-2754 `do_delete_pass()` runs regardless of
    /// read_batch (the replaying generator forks and runs locally, main.c:652-664).
    /// The reunified replay drive skipped every delete site, so `--delete` was a
    /// silent no-op. The paired control (no `--delete`) proves the stale file
    /// survives without the flag, so the deletion is attributable to `--delete`
    /// and not to the transfer itself.
    #[test]
    fn read_batch_delete_removes_extraneous_dest_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("keep.txt"), b"keep payload").unwrap();
        let batch_path = temp.path().join("delete.batch");
        record_recurse_batch(&source, &batch_path, &temp.path().join("write_dest"));

        // Control: replay WITHOUT --delete leaves the extraneous file in place.
        let ctrl_dest = temp.path().join("ctrl");
        std::fs::create_dir_all(&ctrl_dest).unwrap();
        std::fs::write(ctrl_dest.join("stale.txt"), b"stale").unwrap();
        let ctrl_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let ctrl_config = ClientConfig::builder()
            .compress(false)
            .transfer_args([ctrl_dest.to_string_lossy().to_string()])
            .build();
        let ctrl_summary = handle_batch_read(&ctrl_cfg, &ctrl_config)
            .expect("read mode handled")
            .expect("control batch must replay");
        assert!(
            ctrl_dest.join("stale.txt").exists(),
            "without --delete the extraneous file must survive"
        );
        assert_eq!(ctrl_summary.items_deleted(), 0, "control deletes nothing");

        // Treatment: replay WITH --delete removes the extraneous file.
        let del_dest = temp.path().join("del");
        std::fs::create_dir_all(&del_dest).unwrap();
        std::fs::write(del_dest.join("stale.txt"), b"stale").unwrap();
        let del_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let del_config = ClientConfig::builder()
            .compress(false)
            .delete(true)
            .transfer_args([del_dest.to_string_lossy().to_string()])
            .build();
        let del_summary = handle_batch_read(&del_cfg, &del_config)
            .expect("read mode handled")
            .expect("delete batch must replay");
        assert_eq!(
            std::fs::read(del_dest.join("keep.txt")).unwrap(),
            b"keep payload",
            "the transferred file must still land"
        );
        assert!(
            !del_dest.join("stale.txt").exists(),
            "--read-batch --delete must remove the extraneous file"
        );
        assert_eq!(
            del_summary.items_deleted(),
            1,
            "the delete pass must count the extraneous file"
        );
    }

    /// `--read-batch -b` must preserve the pre-image at the backup name, exactly
    /// as the network receiver's commit tier does.
    ///
    /// WHY: upstream generator.c:2280-2288 resolves `get_backup_name` under
    /// read_batch + make_backups; the network commit (sync.rs) renames the
    /// existing file to its backup before the temp file lands, but the reunified
    /// replay drive reimplemented the commit inline and omitted the backup step.
    /// The paired control (no `-b`) proves the pre-image is otherwise lost, so the
    /// preserved backup is attributable to `-b`.
    #[test]
    fn read_batch_backup_preserves_pre_image() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("src");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("top.txt"), b"new payload").unwrap();
        let batch_path = temp.path().join("backup.batch");
        record_recurse_batch(&source, &batch_path, &temp.path().join("write_dest"));

        // Control: replay WITHOUT -b overwrites the pre-image, leaving no backup.
        let ctrl_dest = temp.path().join("ctrl");
        std::fs::create_dir_all(&ctrl_dest).unwrap();
        std::fs::write(ctrl_dest.join("top.txt"), b"old payload").unwrap();
        let ctrl_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let ctrl_config = ClientConfig::builder()
            .compress(false)
            .transfer_args([ctrl_dest.to_string_lossy().to_string()])
            .build();
        handle_batch_read(&ctrl_cfg, &ctrl_config)
            .expect("read mode handled")
            .expect("control batch must replay");
        assert_eq!(
            std::fs::read(ctrl_dest.join("top.txt")).unwrap(),
            b"new payload",
            "control overwrites the destination"
        );
        assert!(
            !ctrl_dest.join("top.txt~").exists(),
            "without -b there is no backup pre-image"
        );

        // Treatment: replay WITH -b keeps the pre-image at "top.txt~".
        let bak_dest = temp.path().join("bak");
        std::fs::create_dir_all(&bak_dest).unwrap();
        std::fs::write(bak_dest.join("top.txt"), b"old payload").unwrap();
        let bak_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let bak_config = ClientConfig::builder()
            .compress(false)
            .backup(true)
            .transfer_args([bak_dest.to_string_lossy().to_string()])
            .build();
        handle_batch_read(&bak_cfg, &bak_config)
            .expect("read mode handled")
            .expect("backup batch must replay");
        assert_eq!(
            std::fs::read(bak_dest.join("top.txt")).unwrap(),
            b"new payload",
            "the destination is updated to the new payload"
        );
        assert_eq!(
            std::fs::read(bak_dest.join("top.txt~")).unwrap(),
            b"old payload",
            "--read-batch -b must preserve the pre-image at the backup name"
        );
    }

    /// `--read-batch` with a custom `--out-format` must itemize each transferred
    /// row through the same owner the network receiver uses, so the CLI renders
    /// the template. The reunified replay drive force-cleared `out_format_active`
    /// and never itemized, so the rows were lost.
    ///
    /// WHY: upstream receiver.c:1290 `log_item(log_code, file, iflags, NULL)`
    /// runs per transferred row regardless of read_batch (generator.c:589's
    /// `!read_batch` guards only the wire itemize header, not the local log).
    /// The paired control (no out-format) proves no events surface without it,
    /// so the itemize rows are attributable to the out-format request.
    #[test]
    fn read_batch_out_format_itemizes_transferred_rows() {
        let temp = tempfile::TempDir::new().unwrap();
        let source = temp.path().join("src");
        std::fs::create_dir_all(source.join("sub")).unwrap();
        std::fs::write(source.join("top.txt"), b"top payload").unwrap();
        std::fs::write(source.join("sub").join("inner.txt"), b"inner payload").unwrap();
        let batch_path = temp.path().join("itemize.batch");
        record_recurse_batch(&source, &batch_path, &temp.path().join("write_dest"));

        // Control: replay WITHOUT a custom out-format buffers no itemize events.
        let ctrl_dest = temp.path().join("ctrl");
        std::fs::create_dir_all(&ctrl_dest).unwrap();
        let ctrl_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let ctrl_config = ClientConfig::builder()
            .compress(false)
            .transfer_args([ctrl_dest.to_string_lossy().to_string()])
            .build();
        let ctrl_summary = handle_batch_read(&ctrl_cfg, &ctrl_config)
            .expect("read mode handled")
            .expect("control batch must replay");
        assert!(
            ctrl_summary.events().is_empty(),
            "no custom out-format means no buffered itemize events"
        );

        // Treatment: a custom out-format collects one metadata event per
        // transferred regular file (the two files, never the dir/root rows).
        let fmt_dest = temp.path().join("fmt");
        std::fs::create_dir_all(&fmt_dest).unwrap();
        let fmt_cfg = BatchConfig::new(
            BatchMode::Read,
            batch_path.to_string_lossy().into_owned(),
            32,
        );
        let fmt_config = ClientConfig::builder()
            .compress(false)
            .render_out_format_locally(true)
            .transfer_args([fmt_dest.to_string_lossy().to_string()])
            .build();
        let fmt_summary = handle_batch_read(&fmt_cfg, &fmt_config)
            .expect("read mode handled")
            .expect("out-format batch must replay");
        let names: Vec<String> = fmt_summary
            .events()
            .iter()
            .map(|e| e.relative_path().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n == "top.txt"),
            "itemize events must cover top.txt, got {names:?}"
        );
        assert!(
            names.iter().any(|n| n == "sub/inner.txt"),
            "itemize events must cover sub/inner.txt, got {names:?}"
        );
        // Every transferred regular file itemizes as a received file (`>f...`),
        // exactly as the network receiver renders it; the row is driven by the
        // recorded stream, not the local plan.
        for name in ["top.txt", "sub/inner.txt"] {
            let event = fmt_summary
                .events()
                .iter()
                .find(|e| e.relative_path().to_string_lossy() == name)
                .expect("file event present");
            let itemize = event
                .itemize_override()
                .expect("a remote itemize event carries its rendered flags");
            assert!(
                itemize.starts_with(">f"),
                "{name} must itemize as a received regular file, got {itemize:?}"
            );
        }
    }

    #[test]
    fn write_batch_skips_read_handling() {
        let batch_cfg = BatchConfig::new(BatchMode::Write, "test_batch".to_owned(), 30);
        let config = config_with_compress(false);
        assert!(handle_batch_read(&batch_cfg, &config).is_none());
    }

    /// Reads back the stream flags recorded by `write_batch_header`.
    fn recorded_flags(compress: bool, proto: i32) -> engine::batch::BatchFlags {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("test.batch");
        let batch_cfg =
            BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), proto)
                .with_checksum_seed(1);

        let writer_arc = create_batch_writer(&batch_cfg).unwrap();
        write_batch_header(&writer_arc, &config_with_compress(compress)).unwrap();
        drop(writer_arc);

        let read_cfg = BatchConfig::new(BatchMode::Read, path.to_string_lossy().to_string(), proto);
        let mut reader = engine::batch::BatchReader::new(read_cfg).unwrap();
        reader.read_header().unwrap()
    }

    /// upstream: batch.c:68 `&do_compression` occupies stream-flag bit 8 and
    /// batch.c:96-113 `write_stream_flags()` sets it whenever the option is
    /// active. A batch recorded under `-z` therefore has to advertise the bit
    /// so `--read-batch` decodes the deflated tokens instead of plain ones.
    #[test]
    fn write_batch_header_sets_do_compression_under_compress() {
        assert!(recorded_flags(true, 31).do_compression);
    }

    /// upstream: batch.c:96-113 - a flag that is off contributes no bit, so a
    /// batch written without `-z` keeps bit 8 clear and its body stays in
    /// `token.c:simple_send_token()` framing.
    #[test]
    fn write_batch_header_leaves_do_compression_clear_without_compress() {
        assert!(!recorded_flags(false, 31).do_compression);
    }

    /// upstream: batch.c:124-125 - `flag_ptr[7]` is NULL below protocol 29, so
    /// bits 7 and 8 do not exist in a proto-28 batch even with `-z` active.
    #[test]
    fn write_batch_header_omits_do_compression_below_protocol_29() {
        assert!(!recorded_flags(true, 28).do_compression);
    }

    #[test]
    fn serialize_empty_rules_returns_empty_string() {
        assert_eq!(serialize_filter_rules(&[]).expect("no rules to reject"), "");
    }

    #[test]
    fn serialize_exclude_rule() {
        let rules = [FilterRuleSpec::exclude("*.tmp")];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "- *.tmp\n");
    }

    #[test]
    fn serialize_include_rule() {
        let rules = [FilterRuleSpec::include("*.rs")];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "+ *.rs\n");
    }

    #[test]
    fn serialize_protect_rule() {
        let rules = [FilterRuleSpec::protect("/data")];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        // upstream: protect is 'P', receiver-only gets 'r' modifier
        assert_eq!(output, "Pr /data\n");
    }

    #[test]
    fn serialize_risk_rule() {
        let rules = [FilterRuleSpec::risk("/temp")];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        // upstream: risk is 'R', receiver-only gets 'r' modifier
        assert_eq!(output, "Rr /temp\n");
    }

    #[test]
    fn serialize_clear_rule() {
        let rules = [FilterRuleSpec::clear()];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "! \n");
    }

    #[test]
    fn serialize_multiple_rules() {
        let rules = [
            FilterRuleSpec::exclude("*.tmp"),
            FilterRuleSpec::include("*/"),
            FilterRuleSpec::include("*.txt"),
            FilterRuleSpec::exclude("*"),
        ];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "- *.tmp\n+ */\n+ *.txt\n- *\n");
    }

    #[test]
    fn serialize_sender_only_rule() {
        let rules = [FilterRuleSpec::hide("*.bak")];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        // upstream: sender-only gets 's' modifier
        assert_eq!(output, "-s *.bak\n");
    }

    #[test]
    fn serialize_perishable_rule() {
        let rules = [FilterRuleSpec::exclude("*.tmp").with_perishable(true)];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "-p *.tmp\n");
    }

    #[test]
    fn serialize_xattr_only_rule() {
        let rules = [FilterRuleSpec::exclude("user.*").with_xattr_only(true)];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "-x user.*\n");
    }

    #[test]
    fn serialize_negated_rule() {
        let rules = [FilterRuleSpec::exclude("*.txt").with_negate(true)];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "-! *.txt\n");
    }

    #[test]
    fn serialize_directory_only_pattern() {
        // FilterRuleSpec stores the trailing '/' as part of the pattern
        let rules = [FilterRuleSpec::exclude("build/")];
        let output = serialize_filter_rules(&rules).expect("rules must serialize");
        assert_eq!(output, "- build/\n");
    }

    /// Full round-trip: serialize rules, embed in batch script, verify output.
    #[test]
    fn serialize_and_embed_in_batch_script() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("roundtrip.batch");
        let batch_cfg = BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), 31)
            .with_checksum_seed(1);

        let rules = [
            FilterRuleSpec::exclude("*.tmp"),
            FilterRuleSpec::include("*/"),
            FilterRuleSpec::include("*.txt"),
            FilterRuleSpec::exclude("*"),
        ];
        let filter_text = serialize_filter_rules(&rules).expect("rules must serialize");

        let result = engine::batch::script::generate_script_with_filters(
            &batch_cfg,
            Some(&filter_text),
            None,
        );
        assert!(result.is_ok());

        let script_path = batch_cfg.script_file_path();
        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--filter='._-'"),
            "Script must include --filter='._-' for protocol >= 29: {content}"
        );
        assert!(content.contains("<<'#E#'"));
        assert!(content.contains("- *.tmp\n+ */\n+ *.txt\n- *\n"));
        assert!(content.contains("#E#"));
        assert!(content.contains("--read-batch="));
    }

    /// Verify finalize_batch embeds filter rules from config.
    #[test]
    fn finalize_batch_embeds_filter_rules_in_script() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("finalize.batch");
        let batch_cfg = BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), 31)
            .with_checksum_seed(1);

        let writer_arc = create_batch_writer(&batch_cfg).unwrap();

        let config = ClientConfig::builder()
            .compress(false)
            .add_filter_rule(FilterRuleSpec::exclude("*.log"))
            .add_filter_rule(FilterRuleSpec::include("*.txt"))
            .batch_config(Some(batch_cfg.clone()))
            .build();

        write_batch_header(&writer_arc, &config).unwrap();

        let summary = ClientSummary::from_summary(engine::local_copy::LocalCopySummary::default());
        let result = finalize_batch(&writer_arc, &batch_cfg, &config, &summary, true);
        assert!(result.is_ok());

        let script_path = batch_cfg.script_file_path();
        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            content.contains("--filter='._-'"),
            "Script should embed filter option: {content}"
        );
        assert!(
            content.contains("- *.log"),
            "Script should contain exclude rule: {content}"
        );
        assert!(
            content.contains("+ *.txt"),
            "Script should contain include rule: {content}"
        );
        assert!(content.contains("<<'#E#'"), "Script should contain heredoc");
    }

    /// A filter pattern carrying a newline must abort `--write-batch` with
    /// upstream's RERR_SYNTAX refusal and leave no replay script behind.
    ///
    /// Each rule is one line of the `<<'#E#'` here-doc, so a pattern such as
    /// `"x\n#E#\ntouch PWNED"` - reachable from a dir-merge or `--exclude-from`
    /// file in an untrusted tree - would end the here-doc early and append a
    /// shell command that runs when the operator executes `BATCH.sh`. The rule
    /// sits after a benign one so a check that only inspects the first rule, or
    /// writes the good rules before refusing, is caught too.
    ///
    /// upstream: batch.c:222-231 write_filter_rules() - rprintf + RERR_SYNTAX.
    #[test]
    fn finalize_batch_refuses_newline_filter_rule_and_writes_no_script() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("inject.batch");
        let batch_cfg = BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), 31)
            .with_checksum_seed(1);

        let writer_arc = create_batch_writer(&batch_cfg).unwrap();

        let config = ClientConfig::builder()
            .compress(false)
            .add_filter_rule(FilterRuleSpec::exclude("*.log"))
            .add_filter_rule(FilterRuleSpec::exclude("x\n#E#\ntouch PWNED"))
            .batch_config(Some(batch_cfg.clone()))
            .build();

        write_batch_header(&writer_arc, &config).unwrap();

        let summary = ClientSummary::from_summary(engine::local_copy::LocalCopySummary::default());
        let err = finalize_batch(&writer_arc, &batch_cfg, &config, &summary, true)
            .expect_err("a newline-bearing filter pattern must be refused");

        assert_eq!(err.exit_code(), 1, "upstream exits RERR_SYNTAX (1)");
        let rendered = err.message().to_string();
        assert!(
            rendered.contains(
                "cannot write a filter rule containing a newline to the batch replay script"
            ),
            "refusal must carry upstream's text: {rendered}"
        );
        assert!(
            !std::path::Path::new(&batch_cfg.script_file_path()).exists(),
            "no replay script may be written once the rule set is refused"
        );
    }

    /// Verify finalize_batch produces clean script when no filter rules.
    #[test]
    fn finalize_batch_no_filters_produces_clean_script() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("nofilt.batch");
        let batch_cfg = BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), 31)
            .with_checksum_seed(1);

        let writer_arc = create_batch_writer(&batch_cfg).unwrap();

        let config = config_with_compress(false);
        write_batch_header(&writer_arc, &config).unwrap();

        let summary = ClientSummary::from_summary(engine::local_copy::LocalCopySummary::default());
        let result = finalize_batch(&writer_arc, &batch_cfg, &config, &summary, true);
        assert!(result.is_ok());

        let script_path = batch_cfg.script_file_path();
        let content = std::fs::read_to_string(&script_path).unwrap();
        assert!(
            !content.contains("--filter"),
            "No --filter without rules: {content}"
        );
        assert!(
            !content.contains("#E#"),
            "No heredoc without rules: {content}"
        );
    }
}
