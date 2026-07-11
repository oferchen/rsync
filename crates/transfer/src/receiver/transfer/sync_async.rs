//! Async (tokio-transfer) twin of the synchronous receiver driver loop.
//!
//! [`ReceiverContext::run_sync_async`] is the `.await` twin of
//! [`run_sync`](super::sync). It reproduces the sync driver's control flow -
//! setup, directory/symlink/missing-args creation, the per-file transfer loop,
//! hardlink creation, `touch_up_dirs`, and finalization - but drives the three
//! wire-facing legs through their async leaves:
//!
//! - setup: [`setup_transfer_async`](ReceiverContext::setup_transfer_async);
//! - per-file: [`receive_file_async`](ReceiverContext::receive_file_async), the
//!   `.await` twin of the sync loop's per-file body (this is the driver that
//!   makes `receive_file_async` live);
//! - finalize: [`finalize_transfer_async`](ReceiverContext::finalize_transfer_async).
//!
//! Every non-IO step - `ensure_relative_parents`, `create_directories`,
//! `create_symlinks`, `process_missing_args_sentinels`, the per-entry skip
//! decisions (non-file / hardlink-follower / min-max size), `create_hardlinks`,
//! and `touch_up_dirs` - is the identical synchronous helper the sync loop
//! calls, and the `TransferStats` are accumulated field-for-field the same way.
//! Only the three wire legs above differ, so for the same wire bytes this
//! produces a byte-identical destination tree and identical `TransferStats`,
//! independent of how the bytes are chunked across `.await` points.
//!
//! Gated on `tokio-transfer` (default off); additive and not wired into
//! [`run`](ReceiverContext::run) yet (that gating is the receiver-routing rung).
//!
//! # Upstream Reference
//!
//! - `receiver.c:720` - `recv_files()` per-file loop (the sync twin mirrors this)

use std::io;

use logging::{PhaseTimer, debug_log, info_log};
use protocol::codec::create_ndx_codec;
use protocol::stats::DeleteStats;

use crate::receiver::quick_check::is_hardlink_follower;
use crate::receiver::stats::TransferStats;
use crate::receiver::{PipelineSetup, ReceiverContext};
use crate::token_reader::TokenReader;

use super::file_async::AsyncFileContext;

impl ReceiverContext {
    /// Async twin of [`run_sync`](Self::run_sync).
    ///
    /// Runs the sequential (non-pipelined) receiver transfer with `.await` on the
    /// wire-facing legs. The request half of each per-file exchange stays a plain
    /// synchronous `Write` (as in [`receive_file_async`](Self::receive_file_async));
    /// only the sender's echoed NDX/attrs, echoed sum-head, delta tokens, trailing
    /// checksum, and the finalization phase/goodbye frames are read via `.await`.
    /// The disk commit (temp-file reconstruct + atomic rename) and all metadata
    /// application run through the identical synchronous helpers the sync loop
    /// uses, so this produces the same committed tree and `TransferStats`.
    ///
    /// The reader flows in as an [`AsyncServerReader`](crate::reader::AsyncServerReader)
    /// (the async twin of the blocking [`ServerReader`](crate::reader::ServerReader)),
    /// whose [`AsyncRead`](tokio::io::AsyncRead) impl yields the same demuxed byte
    /// stream. Every downstream async leaf reads from it uniformly.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:720` - `recv_files()` processes one file at a time
    #[cfg(feature = "tokio-transfer")]
    #[allow(dead_code)]
    pub(in crate::receiver) async fn run_sync_async<R, W>(
        &mut self,
        reader: crate::reader::AsyncServerReader<R>,
        writer: &mut W,
    ) -> io::Result<TransferStats>
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
        W: io::Write + crate::writer::MsgInfoSender + ?Sized,
    {
        let _t = PhaseTimer::new("receiver-transfer-async");
        let (reader, file_count, setup, carry) = self.setup_transfer_async(reader, writer).await?;

        // Prepend the file-list look-ahead carry (demuxed bytes the async flist
        // reader pulled past the end of the list) ahead of the reader so the
        // per-file legs consume them first. These are already-demuxed output of
        // the same reader, so they are chained at the demux-output level (never
        // re-demultiplexed). When the sender flushes the list separately - the
        // common case - `carry` is empty and the chain is a zero-copy passthrough.
        // Without this the async driver would desync on a sender that packs the
        // list and the first per-file response into one multiplex frame.
        let mut reader = tokio::io::AsyncReadExt::chain(std::io::Cursor::new(carry), reader);
        let reader = &mut reader;

        let PipelineSetup {
            dest_dir,
            metadata_opts,
            checksum_length,
            checksum_algorithm,
            acl_cache,
            acl_id_map,
            #[cfg(unix)]
            sandbox,
        } = setup;

        // upstream: receiver.c:653-654 DEBUG_GTE(RECV, 1)
        debug_log!(Recv, 1, "recv_files({}) starting", file_count);

        let mut files_transferred = 0;
        let mut bytes_received = 0u64;

        // First pass: create directories and symlinks from file list.
        // upstream: generator.c:1317-1326 - make_path() for relative_paths
        self.ensure_relative_parents(&dest_dir);
        let mut metadata_errors = self.create_directories(
            &dest_dir,
            &metadata_opts,
            acl_cache.as_deref(),
            acl_id_map.as_deref(),
            writer,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;
        #[cfg(unix)]
        self.create_symlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_symlinks(&dest_dir, writer)?;

        // upstream: generator.c:1348-1354 - missing_args == 2 && file->mode == 0
        // deletes the destination path and skips any creation for the sentinel.
        self.process_missing_args_sentinels(
            &dest_dir,
            #[cfg(unix)]
            sandbox.as_deref(),
        )?;

        let mut ndx_write_codec = protocol::codec::MonotonicNdxWriter::new(self.protocol.as_u8());
        let mut ndx_read_codec = create_ndx_codec(self.protocol.as_u8());

        // upstream: token.c uses a single compression context across all files.
        // For zstd the DCtx must persist across file boundaries (continuous
        // stream), so create the reader once and reuse it across the session.
        let compression = self.negotiated_algorithms.map(|n| n.compression);
        let mut token_reader = TokenReader::new(compression)?;

        let deadline = crate::shared::TransferDeadline::from_system_time(self.config.stop_at);

        // upstream: generator.c:1249 - list-only renders the flist without
        // requesting any file data.
        let list_only_entries = if self.config.flags.list_only {
            self.collect_list_only_entries()
        } else {
            Vec::new()
        };

        // The per-file async leg borrows `&mut self`, which conflicts with an
        // iteration that holds a shared borrow of `self.file_list` across the
        // loop body. Snapshot the entries into an owned vec first (the pipelined
        // path already clones the file list into an Arc for the same reason), so
        // the `.await` call site can take `&mut self` while the loop reads the
        // snapshot. `setup_transfer_async` finished all file-list mutation
        // (sanitize + single-file rename) before returning, so the snapshot is
        // the final list.
        let entries = self.file_list.clone();

        let async_ctx = AsyncFileContext {
            dest_dir: &dest_dir,
            metadata_opts: &metadata_opts,
            checksum_length,
            checksum_algorithm,
            acl_cache: acl_cache.as_ref(),
            acl_id_map: acl_id_map.as_ref(),
            #[cfg(unix)]
            sandbox: sandbox.as_ref(),
        };

        for (file_idx, file_entry) in entries.iter().enumerate() {
            if self.config.flags.list_only {
                break;
            }
            if let Some(ref dl) = deadline {
                if dl.is_reached() {
                    break;
                }
            }

            let relative_path = file_entry.path();
            // upstream: receiver.c:708-709 DEBUG_GTE(RECV, 1)
            debug_log!(Recv, 1, "recv_files({})", relative_path.display());

            if !file_entry.is_file() {
                if file_entry.is_dir()
                    && self.config.flags.verbose
                    && self.config.connection.client_mode
                {
                    if relative_path.as_os_str() == "." {
                        info_log!(Name, 1, "./");
                    } else {
                        info_log!(Name, 1, "{}/", relative_path.display());
                    }
                }
                continue;
            }

            if is_hardlink_follower(file_entry) {
                continue;
            }

            let file_size = file_entry.size();
            if let Some(min_limit) = self.config.file_selection.min_file_size {
                if file_size < min_limit {
                    continue;
                }
            }
            if let Some(max_limit) = self.config.file_selection.max_file_size {
                if file_size > max_limit {
                    continue;
                }
            }

            let outcome = self
                .receive_file_async(
                    reader,
                    writer,
                    file_entry,
                    file_idx,
                    &mut ndx_write_codec,
                    &mut ndx_read_codec,
                    &mut token_reader,
                    &async_ctx,
                )
                .await?;

            metadata_errors.extend(outcome.metadata_errors);
            if outcome.transferred {
                // upstream: io.c:820 stats.total_read only counts bytes read
                // off the wire. Matched-from-basis bytes never traverse the
                // read fd, so exclude them from bytes_received.
                bytes_received += outcome.literal_bytes;
                files_transferred += 1;
            }
        }

        #[cfg(unix)]
        self.create_hardlinks(&dest_dir, sandbox.as_deref(), writer)?;
        #[cfg(not(unix))]
        self.create_hardlinks(&dest_dir, writer)?;

        // upstream: generator.c:2080-2133 - touch_up_dirs() re-applies
        // directory mtimes after file writes clobber them.
        self.touch_up_dirs(&dest_dir);

        self.finalize_transfer_async(reader, writer).await?;

        let total_source_bytes: u64 = self.file_list.iter().map(|e| e.size()).sum();

        Ok(TransferStats {
            files_listed: file_count,
            files_transferred,
            bytes_received,
            bytes_sent: 0,
            total_source_bytes,
            metadata_errors,
            io_error: self.flist_reader_cache.as_ref().map_or(0, |r| r.io_error())
                | self.flist_io_error,
            error_count: 0,
            entries_received: 0,
            directories_created: 0,
            directories_failed: 0,
            files_skipped: 0,
            delete_stats: DeleteStats::new(),
            delete_limit_exceeded: false,
            literal_data: 0,
            matched_data: 0,
            redo_count: 0,
            list_only_entries,
        })
    }
}

#[cfg(all(test, feature = "tokio-transfer"))]
mod async_driver_parity_tests {
    //! Whole-driver sync-vs-async parity for the sequential receiver.
    //!
    //! Records one server->receiver wire (protocol 32, server mode): the file
    //! list, then a per-file echo/delta stream for each transferred regular file,
    //! then the finalization phase-done + goodbye handshake, all wrapped in a
    //! MSG_DATA multiplex frame the receiver's activated input multiplex expects.
    //! The identical recorded wire is fed to both [`ReceiverContext::run_sync`]
    //! (into one temp dest) and [`ReceiverContext::run_sync_async`] (into another,
    //! via a one-byte-per-poll [`ChunkedReader`] to prove poll-correctness), and
    //! the two committed destination trees, the returned [`TransferStats`], and
    //! the Ok/Err outcome are asserted identical.
    //!
    //! Coverage: a directory entry (created by `create_directories`, skipped in
    //! the per-file loop), a fresh regular file reconstructed from a whole-file
    //! literal delta (no basis), and a regular file with a pre-existing
    //! destination basis (so the driver computes and sends a real signature -
    //! the "unchanged file" leg) reconstructed from a literal delta whose bytes
    //! equal the basis. Both files exercise `receive_file_async` end to end; the
    //! finalize leg exercises `finalize_transfer_async`.

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
    /// server mode (not client) means finalize reads no sender stats. This keeps
    /// the recorded wire to flist + per-file echoes + 4 NDX_DONE frames.
    fn config(dest: &Path) -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(dest)],
            ..Default::default()
        }
    }

    /// The flist entries: one directory and two regular files. Sizes match the
    /// literal content each file is reconstructed from.
    fn flist_entries(f1: &[u8], f2: &[u8]) -> Vec<FileEntry> {
        let mut dir = FileEntry::new_directory(PathBuf::from("d"), 0o40755);
        dir.set_mtime(1_700_000_000, 0);
        let mut e1 = FileEntry::new_file(PathBuf::from("d/f1"), f1.len() as u64, 0o100644);
        e1.set_mtime(1_700_000_000, 0);
        let mut e2 = FileEntry::new_file(PathBuf::from("d/f2"), f2.len() as u64, 0o100644);
        e2.set_mtime(1_700_000_000, 0);
        vec![dir, e1, e2]
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
    ///
    /// `async_receiver_driver_matches_sync_output` carries this in a MSG_DATA
    /// frame *separate* from the flist frame, which is how upstream frames the two
    /// phases (the sender flushes the file list before streaming per-file data).
    /// The async receiver no longer depends on that separation:
    /// `read_entry_with_flist_async` fills an ~8 KiB look-ahead `carry` from each
    /// demux read, and the surplus bytes past the end-of-list marker are now
    /// returned by `receive_file_list_async` and prepended to the per-file read
    /// stream by `run_sync_async`. `async_flist_carry_no_byte_loss_shared_frame`
    /// packs the flist and this data into one frame to prove that.
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

    /// Pre-creates the destination basis for the "unchanged file" leg: `d/f2`
    /// already exists with `content`, so the driver finds it, computes a real
    /// signature, and takes the basis branch.
    fn seed_basis(dest: &Path, f2: &[u8]) {
        std::fs::create_dir_all(dest.join("d")).unwrap();
        std::fs::write(dest.join("d/f2"), f2).unwrap();
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
        assert_eq!(
            a.delete_stats.total(),
            b.delete_stats.total(),
            "delete_stats"
        );
        assert_eq!(a.metadata_errors, b.metadata_errors, "metadata_errors");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_receiver_driver_matches_sync_output() {
        let f1: Vec<u8> = {
            let mut v = b"hello f1 ".to_vec();
            v.extend(std::iter::repeat_n(0x5Au8, 200));
            v.extend_from_slice(b" end f1");
            v
        };
        let f2: Vec<u8> = b"original f2 basis contents - unchanged".to_vec();

        let entries = flist_entries(&f1, &f2);
        let flist_wire = encode_flist(&entries);

        // Probe the NDX order on a throwaway receiver (same config shape).
        let probe_dir = tempdir().unwrap();
        let ndx_map = probe_ndx(probe_dir.path(), &flist_wire);
        assert_eq!(ndx_map.len(), 2, "expected two regular-file requests");

        // Map each requested file index to its literal delta. Index 1 = d/f1
        // (fresh), index 2 = d/f2 (basis).
        let requests: Vec<(i32, Vec<u8>)> = ndx_map
            .iter()
            .map(|&(idx, ndx)| {
                let content = if idx == 1 { &f1 } else { &f2 };
                (ndx, encode_literal_wire(content))
            })
            .collect();

        // Two multiplex frames: the flist, then the per-file + finalize data.
        let data_wire = build_data_wire(&requests);
        let mut wire = muxed(&flist_wire);
        wire.extend_from_slice(&muxed(&data_wire));

        // --- sync driver ---
        let sync_dest = tempdir().unwrap();
        seed_basis(sync_dest.path(), &f2);
        let sync_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(sync_dest.path()));
            let reader = ServerReader::new_plain(Cursor::new(wire.clone()));
            let mut writer = CaptureWriter(Vec::new());
            ctx.run_sync(reader, &mut writer)
                .expect("sync driver must succeed")
        };
        let sync_tree = read_tree(sync_dest.path());

        // --- async driver, one byte per poll ---
        let async_dest = tempdir().unwrap();
        seed_basis(async_dest.path(), &f2);
        let async_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(async_dest.path()));
            let reader = AsyncServerReader::new_plain(ChunkedReader::new(wire.clone(), 1));
            let mut writer = CaptureWriter(Vec::new());
            ctx.run_sync_async(reader, &mut writer)
                .await
                .expect("async driver must succeed")
        };
        let async_tree = read_tree(async_dest.path());

        // (a) byte-identical destination trees
        assert_eq!(
            async_tree, sync_tree,
            "async driver committed a different destination tree than sync"
        );
        // Sanity: both files landed with the expected content.
        assert_eq!(std::fs::read(sync_dest.path().join("d/f1")).unwrap(), f1);
        assert_eq!(std::fs::read(sync_dest.path().join("d/f2")).unwrap(), f2);

        // (b) identical TransferStats
        assert_stats_eq(&async_stats, &sync_stats);
        assert_eq!(async_stats.files_transferred, 2);
        assert_eq!(async_stats.bytes_received, (f1.len() + f2.len()) as u64);
    }

    /// The async driver loses no wire bytes when the sender packs the file list
    /// AND the first per-file response into a single multiplex frame (no flush
    /// between the two phases).
    ///
    /// The async flist reader fills an ~8 KiB look-ahead `carry` from each demux
    /// read, so when the per-file bytes ride in the flist's own frame they land
    /// in `carry` after the end-of-list marker is decoded. The sync path never
    /// over-reads. If those surplus bytes were dropped the async per-file read
    /// would start mid-stream and desync; the carry-return path
    /// (`receive_file_list_async` -> `setup_transfer_async` -> `run_sync_async`
    /// prepend) preserves them.
    ///
    /// The same recorded wire - here a *single* MSG_DATA frame holding the flist,
    /// the two per-file echo/delta streams, and the finalize handshake - is fed to
    /// both [`ReceiverContext::run_sync`] and
    /// [`ReceiverContext::run_sync_async`] (one byte per poll), and the committed
    /// trees, [`TransferStats`], and Ok/Err outcome are asserted identical. This
    /// test fails without the carry-return fix (the async driver desyncs) and
    /// passes with it.
    #[tokio::test(flavor = "current_thread")]
    async fn async_flist_carry_no_byte_loss_shared_frame() {
        let f1: Vec<u8> = {
            let mut v = b"hello f1 ".to_vec();
            v.extend(std::iter::repeat_n(0x5Au8, 200));
            v.extend_from_slice(b" end f1");
            v
        };
        let f2: Vec<u8> = b"original f2 basis contents - unchanged".to_vec();

        let entries = flist_entries(&f1, &f2);
        let flist_wire = encode_flist(&entries);

        let probe_dir = tempdir().unwrap();
        let ndx_map = probe_ndx(probe_dir.path(), &flist_wire);
        assert_eq!(ndx_map.len(), 2, "expected two regular-file requests");

        let requests: Vec<(i32, Vec<u8>)> = ndx_map
            .iter()
            .map(|&(idx, ndx)| {
                let content = if idx == 1 { &f1 } else { &f2 };
                (ndx, encode_literal_wire(content))
            })
            .collect();

        let data_wire = build_data_wire(&requests);

        // The whole point: flist AND the per-file + finalize data share ONE
        // multiplex frame - no separator flush between the list and the first
        // per-file response.
        let mut inner = flist_wire.clone();
        inner.extend_from_slice(&data_wire);
        let wire = muxed(&inner);

        // --- sync driver ---
        let sync_dest = tempdir().unwrap();
        seed_basis(sync_dest.path(), &f2);
        let sync_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(sync_dest.path()));
            let reader = ServerReader::new_plain(Cursor::new(wire.clone()));
            let mut writer = CaptureWriter(Vec::new());
            ctx.run_sync(reader, &mut writer)
                .expect("sync driver must succeed")
        };
        let sync_tree = read_tree(sync_dest.path());

        // --- async driver, one byte per poll ---
        let async_dest = tempdir().unwrap();
        seed_basis(async_dest.path(), &f2);
        let async_stats = {
            let mut ctx = ReceiverContext::new_for_test(&handshake(), config(async_dest.path()));
            let reader = AsyncServerReader::new_plain(ChunkedReader::new(wire.clone(), 1));
            let mut writer = CaptureWriter(Vec::new());
            ctx.run_sync_async(reader, &mut writer)
                .await
                .expect("async driver must succeed on a shared flist+data frame")
        };
        let async_tree = read_tree(async_dest.path());

        assert_eq!(
            async_tree, sync_tree,
            "async driver committed a different destination tree than sync \
             when the flist and first per-file response shared one frame"
        );
        assert_eq!(std::fs::read(sync_dest.path().join("d/f1")).unwrap(), f1);
        assert_eq!(std::fs::read(sync_dest.path().join("d/f2")).unwrap(), f2);

        assert_stats_eq(&async_stats, &sync_stats);
        assert_eq!(async_stats.files_transferred, 2);
        assert_eq!(async_stats.bytes_received, (f1.len() + f2.len()) as u64);
    }
}
