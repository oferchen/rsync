# io_uring send data path: basis READ_FIXED + socket SEND_ZC

Tracking task: IUD-3 (oc-rsync follow-up #2363). Implementation phases
are keyed to IUD-6 (basis reader), IUD-7 (socket writer pinning), and
IUD-8 (telemetry + rollout, shared with IUD-2).

Companion docs already in tree:

- `docs/design/iouring-send-zc.md` (#1832) - SEND_ZC primitive and
  the existing `IoUringSocketWriter`.
- `docs/design/iouring-borrowed-slice-consumer.md` (#4218) - pin-counted
  pool that the SEND_ZC notification CQE depends on.
- `docs/design/iouring-registered-buffer-adaptive-sizing.md` - registered
  buffer group sizing and lifecycle.
- `docs/design/iouring-receive-data-path.md` (IUD-2, #2362) - sibling
  doc; the registered-buffer pool design is shared.
- `docs/design/mmap-vs-sqpoll-conflict-resolution.md` - rules out
  mmap-backed bytes inside io_uring SQEs, which constrains the basis
  read path here.
- `docs/design/basis-file-io-policy.md` - selector matrix that switches
  basis reads between `BufferedMap`, `MmapStrategy`, and (under this
  proposal) `RegisteredBufferGroup`.

This document does not change any wired dispatch. It specifies how
delta-source bytes get from the basis file (or the source file) onto
the network without leaving the kernel: the basis file is read via
`IORING_OP_READ_FIXED` into registered buffers, and those same buffers
are handed straight to `IORING_OP_SEND_ZC` against the network socket.
The actual switch is gated by the new feature `iouring-data-sends`
(default off) and lands as the patches in section 6.

## 1. Current send reader + socket write

The send role lives in `crates/transfer/src/generator/`. The relevant
streaming entry points for file data are:

### 1.1 Whole-file send

- `crates/transfer/src/generator/delta.rs:200-274` (the loop that
  starts with `let mut total_bytes: u64 = 0`) is the whole-file
  literal stream. It reads from a generic `R: Read` (today an
  `open_source_with_noatime` wrapped in `BufReader`,
  `crates/transfer/src/generator/open_source.rs`) into a reusable
  buffer, computes the running file checksum, and emits framed wire
  chunks via `writer.write_all(&buf[wire_off..wire_off + 4 + chunk])`
  (line 257). The 4-byte length prefix is packed in-place ahead of
  each 32 KiB wire chunk so the combined write triggers the
  `MplexWriter` bulk fast path
  (`crates/protocol/src/multiplex/writer.rs:175-220`,
  `crates/transfer/src/writer/multiplex.rs:104-108`).
- Buffer size is `MAX_READ_SIZE = 256 * 1024`
  (`delta.rs:221`), reused across files.
- `writer` is the `MplexWriter` returned by
  `crates/transfer/src/writer/multiplex.rs:42-60`. Each write
  ultimately reduces to `Write::write_all` on the underlying
  socket / SSH `ChildStdin` (see
  `docs/design/iouring-send-zc.md` section 1.2).

### 1.2 Delta send with basis re-read

- `crates/transfer/src/generator/delta.rs:289-334`
  (`compute_file_checksum`) re-reads basis-file blocks for the running
  whole-file checksum. The read is via `std::fs::File::seek` +
  `read_exact` into a `Vec<u8>` (lines 322-325). On the wire side, the
  per-block emission is the corresponding token in
  `write_token_stream` (`delta.rs:355-380` for `script_to_wire_delta`,
  then `delta.rs:382-463` for the wire emit).
- The streaming literal-token path in
  `crates/transfer/src/generator/delta.rs:227-237` shares the same
  256 KiB buffer when compression is on; the compressed encoder
  (`CompressedTokenEncoder::send_literal`) is opaque to the byte path
  but still ends at a `Write::write_all` on the multiplex writer.

### 1.3 Multiplex framing constraint

`MplexWriter` wraps every payload in a 4-byte `MSG_DATA` header
(`crates/protocol/src/multiplex/writer.rs:70-128`). The header is built
by `send_msg`
(`crates/protocol/src/multiplex/io/send.rs:16-22`) and emitted via
`write_all_vectored` (line 130) which prefers a two-slice `writev`. The
maximum payload per frame is 8192 bytes (`DEFAULT_MAX_FRAME_SIZE`,
`multiplex/writer.rs:88`). Any io_uring proposal that bypasses
`MplexWriter` for the data slice must still emit the header on the
same socket immediately before each payload chunk.

### 1.4 The two memcpy hops we want to eliminate

For a basis-file block re-read or a whole-file literal byte:

1. Kernel page cache -> user buffer via `read(2)` (kernel-side copy
   into `read_buf`).
2. `MplexWriter` buffer fill -> SQE / `writev` slice
   (`writer/multiplex.rs:42-108`). For chunks above the 32 KiB internal
   threshold this is a pointer hand-off (no copy); below the threshold
   it is a memcpy into the writer's buffer.
3. Kernel-side: socket layer copies the user buffer into the skbuff
   chain on `send(2)`. This is the copy that `IORING_OP_SEND_ZC`
   eliminates by pinning the user pages and emitting the notification
   CQE only after the NIC has consumed them
   (`docs/design/iouring-send-zc.md` section 2).

`SEND_ZC` alone removes hop 3. This design also removes hop 1 by
issuing the basis read as `IORING_OP_READ_FIXED` directly into the
same registered buffer that hop 3 will SEND_ZC. End-to-end the user
buffer becomes a pinned, registered region that is referenced (not
copied) by both the disk read SQE and the socket send SQE.

## 2. Proposed: end-to-end zero-copy on the Linux io_uring path

### 2.1 Topology

The same registered-buffer pool from IUD-2 (see
`docs/design/iouring-receive-data-path.md` section 2.1) is shared with
the send role. The sender owns a `SendBufferGroup` view over the pool:
the same `RegisteredBufferSlotInfo` records, but with the inverse
ping-pong (slots filled by READ_FIXED, drained by SEND_ZC).

The sender holds a single `IoUringSendCoordinator` that bundles:

- A reference to the registered-buffer pool.
- A handle to the basis file's fd (when delta mode) or the source
  file's fd (when whole-file mode), both registered with the ring via
  `try_register_fd`
  (`crates/fast_io/src/io_uring/batching.rs::try_register_fd`).
- A reference to the existing `IoUringSocketWriter`
  (`crates/fast_io/src/io_uring/socket_writer.rs:36-48`).
- The `MplexWriter`'s buffer (for emitting the 4-byte `MSG_DATA`
  header alongside the payload, see section 2.3).

### 2.2 Basis / source read via `IORING_OP_READ_FIXED`

For both `compute_file_checksum`'s basis-block reads
(`delta.rs:289-334`) and the whole-file streaming loop
(`delta.rs:200-274`), the read is replaced with
`submit_read_fixed_batch`
(`crates/fast_io/src/io_uring/registered_buffers/submit.rs:29-152`).
The helper already understands short reads, out-of-order CQEs, and
fixed-fd dispatch via `maybe_fixed_file`. The caller supplies:

- `fd` = the registered-fd token for the file (or the raw fd if
  registration was rejected),
- `output` = the caller's `Vec<u8>` for the no-fixed-buffer fallback,
- `base_offset` = the file offset of the first byte,
- `slots` = the registered-buffer slot info from the pool.

On the read-fixed path the kernel writes bytes directly into the
pinned, registered memory. `submit_read_fixed_batch` then memcpys from
the registered slot into the caller's `output` slice; for the
end-to-end zero-copy path we instead skip that final memcpy and keep
the slot itself as the unit handed onward to SEND_ZC. A new helper
`submit_read_fixed_batch_pinned` returns the filled slots without
copying out (the existing helper stays for callers that need the
copy-back semantics).

### 2.3 Socket send via `IORING_OP_SEND_ZC` with header coalescing

The existing `IoUringSocketWriter::submit_send`
(`crates/fast_io/src/io_uring/socket_writer.rs:83-106`) already routes
payloads above 16 KiB through `send_zc::try_send_zc`. The send-side
data-path proposal calls into the same primitive, but with two
adjustments:

1. **Header + payload as one SEND_ZC.** Each wire chunk needs its
   4-byte `MSG_DATA` header to precede the payload bytes on the socket.
   Two paths:
   - **Pre-pend header inside the slot.** Each registered slot
     reserves 4 leading bytes; the basis read targets `slot.ptr + 4`
     with `want = chunk_size - 4`; SEND_ZC submits the full
     `[header][payload]` range. The header is written via a
     write-then-submit pattern, identical to the in-place packing trick
     already used in `delta.rs:251-258`.
   - **Submit two-buffer SEND_ZC.** `IORING_OP_SEND_ZC` supports a
     `msghdr` with multiple iovecs via `IORING_OP_SENDMSG_ZC`
     (kernel 6.1+). Use this path on kernels that advertise the
     opcode; fall back to the in-slot header pre-pend otherwise.

   The pre-pend path is the default. SENDMSG_ZC is an optimisation
   that section 6's IUD-7 patches can adopt opportunistically.

2. **Frame size bound is unchanged.** `MplexWriter::DEFAULT_MAX_FRAME_SIZE`
   is 8192 bytes. The slot size must therefore stay at or below
   `8192 + 4 = 8196` bytes when used for the send path, even when the
   receive path (IUD-2) prefers larger slots. The pool keeps the slot
   size at the larger value but the sender slices the payload into
   8192-byte chunks before submission. This is the same constraint that
   the existing `MplexWriter` already enforces
   (`crates/protocol/src/multiplex/writer.rs:179-195`).

### 2.4 Per-chunk lifecycle

For each 8 KiB wire chunk under the new path:

1. `IoUringSendCoordinator` claims one registered slot.
2. It submits an `IORING_OP_READ_FIXED` SQE for the basis-file range
   (delta mode) or the source-file range (whole-file mode) into
   `slot.ptr + 4`, with `want = chunk_size - 4`.
3. On the read CQE, the coordinator writes the 4-byte `MSG_DATA`
   header into `slot.ptr[..4]`.
4. It submits an `IORING_OP_SEND_ZC` SQE for `slot.ptr..slot.ptr + 4 +
   actual_read`. Pin count on the slot is incremented.
5. On the send notification CQE (`IORING_CQE_F_NOTIF`), pin count is
   decremented. When it hits zero, the slot returns to the pool.

The submission for step 4 can be link-chained from step 2 via
`IOSQE_IO_LINK` on kernels that support it, so a single
`submit_and_wait` services the round-trip. The infrastructure for that
is in `crates/fast_io/src/io_uring/linked_chain.rs`; section 6 cites
the patch that wires it.

### 2.5 Dispatch surface

The sender currently passes a `MplexWriter<TcpStream>` (or `<SshChild>`
etc.) into the streaming functions in `delta.rs`. The new path is opt-
in: when `feature = "iouring-data-sends"` is on AND the underlying
writer is a real TCP socket (not SSH) AND the registered-buffer pool
exists AND the file size exceeds `IOURING_DATA_SENDS_MIN_BYTES`, the
streaming functions take a different code path:

```text
match send_coordinator {
    Some(coord) => coord.stream_basis_zero_copy(writer, source_fd, ...),
    None        => existing_buffered_path(writer, &mut source, ...),
}
```

The existing path is untouched. The new path is the only consumer of
`stream_basis_zero_copy`. There is no scenario in which the new path
silently degrades a transfer that previously used the buffered
streamer.

## 3. Multiplex framing constraint: tag header

The 4-byte `MSG_DATA` header (tag `MessageCode::Data + MPLEX_BASE = 7`,
24-bit little-endian payload length) is the only invariant that
distinguishes a multiplex socket from a raw socket. The send-side
proposal preserves it via the in-slot pre-pend described in section
2.3:

- The slot layout is `[4 bytes header][N bytes payload]`.
- The payload length stored in bytes 1-3 is the actual basis-read
  count (may be short on EOF, NFS partial reads, etc.).
- The header is written after the read CQE settles, so the length is
  always correct.
- For shorts, the helper resubmits a smaller payload-only chunk in a
  follow-up slot. The header is recomputed for the follow-up's actual
  length. This matches `submit_read_fixed_batch`'s loop structure
  (`submit.rs:48-138`).

Control messages (`MessageCode::Info`, `MessageCode::Warning`, etc.)
continue to flow through the existing `MplexWriter::write_message`
path. The io_uring send coordinator only touches `MSG_DATA` frames;
mixing the two streams on the same socket is forced sequential by the
existing `MplexWriter` API which flushes the data buffer before
emitting a control message
(`crates/protocol/src/multiplex/writer.rs:197-220`).

## 4. Compression interaction

When compression is enabled (`-z`, `--compress-choice=zstd`), the
literal byte stream is fed into `CompressedTokenEncoder::send_literal`
(`crates/transfer/src/generator/delta.rs:227-237`) which produces
compressed framing that is not byte-aligned with the read chunks. The
end-to-end zero-copy path does not apply: compression by definition
copies-and-transforms. The selector therefore gates on
`compression.is_none()` in addition to the conditions in section 2.5.

For the matched (block-copy) path under compression, the
`see_token()` calls into the deflate dictionary are still required
(`delta.rs:382-463`). The basis-read via READ_FIXED is still useful
because it removes hop 1's memcpy even if hop 3 stays on regular
SEND. Section 6 sequences this as an IUD-7 follow-up.

## 5. Feature flag and configuration

`iouring-data-sends` (default off) lives on the `fast_io` crate and is
re-exported by `transfer`:

```text
# fast_io/Cargo.toml
iouring-data-sends = ["io_uring"]

# transfer/Cargo.toml
iouring-data-sends = ["io_uring", "fast_io/iouring-data-sends"]
```

The runtime selector consults `OC_RSYNC_IOURING_DATA_SENDS`
(`auto` / `force` / `off`), parallel to the IUD-2 env var.
`IOURING_DATA_SENDS_MIN_BYTES = 64 * 1024` by default; SEND_ZC's own
floor inside `IoUringSocketWriter` is 16 KiB
(`crates/fast_io/src/io_uring/socket_writer.rs:18-20`), so the
combined gate honours both thresholds (the higher wins).

Kernel requirements:

- `IORING_OP_READ_FIXED` since Linux 5.6, like the receive path.
- `IORING_OP_SEND_ZC` since Linux 6.0
  (`crates/fast_io/src/io_uring/send_zc.rs::is_supported`).
- `IORING_OP_SENDMSG_ZC` since Linux 6.1 (optional, header coalescing).
- `IOSQE_IO_LINK` since Linux 5.5 (always available when io_uring is).

When `SEND_ZC` is unsupported, the path falls back to regular
`IORING_OP_SEND` against the same registered buffers, preserving the
basis-read savings even when the socket-send savings are unavailable.

## 6. Implementation plan

The work is split into five PR-sized steps, keyed to IUD-6 (basis
reader), IUD-7 (socket writer pinning), and IUD-8 (telemetry +
rollout, shared with IUD-2).

1. **IUD-6a: `submit_read_fixed_batch_pinned` helper.** Add the
   no-copy-out variant of the existing helper in
   `crates/fast_io/src/io_uring/registered_buffers/submit.rs`. Returns
   `Vec<(RegisteredBufferSlotInfo, bytes_read)>` for the caller to
   consume. Reuse the short-read loop verbatim from
   `submit_read_fixed_batch:48-138`. Tests: extend
   `crates/fast_io/src/io_uring/registered_buffers/tests/` with
   short-read and out-of-order CQE coverage.

2. **IUD-6b: `BasisReader::read_pinned`.** New trait method (or a free
   function alongside `crates/transfer/src/map_file/mod.rs`) that the
   delta-emit loop calls instead of `MapFile::map_ptr`. Returns a
   pinned slot rather than a `&[u8]`. The buffered and mmap variants
   implement it as "allocate a Vec, fill it, wrap in a fake slot
   handle"; only the new `RegisteredBufferGroup`-backed variant
   actually pins. The selector matrix from
   `docs/design/basis-file-io-policy.md` gains a sixth column
   `iouring_send_data_active`; when true and other constraints pass,
   the matrix returns `RB` (registered buffers) for the basis read.

3. **IUD-7a: `IoUringSendCoordinator` skeleton.** New type in
   `crates/fast_io/src/io_uring/send_coordinator.rs` that owns a ring
   shared with the basis reader and a registered-fd slot for the
   socket. Exposes `submit_read_then_send` that link-chains
   `READ_FIXED` and `SEND_ZC` SQEs (or runs them sequentially when
   `IOSQE_IO_LINK` is rejected). Pin-count tracking follows the
   borrowed-slice consumer design (#4218).

4. **IUD-7b: Wire the coordinator into `delta.rs`.** Add the
   selector at `crates/transfer/src/generator/delta.rs:200-274`
   (whole-file literal stream) and at `delta.rs:289-334`
   (basis re-read). The coordinator is passed through the call chain
   the same way the existing `MultiplexWriter` is. Compression
   gate per section 4. Maintain wire compatibility: byte stream
   on the socket is byte-identical to the buffered path for the same
   input.

5. **IUD-8 (shared with IUD-2): Telemetry + rollout.** Wire counters
   for: sends routed via SEND_ZC + READ_FIXED, sends routed via
   SEND_ZC only (read fell back to buffered), sends routed via plain
   SEND (SEND_ZC unsupported), SENDMSG_ZC adoption, pin-count
   exhaustion fallbacks. Single-line `debug_log!(Io, 1, ..)` on the
   first degradation transition per process. Bench harness:
   `crates/fast_io/benches/iouring_data_sends.rs` measuring against
   the matched workloads in `crates/fast_io/Cargo.toml:111-160`.
   Flip `OC_RSYNC_IOURING_DATA_SENDS` default to `auto` once
   benchmarks demonstrate non-regression.

## 7. Interaction with IUD-2 (receive path)

A given oc-rsync process is either a sender or a receiver on a given
transfer; the two roles never share a registered-buffer pool within
one transfer. The pools are conceptually identical (same slot layout,
same registration call) and the implementation reuses
`RegisteredBufferGroup`
(`crates/fast_io/src/io_uring/registered_buffers/registry.rs`), but
they are distinct instances owned by distinct threads:

- Receive role: the disk-commit thread holds the pool; the network
  thread checks slots out for fill, the disk thread checks them in
  for WRITE_FIXED.
- Send role: the sender thread holds the pool; READ_FIXED checks them
  in from the file, SEND_ZC checks them out to the network.

Crucially, the SQPOLL prohibition from
`docs/design/mmap-vs-sqpoll-conflict-resolution.md` applies to both:
file-backed VMAs (which include any `Mmap`-derived pointer) must never
appear in an SQE issued by the SQPOLL kthread. The registered-buffer
slots are anonymous, page-aligned `Vec<u8>` allocations, so the SQPOLL
constraint does not bite on the data path itself - but the selector
must still keep the basis-file `MmapStrategy` out of the ring on the
send side, mirroring the receive side's exclusion.
