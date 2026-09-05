//! Batch mode support for client transfers.
//!
//! Handles both writing batch files during a transfer and replaying
//! previously recorded batch files. Mirrors upstream `main.c:read_batch()`
//! for replay and, for a local copy only, `main.c:374-383` for the stats
//! trailer.

use std::path::PathBuf;
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

    // upstream: main.c:1482-1491 - reject remote destinations with --read-batch
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
///   `read_final_goodbye()` tees the goodbye `NDX_DONE` (`main.c:1345-1347`).
/// - PULL: both the stats (`main.c:364-373`) and the goodbye `NDX_DONE`
///   (`main.c:904`) arrive over the wire, so the read tee records them.
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

            // upstream: main.c:907 - write_ndx(f_out, NDX_DONE) inside
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

/// Replay a batch file to reconstruct the transfer at the destination.
///
/// Delegates to [`engine::batch::replay::replay`] for the actual delta-application
/// logic, then wraps the result in a [`ClientSummary`].
fn replay_batch(
    batch_cfg: &BatchConfig,
    config: &ClientConfig,
) -> Result<ClientSummary, ClientError> {
    // upstream: main.c - with --read-batch the destination is the last
    // (and typically only) operand, e.g. `rsync --read-batch=FILE dest/`
    let dest_root = config
        .transfer_args()
        .last()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));

    // upstream: batch.c:120 check_batch_flags() reconciles the active options
    // against the batch header during replay, so carry the current flag state
    // into the reader. numeric_ids is not a recorded stream flag (batch.c:59-76);
    // it comes from the replay invocation and gates the post-flist id-list region
    // (uidlist.c:465,473 `numeric_ids <= 0`).
    let replay_cfg = batch_cfg
        .clone()
        .with_active_flags(config_batch_flags(config))
        .with_numeric_ids(config.numeric_ids())
        // --atimes / --crtimes have no flag_ptr[] bit either (batch.c:59-76),
        // and each gates a per-entry flist field, so they must reach the
        // reader from the replay invocation or the entry decode desyncs.
        .with_preserve_atimes(config.preserve_atimes())
        .with_preserve_crtimes(config.preserve_crtimes());

    let result = engine::batch::replay::replay(&replay_cfg, &dest_root, config.verbosity().into())
        .map_err(|e| match e {
            // upstream: batch.c:137-142 - an --iconv mismatch aborts with
            // RERR_SYNTAX (exit 1) printing the bare reconcile message.
            engine::batch::BatchError::FlagMismatch(msg) => {
                ClientError::new(1, rsync_error!(1, "{}", msg).with_role(Role::Client))
            }
            // upstream: compat.c:609-612 setup_protocol() - a batch recorded
            // with a protocol newer than this build supports aborts with
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
        })?;

    #[cfg(feature = "tracing")]
    {
        if result.recurse {
            tracing::info!("Batch mode enabled: recurse");
        }
        tracing::info!(
            file_count = result.file_count,
            total_size = result.total_size,
            "Batch replay complete"
        );
    }

    // upstream: main.c:362-373 - on the --read-batch side the receiver reads
    // total_read / total_written / stats.total_size from the batch trailer and
    // surfaces them through output_summary(). Mirror that by populating the
    // ClientSummary so the "sent X bytes received X bytes" / "total size is X"
    // lines reflect the replayed payload instead of zeros.
    //
    // The replay engine accounts every flist entry against `file_count` and
    // every byte of source-side material against `total_size`. Symlinks and
    // dirs created during replay are counted as files_transferred because the
    // receiver materialised them at the destination, matching upstream's
    // num_files / num_transferred accounting under --read-batch.
    use engine::local_copy::LocalCopySummary;
    let files_listed = usize::try_from(result.file_count).unwrap_or(usize::MAX);
    let files_transferred = files_listed;
    let total_size = result.total_size;
    let summary = LocalCopySummary::from_receiver_stats(
        files_listed,
        files_transferred,
        // upstream: receiver.c:784 total_transferred_size - the replay materialises
        // every flist entry, so the summed transferred-file length is total_size.
        total_size,
        total_size,
        total_size,
        total_size,
        std::time::Duration::ZERO,
        total_size,
        0,
        // The --read-batch replay engine decodes the flist from the batch file
        // without a raw wire counter, so no flist span is measured.
        0,
        protocol::DeleteStats::new(),
        // The --read-batch replay engine does not reconstruct a per-type
        // ITEM_IS_NEW breakdown, so carry every replayed entry as a created
        // regular file (reg = files_transferred, the pre-existing behaviour):
        // `regular()` derives it from `files` with the typed sub-counts at zero.
        protocol::CreatedStats {
            files: files_transferred as u64,
            ..protocol::CreatedStats::new()
        },
        // The --read-batch replay engine does not reconstruct a per-type file
        // breakdown, so leave the tallies at zero: reg = files_listed, the
        // pre-existing behaviour.
        engine::local_copy::FileTypeTotals::default(),
    );
    Ok(ClientSummary::from_summary(summary))
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
            content.contains("--filter=._-"),
            "Script must include --filter=._- for protocol >= 29: {content}"
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
            content.contains("--filter=._-"),
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
