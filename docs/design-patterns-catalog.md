# Design Pattern Usage Catalog

This catalog records concrete sites in the workspace where the project's
documented design patterns have been applied. Each section names the pattern,
the problem it solves in this codebase, the trade-offs accepted, and 2-3
reference locations to read alongside the description.

The pattern set covered here is: Strategy, Builder, State Machine, Chain of
Responsibility, Dependency Inversion, and Single Responsibility (per-crate
scoping). Locations are given as `path:line` with line numbers correct as of
the commit that introduces this catalog.

## Strategy

Interchangeable algorithms behind a uniform interface, selected at runtime
based on negotiated capabilities or configured preferences.

### Sites

- `crates/checksums/src/strong/strategy/trait_def.rs:24` defines
  `ChecksumStrategy` (`Send + Sync`) with `compute`, `compute_into`,
  `digest_len`, `algorithm_kind`. Concrete strategies live in
  `crates/checksums/src/strong/strategy/impls.rs:13` (`Md4Strategy`,
  `Md5Strategy`, plus SHA-1/256/512, XXH64, XXH3, XXH3-128).
- `crates/checksums/src/strong/strategy/selector.rs:41`
  (`ChecksumStrategySelector::for_protocol_version`) returns
  `Box<dyn ChecksumStrategy>` chosen by negotiated protocol version - MD4 for
  pre-30, MD5 for >= 30 - and `:87` lets callers pick a kind directly.
- `crates/compress/src/strategy/traits.rs:26` defines `CompressionStrategy`
  with `compress`, `decompress`, `algorithm_kind`. The factory at
  `crates/compress/src/strategy/selector.rs:42`
  (`CompressionStrategySelector::for_protocol_version`) returns the right
  boxed strategy: `NoCompressionStrategy`, `ZlibStrategy`, optional
  `Lz4Strategy`, optional `ZstdStrategy`.

### Problem solved

Wire compatibility with upstream rsync requires multiple checksum and
compression algorithms negotiated at runtime, but the rest of the engine
should not branch on algorithm choice. A trait object plus a factory keeps
the call sites algorithm-agnostic and lets new algorithms ship behind feature
flags without disturbing receivers, generators, or the delta pipeline.

### Trade-offs

- One `Box<dyn ...>` per session: a small, one-shot allocation.
- Virtual dispatch on the hot path. Mitigated by amortising the call over
  large blocks (checksums) or buffered chunks (compression) so the indirect
  call cost is dwarfed by the inner loop.
- Algorithm-specific tuning has to fit through the trait surface; per-codec
  knobs (e.g. zstd long-range mode) are wired through dedicated builders or
  feature flags rather than the trait.

## Builder

Fluent, validated construction of complex configuration values where many
fields are optional and several pairs are mutually exclusive.

### Sites

- `crates/protocol/src/flist/entry/constructors.rs:19`
  (`FileEntry::new_with_type` plus the `new_file`, `new_directory`,
  `new_symlink`, `new_block_device`, `new_char_device` constructors) plays
  the builder role for `FileEntry` (defined at
  `crates/protocol/src/flist/entry/core.rs:32`). Each constructor commits to
  a `FileType` and threads through the shared internal builder so the boxed
  `FileEntryExtras` is only allocated when an extras field is set.
- `crates/core/src/client/config/builder/mod.rs:135` (`ClientConfigBuilder`)
  is the single source of truth for constructing `ClientConfig`
  (`crates/core/src/client/config/client/mod.rs:58`). `validate()` at line
  275 checks `--inplace`/`--append` against `--delay-updates` and
  `--partial-dir` before `build()` at line 299 hands back the config.
- `crates/transfer/src/config/builder.rs:64` (`ServerConfigBuilder`)
  fluent-constructs `ServerConfig` for the server-side transfer crate, with
  `build()` at line 470 returning `Result<ServerConfig, BuilderError>` after
  enforcing `--inplace` vs `--delay-updates`, `--append` vs `--partial-dir`,
  and `min_file_size <= max_file_size`.

### Problem solved

`ClientConfig` carries dozens of optional flags (preserve_*, delete modes,
filter rules, bandwidth, network, iconv, references). Hand-rolling
`new(...)` would require either an enormous parameter list or a partially
constructed struct. The builder pattern centralises invariant validation
(mutually exclusive options, size constraints) at the moment of `build()`,
producing a value that downstream consumers can treat as already valid.

### Trade-offs

- Every field is duplicated between the config and its builder. The setter
  macro in `crates/core/src/client/config/builder/mod.rs` and consistent
  field naming keep the cost manageable but it is real boilerplate.
- Validation runs only at `build()`, so misuse during builder construction
  cannot be caught by the type system. We accept this in exchange for an
  ergonomic chain.
- For values with very few fields (e.g. `FileEntry::new_directory`) we keep
  classic constructors that delegate to the shared private builder rather
  than expose a public builder type.

## State Machine

Explicit, named states with validated transitions, used wherever a session
or transfer has phases that constrain which operations are legal.

### Sites

- `crates/protocol/src/state/typestate.rs:14` (`ProtocolState<P>`) drives the
  per-session lifecycle through `Negotiation -> FileList -> Transfer ->
  Finalize` at compile time. Phase markers and the data they carry live in
  `crates/protocol/src/state/phases.rs:8` (`ProtocolPhase` trait) with one
  struct per phase. Compile-time typestate prevents calling
  transfer-only methods during negotiation.
- `crates/protocol/src/state/dynamic.rs:12` (`Phase` enum +
  `DynamicProtocolState`) provides the runtime-checked counterpart for cases
  where transitions depend on values that are only known at runtime. Both
  share the same phase vocabulary so logs and metrics line up.
- `crates/daemon/src/daemon/session_registry.rs:35` (`SessionState`) tracks
  daemon connection lifecycle states - `Handshaking`, `Authenticating`,
  `Listing`, `Transferring`, `Completed`, `Failed`. The registry is backed
  by `DashMap` so the accept loop can update state without blocking other
  threads.

### Problem solved

The protocol has phase-dependent operations (e.g. file-list reads only valid
between negotiation and transfer; goodbye stats only valid in finalize) and
the daemon must be able to introspect what each connection is currently
doing for monitoring, timeouts, and graceful shutdown. Encoding states
explicitly catches misuse and keeps observability faithful to the underlying
protocol.

### Trade-offs

- Two parallel state machines (typestate + dynamic) for the protocol. We
  accept the duplication because some callers cannot satisfy the typestate
  constraints (state stored in a field, recovered from disk, shared across
  threads).
- The daemon state enum is monotonic in practice but the registry lets you
  set any state at any time. Discipline in the call sites - the accept loop
  is the only writer for a given session - replaces type-level enforcement.

## Chain of Responsibility

Ordered handlers asked, in turn, to claim a request; the first match wins.
Used for filter rule evaluation across nested directory scopes.

### Sites

- `crates/filters/src/chain.rs:212` (`FilterChain`) holds a global
  `FilterSet`, a `Vec<DirMergeConfig>`, and an ordered `scopes: Vec<DirScope>`
  stack. `crates/filters/src/chain.rs:258` (`allows`) iterates `scopes` in
  reverse - innermost directory first - and falls through to the global set
  on no match.
- `crates/filters/src/chain.rs:274` (`allows_deletion`) reuses the same
  reverse-iteration walk for the receiver-side delete decision so include,
  exclude, and protect rules compose identically across scopes.
- The module preamble at `crates/filters/src/chain.rs:11` documents the
  pattern explicitly and references upstream `exclude.c:push_local_filters`
  and `pop_local_filters`, anchoring the implementation to the C source.

### Problem solved

Per-directory merge files (`.rsync-filter` and friends) introduce nested
rule scopes that override outer scopes. We need a single decision function
(`allows`) that walks scopes in well-defined order, short-circuits on the
first match, and falls back to a default-include when no scope claims the
path. Implementing this as an explicit chain keeps the per-rule cost
proportional to "rules consulted" rather than "rules in tree".

### Trade-offs

- Rule order matters. Authors must understand "first match wins" to write
  correct filter sets; this is intentional and matches upstream semantics.
- We re-walk the chain for each path. The expected scope depth is small and
  most calls short-circuit early; we have not found a pathological case
  worth caching for, but the structure leaves room for memoisation if
  needed.

## Dependency Inversion

High-level modules depend on traits, not concrete types. Implementations are
injected at construction time, which makes hot paths swappable and testable.

### Sites

- `crates/checksums/src/strong/mod.rs:138` (`StrongDigest`) abstracts MD4,
  MD5, SHA-1, SHA-256, SHA-512, XXH3, and XXH3-128 hashers behind a single
  trait with associated `Seed`, `Digest`, and `DIGEST_LEN`. Generators and
  receivers parameterise over `T: StrongDigest` rather than referencing
  concrete hashers.
- `crates/checksums/src/strong/strategy/trait_def.rs:24` (`ChecksumStrategy`)
  is the trait-object form of the same idea - used where the algorithm is
  only known at runtime, e.g. once a daemon negotiation has completed.
- `crates/compress/src/strategy/traits.rs:26` (`CompressionStrategy`) plays
  the same role for codecs. The transfer crate consumes a
  `Box<dyn CompressionStrategy>` chosen by
  `CompressionStrategySelector::for_protocol_version`, never a specific
  zlib/zstd/lz4 type.

### Problem solved

The delta pipeline, file-list builder, and protocol layer must run
identically against any negotiated checksum and codec. Inverting the
dependency lets the engine compile once and link in all algorithms without
threading per-algorithm generic parameters down through every call site.
Tests and benchmarks pick concrete implementations directly while production
selects via the negotiator.

### Trade-offs

- Static dispatch (`T: StrongDigest`) is cheaper but forces monomorphisation
  of every consumer. Dynamic dispatch (`Box<dyn ChecksumStrategy>`) is
  marginally slower but keeps codegen size bounded. The codebase uses both,
  picking the variant that matches the call site's lifetime and selection
  semantics.
- The traits intentionally do not expose algorithm-specific tunables. This
  keeps the abstraction clean but means novel knobs (e.g. zstd dictionary
  IDs) require new public surface in the implementing crate.

## Single Responsibility (per-crate scoping)

Each crate handles one concern. Cross-cutting work happens through narrow,
documented dependencies rather than monolithic modules.

### Sites

- `Cargo.toml:136` lists the workspace members. The crate set splits the
  domain along clean boundaries: `checksums`, `compress`, `bandwidth`,
  `filters`, `flist`, `protocol`, `metadata`, `signature`, `transfer`,
  `engine`, `daemon`, `cli`, `core`, `fast_io`, `logging`, `logging-sink`,
  `branding`, `batch`, `apple-fs`, `match`, `platform`. Each crate's
  `lib.rs` documents its single concern at the top.
- `crates/filters/src/chain.rs:1` and `crates/checksums/src/strong/mod.rs:1`
  both pin a single concern via their module preambles. The filters crate
  knows about rules, scopes, and merge files but never about the wire
  format; the checksums crate owns rolling and strong digests but never
  buffers, sockets, or files.
- Module decomposition continues inside crates: see
  `crates/core/src/client/config/builder/` (split builder by concern from
  flat module) and `crates/transfer/src/config/builder.rs` (validation logic
  isolated from `ServerConfig` definition in
  `crates/transfer/src/config/`). New code extends these submodules instead
  of reintroducing god-files.

### Problem solved

A wire-compatible rsync rewrite touches many domains - hashing, codecs,
filesystem metadata, ACLs, network transports, daemon lifecycle, CLI parsing.
Forcing each into its own crate creates a compile-time coupling graph: if
`metadata` accidentally pulls in `protocol`, `cargo` complains. SRP at the
crate level also lets per-crate clippy lints, unsafe-code policy, and
feature flags stay narrowly scoped.

### Trade-offs

- More crates means more `Cargo.toml` files and more crate-level docs.
  Workspace inheritance and shared dependency tables in the root manifest
  keep churn contained.
- Some logically tight pairs (e.g. `signature` and `match`) sit in separate
  crates to enforce direction. This is occasionally awkward when a shared
  helper would naturally live "between" them; we prefer adding a small,
  well-named helper crate over relaxing the boundary.
- API stability becomes a per-crate concern. Internal-only helpers are kept
  `pub(crate)` or `pub(super)` so the public surface of each crate stays
  small and reviewable.
