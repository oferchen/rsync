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

## 11. Finalization async twins landed (prerequisite for the atomic fork)

Status: The receiver's end-of-transfer wire twins - the last pieces of the
async building-block surface named in section 10.1 as still missing - now
exist alongside the earlier reader-stack, file-list, per-file, and
sender-stats twins:

- `ReceiverContext::read_expected_ndx_done_async` - `.await` NDX_DONE (-1)
  validation via `NdxCodecEnum::read_ndx_async`.
- `ReceiverContext::exchange_phase_done_async` - the phase-done exchange
  with `.await` on the sender's echoed NDX_DONE reads; the request-half
  `write_ndx_done`/`flush`, the phase-counter walk, and the
  `reclaim_oldest_segment` bookkeeping stay the identical sync logic.
- `ReceiverContext::handle_goodbye_async` - the goodbye handshake with
  `.await` on the echo NDX read and the `NDX_DEL_STATS` drain
  (`DeleteStats::read_from_async`); the send-half stays sync.
- `ReceiverContext::finalize_transfer_async` - composes the three twins
  plus the already-landed `receive_stats_async` into the async
  finalization tail, mirroring `finalize_transfer` byte-for-byte with
  `.await` only on the wire reads.

All four are `#[cfg(feature = "tokio-transfer")]`, additive, and have no
non-test caller: like `receive_file_async`, they are the leaves the
deferred atomic receiver fork (section 10.1) will call after its per-file
loop, in place of the sync `finalize_transfer`. They keep the request/send
half synchronous exactly as the per-file leg does, so no async NDX-write
codec is required. Equivalence is pinned by
`finalize_parity_tests` in `receiver/transfer/phases.rs`: the sender-side
finalization wire is built with the sync codec, then driven through both
`finalize_transfer` and `finalize_transfer_async` (including one byte per
poll) with byte-consumption + send-half parity asserted, plus an
error-parity case (a non-NDX_DONE in place of a phase echo rejects
identically). Default build untouched; `cargo clippy -p transfer` (no
feature) and a `x86_64-pc-windows-gnu` cross-build of the feature both
pass - the twins introduce no unix-only type, so the `DirSandbox`/`RawFd`
gating hazard that a prior async leg tripped does not apply here.

This leaves exactly one code rung before the atomic driver fork: nothing.
The async building-block surface (reader stack, file list, per-file
reconstruct+commit, sender stats, phase-done, goodbye, finalization) is
complete. What remains is (1) the atomic ~20-30-function receiver driver
fork of section 10.1 - which stays STOP until it can be CI-verified
byte-identical per section 10.2 - and (2) ASY-4 (the thread-pool-vs-async
benchmark, no code). No other async twin is missing.

## 12. Final wiring rung (route `run()` + adopt daemon socket) - STOP

Status: The atomic receiver driver fork of section 10.1 has since landed
as the `run_sync_async` / `run_pipelined_async` /
`run_pipelined_incremental_async` family (plus `setup_transfer_async` and
the flist look-ahead carry), each `#[cfg(feature = "tokio-transfer")]`,
`#[allow(dead_code)]`, and byte-parity-tested against its sync twin over
in-memory `Cursor` / `CaptureWriter` fixtures. This rung was chartered to
make that family *live*: (RUNG 4) route the receiver entry to the matching
async driver when the feature is on and a real async transport exists, and
(RUNG 5) thread the daemon's `async_socket` clone down so the async driver
reads the real tokio socket via `.await` instead of dropping it after the
`host_sync_on` / `block_on` sync body.

Outcome: **STOP.** The two rungs cannot be shipped as byte-correct,
deadlock-safe minimal wiring. No code was wired. Default build and the
ASY-1/3/4 + reader-stack + async-driver foundation are untouched and remain
byte-identical / default-off. One blocker is load-bearing and decisive; two
more compound it. Per the charter stop clause we record the exact coupling
rather than ship a half-wired or `block_on`-shimmed receiver.

### 12.1 Blocker F (decisive) - the async drivers write synchronously, so adopting the socket re-arms the #503 write-write deadlock

The daemon `rsync://` receiver is the only in-process transport with a
concrete socket to adopt (section 9.2; SSH/stdio are pipe-backed and stay
sync by design). On that path the full-duplex write-write deadlock is real
and is prevented today by the `DrainingReader` background thread
(`daemon/.../transfer/draining_reader.rs`): the receiver delta loop
"writes a batch of file requests, then blocks reading the sender's
response - with no interleaved drain of incoming frames. Once both ~128 KB
kernel socket buffers fill, neither direction can make progress." The drain
thread continuously reads the read-clone fd into an unbounded queue and the
receiver loop pulls from that queue instead of the socket, so the peer's
send buffer is always emptied and the wedge is structurally impossible.

Adopting the `async_socket` clone as the async driver's sole reader is
incompatible with that mechanism in both directions:

- **Drain left on:** the drain thread and the async reader consume from two
  `try_clone()`d fds of the *same* open file description, splitting the
  demuxed byte stream between them - an immediate wire desync.
- **Drain turned off:** the three async drivers keep the request/response
  **write** half a plain blocking `writer: &mut W` (`io::Write`) - verified
  on all three signatures (`run_sync_async` / `run_pipelined_async` /
  `run_pipelined_incremental_async`) - and there is **no async writer
  stack** in `transfer` (no `AsyncWrite` twin of
  `ServerWriter`/`MultiplexWriter`/`CountingWriter`; grep-confirmed). With
  the drain gone and the write side still blocking, a real transfer that
  fills the socket send buffer parks the OS thread inside a sync `write`.
  On the current-thread runtime the reactor cannot poll the `.await` read
  while the thread is parked, so nothing drains the peer and the transfer
  deadlocks exactly as pre-#503. The in-memory parity tests never expose
  this: `Cursor`/`CaptureWriter` have no backpressure, so the drivers have
  never run against a real socket.

A deadlock-safe async daemon receiver therefore needs the **write** side to
also go async and be polled concurrently with the read (a `select`/`join`
over read+write that reproduces the drain thread's "always draining"
guarantee cooperatively). That is a net-new async writer stack plus a
read/write concurrency restructure of `run_pipeline_loop_decoupled` - a
change well beyond "route + adopt socket," and the load-bearing missing
piece for this rung.

### 12.2 Blocker G - the read-position hand-off from the sync setup phase has no carry seam

`run_server_with_handshake` (`crates/transfer/src/lib.rs`) runs
`setup::setup_protocol` (compat-flags / checksum-seed / capability
negotiation) as **synchronous** reads on `stdin: &mut dyn Read` before the
receiver, chaining any `handshake.buffered` bytes (already pulled off the
socket into userspace during daemon argument reading) ahead of it via a
`Cursor(..).chain(stdin)`. The `async_socket` is a separate fd that cannot
see bytes already consumed into that userspace Cursor. A byte-correct
hand-off must thread any unconsumed `handshake.buffered` remainder into the
`AsyncServerReader` as a prepend carry (the same shape the flist look-ahead
carry already uses one layer down). No such carry seam exists between
`setup_protocol` and a would-be async reader stack, and getting it wrong is
a silent wire desync rather than a loud failure.

### 12.3 Blocker H - byte-identity is CI-only; it cannot be proven on this host

The charter's PROOF obligation for a live receiver is a real daemon-PUSH
async-vs-sync equivalence run (dest tree + `--stats` + exit) plus the
transfer/daemon/protocol regression and golden-wire gates. That is gated by
`tools/ci/run_async_equivalence.sh` / `.github/workflows/async-wire-parity.yml`,
which run only in CI; local `cargo nextest` hangs in the listing phase on
this host (tests are CI-only by project policy). Landing the write-stack +
concurrency restructure of 12.1 - the most wire-sensitive receiver code -
and asserting byte-identity without running its gate would violate
"byte-identical or nothing" and the fail-loud rule.

### 12.4 Why RUNG 4 cannot be shipped independently either

RUNG 4's routing branch would live in `ReceiverContext::run`
(`receiver/transfer.rs`), but `run` receives an already-built sync
`ServerReader<R: Read>` and holds no async transport handle to branch on;
the real routing seam is one layer up in `run_server_with_handshake[_on]`,
where the reader stack is constructed (section 10.1, first bullet). Routing
there still has to build the async reader stack from the `async_socket`,
which is exactly what blockers F/G forbid doing safely. Shipping the
routing branch without a transport it can safely fire on would be a
dead-in-production `#[cfg]` branch - the "half-wired path" the charter
names as worse than none.

### 12.5 Prerequisite ordering for the next rung

1. **An async writer stack** - `AsyncWrite` twins of
   `ServerWriter`/`MultiplexWriter`/`CountingWriter` with byte-exact
   `bytes_sent` accounting, so the async driver's request/response half can
   `.await` its writes.
2. **Concurrent read+write in the async driver** - restructure
   `run_pipeline_loop_decoupled`'s async twin to poll the read and the
   write together (replacing the `DrainingReader` thread's always-draining
   guarantee cooperatively), so turning the drain off is deadlock-safe.
3. **A setup->receiver carry seam** - thread the unconsumed
   `handshake.buffered` remainder into the `AsyncServerReader` prepend
   carry (12.2).
4. **Then** route `run_server_with_handshake_on` to build the async reader
   stack from `async_socket` and dispatch the mode-matched async driver,
   with the CI equivalence gate green on a real daemon PUSH.

Until (1)-(3) land as their own reviewable rungs, the live wiring ships
either a desync (drain on), a deadlock (drain off), or an unverified fork -
all worse than none. Per the charter stop clause, we stop and report.

<!-- CI skip-path verification probe for required upstream-testsuite check. -->
