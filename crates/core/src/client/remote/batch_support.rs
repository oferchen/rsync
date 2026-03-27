// Shared batch recording support for SSH and daemon transfer paths.
//
// Provides the adapter and context types needed to wire `--write-batch` into
// any transfer path that uses `run_server_with_handshake`. Extracted from
// the SSH transfer module to avoid duplication with the daemon transfer path.

use std::sync::{Arc, Mutex};

use engine::batch::{BatchFlags, BatchWriter};

use crate::client::config::ClientConfig;

/// Bundles the `BatchWriter` with pre-computed stream flags from `ClientConfig`,
/// since the inner functions only receive `ServerConfig` which lacks batch flag info.
pub(crate) struct BatchContext {
    pub(crate) writer: Arc<Mutex<BatchWriter>>,
    pub(crate) flags: BatchFlags,
}

/// Builds a [`BatchContext`] from a `ClientConfig` and `BatchWriter`.
///
/// Pre-computes batch stream flags from the client configuration so they are
/// available after the handshake when only `ServerConfig` is accessible.
pub(crate) fn build_batch_context(
    config: &ClientConfig,
    writer: Arc<Mutex<BatchWriter>>,
) -> BatchContext {
    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = config.preserve_xattrs();
    #[cfg(not(all(unix, feature = "xattr")))]
    let preserve_xattrs = false;

    let flags = BatchFlags {
        recurse: config.recursive(),
        preserve_uid: config.preserve_owner(),
        preserve_gid: config.preserve_group(),
        preserve_links: config.links(),
        preserve_hard_links: config.preserve_hard_links(),
        always_checksum: config.checksum(),
        xfer_dirs: config.dirs(),
        do_compression: config.compress(),
        preserve_xattrs,
        inplace: config.inplace(),
        append: config.append(),
        append_verify: config.append_verify(),
        ..Default::default()
    };

    BatchContext { writer, flags }
}

/// Builds a `BatchRecording` for `run_server_with_handshake`.
///
/// Creates the callback that writes the batch header with negotiated protocol
/// values (available only after `setup_protocol`), and provides the recorder
/// that gets attached at the multiplex layer for demuxed/pre-mux teeing.
///
/// upstream: `main.c:client_run()` calls `start_write_batch()` after
/// `setup_protocol()`. `start_write_batch` writes protocol_version,
/// compat_flags, checksum_seed, then activates the tee monitor.
pub(crate) fn build_batch_recording(
    batch_ctx: &BatchContext,
    is_sender: bool,
) -> crate::server::BatchRecording {
    let writer_for_header = batch_ctx.writer.clone();
    let flags = batch_ctx.flags;

    // The recorder wraps BatchWriter::write_data as a plain Write impl.
    // This adapts the batch crate's API to the generic Arc<Mutex<dyn Write + Send>>
    // expected by the multiplex layer.
    let recorder: Arc<Mutex<dyn std::io::Write + Send>> = Arc::new(Mutex::new(BatchWriteAdapter {
        inner: batch_ctx.writer.clone(),
    }));

    crate::server::BatchRecording {
        on_setup_complete: Box::new(move |protocol_version, compat_flags, checksum_seed| {
            let mut bw = writer_for_header
                .lock()
                .map_err(|_| std::io::Error::other("batch writer lock poisoned"))?;
            let cfg = bw.config_mut();
            cfg.protocol_version = protocol_version;
            cfg.compat_flags = compat_flags.map(|f| f.bits() as i32);
            cfg.checksum_seed = checksum_seed;
            bw.write_header(flags)
                .map_err(|e| std::io::Error::other(format!("failed to write batch header: {e}")))
        }),
        recorder,
        is_sender,
    }
}

/// Adapts `BatchWriter::write_data` to the `std::io::Write` trait.
///
/// The multiplex recorder expects `Arc<Mutex<dyn Write + Send>>`, but
/// `BatchWriter` uses its own `write_data` method. This adapter bridges
/// the two interfaces without coupling the transfer crate to the batch crate.
struct BatchWriteAdapter {
    inner: Arc<Mutex<BatchWriter>>,
}

impl std::io::Write for BatchWriteAdapter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut bw = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("batch writer lock poisoned"))?;
        bw.write_data(buf)
            .map_err(|e| std::io::Error::other(format!("batch write failed: {e}")))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut bw = self
            .inner
            .lock()
            .map_err(|_| std::io::Error::other("batch writer lock poisoned"))?;
        bw.flush()
            .map_err(|e| std::io::Error::other(format!("batch flush failed: {e}")))
    }
}
