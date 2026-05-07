# Pattern Usage Catalog

Tracking issue: [#2120](https://github.com/oferchen/oc-rsync/issues/2120).

## 1. Goal

Enumerate where each design pattern lives in the codebase so a new contributor
can find prior art before writing one-off code. When a problem looks familiar,
locate the matching pattern below, read the existing implementation, then
extend or copy its shape rather than reinventing one.

## 2. Strategy Pattern

Interchangeable algorithms selected at runtime behind a common trait.

- `crates/checksums/src/strong/` — strong checksum dispatch (MD4/MD5/XXH3/XXH128).
- `crates/compress/src/` — codec selection (zlib, zstd, none) per negotiated session.
- `crates/transfer/src/transfer_ops/` — per-file-type dispatch for transfer ops (#1186).
- `crates/engine/src/local_copy/deletion/strategy.rs` — `DeletionStrategy` for
  before/during/after/delay variants (#1294, done).
- `crates/engine/src/concurrent_delta/strategy.rs` — concurrent delta scheduling.
- `crates/protocol/src/error_recovery/strategy.rs` — error recovery policies.

When to add another instance: the call site needs to pick from a closed set of
algorithms whose inputs and outputs match a single trait.

## 3. Builder Pattern

Stepwise construction with validated finalization.

- `FileEntryBuilder` — file-list entry assembly (`crates/protocol/flist`).
- `CoreConfig` — orchestration surface in `crates/core`.
- `TransferConfigBuilder` — transfer session wiring in `crates/transfer`.
- `FilterChain` builder API — `crates/filters/src/chain.rs`.
- `ProtocolSetupConfig` — handshake/setup config in `crates/transfer/src/setup` (#1202).

When to add another instance: the type has more than ~4 optional fields, or
construction must validate cross-field invariants before yielding the value.

## 4. State Machine Pattern

Explicit lifecycle states with validated transitions.

- Daemon connection: `Greeting -> ModuleSelect -> Authenticating -> Transferring -> Closing`
  (`crates/daemon`).
- Transfer phases: `Handshake -> FilterExchange -> FileListTransfer -> DeltaTransfer ->
  Finalization -> Complete` (`crates/core` orchestration, `crates/transfer` execution).

When to add another instance: a long-lived object must reject operations that
are illegal for its current phase, and the phase set is closed and ordered.

## 5. Chain of Responsibility

Ordered handlers, first match wins.

- Filter rule evaluation: `FilterChain` walks rules in declaration order; first
  matching rule decides include/exclude (`crates/filters/src/chain.rs`).

When to add another instance: a request must be classified by the first rule
that matches, and rules are user-ordered with a documented precedence.

## 6. Dependency Inversion

Traits define seams; concrete implementations are swappable.

- `RollingChecksum`, `StrongChecksum` — `crates/checksums`.
- `Compressor` — `crates/compress`.
- `BufferAllocator` — buffer-pool seam (#1342).
- `IoBackend` — io_uring vs std I/O selection (#1821, `crates/fast_io`).
- `PlatformCopy` — `crates/fast_io/src/platform_copy` (Linux/macOS/Windows fast paths) (#1822).
- `IoStrategy` — pending consolidation seam (#1765, pending).

When to add another instance: a high-level module must remain unchanged when a
new platform, codec, or kernel feature lands behind the same call signature.

## 7. Type-State Pattern

Encode protocol phase in the type, not a runtime flag.

- Compression negotiation: `Negotiating<T> -> Negotiated<T>` so post-handshake
  callers cannot read codec parameters before negotiation completes (#1768, done).

When to add another instance: misuse should fail to compile, not panic, and the
phase set is small (<= 3) with a single forward transition.

## 8. Decorator Pattern

Wrap a writer/reader to add behaviour without changing its interface.

- Sparse writer: zero-run detection layered over the underlying file writer
  (#2132, pending) — adds 16-byte `u128` zero-run elision while preserving the
  `Write` contract.

When to add another instance: a cross-cutting concern (sparse, throttling,
metrics, encryption) must compose over any existing reader/writer without
forcing call sites to know which decorators are stacked.

## 9. Cross-References

- Crate-level summary: `AGENTS.md`, `CLAUDE.md` "Design Patterns" section.
- Architecture overview: `docs/architecture/`.
- Upstream parity: `target/interop/upstream-src/rsync-3.4.1/`.
