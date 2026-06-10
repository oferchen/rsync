# WIN-S.LAND.1.c - TransmitFile call-site trace

Follow-up audit to PR #5552 (WIN-S.LAND.1.a). WIN-S.LAND.1.a found that
`crates/fast_io/src/iocp/transmit_file.rs::try_transmit_file` and its
wrapper `IocpSocketWriter::try_transmit_file_path` exist but have no
production callers outside the `fast_io` crate. WIN-S.LAND.1.c was
scoped as the wiring step that would close that gap.

This audit traces the actual sender-side wire path on Windows, asks
where the missing call site would have to live, and concludes that the
correct fix is a multi-PR refactor, not a single dispatch arm. The
task is downgraded to a follow-up rather than executed as a one-shot
wiring change.

## 1. What the Windows sender wire path looks like today

The daemon sender's file-body transmit path (whole-file and delta
literal-run) lives entirely behind two layers of `dyn Write` type
erasure. Tracing the call graph from the bottom up:

| Layer | Type | File |
|---|---|---|
| Wire byte sink | `TcpStream` (or `DaemonStreamWriter::Tcp(TcpStream)`) | `crates/core/src/client/module_list/connect/mod.rs:44` |
| Daemon hand-off | `Box<dyn Write + Send>` | `crates/daemon/src/daemon/sections/module_access/transfer.rs:306` |
| Transfer entry | `&mut dyn Write` | `crates/transfer/src/lib.rs:281` |
| Frame writer | `ServerWriter<W: Write>` -> `MultiplexWriter<W>` | `crates/transfer/src/writer/server.rs:23`, `crates/transfer/src/writer/multiplex.rs:28` |
| Per-frame send | `protocol::send_msg(&mut self.inner, code, payload)` | `crates/protocol/src/multiplex/io/send.rs` |
| Generator call site | `writer.write_all(&buf[wire_off..wire_off + 4 + chunk])` | `crates/transfer/src/generator/delta.rs:270` |

The wire-byte-stream identity (was this a `TcpStream`?) is erased at
the `Box<dyn Write + Send>` boundary in `module_access/transfer.rs:306`.
By the time bytes reach the wire layer, the writer is a generic
`MultiplexWriter<W: Write>` that calls `Write::write_all` on the inner
type-erased writer. Std's `TcpStream::write` on Windows issues a
synchronous `send()` (not `WSASend` with overlapped I/O), so there is
no existing "WriteFile on a socket handle" call site to swap.

## 2. What WIN-S.LAND.1.c asked for vs. what is reachable

The task description asks: "find the sender-side file transmit call
site... look for what calls `WriteFile` on a socket handle... add a
`#[cfg(target_os = "windows")]` dispatch arm that calls
`try_transmit_file` before the fallback."

That dispatch arm cannot be added at any single site in the current
architecture because:

1. **No raw socket handle is in scope at the writer.** The transfer
   crate accepts `&mut dyn Write` and wraps it in `ServerWriter`. The
   wire-layer code (`MultiplexWriter::write`, `protocol::send_msg`)
   has no access to `AsRawSocket`. Downcasting `dyn Write` to
   `TcpStream` is not possible through `dyn Trait` alone (would
   require a new `RawSocketWrite` trait threaded through every layer
   that touches `ServerWriter`).
2. **No file handle is in scope at the writer either.** The sender
   reads file bytes through `Box<dyn Read>` in
   `stream_whole_file_transfer` (`crates/transfer/src/generator/
   delta.rs:212`). The wire layer never sees the source file handle.
   It receives a length-prefixed payload chunk in a `&[u8]` and
   forwards it. TransmitFile takes a `HANDLE` of the source file
   directly; the byte chunk that reaches the wire is already a copy.
3. **The multiplex framer breaks `TransmitFile`'s win.** Each wire
   chunk in `stream_whole_file_transfer` is at most 32 KiB
   (`CHUNK_SIZE`), prefixed by a 4-byte length, and wrapped in a
   4-byte `MSG_DATA` envelope (`MAX_PAYLOAD_LENGTH = 0x00FF_FFFF`
   per `protocol::envelope::constants`). At 32 KiB chunk
   granularity, the per-call `TransmitFile` setup overhead (handle
   validation, OVERLAPPED bookkeeping, kernel mode switch) dominates
   the page-cache-to-NIC DMA saving.

The existing design documents recognise this constraint. The wiring
plan in `docs/design/windows-transmitfile-zerocopy.md` section 8 lays
out five sequential PRs:

1. `OverlappedSocket(SOCKET)` + `SequentialFile(HANDLE)` newtypes.
2. Introduce a `PlatformSendFile` trait and route
   `generator/delta.rs:245-263` through it - **a behaviour-neutral
   refactor that lifts the file -> writer hop to a typed seam**.
3. Add the Windows `WindowsTransmitFile` impl using
   `lpTransmitBuffers.Head` for the 4-byte multiplex header.
4. Add a `TransmitFilePolicy { Auto, Enabled, Disabled }`
   eligibility probe (`GetFileInformationByHandleEx`) and the AV
   warmup benchmark.
5. Flip `Auto` to `Enabled` for whole-file pushes on local NTFS / ReFS
   once the 30% wall-time validation gate from section 5 is met.

Steps 1 and 2 are the architectural prerequisites: until the typed
seam exists, there is no call site to swap. Steps 3-5 are the
TransmitFile primitive wiring proper.

## 3. Why a one-shot dispatch arm is not viable

Three concrete attempts and why each fails:

### 3.1 Swap inside `MultiplexWriter::write`

`MultiplexWriter::write` (`crates/transfer/src/writer/multiplex.rs:112`)
receives `&[u8]`. Even at this layer the file has already been read
into the caller's buffer, so `TransmitFile` (which reads from a
`HANDLE`) cannot be invoked - the byte copy has already happened.

### 3.2 Swap inside `protocol::send_msg`

Same constraint as above: `send_msg` takes a `&[u8]` payload. By the
time we reach the multiplex envelope writer, the bytes are
user-space buffers; no file handle is available.

### 3.3 Swap inside `stream_whole_file_transfer`

This is the only layer with both a `Read` source and a `Write` sink in
scope. The source is `Box<dyn Read>`, not `&File`, so the file handle
is hidden. The sink is `&mut W: Write`, not a typed socket, so the
raw socket handle is hidden. Downcasting both sides would require:

- A `RawSocketWrite` trait propagated through `ServerWriter` /
  `MultiplexWriter` / the daemon's `Box<dyn Write>`.
- A `RawFileRead` accessor on the boxed reader returned by
  `open_source_unbuffered`.
- A 16 MiB chunking loop (since `MSG_DATA` envelopes are capped at
  `MAX_PAYLOAD_LENGTH`).
- A header buffer wired through `lpTransmitBuffers.Head` per chunk.

That is the design doc's step-2 refactor, not a one-line dispatch
arm.

## 4. Verdict and downgrade

**Downgrade WIN-S.LAND.1.c from "wire the primitive" to "land the
architectural prerequisite first."**

Recommended sequencing:

- **WIN-S.LAND.1.c.1** (separate PR, no behaviour change): introduce
  the `PlatformSendFile` trait per `windows-transmitfile-zerocopy.md`
  step 2 and route `stream_whole_file_transfer` through it. Default
  impl is the existing `Read` -> buffer -> `Write` loop. No socket
  identity is required; this step only creates the typed seam that
  WIN-S.LAND.1.c.2 needs.
- **WIN-S.LAND.1.c.2**: introduce `OverlappedSocket` /
  `SequentialFile` newtypes and a Windows `WindowsTransmitFile` impl
  of `PlatformSendFile`. This step wires `try_transmit_file` into the
  trait but does not flip the default policy.
- **WIN-S.LAND.1.c.3**: add the eligibility probe and the AV warmup
  benchmark, gated behind `--io-policy=transmitfile=auto`.
- **WIN-S.LAND.1.c.4**: flip the default once the 30% wall-time
  validation gate from section 5 of the design doc is met on the 10
  GbE / 1 GiB / warm-cache reference workload.

Each of these is its own PR with its own success criteria. None of
them can be collapsed into a single "wire the dispatch" change because
the wiring requires a typed seam that does not exist today.

## 5. What did get verified in this audit

- `crates/fast_io/src/iocp/transmit_file.rs::try_transmit_file` is
  still feature-gated behind `transmitfile` (off by default per
  `crates/fast_io/Cargo.toml:99`). The default `fast_io` build does
  not even compile the module.
- `crates/fast_io/src/iocp/socket.rs:362
  IocpSocketWriter::try_transmit_file_path` is the only consumer of
  the primitive in the workspace. It is itself unreferenced outside
  the `fast_io` crate.
- `IocpSocketWriter` is constructed only by `fast_io`'s own tests
  (`crates/fast_io/src/iocp/socket.rs:557, 653, 697`). Neither
  `crates/transfer/`, `crates/protocol/`, nor `crates/daemon/` builds
  one. There is no production code path on Windows that holds an
  `IocpSocketWriter` to call `try_transmit_file_path` on.

This matches WIN-S.LAND.1.a's finding (PR #5552). The primitive is
production-ready in isolation; the surrounding architecture is what
prevents wiring.

## 6. Risk if WIN-S.LAND.1.c stays open as "wire it"

Re-attempting the wiring as a one-shot will produce one of two bad
outcomes:

- A dispatch arm at `stream_whole_file_transfer` that downcasts
  `Box<dyn Read>` / `&mut dyn Write` to the concrete `File` / `TcpStream`
  via `Any`. This adds runtime cost (`TypeId` comparison per chunk),
  layers in a footgun (any future writer wrapper breaks the fast
  path silently), and still does not handle the multiplex envelope
  granularity correctly.
- A wholesale refactor of `ServerWriter` and the wire layer to expose
  raw handles, smuggled into a "wiring" PR. That is the design doc's
  step 2 by a different name, and conflating it with the wiring
  invites scope creep and regressions in the non-Windows wire path.

The clean path is to land the typed seam first, on its own, with its
own tests, and add the Windows impl on top.

## 7. Inputs

- PR #5552 - `docs/audits/windows-sendfile-recvfd-reachability.md`
- `docs/design/windows-transmitfile-zerocopy.md` sections 3, 4, 8
- `docs/design/win-s2-sendfile-transmitfile-audit.md` sections 3, 4
- `crates/fast_io/src/iocp/transmit_file.rs`
- `crates/fast_io/src/iocp/socket.rs:343-401`
- `crates/transfer/src/writer/server.rs`
- `crates/transfer/src/writer/multiplex.rs`
- `crates/transfer/src/generator/delta.rs:212-287`
- `crates/transfer/src/generator/transfer/transfer_loop.rs:455-495`
- `crates/transfer/src/lib.rs:277-507`
- `crates/daemon/src/daemon/sections/module_access/transfer.rs:300-435`
- `crates/core/src/client/module_list/connect/mod.rs:44-69`
