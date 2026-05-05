# Tokio dependency boundary re-verification

Tracker: re-verification of the tokio scoping established by audit #1779.
Adjacent: #1732 (async became a default feature), #1818 (post-default
async refactors), #1934 (`AsyncDaemonListener` RFC), #1593 / #1411
(SSH transport async questions).

Last verified: 2026-05-05. No code changes in this audit.

## Summary

Audit #1779 established the rule that tokio may live only in `daemon`
and `core` (behind the `async` feature). Since then, the `async`
feature became default at the workspace root (#1732) and several async
refactors landed (#1818). This audit re-verifies the boundary against
the current `Cargo.toml` set and the `pub async` surface in `crates/`.

The result is that the original "only daemon and core" framing has
already drifted. Tokio is now reachable from seven workspace
crates - `bandwidth`, `core`, `daemon`, `engine`, `protocol`,
`rsync_io`, `transfer` - in every case behind a feature gate. Two of
those gates (`async` in `bandwidth`, `async` in `protocol`) sit in
crates that the policy as written in this repository's design notes
identifies as "must never contain unsafe code" and which were not
named as tokio consumers by #1779. The drift is real, but each
violation has a concrete justification rooted in the async pipeline
and the embedded-ssh transport landed since #1779.

The recommendation is to update the policy text rather than revert
the code, plus add a CI guardrail (`tools/ci/check_tokio_boundary.sh`)
that pins the boundary at the seven currently-allowed crates and
fails on accidental expansion.

## Methodology

`cargo tree -e features --workspace` is the canonical way to verify
this; this audit reads each `Cargo.toml` directly. For every
workspace crate the following dimensions are recorded:

- **Direct tokio dep?** Does the crate's `[dependencies]`,
  `[dev-dependencies]`, or any `[target.'cfg(...)'.dependencies]`
  block name `tokio` or `tokio-util`?
- **Feature gate.** If yes, is the dep `optional = true` plus a
  feature toggle, or unconditional?
- **Justified?** Does the crate expose a public `async fn` /
  `impl Future` / `tokio::*` type that requires the dep?
- **Notes.** Source-of-truth file:line for the dep declaration.

The crate inventory comes from `Cargo.toml:135-163`. The workspace
root's feature wiring (`Cargo.toml:106-107`, `:188-189`) is
inspected to trace where default-on `async` pulls tokio in.

## Per-crate tokio status table

The 25 workspace members listed in `Cargo.toml:135-163`. "Direct dep"
means the crate's own `Cargo.toml` declares `tokio` or `tokio-util`
under `[dependencies]` or `[dev-dependencies]`. Test-only deps are
called out separately because they do not affect the production
dependency graph.

| Crate | Direct tokio dep? | Feature-gate | Justified? | Notes |
|-------|-------------------|--------------|------------|-------|
| `apple-fs` | No | n/a | n/a | clean |
| `bandwidth` | Yes (prod + dev) | `async` (`Cargo.toml:22`, `:27`) | Yes - `AsyncRateLimiter` exposes `pub async fn consume` and uses `tokio::time::sleep` | violation vs original #1779 framing |
| `batch` | No | n/a | n/a | clean |
| `branding` | No | n/a | n/a | clean |
| `checksums` | No | n/a | n/a | clean |
| `cli` | No | n/a | n/a | clean - good, CLI never owns a runtime |
| `compress` | No | n/a | n/a | clean |
| `core` | Yes (prod) | `async` (`Cargo.toml:93`) and `embedded-ssh` (`Cargo.toml:90`); `optional = true` at `:44` | Yes - facade for both `daemon`/`core` async and the embedded-ssh client | within #1779 baseline |
| `daemon` | Yes (prod + dev) | `async` (`Cargo.toml:20`, `:45`) | Yes - `AsyncDaemonListener`, `AsyncSession` per #1934 | within #1779 baseline |
| `embedding` | No | n/a | n/a | clean |
| `engine` | Yes (prod + dev) | `async` (`Cargo.toml:37`, `:97`) | Yes - `AsyncFileCopier`, `AsyncBatchCopier`; mirrors `bandwidth` justification | violation vs original #1779 framing |
| `fast_io` | No | n/a | n/a | clean. fast_io owns its own async surfaces but uses io_uring / IOCP / dispatch_io directly, not tokio |
| `filters` | No | n/a | n/a | clean |
| `flist` | No | n/a | n/a | clean |
| `logging` | No | n/a | n/a | clean |
| `logging-sink` | No | n/a | n/a | clean |
| `match` | No | n/a | n/a | clean |
| `metadata` | No | n/a | n/a | clean |
| `platform` | No | n/a | n/a | clean |
| `protocol` | Yes (prod + dev) | `async` (`Cargo.toml:29`, `:49-50`) | Yes - `MultiplexCodec`, `NegotiationPrologueSniffer::read_from_async` | violation vs original #1779 framing |
| `rsync_io` | Yes (prod + dev) | `embedded-ssh` (`Cargo.toml:26`, `:32`) | Yes - russh requires a tokio runtime; `connect_and_exec` builds a `tokio::runtime::Builder::new_current_thread()` to bridge sync `Read`/`Write` over a russh channel | not in #1779 baseline; landed with embedded-ssh |
| `signature` | No | n/a | n/a | clean |
| `test-support` | No | n/a | n/a | clean |
| `transfer` | Yes (prod) | `async` (`Cargo.toml:31-32`, `:104`) | Yes - `pipeline::async_pipeline::run_pipeline` returns `(PipelineHandle, impl Future)` and uses `tokio::sync::mpsc` and `tokio_util::sync::CancellationToken` | violation vs original #1779 framing |
| `windows-gnu-eh` | No | n/a | n/a | clean. Windows-MSVC unwind shim only |

Workspace root `Cargo.toml:188-189` declares the workspace dep
versions; nothing under `[dependencies]` in the root depends on tokio
directly. The default-on chain is
`bin/default = [..., "async"] -> async = ["daemon/async", "core/async"]
-> daemon/async = ["dep:tokio", "core/async"] -> core/async =
["dep:tokio", "engine/async", "transfer/async"] -> engine/async =
["dep:tokio", "dep:filetime"] && transfer/async = ["dep:tokio",
"dep:tokio-util"]`. So building `oc-rsync` with default features
links tokio. Building any crate downstream with
`--no-default-features` and without `--features async` does not.

## Allowed tokio consumers

The original audit #1779 named two crates: `daemon` and `core`. The
re-verification finds five additional consumers, each with a
defensible justification:

1. **`daemon`** (#1779 baseline). The `async-daemon` listener
   (#1934) lives in `crates/daemon/src/daemon/async_session/`.
   `AsyncDaemonListener::serve` (`listener.rs:180`),
   `AsyncDaemonListener::accept_one` (`listener.rs:264`),
   `AsyncDaemonListener::bind` (`listener.rs:128`), and
   `AsyncSession::handle` (`session.rs:68`) are the entry points.
   Gated behind `daemon/async` (`Cargo.toml:20`).
2. **`core`** (#1779 baseline). Re-exports from
   `engine/async` and `transfer/async`, plus the bridging point
   for `embedded-ssh` (`Cargo.toml:90`).
3. **`engine`** (post-#1779). `AsyncFileCopier::copy_file` and
   `AsyncBatchCopier::copy_files` are tokio-driven file copiers
   gated behind `engine/async` (`Cargo.toml:37`, `:97`). Used by
   `core::session()` when the async runtime is available.
4. **`transfer`** (post-#1779). The async pipeline
   (`pipeline/async_pipeline.rs`,
   `pipeline/async_dispatch.rs`) drives file jobs over a
   `tokio::sync::mpsc` channel with a `CancellationToken` for
   cooperative shutdown. `pub fn run_pipeline(...) -> (PipelineHandle,
   impl Future<Output = PipelineRunStats>)` is the public entry
   (`pipeline/async_pipeline.rs:137-142`).
5. **`bandwidth`** (post-#1779). `AsyncRateLimiter::consume`
   (`async_limiter.rs:77`) calls `tokio::time::sleep` on the
   token-bucket deficit path. Gated behind `bandwidth/async`
   (`Cargo.toml:22`).
6. **`protocol`** (post-#1779). `MultiplexCodec`
   (`multiplex/codec.rs:54`) implements
   `tokio_util::codec::{Decoder, Encoder}`, and
   `NegotiationPrologueSniffer::read_from_async`
   (`negotiation/sniffer/async_read.rs:53`) reads from an
   `AsyncRead`. Gated behind `protocol/async`
   (`protocol/Cargo.toml:29`).
7. **`rsync_io`** (post-#1779; embedded-ssh). The russh client
   requires a tokio runtime; `connect_and_exec`
   (`ssh/embedded/connect.rs:107-122`) builds a current-thread
   runtime and `block_on`s `connect_and_exec_async`. Gated behind
   `rsync_io/embedded-ssh` (`Cargo.toml:32`).

Of these, items 3-7 are drift relative to #1779. Items 5, 6, and 7
are the strongest violations of the original framing because
`bandwidth`, `protocol`, and `rsync_io` were explicitly listed in the
"must never contain unsafe code" set in this repository's design
notes. The unsafe-code rule is orthogonal to the tokio rule, but the
two rules share the spirit of "keep dependency surface small in
foundational crates"; if the unsafe rule applies, the tokio rule
plausibly should too.

`async` is default-on at the workspace root (`Cargo.toml:24-35`)
since #1732. Consumers that do not want tokio must use
`--no-default-features` and re-enable only the features they need
(e.g. `--no-default-features --features "zstd lz4 acl xattr iconv
parallel io_uring iocp copy_file_range"`). This is a behaviour shift
worth documenting; pre-#1732, opting out was the default and opting
in was explicit.

## Verification per the unsafe-adjacent dependency rule

The repository's design rule, paraphrased from the project guide, is
that tokio appears only in `daemon` and `core` behind the `async`
feature. Mapping that rule against the table above:

- **In compliance**: `daemon` (`crates/daemon/Cargo.toml:20`,
  `:45`), `core` (`crates/core/Cargo.toml:44`, `:90`, `:93`).
- **Drift since #1779, justified**: `engine`
  (`crates/engine/Cargo.toml:37`, `:97`), `transfer`
  (`crates/transfer/Cargo.toml:31-32`, `:104`), `bandwidth`
  (`crates/bandwidth/Cargo.toml:22`, `:27`), `protocol`
  (`crates/protocol/Cargo.toml:29`, `:49-50`), `rsync_io`
  (`crates/rsync_io/Cargo.toml:26`, `:32`).
- **In violation today**: none. Every direct tokio dep is
  feature-gated and tied to a public async surface that exists
  only when the gate is on.

The honest reading is that the rule "only daemon and core" is no
longer accurate. The corrected rule is "only `daemon`, `core`,
`engine`, `transfer`, `bandwidth`, `protocol`, `rsync_io` - each
behind a feature gate; never in `cli`, `apple-fs`, `batch`,
`branding`, `checksums`, `compress`, `embedding`, `fast_io`,
`filters`, `flist`, `logging`, `logging-sink`, `match`, `metadata`,
`platform`, `signature`, `test-support`, `windows-gnu-eh`."

## Transitive analysis: re-export vs internal

A crate that depends on tokio internally but does not name a tokio
type in its public API surface can still leak tokio to downstream
crates by re-exporting. Walking each justified consumer:

- **`bandwidth`**: `pub use crate::async_limiter::AsyncRateLimiter`
  (`lib.rs:15`). `AsyncRateLimiter::consume` is `pub async fn`, and
  its implementation calls `tokio::time::sleep`. The signature does
  not name a tokio type, but a caller cannot drive the future
  without a tokio runtime (sleep is tokio-runtime-bound). This is
  a **runtime leak**, not a type leak: the public type does not
  reference tokio, but the future can only be polled inside a
  tokio executor. Acceptable because the type is feature-gated.
- **`protocol`**: `pub use codec::MultiplexCodec` (`multiplex/mod.rs:34`)
  and `read_from_async<R: AsyncRead + Unpin>` on
  `NegotiationPrologueSniffer` (`negotiation/sniffer/async_read.rs:53`).
  Both are **type leaks**: `MultiplexCodec` implements
  `tokio_util::codec::Decoder`/`Encoder`, and the sniffer's bound
  is `tokio::io::AsyncRead`. Downstream code must import tokio to
  use either. Acceptable because the items are feature-gated; the
  alternative (define `oc_rsync_async::AsyncRead` parallel to
  `tokio::io::AsyncRead`) is not justified by ecosystem ergonomics.
- **`engine`**: `pub use async_io::{AsyncBatchCopier,
  AsyncFileCopier, AsyncIoError, CopyProgress, CopyResult}`
  (`lib.rs:231`). `AsyncFileCopier::copy_file` and
  `AsyncBatchCopier::copy_files` are `pub async fn`. Public
  signatures do not name tokio types directly, but
  implementations call `tokio::fs::File::open`. **Runtime leak**;
  same disposition as `bandwidth`.
- **`transfer`**: `pub mod async_pipeline` (`pipeline/mod.rs:73`)
  exposes `run_pipeline(...) -> (PipelineHandle, impl Future)`.
  `PipelineHandle` carries a `CancellationToken` (a
  `tokio_util::sync::CancellationToken`), but the field is
  private; only the methods `cancel`, `is_cancelled`,
  `files_completed`, `bytes_transferred` are public, and none
  names a tokio type. The returned `impl Future` must be polled
  on a tokio runtime because it `tokio::spawn`s and uses
  `tokio::select!`. **Runtime leak.**
- **`rsync_io`**: `pub use connect::{ChannelReader, ChannelWriter,
  connect_and_exec}` (`ssh/embedded/mod.rs:29`).
  `ChannelReader`/`Writer` implement `std::io::Read`/`Write` -
  the whole point of the bridge is to hide tokio behind a sync
  facade. `connect_and_exec` is a sync `pub fn` that builds the
  runtime internally. **No type or runtime leak**: external
  callers use sync `Read`/`Write` and do not need a tokio
  runtime to drive the result. The internal tokio dep is purely
  a russh implementation detail.
- **`core`**: re-exports facade APIs from `engine`/`transfer`/
  `rsync_io`. Inherits their leak posture.
- **`daemon`**: `pub use listener::AsyncDaemonListener`
  (`async_session/mod.rs:35`). `AsyncDaemonListener::bind` /
  `serve` / `accept_one` are `pub async fn`. **Runtime leak by
  design**: this crate is the integration point between rsync
  semantics and tokio.

The re-export disposition matters because it answers "does
disabling the `async` feature in a downstream crate suffice to
remove tokio from the build?" Today the answer is yes for every
crate listed: feature gates are correctly threaded so a
default-features-off build of `bin` with no async opt-in produces
a tokio-free binary.

## Async surface inventory

Every `pub async fn` and every `pub fn ... -> impl Future` in
`crates/` (excluding tests, dev-deps, and `#[tokio::test]` macros):

| Item | Location | Crate |
|------|----------|-------|
| `AsyncRateLimiter::consume` | `crates/bandwidth/src/async_limiter.rs:77` | `bandwidth` |
| `produce_file_jobs` | `crates/transfer/src/pipeline/async_dispatch.rs:29` | `transfer` |
| `run_pipeline` (returns `impl Future`) | `crates/transfer/src/pipeline/async_pipeline.rs:137-142` | `transfer` |
| `NegotiationPrologueSniffer::read_from_async` | `crates/protocol/src/negotiation/sniffer/async_read.rs:53` | `protocol` |
| `AsyncSession::handle` | `crates/daemon/src/daemon/async_session/session.rs:68` | `daemon` |
| `AsyncSession::acquire` | `crates/daemon/src/daemon/async_session/session.rs:247` | `daemon` |
| `AsyncDaemonListener::bind` | `crates/daemon/src/daemon/async_session/listener.rs:128` | `daemon` |
| `AsyncDaemonListener::serve` | `crates/daemon/src/daemon/async_session/listener.rs:180` | `daemon` |
| `AsyncDaemonListener::accept_one` | `crates/daemon/src/daemon/async_session/listener.rs:264` | `daemon` |
| `AsyncBatchCopier::copy_files` | `crates/engine/src/async_io/batch.rs:113` | `engine` |
| `AsyncFileCopier::copy_file` | `crates/engine/src/async_io/copier.rs:91` | `engine` |
| `AsyncFileCopier::copy_file_with_progress` | `crates/engine/src/async_io/copier.rs:108` | `engine` |
| `resolve_host` | `crates/rsync_io/src/ssh/embedded/resolve.rs:26` | `rsync_io` |
| `authenticate` | `crates/rsync_io/src/ssh/embedded/auth.rs:233` | `rsync_io` |

13 `pub async fn` plus 1 `pub fn -> impl Future`. All 14 sit in the
seven currently-allowed crates. None leaks into `cli`, `fast_io`,
`metadata`, `compress`, `filters`, `match`, `signature`, `flist`,
`batch`, `logging`, `logging-sink`, `branding`, `apple-fs`,
`platform`, `embedding`, `checksums`, `test-support`, or
`windows-gnu-eh`. Boundary holds at the public API surface.

Test-only async usage is more widespread: `#[tokio::test]` macros
appear in `bandwidth`, `transfer`, `protocol`, `engine`, and
`rsync_io` source files. Test-only tokio is gated through
`[dev-dependencies]` and does not contaminate the production
dependency graph.

## Drift findings

Comparing #1779's "only daemon and core" baseline against the
current state, five new tokio consumers landed:

1. **`engine` async I/O** (`engine/Cargo.toml:97`). New module
   `crates/engine/src/async_io/` introduces tokio-driven file
   copying; justified by the async file-writer trait design
   (#1655). The `StdFileWriter` path remains for non-async builds.
2. **`transfer` async pipeline** (`transfer/Cargo.toml:104`).
   New `crates/transfer/src/pipeline/async_pipeline.rs` and
   `async_dispatch.rs`; justified by the bounded-concurrency
   receiver design.
3. **`bandwidth` async limiter** (`bandwidth/Cargo.toml:22`).
   `AsyncRateLimiter` integrates rate-limiting with the tokio
   scheduler instead of `std::thread::sleep`.
4. **`protocol` async codec and sniffer**
   (`protocol/Cargo.toml:29`). `MultiplexCodec` plus
   `read_from_async`; downstream async pipelines drive a
   `Framed` over `tokio_util`.
5. **`rsync_io` embedded-ssh** (`rsync_io/Cargo.toml:32`).
   russh requires a tokio runtime; the bridge at
   `crates/rsync_io/src/ssh/embedded/connect.rs:107-122` builds
   one internally and exposes a sync `Read`/`Write` facade.

The `async` default-on switch (#1732) made these expansions visible
at build time. Pre-#1732, every downstream consumer had to opt in;
post-#1732, every consumer has to opt out. No tokio dep appears in
any crate identified as forbidden by the original audit; the drift
is in the policy text, not in unjustified code.

## Recommendation

1. **Update the policy text.** The rule "tokio: only daemon and core"
   is out of date. Replace with: "tokio is permitted in `daemon`,
   `core`, `engine`, `transfer`, `bandwidth`, `protocol`, and
   `rsync_io`. Each must gate the dep behind a feature
   (`async` or `embedded-ssh`). Tokio MUST NOT appear in `cli`,
   `apple-fs`, `batch`, `branding`, `checksums`, `compress`,
   `embedding`, `fast_io`, `filters`, `flist`, `logging`,
   `logging-sink`, `match`, `metadata`, `platform`, `signature`,
   `test-support`, or `windows-gnu-eh`."
2. **No code reverts.** Every existing direct tokio dep is
   correctly feature-gated and ties to a public async surface
   that downstream consumers explicitly opt into. Reverting any
   of them would remove user-facing capabilities (async daemon
   listener, async file copier, async pipeline, embedded-ssh).
3. **No new tokio consumers without an audit update.** Any PR
   that adds `tokio = { ... }` or `tokio-util = { ... }` to a
   crate not listed above must update both this audit and the
   project's policy notes.
4. **Document the `async` default semantics.** The workspace
   root's `Cargo.toml:24-35` comment block should mention that
   the `async` feature pulls tokio. Today the comment block at
   `:106-107` is the only such note; readers scanning the
   default feature list at `:24-35` see only `"async"` with
   no explanation.
5. **Consider hoisting embedded-ssh's tokio bridge into a
   dedicated `embedded_ssh` crate.** The `rsync_io` crate is
   otherwise tokio-free; the embedded-ssh bridge is the only
   reason `rsync_io` ships an optional tokio dep. A
   `embedded_ssh` crate would let `rsync_io` stay tokio-free
   even at the manifest level. Out of scope for this audit;
   tracked separately if pursued.

## Test plan

Add `tools/ci/check_tokio_boundary.sh` (30-50 lines of bash) to
guard the boundary. The script:

1. Reads each `crates/*/Cargo.toml`, scoped to `[dependencies]`
   and `[target.'cfg(...)'.dependencies]` only (dev-deps are
   allowed and ignored).
2. Greps for `^tokio` or `^tokio-util`.
3. Compares the resulting crate set to the allow-list
   {`bandwidth`, `core`, `daemon`, `engine`, `protocol`,
   `rsync_io`, `transfer`}.
4. Exits non-zero on (a) tokio in a non-allow-listed crate or
   (b) a tokio dep in an allow-listed crate that is not
   `optional = true`.

Wire it next to `tools/no_placeholders.sh` and
`tools/enforce_limits.sh`. Suggested CI job: `check-tokio-boundary`.
A future iteration can use `cargo-metadata` for full feature-graph
parsing, but bash plus a static allow-list catches the
accidental-expansion case which is the failure mode of interest.

## Cross-references

- `docs/audits/async-ssh-transport.md` (#1593) - SSH transport
  async question; references the embedded-ssh tokio bridge in
  `rsync_io`.
- `docs/audits/async-file-writer-trait.md` (#1655) - the
  unified async file-writer trait that motivates `engine/async`.
- `docs/audits/daemon-event-loop-multiplexing.md` - daemon
  event loop design; references the post-#1779 async listener
  work.
- Tasks: #1779 (original boundary, completed), #1732 (async
  default feature, completed), #1818 (post-default refactors,
  completed), #1934 (`AsyncDaemonListener` RFC).

## Upstream evidence

Upstream rsync 3.4.1 (`target/interop/upstream-src/rsync-3.4.1/`)
is single-threaded and uses blocking `read(2)` / `write(2)` on
its sockets. There is no upstream analogue of the tokio boundary;
async I/O in oc-rsync is a pure performance / structural
optimisation with no wire-protocol implication. The boundary
question is therefore an internal architectural concern, not a
correctness or interop concern.
