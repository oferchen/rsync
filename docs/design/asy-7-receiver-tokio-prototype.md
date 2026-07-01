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

No code changed. Default build and the ASY-3 `tokio-transfer`
foundation are untouched and remain byte-identical / default-off. The
receiver network-read boundary is deferred pending ASY-4 and a
re-chartered coupled ASY-7 per section 5.

## 7. Cross-references

- `docs/audits/asy-1-threading-model.md` - boundary #4, "Preserved"
  invariants #1/#3/#7.
- `docs/design/asy-2-tokio-runtime-feature.md` - feature flag, runtime
  ownership (section 5), spawn_blocking policy (section 6), open
  question 2 (the ASY-4 transport wrapper this rung depends on).
- `docs/design/asy-3-async-boundary-spec.md` - per-boundary contracts;
  boundaries 4, 6, 7, 9 and section 3 rows 5/7.
