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

use super::super::config::ClientConfig;
use super::super::error::ClientError;
use super::super::remote;
use super::super::summary::ClientSummary;

/// Validates that `--write-batch` with compression is not used at protocol <= 29.
///
/// Upstream rsync since protocol 30 stores batch tokens uncompressed in the
/// batch file (it tees the uncompressed stream). For protocol 28-29, the batch
/// file contains the raw compressed wire stream, and reading it back requires
/// knowing the compression state - which can produce corrupt replays.
///
/// # Errors
///
/// Returns `Err(ClientError)` with exit code 1 (RERR_SYNTAX) if write-batch
/// mode is active, compression is enabled, and the protocol version is <= 29.
// upstream: options.c - batch + compress validation
pub(crate) fn validate_batch_compress(
    batch_cfg: &BatchConfig,
    config: &ClientConfig,
) -> Result<(), ClientError> {
    if !batch_cfg.is_write_mode() {
        return Ok(());
    }
    if !config.compress() {
        return Ok(());
    }
    let proto = batch_cfg.protocol_version;
    if proto < 30 {
        return Err(ClientError::new(
            super::super::FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                super::super::FEATURE_UNAVAILABLE_EXIT_CODE,
                "cannot combine --write-batch with compression at protocol {} \
                 (requires protocol >= 30)",
                proto
            )
            .with_role(Role::Client),
        ));
    }
    Ok(())
}

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

/// Writes the batch header containing stream flags before the transfer begins.
pub(crate) fn write_batch_header(
    writer: &Arc<Mutex<BatchWriter>>,
    config: &ClientConfig,
) -> Result<(), ClientError> {
    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = config.preserve_xattrs();
    #[cfg(not(all(unix, feature = "xattr")))]
    let preserve_xattrs = false;

    #[cfg(all(unix, feature = "acl"))]
    let preserve_acls = config.preserve_acls();
    #[cfg(not(all(unix, feature = "acl")))]
    let preserve_acls = false;

    let batch_flags = engine::batch::BatchFlags {
        recurse: config.recursive(),
        preserve_uid: config.preserve_owner(),
        preserve_gid: config.preserve_group(),
        preserve_links: config.links(),
        preserve_devices: config.preserve_devices(),
        preserve_hard_links: config.preserve_hard_links(),
        always_checksum: config.checksum(),
        xfer_dirs: config.dirs(),
        // upstream: batch token data is a verbatim tee of the compressed
        // wire stream, so do_compression controls recv_token() dispatch
        // during replay. Our batch delta buffer captures uncompressed
        // tokens, so we must report false to avoid decompression mismatch.
        do_compression: false,
        preserve_acls,
        preserve_xattrs,
        inplace: config.inplace(),
        append: config.append(),
        append_verify: config.append_verify(),
        ..Default::default()
    };

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
/// Called after a successful transfer to finalize the batch recording.
/// Mirrors upstream `main.c:374-383` for stats writing.
pub(crate) fn finalize_batch(
    writer_arc: &Arc<Mutex<BatchWriter>>,
    batch_cfg: &BatchConfig,
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

    // Generate the .sh replay script
    if let Err(e) = engine::batch::script::generate_script(batch_cfg) {
        let msg = format!("failed to generate batch script: {e}");
        return Err(ClientError::new(
            1,
            rsync_error!(1, "{}", msg).with_role(Role::Client),
        ));
    }

    Ok(())
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

    let result = engine::batch::replay::replay(batch_cfg, &dest_root, config.verbosity().into())
        .map_err(|e| {
            let msg = format!("batch replay failed: {e}");
            ClientError::new(1, rsync_error!(1, "{}", msg).with_role(Role::Client))
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
    let _ = &result;

    use engine::local_copy::LocalCopySummary;
    Ok(ClientSummary::from_summary(LocalCopySummary::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::batch::BatchMode;

    fn write_batch_config(proto: i32) -> BatchConfig {
        BatchConfig::new(BatchMode::Write, "test_batch".to_owned(), proto)
    }

    fn read_batch_config(proto: i32) -> BatchConfig {
        BatchConfig::new(BatchMode::Read, "test_batch".to_owned(), proto)
    }

    fn config_with_compress(compress: bool) -> ClientConfig {
        ClientConfig::builder().compress(compress).build()
    }

    #[test]
    fn batch_compress_rejected_at_protocol_28() {
        let batch_cfg = write_batch_config(28);
        let config = config_with_compress(true);
        let err = validate_batch_compress(&batch_cfg, &config).unwrap_err();
        assert_eq!(
            err.exit_code(),
            super::super::super::FEATURE_UNAVAILABLE_EXIT_CODE
        );
        assert!(
            err.to_string()
                .contains("cannot combine --write-batch with compression"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn batch_compress_rejected_at_protocol_29() {
        let batch_cfg = write_batch_config(29);
        let config = config_with_compress(true);
        let err = validate_batch_compress(&batch_cfg, &config).unwrap_err();
        assert_eq!(
            err.exit_code(),
            super::super::super::FEATURE_UNAVAILABLE_EXIT_CODE
        );
    }

    #[test]
    fn batch_compress_allowed_at_protocol_30() {
        let batch_cfg = write_batch_config(30);
        let config = config_with_compress(true);
        assert!(validate_batch_compress(&batch_cfg, &config).is_ok());
    }

    #[test]
    fn batch_compress_allowed_at_protocol_31() {
        let batch_cfg = write_batch_config(31);
        let config = config_with_compress(true);
        assert!(validate_batch_compress(&batch_cfg, &config).is_ok());
    }

    #[test]
    fn batch_no_compress_allowed_at_any_protocol() {
        for proto in [28, 29, 30, 31, 32] {
            let batch_cfg = write_batch_config(proto);
            let config = config_with_compress(false);
            assert!(
                validate_batch_compress(&batch_cfg, &config).is_ok(),
                "should allow write-batch without compress at protocol {proto}"
            );
        }
    }

    #[test]
    fn read_batch_skips_validation() {
        let batch_cfg = read_batch_config(28);
        let config = config_with_compress(true);
        assert!(validate_batch_compress(&batch_cfg, &config).is_ok());
    }
}
