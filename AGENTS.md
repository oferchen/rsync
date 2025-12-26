````markdown
# AGENTS.md — Roles, Responsibilities, APIs, and Error/Message Conventions

This document defines the internal actors (“agents”), their responsibilities, APIs, invariants, and how user-visible messages (including Rust source file remapping) are produced. All binaries must route user-visible behavior through these agents via the `core` facade.

---

## Global Conventions

- **Canonical branding metadata** is sourced from `[workspace.metadata.oc_rsync]`
  in `Cargo.toml`. The branded entrypoint is the single binary **`oc-rsync`**
  that covers both client and daemon modes. Downstream packaging may provide
  compatibility symlinks if needed, but the workspace does **not** build
  separate wrapper binaries. The daemon configuration lives under
  `/etc/oc-rsyncd/` (for example `/etc/oc-rsyncd/oc-rsyncd.conf` and
  `/etc/oc-rsyncd/oc-rsyncd.secrets`), the published version string is
  `3.4.1-rust`, and the authoritative source repository URL is
  <https://github.com/oferchen/rsync>. Any user-facing surface
  (rustdoc examples, CLI help, documentation, packaging manifests, CI logs)
  must derive these values from the shared metadata via the `xtask branding`
  helpers or equivalent library APIs rather than hard-coding constants.

- **Error Message Suffix (C→Rust remap)**
  Format:
  `... (code N) at <repo-rel-path>:<line> [<role>=3.4.1-rust]`
  Implemented in `crates/core/src/message.rs` via:
  - `role: Role` enum (`Sender`, `Receiver`, `Generator`, `Server`, `Client`,
    `Daemon`) chosen at call-site.
  - `source_path: &'static str = file!()` and `source_line: u32 = line!()`,
    normalized to a repo-relative path.
  - Central constructor:
    `Message::error(code, text).with_role(role).with_source(file!(), line!())`.

  Roles in trailers **must** mirror upstream semantics exactly, with Rust-specific
  metadata only in the suffix trailer.

- **Centralized message strings**
  All info/warn/error/progress strings are centralized in
  `core::message::strings`. Snapshot tests assert that the shape and content of
  messages remains stable, except for the Rust source suffix and minor
  whitespace.

- **No fallback to system rsync**
  The workspace now runs **exclusively** via the native Rust engine. Client and
  daemon flows must not attempt to spawn a system `rsync` binary or honour any
  fallback environment variables. All code paths that previously delegated to
  external helpers must be removed or replaced with native implementations.

- **Workspace-wide nextest configuration**
  The repository uses `cargo nextest` as the primary test runner. A
  `.config/nextest.toml` file configures the **default** profile so that:

  - A bare `cargo nextest run` behaves sensibly for local development.
  - CI invokes `cargo nextest run --all-features --workspace` to guarantee the
    entire workspace is exercised.

  Any changes to `.config/nextest.toml` must preserve this contract: CI and
  local developers should not need to remember crate lists or non-obvious
  arguments just to run the full suite.

- **Complete test suite command**
  Before committing changes, run the complete validation suite:

  ```sh
  cargo fmt --all -- --check \
    && cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings \
    && cargo nextest run --workspace --all-features \
    && cargo xtask docs
  ```

  This ensures code formatting, lints, tests, and documentation all pass. See
  section 3.0 for details.

- **Standard-library-first**
  Prefer the Rust standard library and well-supported, actively maintained
  crates. Avoid deprecated APIs, pseudo-code, or placeholder logic; every change
  must ship production-ready behaviour with tests and/or parity checks.

- **CPU-accelerated hot paths**

  The rolling checksum and other hot paths use architecture-specific SIMD
  fast paths with scalar fallbacks:

  - `x86`/`x86_64`: AVX2 and SSE2.
  - `aarch64`: NEON.
  - Other architectures: scalar path only.

  Runtime feature detection is cached via `OnceLock` so repeated calls do not
  repeatedly invoke `is_x86_feature_detected!` / `is_aarch64_feature_detected!`.

  In `crates/checksums`:

  - `rolling::checksum::accumulate_chunk` provides the main SIMD entrypoint.
  - SIMD and scalar implementations must remain in **lockstep**, reusing the
    scalar helper for edge cases.
  - Parity tests such as
    `avx2_accumulate_matches_scalar_reference`,
    `sse2_accumulate_matches_scalar_reference`, and
    `neon_accumulate_matches_scalar_reference` must be updated whenever
    optimisations are introduced.

  The SIMD reducers rely on a `horizontal_sum_epi64` helper to collapse 64-bit
  partial sums without spilling to the stack; future AVX2/SSE2 changes should
  continue to reuse that helper.

  Sparse writing:

  - `crates/engine/src/local_copy/executor/file/sparse.rs` batches zero-run
    detection into 16-byte `u128` comparisons first, then falls back to a scalar
    prefix scan.
  - The `zero_run_length_matches_scalar_reference` test keeps vectorized and
    scalar paths aligned.
  - The implementation must preserve the **single seek-per-zero-run** invariant.

  Bandwidth parsing:

  - `crates/bandwidth` uses `memchr` to locate decimal separators and exponent
    markers so ASCII scans remain vectorised.
  - Updates must keep the byte-oriented fast path aligned with the exhaustive
    parser tests.

  Protocol negotiation:

  - Legacy readers in
    `crates/protocol::negotiation::sniffer::legacy` and
    `crates/transport::negotiation::stream::legacy` route newline detection
    through `memchr` so ASCII scanning benefits from SIMD across buffered and
    streaming reads.
  - Changes must preserve vectorised newline lookup and the replay buffer
    invariants.

  Rolling checksum updates:

  - The vectored updater coalesces small `IoSlice` buffers into a 128-byte stack
    scratch space before dispatching so SIMD back-ends can run on aggregated
    input. The `update_vectored_coalesces_small_slices` test must stay green.
  - `RollingChecksum::roll_many` uses weighted-delta aggregation to collapse
    per-byte loops into a handful of arithmetic reductions, with an escape hatch
    to the scalar `roll` path for exotic slice lengths.
  - Future optimisations must preserve the aggregated arithmetic and extend
    `roll_many_matches_single_rolls_for_long_sequences`.

  Version reporting:

  - `VersionInfoConfig::with_runtime_capabilities` surfaces the SIMD detection
    result via `checksums::simd_acceleration_available`, and `--version` output
    must be updated whenever new architecture-specific paths are introduced.

  Any additional CPU offloading should follow the same pattern: runtime feature
  detection (where applicable) plus deterministic tests that compare against a
  scalar reference.

- **Environment guardrails for tests**
  Environment-sensitive logic (fallback overrides, config paths, etc.) must use
  `EnvGuard`-style utilities so environment variables are always restored even if
  tests panic.

- **CI workflow contract**

  The GitHub Actions workflows are part of the public contract:

  - `.github/workflows/ci.yml`
    - Runs on `ubuntu-latest` for pushes to `master`/`main`, pull requests, and
      manual `workflow_dispatch`.
    - Jobs:
      - `lint-and-test`: `cargo fmt --all -- --check`,
        `cargo clippy --workspace --all-targets --all-features --no-deps`,
        and `cargo nextest run --all-features --workspace` (with
        `RUSTFLAGS="-D warnings"` and `Swatinem/rust-cache`).
  - `.github/workflows/build-cross.yml`
    - Builds cross-platform release artifacts from a Linux host using a
      target matrix (Linux/macOS/Windows; x86_64 and aarch64 where applicable).
    - Delegates packaging & SBOM generation to `cargo xtask` commands wherever
      possible so local and CI builds are aligned.

  All additional automation and validation should be surfaced via `cargo xtask`
  subcommands. CI jobs **must not** grow large ad-hoc shell or Python scripts
  that diverge from local tooling.

- **`xtask docs` decomposition**
  The former monolithic `docs.rs` handler now resides under
  `xtask/src/commands/docs/` with modules for:
  - argument parsing (`cli.rs`),
  - command execution (`executor.rs`),
  - validation (`validation/`).

  New helpers stay in these purpose-built modules so the hygiene guard passes and
  future contributors can extend validation logic without reintroducing a single
  huge file.

- **`xtask package` decomposition**
  The packaging command lives in `xtask/src/commands/package/`, split into:
  - `args.rs`,
  - `build.rs`,
  - `tarball.rs`,
  - `tests.rs`.

  Extend argument handling or packaging logic only in these focused modules; do
  not recreate monolithic `package.rs`.

- **CLI execution decomposition**
  `crates/cli/src/frontend/execution` is split into dedicated submodules
  (`options`, `module_listing`, `validation`, etc.). The higher-level drive
  orchestration is in `drive/` and its children:

  - `options.rs` — info/debug/bandwidth/compress parsing
  - `config.rs` — client builder assembly
  - `filters.rs` — include/exclude wiring
  - `summary.rs` — progress & output rendering
  - `fallback.rs` — legacy remote invocation argument preparation (deprecated; replace with native server wiring)
  - `metadata.rs` — preservation flags

  New functionality must join these modules directly (or new siblings), not
  `drive/mod.rs`.

- **Drive workflow layering**
  The orchestrator resides under
  `crates/cli/src/frontend/execution/drive/workflow/`:

  - `preflight.rs` — argument validation
  - `fallback_plan.rs` — legacy remote-fallback assembly (deprecated; replace with native transport plan)
  - `operands.rs` — usage rendering & operand checks

  New flow-control helpers belong in these focused modules (or new siblings)
  instead of inflating `workflow/mod.rs`.

- **CLI argument parser decomposition**
  `crates/cli/src/frontend/arguments` is a module tree:

  - `program_name.rs`
  - `bandwidth.rs`
  - `parsed_args.rs`
  - `env.rs`
  - `parser.rs`

  Parsing helpers and data structures must join the appropriate submodule so
  `parser.rs` remains orchestration-only.

- **Filter rule decomposition**
  CLI filter utilities live in `crates/cli/src/frontend/filter_rules/` covering:

  - filter arguments,
  - CVS exclusions,
  - directive parsing,
  - merge handling,
  - source loading.

  New behaviour belongs in the relevant submodule.

- **Local copy file executor decomposition**
  The large `crates/engine/src/local_copy/executor/file.rs` has been split into
  a `file/` module tree (`copy/`, `links`, `transfer`, etc.). All new file
  transfer behaviour must extend those helpers instead of recreating monolithic
  logic.

- **Bandwidth limiter decomposition**
  Throttling logic is under `crates/bandwidth/src/limiter/`:

  - `change.rs` — configuration
  - `core.rs` — runtime behaviour
  - `sleep.rs` — sleep utilities

- **Dir-merge parser decomposition**
  The dir-merge parser lives in `crates/engine/src/local_copy/dir_merge/parse/`
  (`types.rs`, `line.rs`, `merge.rs`, `dir_merge.rs`, `modifiers.rs`). Extend
  parsing via these modules, not via a new all-in-one file.

---

## Development Workflow Principles

### Upstream Rsync as the Source of Truth

All implementations **must** mirror upstream rsync behaviour. The upstream C
source code is the **ONLY** authoritative reference for protocol behaviour,
wire formats, and algorithmic details.

- **Study the C source**: Before implementing a feature, read the corresponding
  upstream rsync C code (e.g., `options.c` for CLI, `io.c` for I/O, `flist.c`
  for file lists, `generator.c` for sender logic, `receiver.c` for receiver).
  The C code is authoritative—not documentation, not comments, not memory.
- **Local upstream source**: The upstream rsync source is available at
  `target/interop/upstream-src/rsync-3.4.1/` after running interop tests.
  Always consult this when investigating protocol behaviour.
- **Code over documentation**: When upstream docs and code disagree, follow the
  code. Document any discrepancies you discover.
- **Code over memory**: Never rely on what you "remember" about how rsync works.
  Always verify against the actual upstream source code before implementing.
- **Test against upstream**: Run interop tests with upstream rsync versions
  (3.0.9, 3.1.3, 3.4.1) to verify wire-level compatibility.
- **Preserve semantics exactly**: Exit codes, error messages, progress output,
  and option behaviour must match upstream unless there's a documented reason.

### Parallel Task Execution

When possible, work on **multiple independent tasks concurrently**:

- Launch exploration/research agents in parallel when investigating different
  areas of the codebase
- Use parallel tool calls for independent file reads, searches, or tests
- Batch related changes across files when they don't depend on each other
- Run independent build/test commands concurrently

This maximises throughput and reduces total time to completion.

### Documentation Maintenance

Keep documentation synchronized with code:

- Update CLAUDE.md when adding new modules, conventions, or patterns
- Ensure rustdoc stays accurate — stale doc comments are worse than none
- When behaviour changes, update all relevant docs in the same commit
- Prefer inline rustdoc over separate markdown when documenting APIs

---

## Terminology Mapping: Upstream Rsync ↔ oc-rsync

This section cross-references upstream rsync C source with the Rust implementation to aid contributors familiar with the original codebase.

### Module/File Mapping

| Upstream File | oc-rsync Location | Notes |
|---------------|-------------------|-------|
| `flist.c` | `crates/flist/` | File list building and traversal |
| `generator.c` | `crates/core/src/server/generator.rs` | Generator role (sender) implementation |
| `receiver.c` | `crates/core/src/server/receiver.rs` | Receiver role implementation |
| `sender.c` | `crates/engine/` (partial) | Delta generation, mixed with local copy logic |
| `match.c` | `crates/checksums/src/rolling/` | Block matching via rolling checksum |
| `checksum.c` | `crates/checksums/src/strong/` | Strong checksums (MD4/MD5/SHA1/XXH) |
| `io.c` | `crates/io/` | Multiplexed I/O layer (imported as `rsync_io` to avoid `std::io` conflict) |
| `compat.c` | `crates/protocol/src/compat.rs` | Compatibility flags negotiation |
| `clientserver.c` | `crates/daemon/` | Daemon protocol implementation |
| `authenticate.c` | `crates/core/src/auth/` | Daemon authentication |
| `options.c` | `crates/cli/` | CLI argument parsing |
| `log.c` | `crates/logging/` | Logging and message output |
| `rsync.h` | `crates/protocol/src/constants.rs` | Protocol constants and magic numbers |

### Type/Concept Mapping

| Upstream Term | oc-rsync Term | Location |
|---------------|---------------|----------|
| `file_list` | `FileListWalker` | `crates/flist/` |
| `file_struct` | `FileListEntry` | `crates/flist/` |
| `sum_struct` | `Signature` / `FileSignature` | `crates/checksums/` |
| `map_struct` | `MappedFile` | `crates/engine/` |
| `stats` | `TransferStats` | Various |
| `rsum` | `RollingChecksum` | `crates/checksums/src/rolling/` |

### Function Mapping (Key Functions)

| Upstream Function | oc-rsync Equivalent | Location |
|-------------------|---------------------|----------|
| `send_file_list()` | `build_file_list()` | `crates/core/src/server/generator.rs:151` |
| `recv_file_list()` | `receive_file_list()` | `crates/core/src/server/receiver.rs` |
| `generate_files()` | `Generator::run()` | `crates/core/src/server/generator.rs` |
| `receive_data()` | Delta application in receiver | `crates/core/src/server/receiver.rs:13` |
| `send_files()` | `engine::send_files()` | `crates/engine/` |
| `match_sums()` | `DeltaGenerator` with `DeltaSignatureIndex` | `crates/engine/src/delta/` |
| `get_checksum1()` | `RollingChecksum::update()` | `crates/checksums/src/rolling/checksum/mod.rs:53` |

### Constant Mapping

| Upstream Constant | oc-rsync Constant | Location |
|-------------------|-------------------|----------|
| `PROTOCOL_VERSION` | `ProtocolVersion::NEWEST` | `crates/protocol/` |
| `CF_*` flags | `CompatibilityFlags::*` | `crates/protocol/src/compat.rs` |
| `XMIT_*` flags | `TransmitFlags::*` | `crates/protocol/` |
| `RERR_*` codes | `*_EXIT_CODE` constants | `crates/core/src/client/error.rs` |
| `MSG_*` tags | `MessageTag::*` | `crates/protocol/` |

### Version Handling

Version strings follow the format `x.y.z[-rust]`:
- `upstream_version`: Base rsync version (e.g., `3.4.1`)
- `rust_version`: Branded version with suffix (e.g., `3.4.1-rust`)
- Each component (x, y, z) may have leading zeros
- Centralized in `crates/branding/src/workspace/version.rs`
- Validated at build time via `crates/branding/build.rs`

### Crate Naming

Crate names align with upstream rsync file names:

- `flist` — mirrors `flist.c` (file list generation)
- `io` — mirrors `io.c` (imported as `rsync_io` to avoid `std::io` conflict)

Re-exports in `core` provide convenient access:
- `core::flist` — re-exports the `flist` crate
- `core::io` — re-exports `rsync_io` as `io`

---

## Agents Overview

### 1) Client & Daemon Entrypoint (CLI Binary)

- **Binary**: `src/bin/oc-rsync.rs`
- **Depends on**: `cli`, `core`, `io`, `daemon`, `logging`
- **Responsibilities**:
  - Parse CLI (Clap v4) and render upstream-parity help/misuse.
  - Dispatch into:
    - Client mode (default): build `CoreConfig` and call `core::run_client`.
    - Daemon/server mode (e.g. `--daemon`, `--server`): delegate to the daemon
      agent, still via `core`.
  - Route `--msgs2stderr`, `--out-format`, and `--info` / `--debug` flags to
    `logging`.
  - When invoked without transfer operands, emit the usage banner to **stdout**
    before surfacing the canonical “missing source operands” error so behaviour
    matches upstream and existing scripts.

- **Invariants**:
  - Never access protocol or engine directly; only via `core`.
  - `--version` reflects feature gates and prints `3.4.1-rust` with the
    runtime-capabilities trailer (SIMD, enabled features, etc.).
  - Daemon mode is a **mode** of `oc-rsync`, not a separate binary.

**Key API (binary-level)**:

```rust
pub fn main() -> ExitCode {
    let code = cli::run();
    cli::exit_code_from(code)
}
````

(*Dispatch into client vs daemon lives behind the `cli` crate’s frontend.*)

---

### 2) Daemon Agent (`rsyncd` semantics)

* **Crate**: `crates/daemon`

* **Mode**: Entered via `oc-rsync` CLI (e.g. `oc-rsync --daemon`)

* **Depends on**: `core`, `transport`, `logging`, `metadata`

* **Responsibilities**:

  * Listen on TCP; implement legacy `@RSYNCD:` negotiation for older clients,
    and the binary handshake for protocol 30+.
  * Apply `oc-rsyncd.conf` semantics (auth users, secrets 0600, chroot, caps).
  * Enforce daemon `--bwlimit` as both default and **cap**.
  * Integrate with systemd (sd_notify ready/status) for service units.

* **Invariants**:

  * Never bypass `core` for transfers or metadata; per-session work flows
    through `core::run_daemon_session`.
  * Secrets files must be permission-checked and errors rendered with
    upstream-compatible diagnostics.

**Key API**:

```rust
pub fn run_daemon_mode(args: DaemonArgs) -> Result<(), DaemonError> {
    let conf = load_config(&args)?;
    serve(conf) // loops; spawns sessions; per-session -> core::run_daemon_session
}
```

Daemon entrypoints are wired in the `cli` frontend; there is **no** separate
`oc-rsyncd` binary.

---

### 3) Core (Facade)

* **Crate**: `crates/core`

* **Depends on**:
  `protocol`, `engine`, `walk`, `filters`, `compress`, `checksums`, `metadata`,
  `logging`, `transport`

* **Responsibilities**:

  * Single facade for orchestration: file walking, selection, delta pipeline,
    metadata, xattrs/ACLs, messages, and progress.
  * Enforce centralisation: all transfers use `core::session()` and
    `CoreConfig`; both CLI and daemon go through here.
  * Error/message construction, including Rust source suffix and role trailers.

* **Invariants**:

  * No `unwrap` / `expect` on fallible paths; use stable error enums mapped to
    exit codes.
  * Role trailers (`[sender]`, `[receiver]`, `[generator]`, `[server]`,
    `[client]`, `[daemon]`) mirror upstream semantics.

**Key API**:

```rust
pub struct CoreConfig { /* builder-generated */ }

pub fn run_client(cfg: CoreConfig, fmt: logging::Format) -> Result<(), CoreError>;

pub fn run_daemon_session(ctx: DaemonCtx, req: ModuleRequest) -> Result<(), CoreError>;
```

---

### 4) Protocol (Handshake & Multiplexing)

* **Crate**: `crates/protocol`
* **Responsibilities**:

  * Version negotiation (protocol 32 down to 28), with constants copied from
    upstream.
  * Envelope read/write; multiplex `MSG_*` frames.
  * Legacy `@RSYNCD:` fallback and line-based negotiation for older clients.
  * Golden byte streams and fuzz tests for handshake and message framing.

**Key API**:

```rust
pub fn negotiate(io: &mut dyn ReadWrite) -> Result<Proto, ProtoError>;

pub fn send_msg(io: &mut dyn Write, tag: MsgTag, payload: &[u8]) -> io::Result<()>;

pub fn recv_msg(io: &mut dyn Read) -> io::Result<MessageFrame>;
```

---

### 5) Engine (Delta Pipeline)

* **Crate**: `crates/engine`

* **Responsibilities**:

  * Rolling checksum + strong checksum scheduling.
  * Block-match / literal emission per upstream heuristics.
  * `--inplace` / `--partial` behaviour; temp-file commit semantics.
  * Efficient local-copy executor with sparse support.

* **Performance**:

  * Buffer reuse; vectored I/O; cache-friendly data structures.
  * `delta/script.rs::apply_delta` caches the current basis offset so
    sequential `COPY` tokens avoid redundant seeks. The helper advances the
    tracked position with `u64::checked_add` and returns `InvalidInput` on
    overflow while reusing a shared copy buffer to minimise syscall churn.

#### Local Copy Layout

* `crates/engine/src/local_copy/` is decomposed into focused modules, with
  `executor/` containing:

  * `cleanup`
  * `directory`
  * `file`
  * `reference`
  * `sources`
  * `special`
  * `util`

  Sibling helpers:

  * `hard_links.rs`
  * `metadata_sync.rs`
  * `operands.rs`

* `local_copy/context.rs` uses `include!` to split `CopyContext` across
  `context_impl/impl_part*.rs`, each under the hygiene cap.

New work touching local copy must follow this structure.

---

### 6) Walk (File List)

* **Crate**: `crates/walk`
* **Responsibilities**:

  * Deterministic traversal; relative-path enforcement; path-delta compression.
  * Sorted lexicographic order; repeated-field elision for bandwidth savings.

---

### 7) Filters (Selection Grammar)

* **Crate**: `crates/filters`
* **Responsibilities**:

  * Parser/merger for `--filter`, includes/excludes, and `.rsync-filter`.
  * Property tests and snapshot goldens to ensure long-term stability.

---

### 8) Meta (Metadata/XAttrs/ACLs)

* **Crate**: `crates/metadata`
* **Responsibilities**:

  * Apply/record:

    * perms/uid/gid,
    * ns-mtime,
    * link counts,
    * devices/FIFOs/symlinks.
  * `-A/--acls` implies `--perms`; emit upstream-style diagnostics if ACL
    support is unavailable.
  * `-X/--xattrs` namespace rules and feature gating.

---

### 9) Compress (zlib/zstd)

* **Crate**: `crates/compress`
* **Responsibilities**:

  * Upstream defaults and negotiation for `-z` and `--compress-level`.
  * Throughput/ratio benchmarks and regression tests.

---

### 10) Checksums

* **Crate**: `crates/checksums`
* **Responsibilities**:

  * Rolling `rsum`; strong checksums (MD4/MD5/xxhash) as selected by protocol.
  * Property tests (window slide, truncation, seeds, SIMD vs scalar parity).

---

### 11) Transport

* **Crate**: `crates/transport`
* **Responsibilities**:

  * ssh stdio passthrough.
  * `rsync://` TCP transport, including daemon-side caps.
  * stdio multiplexing for subprocess-based fallbacks.
  * Timeouts/back-pressure and graceful shutdown.

---

### 12) Logging & Messages

* **Crates**: `crates/logging`, `crates/core::message`
* **Responsibilities**:

  * Mapping for `--info` / `--debug` flags.
  * `--msgs2stderr` handling; `--out-format` templating.
  * Central construction of user-visible messages via `core::message`.
  * Exit-code mapping and progress/summary parity.

---

## Library Integration Patterns

This section documents the design patterns and external dependencies used across
the codebase. All implementations must follow these patterns for consistency.

### Design Pattern Usage

**Strategy Pattern** — Used for algorithm selection based on runtime conditions:

- **Checksums**: `RollingChecksum` and `StrongChecksum` traits allow swapping
  algorithms (Adler32/SIMD vs MD4/MD5/XXH3) based on protocol version.
- **Protocol Codecs**: `NdxCodec` and `ProtocolCodec` traits select wire
  encoding format (legacy 4-byte LE vs modern varint) per protocol version.
- **Compression**: Algorithm selection (zlib/zstd) via trait objects.

```rust
// Strategy pattern example
pub trait RollingChecksum {
    fn update(&mut self, data: &[u8]);
    fn roll(&mut self, old_byte: u8, new_byte: u8);
    fn digest(&self) -> u32;
}
```

**Builder Pattern** — Used for complex object construction with validation:

- **FileEntry**: `FileEntryBuilder` with `.path()`, `.size()`, `.mtime()` etc.
- **CoreConfig**: Builder for transfer configuration.
- **FilterChain**: `.include()`, `.exclude()` method chaining.

```rust
// Builder pattern example
let entry = FileEntryBuilder::new()
    .path("src/main.rs")
    .file_type(FileType::Regular)
    .size(1024)
    .mtime(SystemTime::now())
    .build()?;
```

**State Machine Pattern** — Used for connection lifecycle management:

- **Daemon connections**: `ConnectionState` enum with transitions:
  `Greeting → ModuleSelect → Authenticating → Transferring → Closing`
- State transitions are explicit and validated.

```rust
// State machine states
pub enum ConnectionState {
    Greeting,
    ModuleSelect,
    Authenticating { module: String },
    Transferring { module: String, read_only: bool },
    Closing,
}
```

**Chain of Responsibility** — Used for filter rule evaluation:

- `FilterChain` evaluates rules in order, first match wins.
- Rules cascade: include → exclude → include patterns.

### External Dependencies

Core dependencies and their purposes:

| Crate | Purpose | Location |
|-------|---------|----------|
| `tokio` | Async runtime for daemon | `crates/daemon/`, `crates/transport/` |
| `tokio-util` | Codec framework for framing | `crates/protocol/` |
| `bytes` | Zero-copy buffer handling | Throughout |
| `governor` | Token bucket rate limiting | `crates/bandwidth/` |
| `flate2` | zlib compression | `crates/compress/` |
| `zstd` | zstd compression | `crates/compress/` |
| `xxhash-rust` | XXH3 strong checksum | `crates/checksums/` |
| `md-5`, `md4` | Legacy checksums | `crates/checksums/` |
| `walkdir` | Directory traversal | `crates/walk/` |
| `globset` | Pattern matching | `crates/filters/` |
| `filetime` | Timestamp preservation | `crates/metadata/` |
| `clap` | CLI parsing (derive macros) | `crates/cli/` |
| `thiserror` | Error derivation | Throughout |
| `indicatif` | Progress bars | `crates/cli/` |

### Version Compatibility Matrix

Protocol version support and corresponding upstream rsync versions:

| Protocol | Upstream Versions | Key Features | oc-rsync Support |
|----------|-------------------|--------------|------------------|
| 32 | 3.4.x | XXH3 checksums, incremental recursion | ✓ Full |
| 31 | 3.1.x–3.3.x | ID0_NAMES, goodbye exchange | ✓ Full |
| 30 | 3.0.x | Binary handshake, modern compat flags | ✓ Full |
| 29 | 2.6.4–2.6.9 | Multi-phase NDX_DONE, improved stats | ✓ Full |
| 28 | 2.6.0–2.6.3 | Legacy 4-byte encoding | ✓ Full |
| < 28 | < 2.6.0 | Obsolete protocols | ✗ Rejected |

**Interop Testing Matrix:**

| oc-rsync Role | Upstream 3.0.9 | Upstream 3.1.3 | Upstream 3.4.1 |
|---------------|----------------|----------------|----------------|
| Client → Daemon | ✓ Push/Pull | ✓ Push/Pull | ✓ Push/Pull |
| Daemon ← Client | ✓ Push/Pull | ✓ Push/Pull | ✓ Push/Pull |

### Library to Rsync Feature Mapping

Summary of which external libraries enable specific rsync functionality:

| Rsync Feature | Primary Library | Crate Location |
|---------------|-----------------|----------------|
| Block checksums | `xxhash-rust` (XXH3), `md-5`, `md4` | `crates/checksums/` |
| Rolling checksum | Custom SIMD (no external) | `crates/checksums/src/rolling/` |
| Delta encoding | Custom (no external) | `crates/engine/src/delta/` |
| File compression | `flate2` (zlib), `zstd` | `crates/compress/` |
| Protocol framing | `tokio-util` (Codec) | `crates/protocol/` |
| Async I/O | `tokio` | `crates/daemon/`, `crates/transport/` |
| Rate limiting | `governor` | `crates/bandwidth/` |
| File traversal | `walkdir` | `crates/walk/` |
| Pattern matching | `globset` | `crates/filters/` |
| Timestamp handling | `filetime` | `crates/metadata/` |
| CLI parsing | `clap` (derive) | `crates/cli/` |
| Progress display | `indicatif` | `crates/cli/` |
| Error handling | `thiserror` | Throughout |

### Subsystem Integration Guidelines

**Checksums (`crates/checksums/`)**:
- Rolling checksum must support `roll()` for sliding window.
- Strong checksums implement digest truncation per protocol.
- SIMD paths must have scalar fallbacks with parity tests.

**Protocol Framing (`crates/protocol/`)**:
- Use `tokio-util::codec` for Encoder/Decoder traits.
- Message size validation in codec, not application layer.
- Golden byte tests for wire format compatibility.

**File Operations (`crates/engine/`, `crates/metadata/`)**:
- Use `AtomicFile` for safe writes (temp file → sync → rename).
- Preserve metadata order: write content → set mtime → set perms.
- Handle `same-file` detection for safety.

**Filters (`crates/filters/`)**:
- Parse rsync filter syntax exactly (anchored `/`, directory `/`).
- Evaluate chain in order, first match determines fate.
- Support merge files (`.rsync-filter`).

**Bandwidth (`crates/bandwidth/`)**:
- Use `governor` for token bucket implementation.
- Handle large transfers by chunking token requests.
- Support dynamic limit changes mid-transfer.

**Daemon (`crates/daemon/`)**:
- Connection limiting via `tokio::sync::Semaphore`.
- Graceful shutdown via `broadcast` channel.
- systemd notification for service readiness.

**Error Handling**:
- Use `thiserror` for error type derivation.
- Map all errors to upstream-compatible exit codes.
- Include path context in I/O errors via extension trait.

```rust
// Error context extension
pub trait IoResultExt<T> {
    fn with_path(self, path: &Path) -> Result<T, RsyncError>;
}
```

---

## Exit Codes & Roles

* Exit codes map 1:1 to upstream rsync. Integration tests assert:

  * Known errors map to the expected code.
  * Unknown errors are clamped to a safe default range.

* Each agent sets its role for message trailers:

  * Client sender path → `[sender]`
  * Client receiver path → `[receiver]`
  * Generator on receive side → `[generator]`
  * Daemon process context → `[server]` / `[daemon]` as upstream does
  * CLI-only paths that are not part of protocol data flow may use `[client]`.

---

## Security & Timeouts

* Path normalisation and traversal prevention mirror upstream:

  * Relative paths only, unless explicitly allowed by options.
  * Symlink and device handling controlled by flags.

* Timeouts are applied at both transport and protocol layers; back-pressure from
  slow receivers must be respected rather than ignored.

* `secrets file` permissions:

  * Must be `0600`.
  * Violations emit upstream-style diagnostics with branded trailers.

---

## Interop & Determinism

* Loopback CI matrix across protocols 32–28 with upstream versions:

  * 3.0.9
  * 3.1.3
  * 3.4.1

* Upstream references are cloned from `https://github.com/RsyncProject/rsync`
  tags (`v3.0.9`, `v3.1.3`, `v3.4.1`) by dedicated interop tooling, which runs
  `./prepare-source` as needed before configuring and installing the binaries.

* Deterministic output:

  * `LC_ALL=C`
  * `COLUMNS=80`
  * Normalised metadata ordering and stable progress formatting.

* Error messages include the Rust source suffix as specified; snapshot tests
  assert presence and shape, but not specific line numbers.

---

## Lint & Hygiene Agents

### 2.1 File Header Convention

Every Rust source file **must** include a module-level doc comment on the first
line indicating the file's path relative to the codebase root:

```rust
//! crates/core/src/server/generator.rs
```

This convention:
- Enables quick identification of files when viewing code snippets
- Helps with navigation when reviewing diffs or logs
- Maintains consistency across the codebase

### 2.2 Comment Hygiene with Rustdoc

All comments must follow rustdoc conventions and provide genuine value:

- **Use `///` for public API documentation** — Explain what, why, and how
- **Use `//!` for module-level documentation** — Describe the module's purpose
- **Use `//` sparingly for inline implementation notes** — Only when the code
  isn't self-explanatory
- **Prefer self-documenting code** — Use descriptive names, extract functions,
  and structure code to minimize comment needs

#### Comment Cleanup Rules

**Proactive cleanup is mandatory.** When reviewing or writing code, actively
remove unhelpful comments. Comments that don't add value are technical debt
that obscures the code and misleads future readers.

1. **Delete restatement comments** — Comments that merely describe what the code
   does are noise. The code itself is the source of truth.
   ```rust
   // Bad: restates the code
   // Read the file
   let contents = fs::read_to_string(path)?;

   // Good: no comment needed, code is self-explanatory
   let contents = fs::read_to_string(path)?;
   ```

2. **Delete outdated comments** — Comments that no longer match the code are
   actively harmful. If you change code, update or remove associated comments.
   ```rust
   // Bad: comment says one thing, code does another
   // Send exactly 4 bytes for the header
   writer.write_all(&header[..8])?;  // Actually sends 8 bytes!

   // Good: remove the lie
   writer.write_all(&header[..8])?;
   ```

3. **Convert inline comments to rustdoc** — If a comment explains public API
   behavior, it belongs in `///` documentation, not inline `//`.
   ```rust
   // Bad: useful info hidden in inline comment
   // This timeout matches upstream rsync's SELECT_TIMEOUT
   pub const DEFAULT_TIMEOUT: u64 = 60;

   // Good: rustdoc makes it discoverable
   /// Default I/O timeout in seconds.
   ///
   /// Matches upstream rsync's `SELECT_TIMEOUT` constant from `io.c`.
   pub const DEFAULT_TIMEOUT: u64 = 60;
   ```

4. **Reference upstream when explaining non-obvious behavior** — When code
   mirrors upstream rsync behavior that isn't intuitive, cite the source.
   ```rust
   // Good: explains WHY with upstream reference
   // Protocol version is sent as (version * 1000000) for backwards compat
   // with rsync < 3.0 (see compat.c:setup_protocol)
   let wire_version = protocol_version * 1_000_000;
   ```

5. **Delete TODO/FIXME in production code** — Use issue tracking instead.
   The `no_placeholders` agent enforces this.

6. **Remove debug/checkpoint code after debugging** — File-based checkpoints
   (`std::fs::write("/tmp/...")`) are essential for daemon debugging but must
   be removed before committing. Search for and remove all checkpoint code:
   ```bash
   git grep 'std::fs::write.*tmp' crates/
   ```

### 2.3 Coding Principles

All code contributions must adhere to these principles:

- **Efficiency and Performance** — Optimize hot paths; avoid unnecessary
  allocations; use appropriate data structures for the access patterns.

- **Elegance and Conciseness** — Prefer clear, expressive code over verbose
  implementations. Remove dead code and simplify complex logic.

- **Modularity** — Follow the clean code philosophy of "do one thing and do it
  well." Each function, module, and trait should have a single, well-defined
  responsibility.

- **Ease of Maintenance** — Write code that future contributors can understand
  and modify. Use descriptive names, keep functions short, and avoid clever
  tricks that obscure intent.

- **Standard Library First** — Prefer `std` library types and traits over
  external dependencies unless there's a substantial, documented advantage.
  When external crates are necessary, choose well-maintained, actively
  supported libraries.

- **Design Patterns** — Apply appropriate design patterns (Strategy, Factory,
  State, etc.) to manage complexity, especially for protocol-version-specific
  behavior and role-based dispatch.

- **No Deprecated APIs** — Never use deprecated functions, methods, or
  libraries. If a dependency deprecates functionality, migrate to the
  recommended replacement promptly.

- **Complete and Functional** — All submitted code must be complete and
  functional for its intended purpose. No pseudo-code, placeholder
  implementations, or stub functions. The `no_placeholders` agent enforces
  this for macro-level violations.

### 2.4 `enforce_limits` Agent

* **Script:** `tools/enforce_limits.sh`

* **Backend:** `cargo run -p xtask -- enforce-limits …`

* **Purpose:** Enforce LoC caps and comment hygiene for Rust source files.

* **Configuration:**

  * Command-line flags (from `xtask enforce-limits`) control:

    * maximum allowed lines per file,
    * comment ratio limits,
    * allowed exceptions (e.g. autogenerated modules).
  * Environment variables such as `MAX_RUST_LINES` may be honoured by the
    command; see `cargo xtask enforce-limits --help`.

* **Usage:**

  ```sh
  bash tools/enforce_limits.sh
  # or, equivalently:
  cargo run -p xtask -- enforce-limits
  ```

### 2.5 `no_placeholders` Agent

* **Script:** `tools/no_placeholders.sh`

* **Purpose:** Ban `todo!`, `unimplemented!`, `FIXME`, `XXX`, and obvious
  placeholder panics from all Rust sources, including untracked files.

* **Implementation details:**

  * Uses a carefully tuned `grep -nEi` pattern that:

    * Matches common placeholder macros and comment tags.
    * Handles escaped quotes inside `panic!` string literals.
    * Avoids tripping on legitimate identifiers like `prefixme`.
  * Scans both tracked and untracked `*.rs` files via
    `git ls-files -z --cached --others --exclude-standard -- '*.rs'`.

* **Usage:**

  ```sh
  bash tools/no_placeholders.sh
  ```

  A non-zero exit code indicates placeholder code that must be removed.

---

## Build & Test Agents

### 3.0 Complete Test Suite

* **Purpose:** Run the complete validation suite locally before committing or submitting PRs.

* **Command:**

  ```sh
  cargo fmt --all -- --check \
    && cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings \
    && cargo nextest run --workspace --all-features \
    && cargo xtask docs
  ```

  This command chain ensures:
  1. Code formatting is correct (`cargo fmt --all -- --check`)
  2. All clippy lints pass with warnings denied (`cargo clippy`)
  3. All tests pass across the workspace (`cargo nextest run`)
  4. Documentation builds without errors (`cargo xtask docs`)

* **Usage notes:**

  * Run this command before committing to catch issues early.
  * The command uses `&&` so it stops at the first failure.
  * If any step fails, fix the issue and re-run from the beginning.
  * CI runs equivalent checks, so passing locally ensures CI will pass.

### 3.1 `lint` Agent (fmt + clippy)

* **Invoker:** `ci.yml` (`lint-and-test` job).

* **Purpose:** Enforce formatting and deny warnings.

* **CI behaviour:**

  * Uses `dtolnay/rust-toolchain@stable` with `rustfmt, clippy` components.
  * Uses `Swatinem/rust-cache@v2` for incremental builds.
  * Runs:

    ```sh
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
    ```

* **Local usage:**

  ```sh
  cargo fmt --all
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  ```

### 3.2 `test` Agent (nextest)

* **Invoker:** `ci.yml` (`lint-and-test` job).

* **Purpose:** Run unit/integration tests across the workspace via `cargo nextest`.

* **CI behaviour (simplified):**

  ```sh
  cargo nextest run --all-features --workspace
  ```

* **Local usage:**

  ```sh
  # default profile, all workspace tests
  cargo nextest run --all-features --workspace

  # alternative profile (e.g. with JUnit output), if configured
  cargo nextest run --profile ci --all-features --workspace
  ```

* **Contract:** CI and local development must both be able to run the **entire
  workspace** without bespoke crate lists. Changes to `.config/nextest.toml`
  must preserve that property.

### 3.3 `build-cross` Agent (release matrix)

* **Workflow:** `.github/workflows/build-cross.yml`

* **Purpose:** Build cross-platform release artifacts for the `oc-rsync` binary
  from a Linux host, targeting multiple OS/arch combinations.

* **Responsibilities:**

  * Use a matrix of targets (Linux/macOS/Windows; x86_64 + aarch64 where supported).
  * Build `oc-rsync` with `cargo build --release` for each target, using
    cross-compilation toolchains (e.g. Zig or target-specific GCC) as required.
  * Package artifacts into `tar.gz` / `.zip` bundles under a consistent
    `target/dist/` layout for release uploads.

* **Constraints:**

  * Automation should rely on `cargo`, `cargo xtask`, and packaging tools
    (`cargo-deb`, `cargo-generate-rpm`, etc.) instead of ad-hoc shell logic.
  * The matrix must keep parity with documented supported platforms.

### 3.4 `package` & `sbom` Agents (via `xtask`)

* **Commands:** `cargo xtask package`, `cargo xtask sbom`

* **Purpose:**

  * Build `.deb` and `.rpm` packages for Linux.
  * Generate a CycloneDX SBOM JSON for the workspace.

* **Example usage:**

  ```sh
  # Build Linux packages (without rebuilding binaries)
  cargo xtask package --no-build

  # Generate default SBOM
  cargo xtask sbom

  # Generate SBOM at a custom location
  cargo xtask sbom --output artifacts/rsync.cdx.json
  ```

* **CI integration:**

  * `build-cross.yml` should call these commands for Linux targets so SBOM and
    packages are produced from the same bits shipped to users.

---

## Troubleshooting & Debugging Agent

### 4.0 Daemon Mode Debugging Principles

When debugging daemon protocol issues, the standard debugging tools are unavailable:

* **Stderr unavailable**: Daemon mode closes or redirects file descriptor 2 (stderr).
  * ALL `eprintln!()` calls will **panic** when stderr is closed.
  * This includes debug logging, error diagnostics, and trace output.
  * The panic is **silent** from the daemon's perspective but causes worker threads to crash.

* **Impact on debugging**:
  * Traditional `dbg!()`, `println!()`, `eprintln!()` are unusable.
  * Panic messages don't appear in daemon logs.
  * Worker thread crashes manifest as protocol errors on the client side.

### 4.1 File-Based Checkpoint Debugging

The **only reliable** debugging technique for daemon worker threads is file-based checkpoints:

```rust
// Safe: writes succeed even when stderr is unavailable
let _ = std::fs::write("/tmp/checkpoint_name", "payload");
let _ = std::fs::write("/tmp/checkpoint_data", format!("{:?}", value));
```

**Checkpoint placement strategy**:

1. **Entry points**: Start of every major function
   ```rust
   pub fn handle_session(...) -> io::Result<()> {
       let _ = std::fs::write("/tmp/handle_session_ENTRY", "1");
       // ... function body
   }
   ```

2. **Before/after critical operations**: Bracket risky code
   ```rust
   let _ = std::fs::write("/tmp/BEFORE_operation", "1");
   let result = risky_operation()?;
   let _ = std::fs::write("/tmp/AFTER_operation", format!("{:?}", result));
   ```

3. **Branch points**: Track which code paths execute
   ```rust
   if protocol.as_u8() >= 30 {
       let _ = std::fs::write("/tmp/compat_MODERN", "1");
   } else {
       let _ = std::fs::write("/tmp/compat_LEGACY", "1");
   }
   ```

4. **Protocol data**: Log actual bytes sent/received
   ```rust
   let _ = std::fs::write("/tmp/varint_VALUE", format!("{}", value));
   let _ = std::fs::write("/tmp/bytes_SENT", format!("{:02x?}", &buffer));
   ```

**Checkpoint analysis**:

```bash
# Clear all checkpoints before test
rm -f /tmp/checkpoint_* /tmp/daemon_* /tmp/handle_*

# Run daemon test
rsync rsync://localhost:8873/testmodule/

# Check which checkpoints were created
ls -1t /tmp/*ENTRY* /tmp/*BEFORE* /tmp/*AFTER* | head -20

# Find the LAST successful checkpoint
ls -1t /tmp/* | head -1

# Read checkpoint data
cat /tmp/checkpoint_data
```

**Gap analysis**: If checkpoint A exists but checkpoint B (which should follow A) does not:
* Code between A and B either crashed or never executed
* Look for `eprintln!()`, `panic!()`, `unwrap()`, or early returns between A and B

### 4.2 Systematic Bug Hunt Methodology

**Phase 1: Establish baseline**
1. Run interop test to confirm failure mode
2. Document exact error message from client
3. Identify which role/agent is reporting the error

**Phase 2: Binary search for crash point**
1. Add checkpoints at function entry points across suspected code path
2. Run test and identify last successful checkpoint
3. Add checkpoints between last successful and first missing
4. Repeat until gap is < 10 lines of code

**Phase 3: Root cause analysis**

Common failure patterns in daemon mode:

* **Pattern 1: Silent eprintln! crash**
  * **Symptom**: Checkpoint A exists, checkpoint B never appears, no error logs
  * **Cause**: `eprintln!()` between A and B
  * **Fix**: Remove ALL `eprintln!()` from daemon code paths
  * **Search**:
    ```bash
    git grep -n 'eprintln!' crates/daemon/ crates/core/src/server/ crates/protocol/
    ```
  * **Critical locations**:
    * Low-level protocol functions (`varint.rs`, `multiplex.rs`)
    * Server role implementations (`generator.rs`, `receiver.rs`, `setup.rs`)
    * Daemon session handlers (`session_runtime.rs`, `module_access.rs`)

* **Pattern 2: Protocol timing issue**
  * **Symptom**: Client reports "unexpected tag N" or protocol parse errors
  * **Cause**: Data sent in wrong order or multiplex activated at wrong time
  * **Investigation**:
    1. Add checkpoints before/after protocol writes
    2. Log actual bytes written: `format!("{:02x?}", buffer)`
    3. Verify order matches upstream rsync flow
  * **Common issues**:
    * Compat flags sent AFTER multiplex activation (should be BEFORE)
    * Buffered data written after stream wrapped (flush before wrapping)
    * Only writer activated for multiplex (must activate BOTH reader and writer)

* **Pattern 3: Stream buffering issues**
  * **Symptom**: Client receives partial data or reads at wrong offset
  * **Cause**: Buffered data not flushed before stream mode changes
  * **Fix**: Always `flush()` before:
    * Activating multiplex
    * Wrapping streams in new abstractions
    * Handing off streams to different threads

* **Pattern 4: Bidirectional protocol deadlock**
  * **Symptom**: Process hangs, checkpoints stop appearing mid-protocol
  * **Cause**: Only one direction activated for multiplex/buffering
  * **Investigation**:
    1. Check if BOTH reader and writer are activated consistently
    2. Verify filter list/file list can be read/written simultaneously
  * **Fix**: Mirror all stream transformations on both reader and writer:
    ```rust
    if protocol.as_u8() >= 23 {
        reader = reader.activate_multiplex()?;  // Must activate BOTH
        writer = writer.activate_multiplex()?;
    }
    ```

### 4.3 Hidden Dependency Debugging

Low-level protocol functions are **high-risk** for hidden `eprintln!()` calls:

**Critical files to audit**:
* `crates/protocol/src/varint.rs` — varint encoding (used by ALL protocol exchanges)
* `crates/protocol/src/multiplex.rs` — message framing
* `crates/protocol/src/filters/wire.rs` — filter list encoding
* `crates/walk/src/wire/` — file list encoding

**Audit technique**:
```bash
# Find all eprintln! in protocol-critical files
git grep -n 'eprintln!' crates/protocol/ crates/walk/

# Check if varint functions have debug code
grep -A5 -B5 'pub fn write_varint' crates/protocol/src/varint.rs
grep -A5 -B5 'pub fn read_varint' crates/protocol/src/varint.rs
```

**Impact of hidden failures**:
* Varint write failure prevents compat flags exchange → client receives wrong data
* Varint read failure prevents filter list parsing → server crashes on first read
* Multiplex write failure sends unframed data → client interprets as wrong message type

### 4.4 Protocol Trace Instrumentation

For protocol-level debugging, wrap streams in tracing adapters:

```rust
// Example: TracingStream wrapper
struct TracingStream<T> {
    inner: T,
    name: &'static str,
}

impl<T: Read> Read for TracingStream<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let _ = std::fs::write(
            format!("/tmp/trace_{}_READ", self.name),
            format!("{} bytes: {:02x?}", n, &buf[..n])
        );
        Ok(n)
    }
}

impl<T: Write> Write for TracingStream<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = std::fs::write(
            format!("/tmp/trace_{}_WRITE", self.name),
            format!("{} bytes: {:02x?}", buf.len(), buf)
        );
        self.inner.write(buf)
    }
}
```

**Usage**:
```rust
let traced_read = TracingStream { inner: read_stream, name: "daemon_read" };
let traced_write = TracingStream { inner: write_stream, name: "daemon_write" };
```

**Analysis**: Trace files show exact byte sequences sent/received, making protocol mismatches visible.

### 4.5 Interop Test Debugging Workflow

When an interop test fails with upstream rsync client:

1. **Capture client error**:
   ```bash
   rsync -vvv rsync://localhost:8873/testmodule/ 2>&1 | tee /tmp/client_error.log
   ```

2. **Add server checkpoints**:
   * Entry points: `handle_session()`, `run_server_with_handshake()`, `Generator::run()`
   * Protocol steps: before/after compat exchange, multiplex activation, filter list read, file list send

3. **Run test and analyze gap**:
   ```bash
   rm -f /tmp/*ENTRY* /tmp/*BEFORE* /tmp/*AFTER*
   rsync rsync://localhost:8873/testmodule/
   ls -1t /tmp/* | head -20  # Find last successful checkpoint
   ```

4. **Focus search on gap**:
   * Add checkpoints every 5-10 lines in the gap
   * Log variable values: `format!("{:?}", value)`
   * Log buffer contents: `format!("{:02x?}", buffer)`

5. **Compare with upstream**:
   * Check upstream rsync source for equivalent flow (e.g., `daemon.c`, `main.c`)
   * Verify order of operations matches
   * Confirm data encoding matches wire format expectations

### 4.6 Debugging Checklist for Daemon Protocol Issues

Before investigating complex protocol behavior, verify these common issues first:

- [ ] **No eprintln! in daemon code paths** (search: `git grep 'eprintln!' crates/{daemon,core,protocol}/`)
- [ ] **Varint functions are clean** (`crates/protocol/src/varint.rs` has NO eprintln!)
- [ ] **Compat flags sent BEFORE multiplex** (if protocol >= 30)
- [ ] **Both reader and writer activated for multiplex** (if protocol >= 23)
- [ ] **Flush before activating multiplex** (`stdout.flush()` before wrapping)
- [ ] **Buffered data extracted before stream handoff** (`handshake.buffered` chained correctly)
- [ ] **File-based checkpoints at all critical points** (entry, before/after, branches)

### 4.7 Post-Resolution Cleanup

After fixing daemon issues, clean up temporary debugging code:

**Remove checkpoints**:
```bash
# Find all checkpoint writes
git grep -n 'std::fs::write.*tmp.*checkpoint' crates/

# Remove the lines (after verifying fix works without them)
# Keep only critical error-path logging that uses proper logging framework
```

**Remove tracing wrappers**:
```bash
# Find TracingStream usage
git grep -n 'TracingStream' crates/

# Remove wrapper instantiation and revert to plain streams
```

**Verify clean build**:
```bash
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --all-features
```

**Update investigation notes**: Document root cause and fix in `investigation.md` or commit message for future reference.

---

This document is the contract between the internal agents and the external
behaviour of **oc-rsync**. Changes to binaries, crates, or CI workflows must be
reflected here so contributors and reviewers can reason about the system as a
whole.

```
