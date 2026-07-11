//! Async (tokio-transfer) twin of the pipelined receiver driver loop.
//!
//! [`ReceiverContext::run_pipelined_async`] is the `.await` twin of
//! [`run_pipelined`](super::pipelined). It reproduces the pipelined driver's
//! control flow - setup, directory/symlink/missing-args creation, the optional
//! delete pass, the two-phase (phase-1 + redo) pipeline loop, verbose dir
//! listing, hardlink creation, delayed-updates, `touch_up_dirs`, and
//! finalization - but drives the three wire-facing legs through their async
//! leaves:
//!
//! - setup: [`setup_transfer_async`](ReceiverContext::setup_transfer_async)
//!   (returns the flist look-ahead carry, consumed exactly as `run_sync_async`
//!   does);
//! - per-file pipeline: [`run_pipeline_loop_decoupled_async`](ReceiverContext::run_pipeline_loop_decoupled_async),
//!   the `.await` twin of the network-read side of the sync pipeline loop, which
//!   still hands each reconstructed chunk to the same synchronous SPSC->disk
//!   thread;
//! - finalize: [`finalize_transfer_async`](ReceiverContext::finalize_transfer_async).
//!
//! Every non-IO step - `ensure_relative_parents`, `create_directories`,
//! `create_symlinks`, `process_missing_args_sentinels`, the delete pass,
//! `build_files_to_transfer`, `create_hardlinks`, `handle_delayed_updates`, and
//! `touch_up_dirs` - is the identical synchronous helper the sync driver calls,
//! and the `TransferStats` are accumulated field-for-field the same way. Only the
//! three wire legs above differ, so for the same wire bytes this produces a
//! byte-identical destination tree and identical `TransferStats`, independent of
//! how the bytes are chunked across `.await` points.
//!
//! Gated on `tokio-transfer` (default off); additive and not wired into
//! [`run`](ReceiverContext::run) yet (that gating is the receiver-routing rung).
//!
//! # Upstream Reference
//!
//! - `receiver.c:720` - `recv_files()` main loop
//! - `generator.c:2157-2163` - phase 1 vs phase 2 checksum length

use std::io;
use std::path::PathBuf;

use logging::{PhaseTimer, debug_log, info_log};
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;

use crate::pipeline::PipelineConfig;
use crate::receiver::stats::TransferStats;
use crate::receiver::{REDO_CHECKSUM_LENGTH, ReceiverContext};

impl ReceiverContext {
    /// Async twin of [`run_pipelined`](Self::run_pipelined).
    ///
    /// Runs the two-phase pipelined receiver with `.await` on the wire-read legs.
    /// The request half of each per-file exchange stays a plain synchronous
    /// `Write`; only the sender's echoed NDX/attrs, echoed sum-head, delta
    /// tokens, trailing checksums, and the finalization phase/goodbye frames are
    /// read via `.await`. The SPSC channel and the dedicated background
    /// disk-commit thread are unchanged from the sync path, so this produces the
    /// same committed tree and `TransferStats`.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` main loop
    #[cfg(feature = "tokio-transfer")]
    #[allow(dead_code)]
    pub(in crate::receiver) async fn run_pipelined_async<R, W>(
        &mut self,
        reader: crate::reader::AsyncServerReader<R>,
        writer: &mut W,
        pipeline_config: PipelineConfig,
    ) -> io::Result<TransferStats>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: io::Write + crate::writer::MsgInfoSender + ?Sized,
    {
        let _t = PhaseTimer::new("receiver-transfer-pipelined-async");
        let (reader, file_count, mut setup, carry) =
            self.setup_transfer_async(reader, writer).await?;

        // Prepend the file-list look-ahead carry (demuxed bytes the async flist
        // reader pulled past the end of the list) ahead of the reader so the
        // per-file legs consume them first. Identical to `run_sync_async`.
        let mut reader = tokio::io::AsyncReadExt::chain(std::io::Cursor::new(carry), reader);
        let reader = &mut reader;

        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&setup.dest_dir);
        let mut metadata_errors = self.create_directories(
            &setup.dest_dir,
            &setup.metadata_opts,
            setup.acl_cache.as_deref(),
            setup.acl_id_map.as_deref(),
            writer,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        )?;
        #[cfg(unix)]
        self.create_symlinks(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&setup.dest_dir, writer)?;

        // upstream: generator.c:1348-1354 - missing_args == 2 && file->mode == 0
        // deletes the destination path and skips any creation for the sentinel.
        self.process_missing_args_sentinels(
            &setup.dest_dir,
            #[cfg(unix)]
            setup.sandbox.as_deref(),
        )?;

        // upstream: receiver.c:653-654 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        let mut delete_stats = DeleteStats::new();
        let mut delete_limit_exceeded = false;
        let mut delete_io_error: i32 = 0;
        if self.config.flags.delete {
            let (ds, exceeded, io_bits) = self.delete_extraneous_files(
                &setup.dest_dir,
                #[cfg(unix)]
                setup.sandbox.as_ref(),
                writer,
            )?;
            delete_stats = ds;
            delete_limit_exceeded = exceeded;
            delete_io_error = io_bits;
            // Carry the per-type counters into the receiver context so the
            // goodbye handshake can emit NDX_DEL_STATS to the peer sender.
            // upstream: generator.c:2393-2398 - early write_del_stats() emission.
            self.pending_del_stats = delete_stats;
        }

        let mut stats = TransferStats {
            files_listed: file_count,
            entries_received: file_count as u64,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error
                | delete_io_error,
            ..Default::default()
        };
        let files_to_transfer = self.build_files_to_transfer(
            writer,
            &setup.dest_dir,
            &setup.metadata_opts,
            None,
            &mut metadata_errors,
            &mut stats,
            setup.acl_cache.as_deref(),
            setup.acl_id_map.as_deref(),
        );

        let mut files_transferred: usize = 0;
        let mut bytes_received: u64 = 0;
        let mut literal_data: u64 = 0;
        let mut matched_data: u64 = 0;
        let mut redo_count: usize = 0;
        let mut all_delayed_updates: Vec<(PathBuf, PathBuf)> = Vec::new();

        // upstream: generator.c:1249 - list-only renders every flist entry via
        // list_file_entry() and sends NO per-file NDX request. This branch must
        // precede the dry_run check: list-only is not dry-run.
        if self.config.flags.list_only {
            stats.list_only_entries = self.collect_list_only_entries();
            writer.flush()?;
        } else if self.config.flags.dry_run {
            self.run_dry_run_loop_async(reader, writer, &files_to_transfer)
                .await?;
        } else {
            let total_files = files_to_transfer.len();
            let redo_config = pipeline_config.clone();
            let mut no_progress: Option<&mut dyn crate::TransferProgressCallback> = None;
            let redo_indices;
            let delayed;
            (
                files_transferred,
                bytes_received,
                literal_data,
                matched_data,
                redo_indices,
                delayed,
            ) = self
                .run_pipeline_loop_decoupled_async(
                    reader,
                    writer,
                    pipeline_config,
                    &setup,
                    files_to_transfer,
                    &mut metadata_errors,
                    false,
                    total_files,
                    &mut no_progress,
                )
                .await?;
            all_delayed_updates.extend(delayed);

            // Phase 2: redo pass for files that failed checksum verification.
            redo_count = redo_indices.len();
            if !redo_indices.is_empty() {
                setup.checksum_length = REDO_CHECKSUM_LENGTH;

                // upstream: generator.c:1926 - the phase-2 redo re-itemizes with
                // ITEM_TRANSFER; the basis comparison is not re-run for the retry.
                let redo_files: Vec<(usize, &FileEntry, PathBuf, u32)> = redo_indices
                    .iter()
                    .filter_map(|&idx| {
                        self.file_list.get(idx).map(|entry| {
                            let p = entry.path();
                            let file_path = if p.as_os_str() == "." {
                                setup.dest_dir.clone()
                            } else {
                                setup.dest_dir.join(p)
                            };
                            (
                                idx,
                                entry,
                                file_path,
                                crate::generator::ItemFlags::ITEM_TRANSFER,
                            )
                        })
                    })
                    .collect();

                let (redo_transferred, redo_bytes, redo_literal, redo_matched, _, redo_delayed) =
                    self.run_pipeline_loop_decoupled_async(
                        reader,
                        writer,
                        redo_config,
                        &setup,
                        redo_files,
                        &mut metadata_errors,
                        true,
                        total_files,
                        &mut no_progress,
                    )
                    .await?;

                files_transferred += redo_transferred;
                bytes_received += redo_bytes;
                literal_data += redo_literal;
                matched_data += redo_matched;
                all_delayed_updates.extend(redo_delayed);
            }
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            for file_entry in &self.file_list {
                if file_entry.is_dir() {
                    let relative_path = file_entry.path();
                    if relative_path.as_os_str() == "." {
                        info_log!(Name, 1, "./");
                    } else {
                        info_log!(Name, 1, "{}/", relative_path.display());
                    }
                }
            }
        }

        #[cfg(unix)]
        self.create_hardlinks(&setup.dest_dir, setup.sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(&setup.dest_dir, writer)?;

        // upstream: receiver.c:584-585 - handle_delayed_updates() at phase 2
        if !all_delayed_updates.is_empty() {
            let backup_cfg = if self.config.flags.backup {
                Some(crate::disk_commit::BackupConfig {
                    dest_dir: setup.dest_dir.clone(),
                    backup_dir: self.config.backup_dir.as_ref().map(PathBuf::from),
                    suffix: self.config.effective_backup_suffix().into(),
                })
            } else {
                None
            };
            super::handle_delayed_updates(&all_delayed_updates, backup_cfg);
        }

        // upstream: generator.c:2080-2133 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&setup.dest_dir);

        self.finalize_transfer_async(reader, writer).await?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        stats.files_transferred = files_transferred;
        stats.bytes_received = bytes_received;
        stats.literal_data = literal_data;
        stats.matched_data = matched_data;
        stats.total_source_bytes = total_source_bytes;
        if !metadata_errors.is_empty() {
            stats.io_error |= crate::generator::io_error_flags::IOERR_GENERAL;
        }
        stats.metadata_errors = metadata_errors;
        stats.delete_stats = delete_stats;
        stats.delete_limit_exceeded = delete_limit_exceeded;
        stats.redo_count = redo_count;

        Ok(stats)
    }
}

#[cfg(all(test, feature = "tokio-transfer"))]
mod async_pipelined_driver_parity_tests {
    //! Whole-driver sync-vs-async parity for the *pipelined* receiver.
    //!
    //! Records one server->receiver wire (protocol 32, server mode): the file
    //! list, then a per-file echo/delta stream for each transferred regular file
    //! (enough files to fill the pipeline window so the loop actually pipelines),
    //! then the finalization phase-done + goodbye handshake, all wrapped in
    //! MSG_DATA multiplex frames. The identical recorded wire is fed to both
    //! [`ReceiverContext::run_pipelined`] (sync, into one temp dest) and
    //! [`ReceiverContext::run_pipelined_async`] (into another, via a
    //! one-byte-per-poll [`ChunkedReader`] to prove poll-correctness), and the two
    //! committed destination trees, the returned [`TransferStats`], and the
    //! Ok/Err outcome are asserted identical.
    //!
    //! Coverage: a directory entry (created by `create_directories`) plus four
    //! regular files - two fresh (whole-file literal delta, no basis) and two with
    //! pre-existing destination bases (so the driver computes and sends a real
    //! signature) reconstructed from literal deltas whose bytes equal the basis.
    //! Four files force the pipeline window to hold multiple outstanding requests
    //! at once, so the async loop is exercised in its genuinely pipelined regime
    //! (not a single-file degenerate case). The finalize leg exercises
    //! `finalize_transfer_async`; the SPSC->disk-thread hand-off is exercised
    //! end to end for every file.

    use std::ffi::OsString;
    use std::io::{Cursor, Read, Write};
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use protocol::ChecksumAlgorithm;
    use protocol::codec::{MonotonicNdxWriter, NdxCodec, create_ndx_codec};
    use protocol::flist::{FileEntry, FileListWriter};
    use protocol::{MessageCode, ProtocolVersion, send_msg};
    use tempfile::tempdir;
    use tokio::io::{AsyncRead, ReadBuf};

    use crate::config::ServerConfig;
    use crate::delta_apply::ChecksumVerifier;
    use crate::handshake::HandshakeResult;
    use crate::pipeline::PipelineConfig;
    use crate::reader::{AsyncServerReader, ServerReader};
    use crate::receiver::ReceiverContext;
    use crate::receiver::stats::TransferStats;
    use crate::receiver::wire::{SenderAttrs, SumHead};
    use crate::role::ServerRole;

    const PROTOCOL: u8 = 32;
    const ALGO: ChecksumAlgorithm = ChecksumAlgorithm::MD5;
    const SEED: i32 = 0;

    /// Delivers at most `chunk` bytes per `poll_read`, forcing async wire reads
    /// to cross `.await` boundaries mid-token when `chunk == 1`.
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk: usize,
    }

    impl ChunkedReader {
        fn new(data: Vec<u8>, chunk: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk: chunk.max(1),
            }
        }
    }

    impl AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let remaining = self.data.len() - self.pos;
            if remaining == 0 {
                return Poll::Ready(Ok(()));
            }
            let take = remaining.min(self.chunk).min(buf.remaining());
            let start = self.pos;
            let end = start + take;
            buf.put_slice(&self.data[start..end]);
            self.pos = end;
            Poll::Ready(Ok(()))
        }
    }

    /// A capturing writer with a no-op `MsgInfoSender` so the receiver's
    /// request-half bytes are absorbed (the sender side is pre-recorded).
    struct CaptureWriter(Vec<u8>);
    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl crate::writer::MsgInfoSender for CaptureWriter {
        fn send_msg_info(&mut self, _data: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    /// Server-mode protocol-32 receiver rooted at `dest`. `--delete` is off, so
    /// setup reads no wire filter list and finalize emits no `NDX_DEL_STATS`;
    /// server mode (not client) means finalize reads no sender stats.
    fn config(dest: &Path) -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(dest)],
            ..Default::default()
        }
    }

    /// The flist entries: one directory and four regular files under it. Sizes
    /// match the literal content each file is reconstructed from.
    fn flist_entries(contents: &[Vec<u8>]) -> Vec<FileEntry> {
        let mut dir = FileEntry::new_directory(PathBuf::from("d"), 0o40755);
        dir.set_mtime(1_700_000_000, 0);
        let mut entries = vec![dir];
        for (i, c) in contents.iter().enumerate() {
            let mut e =
                FileEntry::new_file(PathBuf::from(format!("d/f{i}")), c.len() as u64, 0o100644);
            e.set_mtime(1_700_000_000, 0);
            entries.push(e);
        }
        entries
    }

    fn encode_flist(entries: &[FileEntry]) -> Vec<u8> {
        let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
        let mut wire = Vec::new();
        let mut writer = FileListWriter::new(protocol);
        for e in entries {
            writer.write_entry(&mut wire, e).unwrap();
        }
        writer.write_end(&mut wire, None).unwrap();
        wire
    }

    /// Encodes a literal-only whole-file delta: each literal as a positive i32-LE
    /// length prefix + bytes, a zero-length terminator, then the trailing
    /// whole-file checksum. Mirrors the sender's `token.c` literal emission.
    fn encode_literal_wire(content: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        let len = i32::try_from(content.len()).unwrap();
        wire.extend_from_slice(&len.to_le_bytes());
        wire.extend_from_slice(content);
        wire.extend_from_slice(&0_i32.to_le_bytes());
        let mut verifier = ChecksumVerifier::for_algorithm_seeded(ALGO, SEED);
        verifier.update(content);
        let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
        let n = verifier.finalize_into(&mut digest);
        wire.extend_from_slice(&digest[..n]);
        wire
    }

    /// Runs `setup_transfer` on a throwaway receiver to learn the exact
    /// post-sanitize file-list order and the wire NDX each regular file will be
    /// requested with, so the recorded echo stream matches what the driver sends.
    fn probe_ndx(dest: &Path, flist_wire: &[u8]) -> Vec<(usize, i32)> {
        let mut ctx = ReceiverContext::new_for_test(&handshake(), config(dest));
        let reader = ServerReader::new_plain(Cursor::new(muxed(flist_wire)));
        let mut sink = std::io::sink();
        let (_r, _count, _setup) = ctx.setup_transfer(reader, &mut sink).unwrap();
        ctx.file_list()
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_file())
            .map(|(idx, _)| (idx, ctx.flat_to_wire_ndx(idx)))
            .collect()
    }

    /// Wraps `inner` in a single MSG_DATA multiplex frame.
    fn muxed(inner: &[u8]) -> Vec<u8> {
        let mut wire = Vec::new();
        send_msg(&mut wire, MessageCode::Data, inner).unwrap();
        wire
    }

    /// Builds the demuxed per-file + finalize payload (everything after the
    /// flist): for each requested file (in driver order) the echoed NDX +
    /// `ITEM_TRANSFER` iflags + an empty echoed sum-head + the literal delta,
    /// then the finalize handshake (3 phase NDX_DONE + 1 goodbye-echo NDX_DONE).
    fn build_data_wire(requests: &[(i32, Vec<u8>)]) -> Vec<u8> {
        let mut data = Vec::new();

        // Per-file echoes share one monotonic NDX writer across files, matching
        // the receiver's single per-loop `ndx_read_codec`.
        let mut echo_ndx = MonotonicNdxWriter::new(PROTOCOL);
        for (ndx, delta) in requests {
            echo_ndx.write_ndx(&mut data, *ndx).unwrap();
            data.extend_from_slice(&SenderAttrs::ITEM_TRANSFER.to_le_bytes());
            SumHead::empty().write(&mut data).unwrap();
            data.extend_from_slice(delta);
        }

        // Finalize reads 4 NDX_DONE via a fresh codec (finalize_transfer builds
        // its own): 3 for the phase exchange (non-inc-recurse proto 32) + 1 for
        // the extended-goodbye echo.
        let mut fin = create_ndx_codec(PROTOCOL);
        for _ in 0..4 {
            fin.write_ndx_done(&mut data).unwrap();
        }
        data
    }

    /// Reads a destination tree into a sorted `(relative_path, bytes)` vector for
    /// byte-exact comparison across the two drivers.
    fn read_tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &Path, root: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut entries: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap())
                .collect();
            entries.sort_by_key(std::fs::DirEntry::path);
            for entry in entries {
                let path = entry.path();
                let ty = entry.file_type().unwrap();
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                if ty.is_dir() {
                    out.push((rel, Vec::new()));
                    walk(&path, root, out);
                } else {
                    let mut bytes = Vec::new();
                    std::fs::File::open(&path)
                        .unwrap()
                        .read_to_end(&mut bytes)
                        .unwrap();
                    out.push((rel, bytes));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out
    }

    /// Pre-creates destination bases for the "unchanged file" legs so the driver
    /// finds them, computes a real signature, and takes the basis branch.
    fn seed_basis(dest: &Path, files: &[(usize, &[u8])]) {
        std::fs::create_dir_all(dest.join("d")).unwrap();
        for (i, content) in files {
            std::fs::write(dest.join(format!("d/f{i}")), content).unwrap();
        }
    }

    fn assert_stats_eq(a: &TransferStats, b: &TransferStats) {
        assert_eq!(a.files_listed, b.files_listed, "files_listed");
        assert_eq!(
            a.files_transferred, b.files_transferred,
            "files_transferred"
        );
        assert_eq!(a.bytes_received, b.bytes_received, "bytes_received");
        assert_eq!(
            a.total_source_bytes, b.total_source_bytes,
            "total_source_bytes"
        );
        assert_eq!(a.literal_data, b.literal_data, "literal_data");
        assert_eq!(a.matched_data, b.matched_data, "matched_data");
        assert_eq!(a.io_error, b.io_error, "io_error");
        assert_eq!(a.error_count, b.error_count, "error_count");
        assert_eq!(a.redo_count, b.redo_count, "redo_count");
        assert_eq!(a.entries_received, b.entries_received, "entries_received");
        assert_eq!(
            a.delete_stats.total(),
            b.delete_stats.total(),
            "delete_stats"
        );
        assert_eq!(a.metadata_errors, b.metadata_errors, "metadata_errors");
    }

    /// The pipelined async driver commits a byte-identical destination tree and
    /// returns identical `TransferStats` to the sync pipelined driver over the
    /// same recorded wire, with multiple files in flight (genuine pipelining) and
    /// the wire delivered one byte per poll.
    #[tokio::test(flavor = "current_thread")]
    async fn async_pipelined_driver_matches_sync_output() {
        // Four regular files: f0/f1 fresh (no basis), f2/f3 unchanged (basis).
        let contents: Vec<Vec<u8>> = vec![
            {
                let mut v = b"fresh f0 ".to_vec();
                v.extend(std::iter::repeat_n(0x11u8, 250));
                v.extend_from_slice(b" end f0");
                v
            },
            {
                let mut v = b"fresh f1 ".to_vec();
                v.extend(std::iter::repeat_n(0x22u8, 300));
                v.extend_from_slice(b" end f1");
                v
            },
            b"unchanged f2 basis contents".to_vec(),
            b"unchanged f3 basis contents - a bit longer than f2".to_vec(),
        ];

        let entries = flist_entries(&contents);
        let flist_wire = encode_flist(&entries);

        // Probe the NDX request order on a throwaway receiver (same config shape).
        let probe_dir = tempdir().unwrap();
        let ndx_map = probe_ndx(probe_dir.path(), &flist_wire);
        assert_eq!(ndx_map.len(), 4, "expected four regular-file requests");

        // Map each requested file index to its literal delta. Flist index i+1 maps
        // to contents[i] (index 0 is the directory `d`).
        let requests: Vec<(i32, Vec<u8>)> = ndx_map
            .iter()
            .map(|&(idx, ndx)| {
                let content = &contents[idx - 1];
                (ndx, encode_literal_wire(content))
            })
            .collect();

        // Two multiplex frames: the flist, then the per-file + finalize data.
        let data_wire = build_data_wire(&requests);
        let mut wire = muxed(&flist_wire);
        wire.extend_from_slice(&muxed(&data_wire));

        // The basis files to pre-seed on both dests: f2 and f3.
        let basis: Vec<(usize, &[u8])> =
            vec![(2usize, &contents[2][..]), (3usize, &contents[3][..])];

        // --- sync pipelined driver ---
        let sync_dest = tempdir().unwrap();
        seed_basis(sync_dest.path(), &basis);
        let sync_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(sync_dest.path()));
            let reader = ServerReader::new_plain(Cursor::new(wire.clone()));
            let mut writer = CaptureWriter(Vec::new());
            ctx.run_pipelined(reader, &mut writer, PipelineConfig::default())
                .expect("sync pipelined driver must succeed")
        };
        let sync_tree = read_tree(sync_dest.path());

        // --- async pipelined driver, one byte per poll ---
        let async_dest = tempdir().unwrap();
        seed_basis(async_dest.path(), &basis);
        let async_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(async_dest.path()));
            let reader = AsyncServerReader::new_plain(ChunkedReader::new(wire.clone(), 1));
            let mut writer = CaptureWriter(Vec::new());
            ctx.run_pipelined_async(reader, &mut writer, PipelineConfig::default())
                .await
                .expect("async pipelined driver must succeed")
        };
        let async_tree = read_tree(async_dest.path());

        // (a) byte-identical destination trees
        assert_eq!(
            async_tree, sync_tree,
            "async pipelined driver committed a different destination tree than sync"
        );
        // Sanity: every file landed with the expected content.
        for (i, content) in contents.iter().enumerate() {
            assert_eq!(
                &std::fs::read(sync_dest.path().join(format!("d/f{i}"))).unwrap(),
                content,
                "sync f{i} content mismatch"
            );
        }

        // (b) identical TransferStats
        assert_stats_eq(&async_stats, &sync_stats);
        assert_eq!(async_stats.files_transferred, 4);
        let total_literal: u64 = contents.iter().map(|c| c.len() as u64).sum();
        assert_eq!(async_stats.bytes_received, total_literal);
    }

    /// BENCHMARK-ONLY correctness gate for the async-bench receiver wiring.
    ///
    /// Drives the same recorded multi-file protocol-32 wire through BOTH the
    /// threaded receiver ([`ReceiverContext::run`], into one temp dest) and the
    /// async-bench receiver ([`ReceiverContext::run_receiver_async_bench`], into
    /// another) and asserts byte-identical destination trees, identical
    /// [`TransferStats`], the same Ok outcome, and no hang. Unlike the driver
    /// parity tests above (in-memory `ChunkedReader`), the async side runs over a
    /// *real* loopback TCP socket on a multi-thread runtime - exactly the path
    /// the daemon benchmark takes - so this exercises the socket split (async
    /// read half via `tokio::net::TcpStream::from_std`, blocking write half via a
    /// separate sink) and the multi-thread `block_on`. A benchmark of a
    /// desyncing or deadlocking path would be worthless, so this must pass before
    /// the bench wiring is trusted.
    ///
    /// The peer delivers the whole recorded wire then closes its write side; the
    /// receiver's request half is absorbed by a separate `CaptureWriter` sink, so
    /// the socket carries only server->receiver bytes and cannot write-write
    /// deadlock. If the async read half stranded wire bytes or the runtime
    /// starved the read, the tree/stats assertions would fail (or the test would
    /// hang under the harness timeout).
    #[cfg(feature = "async-bench")]
    #[test]
    fn async_bench_receiver_matches_threaded_over_real_socket() {
        use std::io::Write as _;
        use std::net::{Shutdown, TcpListener, TcpStream};

        // Four regular files: f0/f1 fresh (no basis), f2/f3 unchanged (basis).
        let contents: Vec<Vec<u8>> = vec![
            {
                let mut v = b"fresh f0 ".to_vec();
                v.extend(std::iter::repeat_n(0x11u8, 250));
                v.extend_from_slice(b" end f0");
                v
            },
            {
                let mut v = b"fresh f1 ".to_vec();
                v.extend(std::iter::repeat_n(0x22u8, 300));
                v.extend_from_slice(b" end f1");
                v
            },
            b"unchanged f2 basis contents".to_vec(),
            b"unchanged f3 basis contents - a bit longer than f2".to_vec(),
        ];

        let entries = flist_entries(&contents);
        let flist_wire = encode_flist(&entries);

        let probe_dir = tempdir().unwrap();
        let ndx_map = probe_ndx(probe_dir.path(), &flist_wire);
        assert_eq!(ndx_map.len(), 4, "expected four regular-file requests");

        let requests: Vec<(i32, Vec<u8>)> = ndx_map
            .iter()
            .map(|&(idx, ndx)| {
                let content = &contents[idx - 1];
                (ndx, encode_literal_wire(content))
            })
            .collect();

        // Two multiplex frames: the flist, then the per-file + finalize data.
        let data_wire = build_data_wire(&requests);
        let mut wire = muxed(&flist_wire);
        wire.extend_from_slice(&muxed(&data_wire));

        let basis: Vec<(usize, &[u8])> =
            vec![(2usize, &contents[2][..]), (3usize, &contents[3][..])];

        // --- threaded receiver (production sync dispatch) over the recorded wire ---
        let sync_dest = tempdir().unwrap();
        seed_basis(sync_dest.path(), &basis);
        let sync_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(sync_dest.path()));
            let reader = ServerReader::new_plain(Cursor::new(wire.clone()));
            let mut writer = CaptureWriter(Vec::new());
            ctx.run(reader, &mut writer, None)
                .expect("threaded receiver must succeed")
        };
        let sync_tree = read_tree(sync_dest.path());

        // --- async-bench receiver over a real loopback socket on a multi-thread rt ---
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut peer = TcpStream::connect(addr).unwrap();
        let (recv_sock, _peer_addr) = listener.accept().unwrap();
        // The recorded wire is a couple of KiB - well within the socket buffer -
        // so writing it up front then closing the write side is deterministic:
        // the bytes stay in the receiver's kernel buffer and the receiver reads
        // them then observes EOF after the finalize handshake.
        peer.write_all(&wire).unwrap();
        peer.flush().unwrap();
        peer.shutdown(Shutdown::Write).unwrap();

        let async_dest = tempdir().unwrap();
        seed_basis(async_dest.path(), &basis);
        // Multi-thread runtime (>= 2 workers), matching the daemon bench path: a
        // blocking write parks one worker while another polls the `.await` read.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let async_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(async_dest.path()));
            let mut writer = CaptureWriter(Vec::new());
            runtime
                .block_on(ctx.run_receiver_async_bench(recv_sock, &mut writer))
                .expect("async-bench receiver must succeed over a real socket")
        };
        let async_tree = read_tree(async_dest.path());
        drop(peer);

        // (a) byte-identical destination trees
        assert_eq!(
            async_tree, sync_tree,
            "async-bench receiver committed a different destination tree than the threaded receiver"
        );
        for (i, content) in contents.iter().enumerate() {
            assert_eq!(
                &std::fs::read(async_dest.path().join(format!("d/f{i}"))).unwrap(),
                content,
                "async-bench f{i} content mismatch"
            );
        }

        // (b) identical TransferStats, (c) no hang (reaching here proves it)
        assert_stats_eq(&async_stats, &sync_stats);
        assert_eq!(async_stats.files_transferred, 4);
        let total_literal: u64 = contents.iter().map(|c| c.len() as u64).sum();
        assert_eq!(async_stats.bytes_received, total_literal);
    }
}
