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

    #[cfg(all(any(unix, windows), feature = "acl"))]
    let preserve_acls = config.preserve_acls();
    #[cfg(not(all(any(unix, windows), feature = "acl")))]
    let preserve_acls = false;

    let flags = BatchFlags {
        recurse: config.recursive(),
        preserve_uid: config.preserve_owner(),
        preserve_gid: config.preserve_group(),
        preserve_links: config.links(),
        preserve_devices: config.preserve_devices(),
        preserve_hard_links: config.preserve_hard_links(),
        always_checksum: config.checksum(),
        xfer_dirs: config.dirs(),
        // upstream tees raw (compressed) wire bytes to the batch file
        // and sets do_compression=true in stream flags. On read-batch,
        // upstream's parse_compress_choice() (compat.c:194-195) maps
        // do_compression=1 to CPRES_ZLIB regardless of the actual
        // algorithm used during write. This causes upstream to fail
        // reading its own batch files when the original transfer
        // auto-negotiated zstd (rsync 3.4.1 with SUPPORT_ZSTD).
        //
        // oc-rsync avoids this upstream limitation by capturing
        // post-decompression data. Always false so replay reads
        // uncompressed tokens correctly and batch files are portable
        // across all compression backends.
        do_compression: false,
        preserve_acls,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `build_batch_context` always sets `do_compression=false`,
    /// even when the client configuration has compression enabled. oc-rsync
    /// captures post-decompression data, so the batch file body is always
    /// uncompressed. This avoids an upstream rsync 3.4.1 limitation where
    /// batch files written with zstd compression are unreadable because
    /// read-batch forces CPRES_ZLIB (upstream compat.c:194-195).
    #[test]
    fn batch_context_never_sets_do_compression() {
        let config = ClientConfig::builder().compress(true).build();
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("test.batch");
        let batch_cfg = engine::batch::BatchConfig::new(
            engine::batch::BatchMode::Write,
            path.to_string_lossy().to_string(),
            31,
        );
        let writer = Arc::new(Mutex::new(BatchWriter::new(batch_cfg).unwrap()));
        let ctx = build_batch_context(&config, writer);
        assert!(
            !ctx.flags.do_compression,
            "do_compression must be false - oc-rsync captures uncompressed data"
        );
    }

    /// Verifies do_compression is false for all compression-related configs.
    ///
    /// This documents that oc-rsync's batch design avoids the upstream rsync
    /// 3.4.1 limitation where the batch format does not record which
    /// compression algorithm was used. Upstream's read-batch always assumes
    /// CPRES_ZLIB (compat.c:194-195), but the write-batch may have used
    /// zstd, lz4, or zlibx - causing unreadable batch files.
    #[test]
    fn batch_context_do_compression_false_regardless_of_compress_config() {
        for compress_enabled in [true, false] {
            let config = ClientConfig::builder().compress(compress_enabled).build();
            let temp = tempfile::TempDir::new().unwrap();
            let path = temp.path().join("test.batch");
            let batch_cfg = engine::batch::BatchConfig::new(
                engine::batch::BatchMode::Write,
                path.to_string_lossy().to_string(),
                32,
            );
            let writer = Arc::new(Mutex::new(BatchWriter::new(batch_cfg).unwrap()));
            let ctx = build_batch_context(&config, writer);
            assert!(
                !ctx.flags.do_compression,
                "do_compression must always be false (compress={compress_enabled})"
            );
        }
    }
}
