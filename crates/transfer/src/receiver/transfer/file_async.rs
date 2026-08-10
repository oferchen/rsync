//! Async (tokio-transfer) per-file receiver reconstruct-and-commit leg.
//!
//! This is the `.await` twin of the byte-producing core of the synchronous
//! per-file loop in [`super::sync`] (`run_sync`). It is the first *live* async
//! receiver call site for the async reconstruct leaf added in the base of this
//! stack (`apply_delta_stream_async` / `DeltaApplicator::finish_async`): the
//! ASY reconstruct primitives were previously exercised only by their
//! standalone parity tests, never by a receiver that drives a real transport.
//!
//! # Scope of the leg
//!
//! [`ReceiverContext::receive_file_async`] performs exactly the byte-producing
//! per-file work the sync loop does for one transferred regular file:
//!
//! 1. write the NDX + (protocol >= 29) `ITEM_TRANSFER` iflags to the sender
//!    (the request half is a plain synchronous `Write`, unchanged from sync);
//! 2. locate the basis file, write the sum-head + signature blocks, flush;
//! 3. read the sender's echoed NDX + attrs and echoed sum-head **off the async
//!    transport** ([`SenderAttrs::read_with_codec_xattr_async`],
//!    [`SumHead::read_async`]);
//! 4. open the sandboxed temp file, reconstruct it via
//!    [`apply_delta_stream_async`] + [`DeltaApplicator::finish_async`] (the sole
//!    `.await` reconstruct leaf), and verify the trailing whole-file checksum;
//! 5. run the identical sparse-size check, fsync, atomic rename, and metadata /
//!    xattr / ACL application the sync loop runs.
//!
//! Only the wire reads are `.await`ed. Every byte the tokens turn into, the
//! temp-file commit, and the metadata application run through the exact same
//! synchronous helpers the sync loop uses, so for the same wire bytes this
//! produces a byte-identical committed destination file and the same
//! per-file [`FileReceiveOutcome`]. The equivalence is pinned by this module's
//! own `tests` (`async_receiver_file_matches_sync_reconstruct`), which drive a
//! real constructed delta wire through both this async leg and the proven sync
//! reconstruct path and assert byte-identical committed files, `literal_bytes`,
//! and error kinds.
//!
//! # What is deliberately NOT here
//!
//! This is the smallest genuinely end-to-end async receiver leg. The
//! surrounding session scaffold (file-list reception, directory/symlink/hardlink
//! creation, the phase-done + goodbye handshake) still runs on the synchronous
//! path; converting those to `.await` is the remaining ASY receiver work. This
//! leg does not change the default (threaded) build: the whole module is
//! compiled out unless `tokio-transfer` is on.
//!
//! # Upstream Reference
//!
//! - `receiver.c:720` - `recv_files()` per-file loop (the sync twin mirrors this)
//! - `receiver.c:240` - `receive_data()` reconstruct (the `.await` leaf)

// The async per-file receiver leg is driven end-to-end by this module's own
// equivalence tests (`async_receiver_file_matches_sync_reconstruct`) but has no
// non-test caller yet: the surrounding async receiver loop that would call it on
// every file is the remaining ASY receiver rung. Allow dead_code so the leg can
// land with its equivalence gate ahead of that loop without tripping
// `-D warnings`; the allow is removed when the loop wires it in. Mirrors the
// same pattern on the `AsyncTransport` seam in `pipeline/async_transport.rs`.
#![allow(dead_code)]

use std::io;

use logging::info_log;
use metadata::apply_metadata_with_cached_stat;
use protocol::acl::AclCache;
use protocol::codec::{MonotonicNdxWriter, NdxCodec, NdxCodecEnum};

use engine::CleanupManager;

use crate::delta_apply::ChecksumVerifier;
use crate::receiver::basis::find_basis_file_with_config;
use crate::receiver::wire::{SenderAttrs, SumHead, write_signature_blocks};
use crate::receiver::{ReceiverContext, apply_acls_from_receiver_cache};
#[cfg(not(unix))]
use crate::temp_guard::open_tmpfile;
#[cfg(unix)]
use crate::temp_guard::open_tmpfile_sandboxed;
use crate::token_reader::TokenReader;

use protocol::flist::FileEntry;

/// Outcome of a single async per-file receive.
///
/// Mirrors the per-iteration bookkeeping the sync loop folds into its running
/// totals so the async loop (and the equivalence test) can accumulate them the
/// same way.
#[derive(Debug, Default)]
pub(in crate::receiver) struct FileReceiveOutcome {
    /// Whether a file was actually reconstructed and committed. `false` when the
    /// file was skipped (temp-open failure drained the delta and continued).
    pub(in crate::receiver) transferred: bool,
    /// Literal bytes that traversed the read fd, mapped to `bytes_received`
    /// exactly as the sync loop does (matched-from-basis bytes excluded).
    pub(in crate::receiver) literal_bytes: u64,
    /// Per-file metadata errors, appended to the receiver's running list.
    pub(in crate::receiver) metadata_errors: Vec<(std::path::PathBuf, String)>,
}

/// Inputs shared across every `receive_file_async` call in one transfer.
///
/// Bundles the per-transfer setup the sync loop reads out of `PipelineSetup`
/// so the async per-file method can borrow them without re-deriving anything.
pub(in crate::receiver) struct AsyncFileContext<'a> {
    pub(in crate::receiver) dest_dir: &'a std::path::Path,
    pub(in crate::receiver) metadata_opts: &'a metadata::MetadataOptions,
    pub(in crate::receiver) checksum_length: std::num::NonZeroU8,
    pub(in crate::receiver) checksum_algorithm: signature::SignatureAlgorithm,
    pub(in crate::receiver) acl_cache: Option<&'a std::sync::Arc<AclCache>>,
    pub(in crate::receiver) acl_id_map: Option<&'a std::sync::Arc<metadata::AclIdMapper>>,
    #[cfg(unix)]
    pub(in crate::receiver) sandbox: Option<&'a std::sync::Arc<fast_io::DirSandbox>>,
}

impl ReceiverContext {
    /// Reconstructs and commits one regular file, reading the delta off an
    /// [`AsyncRead`](tokio::io::AsyncRead) transport.
    ///
    /// This is the async twin of the byte-producing per-file body of `run_sync`
    /// (`sync.rs` lines 155-494). The request half (`writer`) stays a plain
    /// synchronous [`Write`](std::io::Write); only the sender's echoed
    /// NDX/attrs, echoed sum-head, delta tokens, and trailing checksum are read
    /// via `.await`. The reconstruct itself runs through the async leaf
    /// [`apply_delta_stream_async`](crate::delta_apply::apply_delta_stream_async)
    /// and [`DeltaApplicator::finish_async`](crate::delta_apply::DeltaApplicator::finish_async).
    /// Everything downstream of the reconstruct - the sparse-size check, fsync,
    /// atomic rename, and metadata/xattr/ACL application - is the identical
    /// synchronous logic the sync loop runs.
    ///
    /// `token_reader` and the NDX codecs are threaded in from the caller so the
    /// compression dictionary and monotonic NDX state persist across files,
    /// exactly as the session-shared instances do in the sync loop.
    ///
    /// # Errors
    ///
    /// Propagates any wire, reconstruct, or filesystem error. A temp-open
    /// failure is NOT fatal: it drains this file's delta off the wire and
    /// returns a non-transferred outcome (mirroring `receiver.c:999-1006`).
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` per-file body
    #[allow(clippy::too_many_arguments)]
    pub(in crate::receiver) async fn receive_file_async<R, W>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
        file_entry: &FileEntry,
        file_idx: usize,
        ndx_write_codec: &mut MonotonicNdxWriter,
        ndx_read_codec: &mut NdxCodecEnum,
        token_reader: &mut TokenReader,
        ctx: &AsyncFileContext<'_>,
    ) -> io::Result<FileReceiveOutcome>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
        W: io::Write + crate::writer::MsgInfoSender + ?Sized,
    {
        let mut outcome = FileReceiveOutcome::default();
        let dest_dir = ctx.dest_dir;
        let relative_path = file_entry.path();
        let file_path = if relative_path.as_os_str() == "." {
            dest_dir.to_path_buf()
        } else {
            dest_dir.join(relative_path)
        };

        // --- request half: NDX + iflags (synchronous Write, as in sync.rs) ---
        let ndx = self.flat_to_wire_ndx(file_idx);
        ndx_write_codec.write_ndx(&mut *writer, ndx)?;

        // The basis search precedes the iflags write so a --partial-dir resume
        // basis can set ITEM_BASIS_TYPE_FOLLOWS (generator.c:1942-1943).
        let basis_config = self.build_basis_file_config(
            &file_path,
            dest_dir,
            relative_path,
            file_entry.size(),
            file_entry.mtime(),
            ctx.checksum_length,
            ctx.checksum_algorithm,
        );
        let basis_result = find_basis_file_with_config(&basis_config);
        let signature_opt = basis_result.signature;
        let basis_path_opt = basis_result.basis_path;
        let fnamecmp_type = basis_result.fnamecmp_type;

        if self.protocol.supports_iflags() {
            let mut iflags = SenderAttrs::ITEM_TRANSFER;
            let emit_basis_type = fnamecmp_type != protocol::FnameCmpType::Fname;
            if emit_basis_type {
                iflags |= SenderAttrs::ITEM_BASIS_TYPE_FOLLOWS;
            }
            writer.write_all(&iflags.to_le_bytes())?;
            if emit_basis_type {
                writer.write_all(&[u8::from(fnamecmp_type)])?;
            }
        }

        let sum_head = match signature_opt {
            Some(ref signature) => SumHead::from_signature(signature),
            None => SumHead::empty(),
        };
        sum_head.write(&mut *writer)?;
        if !self.config.flags.append {
            if let Some(ref signature) = signature_opt {
                write_signature_blocks(&mut *writer, signature, sum_head.s2length)?;
            }
        }
        writer.flush()?;

        // --- response half: echoed NDX/attrs + sum-head, read off the wire ---
        let use_xattr_stream = self.protocol.as_u8() >= 31
            && self.compat_flags.is_some_and(|f| {
                !f.contains(protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION)
            });
        let (echoed_ndx, _sender_attrs) = SenderAttrs::read_with_codec_xattr_async(
            reader,
            ndx_read_codec,
            self.config.flags.xattrs,
            use_xattr_stream,
        )
        .await?;
        debug_assert_eq!(
            echoed_ndx, ndx,
            "sender echoed NDX {echoed_ndx} but we requested {ndx}"
        );

        let _echoed_sum_head = SumHead::read_async(reader).await?;

        // --- temp open (sync FS, identical to sync.rs) ---
        #[cfg(unix)]
        let open_result = open_tmpfile_sandboxed(
            &file_path,
            self.config.temp_dir.as_deref(),
            ctx.sandbox,
            Some(dest_dir),
        );
        #[cfg(not(unix))]
        let open_result = open_tmpfile(&file_path, self.config.temp_dir.as_deref());

        let (file, mut temp_guard) = match open_result {
            Ok(pair) => pair,
            Err(open_err) => {
                // upstream: receiver.c:999-1006 - drain the delta and continue.
                let checksum_len = ChecksumVerifier::new(
                    self.negotiated_algorithms.as_ref(),
                    self.protocol,
                    self.checksum_seed,
                    self.compat_flags.as_ref(),
                )
                .digest_len();
                token_reader.reset();
                crate::delta_apply::discard_delta_stream_async(reader, token_reader, checksum_len)
                    .await?;
                outcome
                    .metadata_errors
                    .push((file_path.clone(), format!("mkstemp failed: {open_err}")));
                self.flist_io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
                return Ok(outcome);
            }
        };
        CleanupManager::global().register_temp_file(temp_guard.path().to_path_buf());
        temp_guard.mark_registered();

        let file_verifier = ChecksumVerifier::new(
            self.negotiated_algorithms.as_ref(),
            self.protocol,
            self.checksum_seed,
            self.compat_flags.as_ref(),
        );
        let config = crate::delta_apply::DeltaApplyConfig {
            sparse: self.config.flags.sparse,
            writer_kind: crate::delta_apply::BasisWriterKind::Standard,
            cow_policy: fast_io::CowPolicy::Auto,
        };

        token_reader.reset();
        let mut applicator = crate::delta_apply::DeltaApplicator::new(
            file,
            &config,
            file_verifier,
            signature_opt.as_ref(),
            basis_path_opt.as_deref(),
        )?;

        // --- the async reconstruct leaf ---
        crate::delta_apply::apply_delta_stream_async(reader, &mut applicator, token_reader).await?;
        let (file, result) = applicator.finish_async(reader, None).await?;

        if let Some(final_pos) = result.final_pos {
            let expected_size = file_entry.size();
            if final_pos != expected_size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "sparse file size mismatch for {file_path:?}: \
                         expected {expected_size} bytes, got {final_pos} bytes {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver(),
                    ),
                ));
            }
        }

        let literal_bytes = result.literal_bytes;

        if self.config.write.fsync {
            file.sync_all().map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "fsync failed for {file_path:?}: {e} {}{}",
                        crate::role_trailer::error_location!(),
                        crate::role_trailer::receiver()
                    ),
                )
            })?;
        }
        drop(file);

        // --- atomic rename (sync FS, identical to sync.rs commit) ---
        if let Some(rename_result) = fast_io::try_rename_via_io_uring(temp_guard.path(), &file_path)
        {
            rename_result?;
        } else {
            #[cfg(unix)]
            {
                let temp_path = temp_guard.path();
                let temp_rel = temp_path
                    .strip_prefix(dest_dir)
                    .map(std::path::Path::to_path_buf)
                    .unwrap_or_else(|_| temp_path.to_path_buf());
                fast_io::renameat_via_sandbox_or_fallback(
                    ctx.sandbox.map(std::sync::Arc::as_ref),
                    dest_dir,
                    &temp_rel,
                    temp_path,
                    dest_dir,
                    relative_path,
                    &file_path,
                    true,
                )?;
            }
            #[cfg(not(unix))]
            {
                std::fs::rename(temp_guard.path(), &file_path)?;
            }
        }
        CleanupManager::global().unregister_temp_file(temp_guard.path());
        temp_guard.keep();

        // --- metadata / xattr / ACL (sync, identical to sync.rs) ---
        if let Err(meta_err) =
            apply_metadata_with_cached_stat(&file_path, file_entry, ctx.metadata_opts, None)
        {
            outcome
                .metadata_errors
                .push((file_path.clone(), meta_err.to_string()));
        } else if let Some(ref xattr_list) = self.resolve_xattr_list(file_entry) {
            if let Err(e) = metadata::apply_xattrs_from_list(&file_path, xattr_list, true) {
                outcome
                    .metadata_errors
                    .push((file_path.clone(), e.to_string()));
            }
        }

        if let Err(acl_err) = apply_acls_from_receiver_cache(
            &file_path,
            file_entry,
            ctx.acl_cache.map(std::sync::Arc::as_ref),
            ctx.acl_id_map.map(std::sync::Arc::as_ref),
            !file_entry.is_symlink(),
        ) {
            outcome
                .metadata_errors
                .push((file_path.clone(), acl_err.to_string()));
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            info_log!(Name, 1, "{}", relative_path.display());
        }

        // upstream: io.c:820 - only literal bytes traverse the read fd.
        outcome.transferred = true;
        outcome.literal_bytes = literal_bytes;
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    //! End-to-end equivalence for the async per-file receiver leg.
    //!
    //! Drives a real constructed delta wire (echoed NDX + iflags + sum-head +
    //! literal delta tokens + trailing whole-file checksum) through the live
    //! async receiver ([`ReceiverContext::receive_file_async`]) and asserts the
    //! committed destination file is byte-identical to the reference the SYNC
    //! reconstruct path ([`apply_delta_stream`] + [`DeltaApplicator::finish`])
    //! produces from the identical delta stream. The sync applicator path is the
    //! one the base-of-stack `delta_applicator_equivalence.rs` already proves
    //! matches the live sync receiver loop, so equality here transitively pins
    //! the async receiver leg to the sync receiver's byte output.
    //!
    //! The async wire is fed through a [`ChunkedReader`] that hands over one byte
    //! per poll, forcing every wire read to cross an `.await` boundary mid-token
    //! - the case a naive async decoder would corrupt.

    use super::*;
    use std::io::{Cursor, Read, Write};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use protocol::ChecksumAlgorithm;
    use protocol::ProtocolVersion;
    use protocol::codec::create_ndx_codec;
    use tempfile::tempdir;
    use tokio::io::{AsyncRead, ReadBuf};

    use crate::config::ServerConfig;
    use crate::delta_apply::{
        ChecksumVerifier, DeltaApplicator, DeltaApplyConfig, TokenReader, apply_delta_stream,
    };
    use crate::handshake::HandshakeResult;
    use crate::role::ServerRole;

    /// Delivers at most `chunk` bytes per `poll_read`, forcing async reads to
    /// cross `.await` boundaries mid-token when `chunk == 1`.
    struct ChunkedReader {
        inner: Cursor<Vec<u8>>,
        chunk: usize,
    }

    impl AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let limit = self.chunk.min(buf.remaining());
            if limit == 0 {
                return Poll::Ready(Ok(()));
            }
            let pos = self.inner.position() as usize;
            let data = self.inner.get_ref();
            if pos >= data.len() {
                return Poll::Ready(Ok(()));
            }
            let end = (pos + limit).min(data.len());
            let slice = data[pos..end].to_vec();
            buf.put_slice(&slice);
            self.inner.set_position(end as u64);
            Poll::Ready(Ok(()))
        }
    }

    fn test_handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    fn test_config() -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(32u8).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![std::ffi::OsString::from(".")],
            ..Default::default()
        }
    }

    /// Encodes a literal-only (whole-file, no basis) delta wire: each literal as
    /// a positive i32 LE length prefix followed by the bytes, terminated by a
    /// zero-length token, then the trailing whole-file checksum.
    fn encode_literal_wire(literals: &[&[u8]], algo: ChecksumAlgorithm, seed: i32) -> Vec<u8> {
        let mut wire = Vec::new();
        let mut output = Vec::new();
        for lit in literals {
            let len = i32::try_from(lit.len()).unwrap();
            wire.extend_from_slice(&len.to_le_bytes());
            wire.extend_from_slice(lit);
            output.extend_from_slice(lit);
        }
        wire.extend_from_slice(&0_i32.to_le_bytes());
        let mut verifier = ChecksumVerifier::for_algorithm_seeded(algo, seed);
        verifier.update(&output);
        let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let n = verifier.finalize_into(&mut digest);
        wire.extend_from_slice(&digest[..n]);
        wire
    }

    /// Builds the full sender->receiver echo wire the async leg reads: the
    /// echoed NDX (serialized with the same writer the receiver uses so the read
    /// side decodes it), the `ITEM_TRANSFER` iflags, an empty sum-head (no
    /// basis), then the delta+checksum stream.
    fn build_echo_wire(ndx: i32, delta_wire: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        let mut ndx_writer = MonotonicNdxWriter::new(32);
        ndx_writer.write_ndx(&mut wire, ndx).unwrap();
        wire.extend_from_slice(&SenderAttrs::ITEM_TRANSFER.to_le_bytes());
        SumHead::empty().write(&mut wire).unwrap();
        wire.extend_from_slice(delta_wire);
        wire
    }

    /// Sync reference: reconstruct the same delta stream with the proven sync
    /// `DeltaApplicator` path and return the committed bytes + literal count.
    fn sync_reference(
        delta_wire: &[u8],
        algo: ChecksumAlgorithm,
        seed: i32,
        out_path: &std::path::Path,
    ) -> (Vec<u8>, u64) {
        let out = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(out_path)
            .unwrap();
        let config = DeltaApplyConfig::default();
        let verifier = ChecksumVerifier::for_algorithm_seeded(algo, seed);
        let mut applicator = DeltaApplicator::new(out, &config, verifier, None, None).unwrap();
        let mut token_reader = TokenReader::new(None).unwrap();
        let mut cursor = Cursor::new(delta_wire.to_vec());
        apply_delta_stream(&mut cursor, &mut applicator, &mut token_reader).unwrap();
        let (_out, result) = applicator.finish(&mut cursor, None).unwrap();
        let mut bytes = Vec::new();
        std::fs::File::open(out_path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        (bytes, result.literal_bytes)
    }

    /// A minimal writer that captures the receiver's request-half bytes and
    /// provides a no-op `MsgInfoSender`.
    struct CaptureWriter(Vec<u8>);
    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl crate::writer::MsgInfoSender for CaptureWriter {
        fn send_msg_info(&mut self, _data: &[u8]) -> io::Result<()> {
            Ok(())
        }
    }

    /// The async per-file receiver leg commits a destination file byte-identical
    /// to the sync reconstruct path over the same delta stream, and reports the
    /// same `literal_bytes` - even when the wire is delivered one byte per poll.
    #[test]
    fn async_receiver_file_matches_sync_reconstruct() {
        let algo = ChecksumAlgorithm::MD5;
        let seed = 0;
        let literals: &[&[u8]] = &[b"hello world ", &[0xABu8; 300], b" tail literal"];
        let delta_wire = encode_literal_wire(literals, algo, seed);

        let dir = tempdir().unwrap();
        let ref_out = dir.path().join("sync_ref.bin");
        let (expected_bytes, expected_literal) = sync_reference(&delta_wire, algo, seed, &ref_out);

        for chunk in [1usize, 3, delta_wire.len().max(1)] {
            let dest_dir = dir.path().join(format!("dest_{chunk}"));
            std::fs::create_dir_all(&dest_dir).unwrap();

            let entry = FileEntry::new_file("file.bin".into(), expected_bytes.len() as u64, 0o644);
            let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
            ctx.file_list = vec![entry.clone()];

            let ndx = ctx.flat_to_wire_ndx(0);
            let echo_wire = build_echo_wire(ndx, &delta_wire);

            let mut reader = ChunkedReader {
                inner: Cursor::new(echo_wire),
                chunk: chunk.max(1),
            };
            let mut writer = CaptureWriter(Vec::new());
            let mut ndx_write_codec = MonotonicNdxWriter::new(32);
            let mut ndx_read_codec = create_ndx_codec(32);
            let mut token_reader = TokenReader::new(None).unwrap();

            let metadata_opts = metadata::MetadataOptions::default();
            let async_ctx = AsyncFileContext {
                dest_dir: &dest_dir,
                metadata_opts: &metadata_opts,
                checksum_length: std::num::NonZeroU8::new(2).unwrap(),
                checksum_algorithm: signature::SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                },
                acl_cache: None,
                acl_id_map: None,
                #[cfg(unix)]
                sandbox: None,
            };

            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            let outcome = rt
                .block_on(async {
                    ctx.receive_file_async(
                        &mut reader,
                        &mut writer,
                        &entry,
                        0,
                        &mut ndx_write_codec,
                        &mut ndx_read_codec,
                        &mut token_reader,
                        &async_ctx,
                    )
                    .await
                })
                .unwrap_or_else(|e| panic!("chunk={chunk}: async receive failed: {e}"));

            assert!(
                outcome.transferred,
                "chunk={chunk}: file must be transferred"
            );
            assert_eq!(
                outcome.literal_bytes, expected_literal,
                "chunk={chunk}: literal_bytes must match sync path"
            );

            let mut committed = Vec::new();
            std::fs::File::open(dest_dir.join("file.bin"))
                .unwrap()
                .read_to_end(&mut committed)
                .unwrap();
            assert_eq!(
                committed, expected_bytes,
                "chunk={chunk}: async committed file must be byte-identical to sync"
            );
        }
    }

    /// A corrupted trailing checksum makes the async leg reject with
    /// `InvalidData`, exactly like the sync `finish` - the async path must not
    /// silently accept a bad digest.
    #[test]
    fn async_receiver_rejects_bad_checksum() {
        let algo = ChecksumAlgorithm::MD5;
        let seed = 0;
        let literals: &[&[u8]] = &[b"payload bytes"];
        let mut delta_wire = encode_literal_wire(literals, algo, seed);
        let last = delta_wire.len() - 1;
        delta_wire[last] ^= 0xFF;

        let dir = tempdir().unwrap();
        let dest_dir = dir.path().join("dest");
        std::fs::create_dir_all(&dest_dir).unwrap();

        let entry = FileEntry::new_file("bad.bin".into(), 13, 0o644);
        let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
        ctx.file_list = vec![entry.clone()];
        let ndx = ctx.flat_to_wire_ndx(0);
        let echo_wire = build_echo_wire(ndx, &delta_wire);

        let mut reader = ChunkedReader {
            inner: Cursor::new(echo_wire),
            chunk: 1,
        };
        let mut writer = CaptureWriter(Vec::new());
        let mut ndx_write_codec = MonotonicNdxWriter::new(32);
        let mut ndx_read_codec = create_ndx_codec(32);
        let mut token_reader = TokenReader::new(None).unwrap();
        let metadata_opts = metadata::MetadataOptions::default();
        let async_ctx = AsyncFileContext {
            dest_dir: &dest_dir,
            metadata_opts: &metadata_opts,
            checksum_length: std::num::NonZeroU8::new(2).unwrap(),
            checksum_algorithm: signature::SignatureAlgorithm::Md5 {
                seed_config: checksums::strong::Md5Seed::none(),
            },
            acl_cache: None,
            acl_id_map: None,
            #[cfg(unix)]
            sandbox: None,
        };

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt
            .block_on(async {
                ctx.receive_file_async(
                    &mut reader,
                    &mut writer,
                    &entry,
                    0,
                    &mut ndx_write_codec,
                    &mut ndx_read_codec,
                    &mut token_reader,
                    &async_ctx,
                )
                .await
            })
            .expect_err("async leg must reject a corrupted checksum");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
