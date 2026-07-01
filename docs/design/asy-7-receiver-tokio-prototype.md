# ASY-7: Receiver network-read boundary - scoping result (STOP)

Status: Scoping outcome. ASY-7 was chartered to convert the receiver's
network-read boundary (ASY-1 boundary #4) to run natively on tokio
(`.await`) under the `tokio-transfer` feature, replacing the
`block_on`-hosted sync pipeline that ASY-3 scaffolded, while keeping the
default threaded path byte-identical.

This document records the scoping outcome: **the boundary is not
cleanly separable at one rung.** A byte-identical single-boundary
conversion of the receiver socket read is not achievable without a
larger cross-boundary change than one rung, so no tokio-receiver code
was shipped. Per the ASY-7 charter's stop clause, we stop and report
the exact coupling rather than ship a tokio receiver that diverges from
the threaded wire output.

No `.rs` changed. The `tokio-transfer` foundation (ASY-3) is unaffected
and remains default-off.

## 1. What ASY-3 built (the starting point)

ASY-3 (master `5473901d5`) hosts the **entire synchronous**
`transfer::run_server_with_handshake` body inside a single tokio future:

```rust
// crates/transfer/src/pipeline/tokio_driver.rs
fn host_sync_on<R>(handle: &Handle, f: impl FnOnce() -> R) -> R {
    handle.block_on(async move { f() })   // f() is the whole sync server
}
```

`core::session::with_transfer_runtime` (crates/core/src/session.rs)
adopts an ambient runtime or builds a `current_thread` one, then
`block_on`s the sync closure on the calling thread. The sync body still
does blocking `read` on `stdin: &mut dyn Read`; ASY-3 changed only
*where* the sync body runs, not *how* it reads. It is a runtime-hosting
scaffold, not a boundary conversion.

## 2. The boundary and the leaf read

ASY-1 boundary #4 is the receiver's delta-token wire read. Its call
chain (verified on this branch's tree; note the ASY-3 spec's file path
`transfer_ops/response.rs` moved to `transfer_ops/streaming.rs` and the
loop to `receiver/transfer/pipeline.rs` after the #6210 receiver split):

```
run_server_with_handshake                 crates/transfer/src/lib.rs:339
  reader = ServerReader<BufReader<CountingReader<Box<dyn Read>>>>  lib.rs:535-538
  ReceiverContext::run<R: Read>           receiver/transfer.rs:43
    run_pipelined<R: Read>                receiver/transfer/pipelined.rs:33
      setup_transfer<R: Read>             receiver/transfer/setup/context.rs:41
        reader.activate_multiplex()       setup/context.rs:71
      run_pipeline_loop_decoupled<R: Read>  receiver/transfer/pipeline.rs:43
        process_file_response_streaming<R: Read>  transfer_ops/streaming.rs:74
          TokenReader::read_token<R: Read>  token_reader.rs:142
            reader.read_exact(..)         token_reader.rs:146  (plain)
            decoder.recv_token(reader)                          (compressed)
              ServerReader::read          reader/server.rs:229
                MultiplexReader::read     reader/multiplex.rs:339
                  protocol::recv_msg_into(&mut self.inner, ..)  multiplex.rs:368
                    inner BufReader/CountingReader -> OS read
```

## 3. Why the boundary is not separable at one rung

Three independent couplings each force a larger change than "make one
socket read `.await`". Any one of them is sufficient to stop; all three
hold.

### 3.1 The reader is a monomorphized `R: Read`, not a trait object

`R: Read` is threaded as a generic type parameter through **seven**
functions from `ReceiverContext::run` down to `TokenReader::read_token`
(section 2). To swap the leaf read to `AsyncRead`/`.await`, every one of
those seven signatures must become `async fn` simultaneously, because
Rust cannot `.await` from a synchronous `fn`. There is no intermediate
seam where a single `fn` can be converted while its callers stay sync
without wrapping in `block_on` - which reintroduces exactly the
`block_on`-hosted sync shape ASY-3 already has and yields zero async
benefit. This is a whole-call-tree conversion, not one boundary.

### 3.2 The demux read is a stateful sync state machine, not a raw read

`MultiplexReader::read` (reader/multiplex.rs:339) is not a passthrough
socket read. Each call:

- loops calling `protocol::recv_msg_into` until a `MSG_DATA` frame
  arrives, **dispatching control frames as side effects** in between
  (`dispatch_message`, `check_error_exit` at multiplex.rs:370,392):
  MSG_INFO / MSG_ERROR / MSG_LOG emission, keep-alive handling, and
  error-exit propagation;
- carries an internal `buffer`/`pos` cursor spanning multiple `read`
  calls (multiplex.rs:341-361);
- tees post-demux bytes to the batch recorder (multiplex.rs:352-358,
  382-388);
- skips length-0 activation frames (multiplex.rs:375).

This state machine, the compression layer above it
(`CompressedReader`), and the token decoder (`TokenReader`) are all
written against `std::io::Read`. Converting only the leaf OS read to
`.await` while leaving this stack sync is impossible: the stack owns the
framing, the control-message side effects, and the buffer cursor.
Making the read async means rewriting the demux + decompress + token
decode as an async state machine - ASY-4's transport wrapper does not
exist yet, and even it would not cover the control-frame dispatch that
lives above the transport.

### 3.3 The read is fused to the SPSC disk bridge inside one sync frame

`process_file_response_streaming` (transfer_ops/streaming.rs:74) does
not just read - it reads a delta token **and** hands it to the
disk-commit thread over `spsc::Sender<FileMessage>` within the same
synchronous call (streaming.rs: `read_token` then `file_tx.send(..)`),
recycling buffers back over `spsc::Receiver<Vec<u8>>`. ASY-3's spec
(section 2, boundaries 6/7) requires swapping SPSC -> tokio `mpsc` in
lockstep with the read conversion, and boundary 9 requires the disk
commit to become a long-lived `spawn_blocking` task driven by
`block_on`. So converting the read `.await` drags in the channel swap
(#6/#7) and the disk-task restructure (#9) - three ASY-1 boundaries move
together, not one. Splitting them would put an async reader and a sync
SPSC producer in the same stack frame with no valid bridge.

### 3.4 ASY-3's runtime-hosting model blocks an async prefetch shim

The one shape that could isolate the read - an async task that `.await`s
the socket and feeds a buffer that a **sync** demux stack drains under
`spawn_blocking` (the `rsync_io::channel_adapter::ChannelReader`
pattern) - is incompatible with ASY-3's model. ASY-3 runs the *entire*
sync server on the runtime's current thread via `block_on`, not in
`spawn_blocking`. Introducing an async prefetch task feeding a
`spawn_blocking` demux is a restructure of the ASY-3 runtime-ownership
scaffold itself (who owns the thread, how the server body is split
across an async half and a blocking half), plus the flush-before-block
invariant (ASY-1 "Preserved"; ASY-3 spec section 3 row 7) must be
re-established across the new task boundary. That is a larger change
than one rung and belongs to ASY-4 (transport wrapper) landing first.

## 4. Invariants that a partial conversion would break

- **Wire-byte parity** (ASY-1 "Preserved" #1): the demux control-frame
  dispatch (#3.2) is where MSG_INFO/MSG_ERROR ordering is produced.
  Splitting the read from the demux risks reordering or dropping those
  frames.
- **Flush-before-block** (ASY-1 "Preserved" #7): an async reader task
  separated from the sync writer would need the flush-before-read
  ordering re-encoded across the task boundary (ASY-3 spec's
  `WriterGuard`), which does not exist yet.
- **In-order disk commit** (ASY-1 "Preserved" #3): the read is fused to
  the SPSC `send` (#3.3); converting one without the other has no valid
  in-order bridge.

## 5. What the next rung must do first

ASY-7 cannot be a single receiver-read rung. The dependency order is:

1. **ASY-4 transport wrapper (prerequisite - BUILT).** The
   `tokio::io::AsyncRead`/`AsyncWrite` shim over the socket-backed
   transport now exists as
   `crates/transfer/src/pipeline/async_transport.rs`
   (`pub(crate) AsyncTransport`, gated on `tokio-transfer`, default-off).
   It wraps a `std::net::TcpStream` via `TcpStream::from_std` after
   flipping the socket non-blocking in `from_std_tcp`, and applies only
   to socket-backed transports (daemon `rsync://` /
   `DaemonStream::Plain`); pipe-backed SSH / stdio transports
   (`DaemonStream::Stdio`) have no socket and stay on the sync path,
   mirroring the NSV-1 `Option<fd>` shape. The adapter is additive and
   **unwired**: it is not connected to the receiver read path, the
   multiplex demux, the SPSC bridge, or `core::session`. The remaining
   ASY-4 work that this rung still needs - moving the multiplex demux,
   decompression, and control-frame dispatch onto async, or a
   prefetch-into-buffer adapter with a documented flush-before-block
   guard - is deferred to the coupled ASY-7-redo rung below.
2. **Re-charter ASY-7 as a coupled rung**, converting boundaries 4
   (read), 6+7 (SPSC -> mpsc), and the async half of 9 (disk task)
   together, because section 3.3 shows they share a stack frame. The
   monomorphized `R: Read` chain (section 3.1) becomes async across all
   seven functions in one change, gated on `tokio-transfer`, with the
   threaded path kept via `#[cfg]`-split sync/async bodies.
3. Only then can the equivalence test (threaded vs tokio-receiver
   byte-diff) be written meaningfully; there is no partial tokio
   receiver to test at this rung.

## 6. Result

No receiver code changed. Default build and the ASY-3/ASY-4
`tokio-transfer` foundation are untouched and remain byte-identical /
default-off. The single-boundary conversion is deferred pending ASY-4
(now BUILT) and a re-chartered coupled ASY-7 per section 5.

The re-chartered coupled rung was subsequently scoped and also stopped;
section 8 records that outcome, the four hard blockers, and the
prerequisite ordering the coupled conversion actually needs before any
async receiver code can land byte-identically.

## 7. Cross-references

- `docs/audits/asy-1-threading-model.md` - boundary #4, "Preserved"
  invariants #1/#3/#7.
- `docs/design/asy-2-tokio-runtime-feature.md` - feature flag, runtime
  ownership (section 5), spawn_blocking policy (section 6), open
  question 2 (the ASY-4 transport wrapper this rung depends on).
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary contracts;
  boundaries 4, 6, 7, 9 and section 3 rows 5/7.

## 8. Coupled rung (ASY-7-redo) scoping outcome - STOP

Status: The re-chartered coupled rung (section 5.2: convert boundary 4
read + 6/7 SPSC->mpsc + the async half of 9 disk task together, with the
`R: Read` chain `#[cfg]`-split sync/async) was scoped against the master
tree after ASY-4 (`c41ac883c`). Outcome: **still STOP.** A byte-identical
tokio receiver that genuinely `.await`s the socket is not shippable at
this rung. No receiver `.rs` was converted. The default build and the
ASY-3/ASY-4 foundation are untouched and remain byte-identical /
default-off.

The single-boundary analysis (section 3) already stopped; coupling the
three boundaries does not remove the blockers - it surfaces a fourth.
Four independent blockers hold; any one is sufficient to stop.

### 8.1 Blocker A - the async read requires touching `protocol` (design-forbidden)

The receiver's two leaf reads both live in the `protocol` crate:

- the multiplex demux drives `protocol::recv_msg_into`
  (`crates/protocol/src/multiplex/io/recv.rs:31`), which calls
  `read_header` / `read_payload_into` -> `reader.read_exact`;
- the compressed-token path drives
  `CompressedTokenDecoder::recv_token<R: Read>`
  (`crates/protocol/src/wire/compressed_token/decoder.rs:114`), an inflate
  state machine wholly inside `protocol`.

A genuine `.await` on the socket requires `async fn` variants of both,
inside `protocol` (and the `compress`-backed inflate). ASY-2 section 2.2
lists `protocol` among the crates that **stay sync and are called from
`spawn_blocking` islands** ("Crates NOT touched: engine, fast_io,
signature, protocol, ..."). Converting the read at the `transfer` layer
only (per this rung's charter) cannot reach the leaf: the leaf is one
crate down, behind a boundary the design forbids crossing.

### 8.2 Blocker B - the transport is type-erased before the receiver

The ASY-4 seam (`AsyncTransport::from_std_tcp`,
`crates/transfer/src/pipeline/async_transport.rs`) needs a concrete
`std::net::TcpStream`. By the time control reaches the receiver, the
transport is `stdin: &mut dyn Read` (`run_server_with_handshake`,
`crates/transfer/src/lib.rs:342`; `run_server_stdio`,
`crates/core/src/session.rs:60`). The socket / pipe fd is erased. On the
chartered PULL path the transport is an SSH/rsh **pipe**, which
`AsyncTransport` structurally cannot wrap (ASY-4 doc: "applies only to
socket-backed transports"). There is no concrete async socket to adopt at
the receiver read boundary.

### 8.3 Blocker C - the chartered PULL receiver bypasses the tokio seam

The only tokio-driven entry is `core::session::run_server_stdio`
(session.rs:60), which wraps the **`--server` process**. In a PULL
(remote src -> local dst) the `--server` is the **generator/sender**; the
oc **receiver is the local client**, which runs
`crate::server::run_server_with_handshake` directly
(`crates/core/src/client/remote/ssh_transfer/drive.rs:292`) and never
touches the tokio session driver. So the rung's premise - "a PULL
transfer (server=generator, oc receiver) with tokio-transfer on actually
reads async" - is architecturally unreachable through the current seam.
The tokio driver can host a receiver only in a **PUSH** (local src ->
remote dst, remote `--server` = receiver, run.rs:280); even there the
read chain is the same sync `protocol`-leaf chain (blockers A/B).

### 8.4 Blocker D - demux side-effect ordering

`MultiplexReader::dispatch_message`
(`crates/transfer/src/reader/multiplex.rs:215`) performs inline
`print!` (FINFO/FCLIENT), `eprint!` (FWARNING/FERROR*),
`io::stdout().flush()`, and error-exit propagation between frames. Any
split that separates an async socket read from this sync demux (e.g. an
async prefetch task feeding a `spawn_blocking` demux) reorders those
side effects relative to disk-commit and writer flushes - the ASY-1
"Preserved" #1 wire/output-parity break flagged in sections 3.2 and 4.

### 8.5 What was verified empirically

The ASY-3 tokio driver hosts the **sync** server body under
`Handle::block_on` (`pipeline/tokio_driver.rs:81`), so it is expected to
be dest-identical - and is, confirmed by a real oc-rsync self-transfer
(`--rsh` lsh, pinned `--checksum-seed=1234`, mixed corpus: whole-file +
delta-candidate + `-z` compressed + multi-file + nested + empty):

- **PULL** (feature-on client+server vs feature-off): `diff -r` of the
  two dest trees is empty; per-file SHA-1 identical across all files;
  `--stats` identical on file count, total file size, literal, matched.
- **PUSH** (feature-off client -> feature-on `--server` receiver vs
  feature-off `--server` receiver): `diff -r` of dest trees empty. This
  is a real receiver running on the tokio runtime, dest-byte-identical to
  the threaded receiver.

One pre-existing caveat, orthogonal to this rung: earlier scoping (against
`9d7ea8196` / `c81687a3d`) observed the feature-on build's `--stats` "Total
bytes received" differing from feature-off by a small fixed amount (6-12
bytes, deterministic on each side), recorded here so the stats-parity gate
would account for it before flipping the default.

**Update (master `795593f0c`): the quirk no longer reproduces.** Re-running
the exact reproduction with a default binary and a `--features tokio-transfer`
binary across every reachable wire path - SSH/rsh PUSH and PULL (all four
client/server feature combinations, pinned via `--rsync-path`), daemon-module
PUSH and PULL through the tokio-driven module receiver, both compressed (`-az`)
and uncompressed (`-a`) - yields byte-identical `--stats` "Total bytes
received" and byte-identical dest trees on all legs. The delta is zero; there
is no counting site to change. The most likely cause of the original delta was
resolved incidentally by the reader-stack prerequisite rungs (the async
`CountingReader` and its byte-exact accounting, `reader/counting.rs:80`) landed
after the earlier scoping. Rather than patch a quirk that no longer exists,
the invariant is now **CI-enforced** by `tools/ci/run_async_equivalence.sh`
(wired into `.github/workflows/async-wire-parity.yml`), which byte-diffs a real
default-vs-tokio-transfer transfer and fails on any dest-tree, `--stats`, or
exit-code divergence. This makes the stats-parity invariant a standing gate the
atomic receiver fork must keep green, exactly as §10.3 item 2 requires.

### 8.6 Prerequisite ordering the coupled rung actually needs

The section 5 ordering is necessary but insufficient; the true blocking
prerequisites are:

1. **An async read leaf that does not touch `protocol`.** Either (a)
   relocate `recv_msg_into` + the compressed-token inflate behind an
   async-capable seam that ASY-2 permits, or (b) explicitly amend ASY-2
   section 2.2 to allow async variants inside `protocol`/`compress`
   (a design change requiring sign-off, not an implementation rung). The
   `spawn_blocking`-prefetch alternative (an async task feeding a sync
   demux) is barred by blocker D (side-effect reordering) and section 3.4.
2. **A concrete async socket at the receiver boundary.** Thread the
   `Option<TcpStream>` (NSV-1 shape) down to the receiver so the daemon
   `rsync://` path can adopt `AsyncTransport`; the SSH/rsh pipe path has
   no socket and stays sync by design. This is a plumbing change from
   `core` through `run_server_with_handshake`, not a receiver-internal
   change.
3. **A receiver reachable through the tokio seam for the intended
   direction.** For the chartered PULL, the local-client receiver must be
   routed through a tokio-driven entry (today it is not). This is a
   `core` client-transport change (`ssh_transfer::drive`), out of scope
   for a `transfer`-only receiver rung.

Only after (1)-(3) land can the `R: Read` chain be `#[cfg]`-split to an
async variant and the equivalence test (section 5.3) be written against a
real converted async receiver. Until then, shipping a tokio receiver
means shipping either a `block_on`-hosted sync read (zero async benefit,
the ASY-3 shape) or a divergent one (blocker D) - both worse than none.
Per the ASY-7 charter stop clause, we stop.

## 9. Third scoping (blockers A/D resolved) - STILL STOP

Status: Re-scoped against master `9d7ea8196`, after the two hardest
prerequisites landed:

- **Prerequisite 1 / blocker A RESOLVED.** The async read leaves now
  exist inside `protocol` (ASY-2 section 2.2 was amended to permit async
  variants): `protocol::recv_msg_into_async`
  (`crates/protocol/src/multiplex/io/async_recv.rs:40`,
  `R: AsyncRead + Unpin`) and `CompressedTokenDecoder::recv_token_async`
  (`crates/protocol/src/wire/compressed_token/decoder.rs:169`).
- **Blocker D RESOLVED.** The async demux
  `MultiplexReader::read_async_with<S: MuxSink>`
  (`crates/transfer/src/reader/multiplex.rs:537`) reproduces the sync
  demux's control-frame side effects in the same order via an injectable
  `MuxSink`/`RealSink`, with a parity test
  (`reader/multiplex_parity_tests.rs`).

Outcome: **still STOP.** A byte-identical tokio receiver that genuinely
`.await`s the socket is not shippable in a reviewable slice. No receiver
`.rs` was converted. Default build and the ASY-3/ASY-4/demux foundation
are untouched and remain byte-identical / default-off. Two blockers that
resolving A/D did not remove hold; each is independently sufficient.

### 9.1 Blocker B' - the async reader stack above the leaf does not exist

`read_async` is only defined for `MultiplexReader<R>` where
`R: AsyncRead + Unpin` (multiplex.rs:514). In production the multiplex
inner `R` is `BufReader<CountingReader<Box<dyn Read>>>` - a
`std::io::Read` stack, not `AsyncRead` - so `read_async` is uncallable on
the real chain: there is no `AsyncRead`-typed reader to instantiate it
over. The entire reader stack the receiver builds (lib.rs:535-538) is
`std::io::Read`-typed and has **no async twin**:

- `CountingReader` (`reader/counting.rs`) - counts raw wire bytes for the
  `bytes_received` stat (lib.rs:535-536); no `AsyncRead` impl.
- `std::io::BufReader` (64 KB, matches upstream `iobuf.in`) - a sync-only
  type; the async path needs `tokio::io::BufReader` or none.
- `ServerReader<R: Read>` (`reader/server.rs:15`) - the
  Plain/Multiplex/Compressed state machine; no async read.
- `CompressedReader` (the `ServerReaderInner::Compressed` layer) - no
  async twin.

Building the async path requires an end-to-end `AsyncRead`-typed reader
chain (async CountingReader for byte-identical `bytes_received`, async
ServerReader state machine, async CompressedReader) so `read_async` has a
concrete socket-backed `R` to run over. That is a net-new reader stack,
not a single-boundary swap. Grep confirms the only async-capable reader
in `transfer` today is `MultiplexReader::read_async` plus a test-only
`ChunkedReader`; every other layer is sync.

### 9.2 Blocker C' - neither reachable receiver is a socket-backed tokio receiver

Two receivers exist; neither is simultaneously (a) reachable through the
tokio driver and (b) backed by a concrete async socket:

- **CLI `--server` receiver** - the only caller of the tokio driver
  (`core::session::run_server_stdio` ->
  `transfer::run_server_with_handshake_on`,
  `crates/core/src/session.rs:71`). But its transport is
  `io::stdin().lock()` (`crates/cli/src/frontend/server/run.rs:57`) - a
  **pipe**, which `AsyncTransport` structurally cannot wrap (ASY-4:
  socket-backed only). Tokio-driven but no socket.
- **Daemon module receiver** - runs in-process with a concrete
  `TcpStream` clone available
  (`daemon/.../transfer/streams.rs:50`, `tcp.try_clone()`), but it calls
  the **sync** `run_server_with_handshake` directly (streams.rs:132),
  never the tokio driver / `with_transfer_runtime`. The clone is boxed to
  `Box<dyn Read + Send>` at streams.rs:69 before it reaches the server
  body. Socket-backed but not tokio-driven, and type-erased at the
  boundary.

Routing the daemon receiver through the tokio driver AND threading the
pre-erasure `TcpStream` (as `Option<TcpStream>`, NSV-1 shape) down to the
receiver so `AsyncTransport::from_std_tcp` can adopt it is a `daemon` +
`core` plumbing change, not a `transfer`-internal receiver rung. It also
forks the daemon transfer entry (sync vs tokio) under `#[cfg]`.

### 9.3 Blocker E - the fused read->SPSC frame and the 7-fn cfg-split

Even with 9.1/9.2 solved, the coupled conversion still requires, in one
change: async twins of the seven `R: Read` receiver functions
(`ReceiverContext::run` -> `run_pipelined` -> `setup_transfer` ->
`run_pipeline_loop_decoupled` -> `process_file_response_streaming` ->
`TokenReader::read_token`, plus `ServerReader`/`MultiplexReader` reads),
each `#[cfg]`-split sync/async; and a bridge for the fused read->send
frame. `process_file_response_streaming`
(`transfer_ops/streaming.rs:74`) reads a token (streaming.rs:138) and
`file_tx.send(..)`s it to the disk-commit `std::thread` (streaming.rs:156)
in the same synchronous frame, over a bounded spin-wait
`spsc::channel` (`pipeline/spsc.rs`). The async twin must `.await` the
read then hand the SPSC producer to a `spawn_blocking` island without
reordering the in-order disk commits or the goodbye/flush sequence - a
task boundary inside the previously-fused frame, re-establishing the
flush-before-block invariant across it.

### 9.4 Reviewable-slice verdict

A byte-identical async daemon receiver is now *architecturally reachable*
(A and D are resolved and the socket exists in-process), but not in a
reviewable slice: it requires (1) a net-new end-to-end `AsyncRead` reader
stack incl. a byte-exact async `bytes_received` counter (9.1), (2) a
`daemon`+`core` re-route + `Option<TcpStream>` plumbing forking the
daemon transfer entry (9.2), and (3) a 7-function sync/async `#[cfg]`
fan-out plus an async-read -> `spawn_blocking`-SPSC bridge preserving
in-order commit and flush-before-block (9.3). Landing that atomically is
the only way to keep it byte-identical - a partial split leaves an async
reader feeding a sync SPSC producer in one frame with no valid bridge
(section 3.3), or an async `read_async` with no `AsyncRead` stack to run
on (9.1). The known ASY-3/4 `bytes_received` 6-12 byte foundation quirk
(section 8.5) would also be reproduced-and-amplified by an async
`CountingReader` unless it counts byte-identically, so the stats-parity
gate must be green on the new counter before the default can flip.

Per the ASY-7 charter stop clause, we stop and report rather than ship
either a `block_on`-hosted sync receiver (zero async benefit, the ASY-3
shape already in place) or a receiver split across a boundary that
diverges. The prerequisite ordering for the next rung is the three items
in 9.4, landed as separate reviewable rungs (async reader stack; daemon
tokio re-route + socket plumbing; the fused 7-fn conversion + SPSC
bridge), in that dependency order, before an equivalence test can be
written against a real converted async receiver.

## 10. Fourth scoping (prerequisites 1+2 landed) - STOP on verifiability

Status: Re-scoped against master `c81687a3d`, after the first two of the
three 9.4 prerequisites landed as their own rungs:

- **Prerequisite 1 (async reader stack) RESOLVED.** The full
  `AsyncRead` reader stack now exists and is byte-parity-tested at every
  layer: `AsyncTransport` (`pipeline/async_transport.rs`) ->
  `CountingReader` async twin with a byte-exact `bytes_received` counter
  (`reader/counting.rs:80`) -> `AsyncServerReader` plain+multiplex
  (`reader/server.rs:285`) -> `MultiplexReader::read_async`
  (`reader/multiplex.rs:524`) -> `TokenReader` compressed leaf
  `recv_token_async` (zlib/zstd/lz4,
  `protocol/src/wire/compressed_token/decoder.rs:169`).
- **Prerequisite 2 (daemon tokio re-route + socket plumbing) RESOLVED.**
  The socket-backed daemon receiver is routed through the tokio driver
  (`run_daemon_transfer` -> `with_daemon_transfer_runtime` ->
  `run_server_with_handshake_on`) and the concrete `TcpStream` is
  threaded to that entry as `TransferStreams.async_socket:
  Option<TcpStream>`
  (`daemon/.../transfer/streams.rs`), currently dropped at scope end and
  awaiting adoption as an `AsyncTransport`.

The deadlock concern is also cleared on inspection: the disk-commit
consumer is a dedicated `std::thread`
(`disk_commit/thread.rs:60`, `thread::Builder::new().spawn`), not a
tokio task, so it drains the spin-wait `spsc::channel` independently of
the reader's runtime. An async reader can therefore call the existing
sync `spsc::Sender::send` directly (a non-`.await` spin op) without a
runtime-cooperation deadlock; the "SPSC -> spawn_blocking" rebuild that
9.3 anticipated is not structurally required for correctness.

Outcome: **STOP** - not on architecture (9.4 already found it
*architecturally reachable*), but on **verifiable byte-identity for this
rung, in this environment.** No receiver `.rs` was converted. Default
build and the ASY-1/3/4 + reader-stack foundation are untouched and
remain byte-identical / default-off. Two independent reasons hold; each
is sufficient under the charter's "byte-identical or nothing" clause.

### 10.1 The conversion is an atomic ~20-30-function fork, not a 7-fn split

9.3's "seven `R: Read` functions" undercounts the real fan-out. A
receiver that genuinely `.await`s a real daemon-PUSH read requires async
twins, in one commit, of the entire receiver driver, because the read is
fused to wire-observable side effects inside single loop iterations:

- `run_server_with_handshake_on` (build the async reader stack from
  `async_socket` instead of `ServerReader<BufReader<CountingReader<Box<
  dyn Read>>>>`);
- `ReceiverContext::run` -> `run_pipelined` **and**
  `run_pipelined_incremental` -> `setup_transfer` ->
  `run_pipeline_loop_decoupled` -> `process_file_response_streaming` ->
  `process_remaining_tokens` + `literal_to_buf` + `read_response_header`
  + `SenderAttrs::read_with_codec_xattr` -> `TokenReader::read_token`;
- the wire-reading `phases.rs` twins (`exchange_phase_done`,
  `read_expected_ndx_done`, `handle_goodbye`, `receive_stats`,
  `finalize_transfer`) and the setup-phase filter-list read.

`run_pipeline_loop_decoupled` (`receiver/transfer/pipeline.rs`, 250+
lines) is the crux: it interleaves rayon `par_iter` signature
computation, `send_file_request`, the `flushed_pending`-gated
`writer.flush()` (the flush-before-block invariant), the SPSC disk
sends, `emit_itemize` (MSG_INFO ordering), and progress callbacks - all
in one loop iteration whose ordering is wire-observable. Splitting the
awaited read out of that frame without reordering any of those effects
across the new await points is the whole change; there is no smaller
slice that both genuinely awaits a real read and stays byte-identical.
An empirical check confirms **zero** async twins exist anywhere under
`receiver/` or `transfer_ops/` today: the async building blocks stop at
the reader-stack layer, so this rung is the entire driver fork.

### 10.2 Byte-identity is unverifiable in this environment for this rung

The charter's PROOF obligations are the async-vs-sync daemon-PUSH
equivalence test plus the transfer/daemon/protocol regression suites and
the golden-wire gate. On this host the `cargo nextest` listing phase
hangs (project policy: tests are CI-only; see the repo's local-hang
note), so the mandated equivalence test and the regression gates cannot
be run locally. Landing a ~20-30-function async fork of the most
wire-sensitive receiver code and asserting byte-identity **without**
running its test gate would violate "byte-identical or nothing" and the
fail-loud rule.

A second, concrete divergence blocks it independently: the ASY-3/4
foundation carries a still-open, un-root-caused `--stats` "Total bytes
received" quirk (section 8.5: feature-on vs feature-off differs by a
deterministic 6-12 bytes). This rung would put the async `CountingReader`
on the real receiver path for the first time, directly exposing that
un-zeroed `bytes_received` delta on a real transfer - exactly the "NEW
stats divergence (beyond the byte-exact counter)" the charter names as a
STOP trigger. 9.4 already flagged that the stats-parity gate must be
green on the new counter before this can land; that gate is a foundation
fix, not part of this receiver rung.

### 10.3 What must land before the conversion can be shipped

1. **Zero the ASY-3/4 `bytes_received` foundation quirk** (section 8.5) -
   **DONE.** The delta is already zero at master `795593f0c` (section 8.5
   update); the empirical re-run shows byte-identical `bytes_received` and
   dest trees on every SSH/rsh and daemon leg, compressed and uncompressed.
   No counting site needed changing.
2. **A CI-run equivalence gate** (extend `async-wire-parity.yml`) that
   exercises a real local daemon-module PUSH with `tokio-transfer` on vs
   off and byte-diffs dest tree + `--stats` + exit, so the atomic
   driver-fork can be verified where nextest actually runs - **DONE.**
   `tools/ci/run_async_equivalence.sh` runs daemon PUSH/PULL (tokio-driven
   module receiver) plus compressed/whole-file legs twice, once per feature
   state, and fails on any dest-tree, `--stats`, or exit-code divergence; a
   new `async-equivalence` job in `.github/workflows/async-wire-parity.yml`
   runs it on the Linux runner. Verified locally: identical binaries pass,
   an injected 6-byte `bytes_received` delta and an injected dest-tree delta
   both fail the gate with a clear diff.
3. Only then land the atomic async driver fork (10.1) as one reviewable,
   CI-verified commit. A `block_on`-hosted sync fork adds zero async
   benefit (the ASY-3 shape already exists), and an unverified fork risks
   a wire/stats divergence - both worse than none per the stop clause.

## 11. Fifth scoping (prereqs 1+2 DONE, gate live) - STOP on read-surface coverage

Status: Re-scoped against master `15f2425a9`, after the two §10.3 prerequisites
landed (the `bytes_received` foundation quirk is zeroed; the CI-run
`async-equivalence` gate is live: `.github/workflows/async-wire-parity.yml` +
`tools/ci/run_async_equivalence.sh`, byte-diffing a real feature-off-vs-on
daemon PUSH/PULL across five legs). The charter's two named prior blockers
(verifiability, `bytes_received`) are indeed resolved.

Outcome: **STOP.** The atomic async receiver fork is still not shippable
byte-identically as a consume-the-merged-blocks rung, because the merged async
building blocks cover only the **delta-token demux path**, not the full set of
wire reads the daemon PUSH receiver executes. A byte-identical async receiver
requires net-new async twins across `protocol` + `transfer` that are not part
of the building-block set the charter says to consume. Per the charter's
"byte-identical or nothing" / "a divergent tokio receiver is worse than none"
clause, we stop and report the exact remaining coupling rather than ship a fork
that either diverges or falls back to `block_on` (zero async benefit).

### 11.1 What the merged blocks cover (and what they do not)

Verified present and byte-parity-tested (the delta-token path):

- async transport (`AsyncTransport::from_std_tcp`,
  `pipeline/async_transport.rs`);
- async byte-exact `CountingReader` (`reader/counting.rs:80`);
- async multiplex demux `MultiplexReader::read_async` /
  `read_async_with<S: MuxSink>` with ordered control-frame side effects
  (`reader/multiplex.rs:514`);
- `AsyncServerReader` **plain + multiplex only** (`reader/server.rs:284`);
- async leaf reads inside `protocol`: `recv_msg_into_async`
  (`multiplex/io/async_recv.rs:40`) and `CompressedTokenDecoder::recv_token_async`
  (zlib/zstd/lz4, `wire/compressed_token/decoder.rs:169`).

The compressed leaf being present at the `protocol` layer is a real advance
over the section 9.1 blocker: `recv_token_async<R: AsyncRead>` drives the
sans-io inflate directly, so the compressed token path does **not** need a
`spawn_blocking` island - it awaits. Good.

But the daemon PUSH receiver's read chain reads far more than delta tokens off
the same demuxed stream, and **none** of those reads have an async twin
(grep across `receiver/`, `transfer_ops/`, `protocol/`, and the `flist` crate
returns zero `*_async` twins for them):

- the **file-list reader** - `receive_file_list` /
  `receive_extra_file_lists` (`receiver/file_list/receive.rs`) delegate to
  `protocol::flist::read::read_entry_with_flist<R: Read>`
  (`protocol/src/flist/read/mod.rs:491`), a large sync `R: Read` decoder in
  `protocol`. This runs in `setup_transfer` **before** any token read and is
  the bulk of the wire read on a multi-file transfer;
- `read_filter_list<R: Read>` (setup, demuxed) - no async twin;
- `SenderAttrs::read_with_codec_xattr` (`receiver/wire.rs:217`, seven
  `read_exact`/`read_ndx`/vstring sites per file) and `SumHead::read` - the
  per-file response header, no async twin;
- the `NdxCodec::read_ndx` path (`read_response_header`, `phases.rs`
  NDX_DONE, goodbye) - no async twin;
- `receive_stats` (five varint `read_stat`s, `phases.rs:208`) and
  `DeleteStats::read_from` (`phases.rs`) - no async twin.

So the async surface stops at the token demux. Everything the receiver reads
around the token loop - flist, filter list, per-file attrs, sum_head, NDX
markers, stats, delete-stats - is still sync `R: Read` with no `.await`
variant.

### 11.2 Why this is a hard STOP for a byte-identical rung

A real daemon PUSH receiver that genuinely `.await`s its socket must `.await`
**every** read on the demuxed stream, not just the token reads, because they
all share one reader and one framing cursor. Splitting the chain so that flist
/ attrs / stats reads run sync (`block_on` or a `spawn_blocking` island) while
only the token reads `.await` puts a sync `R: Read` consumer and an async
`AsyncRead` consumer on the **same** `MultiplexReader` buffer/`pos` cursor
across a task boundary - the section 3.3 "no valid bridge" hazard, now for the
framing cursor rather than the SPSC. It also reorders the demux control-frame
side effects (blocker D) relative to the sync reads. Either way the wire /
`--stats` output can diverge, and the live `async-equivalence` gate would (
correctly) go red - which is exactly the STOP trigger the charter names.

The only byte-identical shape is the atomic fork of §10.1, but §10.1's
"~20-30 functions" undercounts again: it must **also** include async twins of
the entire flist read subsystem (`read_entry_with_flist` and its callers in
`protocol` + `receiver/file_list/`), `read_filter_list`, `read_with_codec_xattr`,
`SumHead::read`, the `NdxCodec::read_ndx` family, `read_stat`, and
`DeleteStats::read_from`. Those are net-new async code in `protocol` and the
`flist` path, not a consumption of the merged blocks, and adding async variants
inside `protocol`'s flist reader is a further ASY-2 section 2.2 design
amendment beyond the two already granted (`recv_msg_into_async`,
`recv_token_async`).

### 11.3 What must land before this rung is a consume-only fork

The building-block set must first grow async twins for the non-token reads,
each byte-parity-tested against its sync twin, landed as their own reviewable
rungs (mirroring how the token demux path was built), before the atomic
receiver fork can be a pure consumption of merged blocks:

1. async `read_ndx` / `read_stat` on the `NdxCodec` / stats codec
   (`protocol`), + `SumHead::read_async`, + `DeleteStats::read_from_async`;
2. async `SenderAttrs::read_with_codec_xattr` twin (`receiver/wire.rs`),
   consuming (1);
3. async `read_filter_list` (`protocol::filters`) + async
   `read_entry_with_flist` and its `receiver/file_list/` callers - the flist
   read subsystem, the largest piece, requiring an ASY-2 section 2.2
   amendment for the `protocol` flist reader;
4. `AsyncServerReader` gaining a `tokio::io::AsyncRead` impl (today it exposes
   only an inherent `read_async` method), so `recv_token_async<R: AsyncRead>`,
   `recv_msg_into_async`, and the flist/attrs async readers can all take the
   one demuxed async reader as their `R` uniformly.

Only after 1-4 exist can the receiver driver's ~20-30 functions be
`#[cfg]`-split to await a real read across the **whole** chain (flist through
goodbye), wired to the daemon `async_socket` (`streams.rs`, currently threaded
and dropped), with the SPSC->disk bridge left as the existing sync spin-send
(the disk consumer is a dedicated `std::thread`, so no runtime-cooperation
deadlock - that part of the charter analysis holds). Shipping the fork before
1-4 means either a divergent split (11.2) or a `block_on` fallback (zero async
benefit) - both worse than none. Per the charter stop clause, we stop.
