//! Batch mode support for client transfers.
//!
//! Handles both writing batch files during a transfer and replaying
//! previously recorded batch files. Mirrors upstream `main.c:read_batch()`
//! for replay and `main.c:374-383` for batch stats finalization.

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

    // upstream: main.c:1464-1473 - reject remote destinations with --read-batch
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
        // upstream: batch.c:68 - do_compression is bit 8 in stream flags.
        // Upstream tees the raw (pre-decompression) wire bytes to
        // batch_fd via write_batch_monitor_in in io.c:read_buf(),
        // so its batch files contain compressed tokens and the header
        // says do_compression=true.
        // oc-rsync captures post-decompression (uncompressed) data at
        // the CompressedReader layer, so the batch body is always
        // uncompressed. Setting do_compression=false ensures that both
        // oc-rsync and upstream rsync replay the file without trying to
        // decompress already-uncompressed tokens.
        do_compression: false,
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

/// Writes trailing stats, flushes the batch file, and generates the replay script.
///
/// When filter rules are active in `config`, the replay script embeds them
/// using the same heredoc format as upstream `batch.c:write_filter_rules()`,
/// ensuring the replay applies identical filters.
///
/// Called after a successful transfer to finalize the batch recording.
/// Mirrors upstream `main.c:374-383` for stats writing.
pub(crate) fn finalize_batch(
    writer_arc: &Arc<Mutex<BatchWriter>>,
    batch_cfg: &BatchConfig,
    config: &ClientConfig,
    summary: &ClientSummary,
) -> Result<(), ClientError> {
    {
        let mut writer = writer_arc.lock().map_err(|_| {
            ClientError::new(
                1,
                rsync_error!(1, "batch writer lock poisoned").with_role(Role::Client),
            )
        })?;

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

        // upstream: main.c:1119-1122 - write_ndx(f_out, NDX_DONE) as final
        // goodbye after stats. read_final_goodbye() (main.c:875-906) reads
        // this from the batch file. For protocol >= 30, NDX_DONE = 0x00
        // (single byte). For protocol < 30, NDX_DONE = 0xFFFFFFFF (4 bytes).
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

        if let Err(e) = writer.flush() {
            let msg = format!("failed to flush batch file: {e}");
            return Err(ClientError::new(
                1,
                rsync_error!(1, "{}", msg).with_role(Role::Client),
            ));
        }
    }

    // upstream: batch.c:305-306 - embed filter rules in the replay script
    let filter_text = serialize_filter_rules(config.filter_rules());
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
/// # Upstream Reference
///
/// - `batch.c:205-222`: `write_filter_rules()` iterates filter_list
/// - `exclude.c:1525-1587`: `get_rule_prefix()` builds the prefix string
fn serialize_filter_rules(rules: &[FilterRuleSpec]) -> String {
    if rules.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    for rule in rules {
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

    output
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
    // into the reader.
    let replay_cfg = batch_cfg
        .clone()
        .with_active_flags(config_batch_flags(config));

    let result = engine::batch::replay::replay(&replay_cfg, &dest_root, config.verbosity().into())
        .map_err(|e| match e {
            // upstream: batch.c:137-142 - an --iconv mismatch aborts with
            // RERR_SYNTAX (exit 1) printing the bare reconcile message.
            engine::batch::BatchError::FlagMismatch(msg) => {
                ClientError::new(1, rsync_error!(1, "{}", msg).with_role(Role::Client))
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
        protocol::DeleteStats::new(),
        // The --read-batch replay engine does not reconstruct a per-type
        // ITEM_IS_NEW breakdown, so carry every replayed entry as a created
        // regular file (reg = files_transferred, the pre-existing behaviour):
        // `regular()` derives it from `files` with the typed sub-counts at zero.
        protocol::CreatedStats {
            files: files_transferred as u64,
            ..protocol::CreatedStats::new()
        },
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

    #[test]
    fn write_batch_skips_read_handling() {
        let batch_cfg = BatchConfig::new(BatchMode::Write, "test_batch".to_owned(), 30);
        let config = config_with_compress(false);
        assert!(handle_batch_read(&batch_cfg, &config).is_none());
    }

    /// Batch header must always have do_compression=false because oc-rsync
    /// captures post-decompression (uncompressed) data in the batch file.
    /// upstream tees pre-decompression data and sets do_compression=true,
    /// but our approach avoids that complexity.
    #[test]
    fn write_batch_header_never_sets_do_compression() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("test.batch");
        let batch_cfg = BatchConfig::new(BatchMode::Write, path.to_string_lossy().to_string(), 31)
            .with_checksum_seed(1);

        let writer_arc = create_batch_writer(&batch_cfg).unwrap();

        let config = config_with_compress(true);
        write_batch_header(&writer_arc, &config).unwrap();
        drop(writer_arc);

        let read_cfg = BatchConfig::new(BatchMode::Read, path.to_string_lossy().to_string(), 31);
        let mut reader = engine::batch::BatchReader::new(read_cfg).unwrap();
        let flags = reader.read_header().unwrap();
        assert!(
            !flags.do_compression,
            "do_compression must be false - oc-rsync captures uncompressed data"
        );
    }

    #[test]
    fn serialize_empty_rules_returns_empty_string() {
        assert_eq!(serialize_filter_rules(&[]), "");
    }

    #[test]
    fn serialize_exclude_rule() {
        let rules = [FilterRuleSpec::exclude("*.tmp")];
        let output = serialize_filter_rules(&rules);
        assert_eq!(output, "- *.tmp\n");
    }

    #[test]
    fn serialize_include_rule() {
        let rules = [FilterRuleSpec::include("*.rs")];
        let output = serialize_filter_rules(&rules);
        assert_eq!(output, "+ *.rs\n");
    }

    #[test]
    fn serialize_protect_rule() {
        let rules = [FilterRuleSpec::protect("/data")];
        let output = serialize_filter_rules(&rules);
        // upstream: protect is 'P', receiver-only gets 'r' modifier
        assert_eq!(output, "Pr /data\n");
    }

    #[test]
    fn serialize_risk_rule() {
        let rules = [FilterRuleSpec::risk("/temp")];
        let output = serialize_filter_rules(&rules);
        // upstream: risk is 'R', receiver-only gets 'r' modifier
        assert_eq!(output, "Rr /temp\n");
    }

    #[test]
    fn serialize_clear_rule() {
        let rules = [FilterRuleSpec::clear()];
        let output = serialize_filter_rules(&rules);
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
        let output = serialize_filter_rules(&rules);
        assert_eq!(output, "- *.tmp\n+ */\n+ *.txt\n- *\n");
    }

    #[test]
    fn serialize_sender_only_rule() {
        let rules = [FilterRuleSpec::hide("*.bak")];
        let output = serialize_filter_rules(&rules);
        // upstream: sender-only gets 's' modifier
        assert_eq!(output, "-s *.bak\n");
    }

    #[test]
    fn serialize_perishable_rule() {
        let rules = [FilterRuleSpec::exclude("*.tmp").with_perishable(true)];
        let output = serialize_filter_rules(&rules);
        assert_eq!(output, "-p *.tmp\n");
    }

    #[test]
    fn serialize_xattr_only_rule() {
        let rules = [FilterRuleSpec::exclude("user.*").with_xattr_only(true)];
        let output = serialize_filter_rules(&rules);
        assert_eq!(output, "-x user.*\n");
    }

    #[test]
    fn serialize_negated_rule() {
        let rules = [FilterRuleSpec::exclude("*.txt").with_negate(true)];
        let output = serialize_filter_rules(&rules);
        assert_eq!(output, "-! *.txt\n");
    }

    #[test]
    fn serialize_directory_only_pattern() {
        // FilterRuleSpec stores the trailing '/' as part of the pattern
        let rules = [FilterRuleSpec::exclude("build/")];
        let output = serialize_filter_rules(&rules);
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
        let filter_text = serialize_filter_rules(&rules);

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
        let result = finalize_batch(&writer_arc, &batch_cfg, &config, &summary);
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
        let result = finalize_batch(&writer_arc, &batch_cfg, &config, &summary);
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
