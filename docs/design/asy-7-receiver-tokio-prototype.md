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

One pre-existing caveat, orthogonal to this rung: the feature-on build's
`--stats` "Total bytes received" differs from feature-off by a small
fixed amount (6-12 bytes, deterministic on each side). It is **not** a
receiver-data-path difference (dest trees and payload are byte-identical)
and it reproduces with the feature-on *client* even when that client's
receiver does not use the tokio driver, so it is a build-level
`bytes_received` counting artifact in the ASY-3/ASY-4 foundation
(unrelated to any receiver conversion, which this rung did not do). It is
recorded here so the ASY-12 stats-parity gate accounts for it before
flipping the default; it must be root-caused and zeroed as part of the
foundation, not this rung.

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
