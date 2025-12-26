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

### Crate Dependency Graph

```
┌─────────────────────────────────────────────────────────────┐
│                          cli                                │
│                   (clap, indicatif)                         │
└──────────────────────────┬──────────────────────────────────┘
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                         core                                │
│              (orchestration facade)                         │
└───────┬─────────────┬─────────────┬─────────────┬───────────┘
        │             │             │             │
        ▼             ▼             ▼             ▼
┌───────────────┐ ┌───────────┐ ┌───────────┐ ┌───────────────┐
│    engine     │ │  daemon   │ │ transport │ │   logging     │
│ (delta xfer)  │ │  (rsyncd) │ │ (ssh/tcp) │ │ (sink/format) │
└───────┬───────┘ └─────┬─────┘ └─────┬─────┘ └───────────────┘
        │               │             │
        ▼               ▼             ▼
┌─────────────────────────────────────────────────────────────┐
│                       protocol                              │
│        (multiplex, negotiation, flist, varint)              │
└───────┬─────────────┬─────────────┬─────────────┬───────────┘
        │             │             │             │
        ▼             ▼             ▼             ▼
┌───────────┐ ┌───────────┐ ┌───────────┐ ┌───────────────────┐
│ checksums │ │  filters  │ │ compress  │ │     bandwidth     │
│ (xxh/md5) │ │ (globset) │ │(zlib/zstd)│ │  (token bucket)   │
└───────────┘ └───────────┘ └───────────┘ └───────────────────┘
        │             │             │             │
        └─────────────┴─────────────┴─────────────┘
                              │
                              ▼
                    ┌─────────────────┐
                    │    metadata     │
                    │ (perms/xattr)   │
                    └─────────────────┘
```

**Data Flow:**
- CLI parses arguments → builds `CoreConfig` → calls `core::run_client()`
- Core orchestrates: walk → filter → delta → compress → transport
- Daemon handles: accept → negotiate → auth → spawn transfer thread

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

### API Reference: Key Types

**Checksums (`crates/checksums/`)**:

```rust
// Rolling checksum for block matching
pub struct RollingChecksum {
    s1: u32,  // sum of bytes
    s2: u32,  // weighted sum
}

impl RollingChecksum {
    pub fn new() -> Self;
    pub fn update(&mut self, data: &[u8]);           // Process block
    pub fn roll(&mut self, old: u8, new: u8);        // Slide window
    pub fn roll_many(&mut self, old: &[u8], new: &[u8]);  // Batch roll
    pub fn digest(&self) -> u32;                      // Get checksum
}

// Strong checksum trait (MD4/MD5/SHA1/XXH3)
pub trait StrongDigest: Default {
    type Output: AsRef<[u8]>;
    fn update(&mut self, data: &[u8]);
    fn finalize(self) -> Self::Output;
}
```

**Protocol (`crates/protocol/`)**:

```rust
// Message codes for multiplexing
pub enum MessageCode {
    Data = 0,      // MSG_DATA - file content
    Error = 1,     // MSG_ERROR_XFER
    Info = 2,      // MSG_INFO
    // ... (see rsync.h MSG_* constants)
}

// Multiplexed frame
pub struct MessageFrame {
    code: MessageCode,
    payload: Vec<u8>,
}

// Protocol version-aware encoding
pub trait NdxCodec {
    fn write_ndx(&self, w: &mut impl Write, ndx: i32) -> io::Result<()>;
    fn read_ndx(&self, r: &mut impl Read) -> io::Result<i32>;
}
```

**File Operations (`crates/walk/`, `crates/engine/`)**:

```rust
// File metadata entry
pub struct FileEntry {
    pub path: PathBuf,
    pub file_type: FileType,
    pub size: u64,
    pub mtime: SystemTime,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

// Iterator over file tree
pub struct FileListWalker { /* ... */ }

impl Iterator for FileListWalker {
    type Item = io::Result<FileEntry>;
}

// Atomic file write (temp → sync → rename)
pub struct DestinationWriteGuard { /* ... */ }
```

**Filters (`crates/filters/`)**:

```rust
pub enum FilterAction { Include, Exclude, Protect, Risk, Clear }

pub struct FilterRule {
    pub action: FilterAction,
    pub pattern: Pattern,
    pub anchored: bool,
    pub directory_only: bool,
}

pub struct FilterSet {
    rules: Vec<FilterRule>,
}

impl FilterSet {
    pub fn matches(&self, path: &Path, is_dir: bool) -> Option<FilterAction>;
}
```

**Delta Operations (`crates/engine/src/delta/`)**:

```rust
// Delta instruction
pub enum DeltaOp {
    Copy { offset: u64, length: u32 },  // Copy from basis
    Literal(Vec<u8>),                    // New data
}

// Signature for delta generation
pub struct SignatureBlock {
    pub weak: u32,       // Rolling checksum
    pub strong: Vec<u8>, // Truncated strong hash
}

// Index for O(1) weak checksum lookup
pub struct DeltaSignatureIndex { /* HashMap<u32, Vec<usize>> */ }
```

**Error Types (`crates/core/`)**:

```rust
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, #[source] source: io::Error },

    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),

    // ... other variants
}

impl TransferError {
    /// Map to upstream rsync exit code
    pub fn exit_code(&self) -> i32;
}
```

### Workspace Dependency Configuration

The workspace uses a hybrid approach for dependency management:

**Workspace-level metadata** (`Cargo.toml` root):

```toml
[workspace.package]
edition = "2024"
rust-version = "1.88"
authors = ["..."]
license = "GPL-3.0-or-later"
version = "3.4.1-rust"
repository = "https://github.com/oferchen/rsync"
```

**Crate-level inheritance** (each `crates/*/Cargo.toml`):

```toml
[package]
name = "checksums"
version.workspace = true      # Inherit from workspace
edition.workspace = true      # Inherit from workspace
license.workspace = true      # Inherit from workspace

[dependencies]
# External dependencies - version specified per-crate
xxhash-rust = { version = "0.8", features = ["xxh64", "xxh3"] }
md-5 = { version = "0.10", features = ["std"] }

# Internal crates - always use path references
logging = { path = "../logging" }
protocol = { path = "../protocol" }
```

**Dependency guidelines:**

- External dependency versions are specified per-crate (no `[workspace.dependencies]`)
- Internal crates always use relative `path = "../crate_name"` references
- Feature flags follow the pattern: `feature = ["dep/feature"]`
- Optional dependencies use `dep:name` syntax in feature flags

### Async Transport Layer

The transport layer uses `tokio` for async I/O in daemon mode:

```rust
// Async transport pattern (crates/transport/)
use tokio::io::{AsyncRead, AsyncWrite, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub struct AsyncTransport<S> {
    stream: S,
    read_buffer: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncTransport<S> {
    pub async fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.stream.write_all(data).await
    }

    pub async fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf).await
    }
}
```

**Tokio codec pattern** (for protocol framing):

```rust
use tokio_util::codec::{Decoder, Encoder, Framed};
use bytes::{Buf, BufMut, BytesMut};

pub struct MultiplexCodec;

impl Decoder for MultiplexCodec {
    type Item = MessageFrame;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 { return Ok(None); }  // Need header
        let len = u32::from_le_bytes(src[..4].try_into().unwrap()) as usize;
        if src.len() < 4 + len { return Ok(None); }  // Need payload

        let header = src.split_to(4);
        let payload = src.split_to(len);
        Ok(Some(MessageFrame::decode(&header, &payload)?))
    }
}

impl Encoder<MessageFrame> for MultiplexCodec {
    type Error = io::Error;

    fn encode(&mut self, item: MessageFrame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let encoded = item.encode();
        dst.reserve(encoded.len());
        dst.put_slice(&encoded);
        Ok(())
    }
}

// Usage with Framed adapter
let framed = Framed::new(tcp_stream, MultiplexCodec);
```

### CLI Command Pattern

The CLI uses clap derive macros for argument parsing:

```rust
// CLI structure (crates/cli/)
use clap::{Parser, Subcommand, Args};

#[derive(Parser)]
#[command(name = "oc-rsync", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Source paths
    pub sources: Vec<PathBuf>,

    /// Destination path
    pub dest: Option<PathBuf>,

    /// Verbose output
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Recursive transfer
    #[arg(short, long)]
    pub recursive: bool,

    /// Archive mode (equals -rlptgoD)
    #[arg(short, long)]
    pub archive: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run as daemon
    #[command(name = "--daemon")]
    Daemon(DaemonArgs),
}

#[derive(Args)]
pub struct DaemonArgs {
    /// Configuration file path
    #[arg(long, default_value = "/etc/oc-rsyncd/oc-rsyncd.conf")]
    pub config: PathBuf,

    /// Port to listen on
    #[arg(long, default_value = "873")]
    pub port: u16,

    /// Run in foreground (don't detach)
    #[arg(long)]
    pub no_detach: bool,
}
```

**Argument expansion** (archive mode example):

```rust
impl Cli {
    pub fn expand_archive_mode(&mut self) {
        if self.archive {
            self.recursive = true;
            self.preserve_links = true;
            self.preserve_perms = true;
            self.preserve_times = true;
            self.preserve_group = true;
            self.preserve_owner = true;
            self.preserve_devices = true;
        }
    }
}
```

### Signature and Delta Generation

**Block signature generation** (rolling + strong checksums):

```rust
// Signature generation (crates/checksums/)
pub struct SignatureBlock {
    pub weak: u32,        // Rolling checksum
    pub strong: [u8; 16], // Truncated strong hash
}

pub fn generate_signatures(
    data: &[u8],
    block_size: usize,
    strong_hasher: &dyn StrongDigest,
) -> Vec<SignatureBlock> {
    data.chunks(block_size)
        .map(|block| {
            let mut rolling = RollingChecksum::new();
            rolling.update(block);

            let mut hasher = strong_hasher.clone();
            hasher.update(block);
            let strong = hasher.finalize();

            SignatureBlock {
                weak: rolling.digest(),
                strong: strong.as_ref()[..16].try_into().unwrap(),
            }
        })
        .collect()
}
```

**Delta generation with rolling search**:

```rust
// Delta generation (crates/engine/src/delta/)
pub fn generate_delta(
    new_data: &[u8],
    index: &DeltaSignatureIndex,
    block_size: usize,
) -> Vec<DeltaOp> {
    let mut ops = Vec::new();
    let mut rolling = RollingChecksum::new();
    let mut pos = 0;
    let mut literal_start = 0;

    // Initialize with first block
    if new_data.len() >= block_size {
        rolling.update(&new_data[..block_size]);
    }

    while pos + block_size <= new_data.len() {
        let weak = rolling.digest();

        // O(1) lookup in hash map
        if let Some(block_idx) = index.find_match(weak, &new_data[pos..pos + block_size]) {
            // Emit accumulated literal data
            if literal_start < pos {
                ops.push(DeltaOp::Literal(new_data[literal_start..pos].to_vec()));
            }

            // Emit copy instruction
            ops.push(DeltaOp::Copy {
                offset: (block_idx * block_size) as u64,
                length: block_size as u32,
            });

            pos += block_size;
            literal_start = pos;

            // Reset rolling checksum for next block
            if pos + block_size <= new_data.len() {
                rolling = RollingChecksum::new();
                rolling.update(&new_data[pos..pos + block_size]);
            }
        } else {
            // No match - roll the window forward by one byte
            if pos + block_size < new_data.len() {
                rolling.roll(new_data[pos], new_data[pos + block_size]);
            }
            pos += 1;
        }
    }

    // Emit final literal data
    if literal_start < new_data.len() {
        ops.push(DeltaOp::Literal(new_data[literal_start..].to_vec()));
    }

    ops
}
```

### Compression Decorator Pattern

Compression uses the decorator pattern for transparent stream wrapping:

```rust
// Compression decorator (crates/compress/)
use flate2::{Compress, Decompress, FlushCompress, FlushDecompress};

pub struct CompressedWriter<W: Write> {
    inner: W,
    compressor: Compress,
    buffer: Vec<u8>,
}

impl<W: Write> CompressedWriter<W> {
    pub fn new(writer: W, level: u32) -> Self {
        Self {
            inner: writer,
            compressor: Compress::new(flate2::Compression::new(level), true),
            buffer: vec![0; 32768],
        }
    }
}

impl<W: Write> Write for CompressedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let before = self.compressor.total_in();
        self.compressor.compress(buf, &mut self.buffer, FlushCompress::None)?;
        let after = self.compressor.total_in();

        // Write compressed output
        let compressed_len = self.compressor.total_out() as usize;
        self.inner.write_all(&self.buffer[..compressed_len])?;

        Ok((after - before) as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        // Finish compression and flush
        self.compressor.compress(&[], &mut self.buffer, FlushCompress::Finish)?;
        let len = self.compressor.total_out() as usize;
        self.inner.write_all(&self.buffer[..len])?;
        self.inner.flush()
    }
}

// Usage: wrap transport in compression
let compressed = CompressedWriter::new(transport, 6);
```

### Bandwidth Token Bucket

Rate limiting uses the governor crate for token bucket implementation:

```rust
// Bandwidth limiter (crates/bandwidth/)
use governor::{Quota, RateLimiter, Jitter};
use std::num::NonZeroU32;

pub struct BandwidthLimiter {
    limiter: RateLimiter</* ... */>,
    bytes_per_token: usize,
}

impl BandwidthLimiter {
    /// Create limiter with specified bytes per second
    pub fn new(bytes_per_second: u64) -> Self {
        // Token = 1KB, rate = bytes_per_second / 1024 tokens/sec
        let tokens_per_second = (bytes_per_second / 1024).max(1);
        let quota = Quota::per_second(NonZeroU32::new(tokens_per_second as u32).unwrap());

        Self {
            limiter: RateLimiter::direct(quota),
            bytes_per_token: 1024,
        }
    }

    /// Wait for permission to transfer `bytes` bytes
    pub async fn acquire(&self, bytes: usize) {
        let tokens = (bytes / self.bytes_per_token).max(1);

        for _ in 0..tokens {
            self.limiter.until_ready_with_jitter(Jitter::up_to(
                std::time::Duration::from_millis(10)
            )).await;
        }
    }

    /// Check if transfer can proceed immediately
    pub fn try_acquire(&self, bytes: usize) -> bool {
        let tokens = (bytes / self.bytes_per_token).max(1);
        for _ in 0..tokens {
            if self.limiter.check().is_err() {
                return false;
            }
        }
        true
    }
}

// Usage in transfer loop
async fn transfer_with_limit(data: &[u8], limiter: &BandwidthLimiter) {
    for chunk in data.chunks(8192) {
        limiter.acquire(chunk.len()).await;
        transport.write_all(chunk).await?;
    }
}
```

### Role Separation Architecture

The rsync protocol defines three primary roles that operate in a pipeline:

```
┌─────────────────────────────────────────────────────────────────┐
│                     SENDER SIDE                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │  Generator  │───▶│   Sender    │───▶│  Transport  │─────────┼──▶
│  │ (file list) │    │  (deltas)   │    │   (wire)    │         │
│  └─────────────┘    └─────────────┘    └─────────────┘         │
└─────────────────────────────────────────────────────────────────┘
                                                                  │
┌─────────────────────────────────────────────────────────────────┐
│                    RECEIVER SIDE                                │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │  Transport  │───▶│  Receiver   │───▶│   Writer    │         │
│  │   (wire)    │    │  (apply)    │    │  (atomic)   │         │
│  └─────────────┘    └─────────────┘    └─────────────┘         │
└─────────────────────────────────────────────────────────────────┘
```

**Generator** (`crates/core/src/server/generator.rs`):
- Builds file list from source directory
- Sends file metadata to receiver
- Coordinates transfer order

**Sender** (`crates/engine/`):
- Receives checksums from receiver
- Generates delta instructions
- Streams delta + literal data

**Receiver** (`crates/core/src/server/receiver.rs`):
- Applies delta instructions to basis files
- Writes output via atomic file operations
- Preserves metadata (permissions, times, ownership)

### Atomic File Operations

Safe file writing uses the temp file → sync → rename pattern:

```rust
// Atomic file operations (crates/engine/src/local_copy/executor/file/)
use std::fs::{File, OpenOptions};
use std::io::{self, Write, BufWriter};
use std::path::Path;

pub struct DestinationWriteGuard {
    temp_path: PathBuf,
    final_path: PathBuf,
    file: BufWriter<File>,
    committed: bool,
}

impl DestinationWriteGuard {
    pub fn new(final_path: &Path) -> io::Result<Self> {
        // Create temp file in same directory (for atomic rename)
        let temp_path = final_path.with_extension(".tmp");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;

        Ok(Self {
            temp_path,
            final_path: final_path.to_path_buf(),
            file: BufWriter::new(file),
            committed: false,
        })
    }

    pub fn commit(mut self) -> io::Result<()> {
        // Flush buffered data
        self.file.flush()?;

        // Sync to disk
        self.file.get_ref().sync_all()?;

        // Atomic rename
        std::fs::rename(&self.temp_path, &self.final_path)?;

        self.committed = true;
        Ok(())
    }
}

impl Write for DestinationWriteGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for DestinationWriteGuard {
    fn drop(&mut self) {
        if !self.committed {
            // Clean up temp file on failure
            let _ = std::fs::remove_file(&self.temp_path);
        }
    }
}
```

**Metadata preservation order** (critical for correct behavior):

```rust
// Apply metadata in correct order after content write
pub fn apply_metadata(path: &Path, entry: &FileEntry) -> io::Result<()> {
    // 1. Set modification time FIRST (some systems reset on chmod)
    filetime::set_file_mtime(path, entry.mtime.into())?;

    // 2. Set permissions
    std::fs::set_permissions(path, entry.mode.into())?;

    // 3. Set ownership (requires privileges)
    #[cfg(unix)]
    {
        use std::os::unix::fs::chown;
        let _ = chown(path, Some(entry.uid), Some(entry.gid));
    }

    Ok(())
}
```

### File List Generation

File entries use the builder pattern with support for special files:

```rust
// File entry builder (crates/walk/)
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub file_type: FileType,
    pub size: u64,
    pub mtime: SystemTime,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub link_target: Option<PathBuf>,
    pub device_info: Option<DeviceInfo>,
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub major: u32,
    pub minor: u32,
}

pub struct FileEntryBuilder {
    entry: FileEntry,
}

impl FileEntryBuilder {
    pub fn new() -> Self {
        Self {
            entry: FileEntry {
                path: PathBuf::new(),
                file_type: FileType::Regular,
                size: 0,
                mtime: SystemTime::UNIX_EPOCH,
                mode: 0o644,
                uid: 0,
                gid: 0,
                link_target: None,
                device_info: None,
            },
        }
    }

    pub fn path(mut self, path: impl Into<PathBuf>) -> Self {
        self.entry.path = path.into();
        self
    }

    pub fn file_type(mut self, ft: FileType) -> Self {
        self.entry.file_type = ft;
        self
    }

    pub fn size(mut self, size: u64) -> Self {
        self.entry.size = size;
        self
    }

    pub fn mtime(mut self, mtime: SystemTime) -> Self {
        self.entry.mtime = mtime;
        self
    }

    pub fn mode(mut self, mode: u32) -> Self {
        self.entry.mode = mode;
        self
    }

    pub fn device(mut self, major: u32, minor: u32) -> Self {
        self.entry.device_info = Some(DeviceInfo { major, minor });
        self
    }

    pub fn symlink_target(mut self, target: impl Into<PathBuf>) -> Self {
        self.entry.link_target = Some(target.into());
        self
    }

    pub fn build(self) -> Result<FileEntry, BuildError> {
        if self.entry.path.as_os_str().is_empty() {
            return Err(BuildError::MissingPath);
        }
        Ok(self.entry)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
    BlockDevice,
    CharDevice,
    Fifo,
    Socket,
}
```

**Path encoding for wire protocol**:

```rust
// Path component encoding (crates/protocol/)
pub struct PathComponent<'a> {
    bytes: &'a [u8],
}

impl<'a> PathComponent<'a> {
    /// Encode path for wire transmission
    /// - Paths are relative, never absolute
    /// - Uses '/' separator regardless of platform
    /// - Encodes special characters per rsync protocol
    pub fn encode(path: &'a Path) -> Self {
        // Convert to unix-style path bytes
        let bytes = path.to_str()
            .expect("path must be valid UTF-8")
            .replace('\\', "/")
            .into_bytes();
        Self { bytes: bytes.leak() }
    }
}
```

### Enhanced Filter Engine

Filters use Chain of Responsibility with explicit result types:

```rust
// Filter engine (crates/filters/)
use globset::{Glob, GlobMatcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterResult {
    Include,   // Explicitly included
    Exclude,   // Explicitly excluded
    NoMatch,   // No rule matched, continue to next
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterAction {
    Include,
    Exclude,
    Protect,   // Like exclude, but prevents deletion
    Risk,      // Like include, but allows deletion
    Hide,      // Exclude from transfer, but don't delete
    Show,      // Include in transfer listing
    Clear,     // Clear current filter rules
}

pub struct FilterRule {
    pub action: FilterAction,
    pub pattern: GlobMatcher,
    pub anchored: bool,        // Pattern starts with /
    pub directory_only: bool,  // Pattern ends with /
    pub negated: bool,         // Pattern starts with !
}

impl FilterRule {
    /// Parse a filter rule from rsync filter syntax
    pub fn parse(rule: &str) -> Result<Self, ParseError> {
        let (action, pattern) = if rule.starts_with("+ ") {
            (FilterAction::Include, &rule[2..])
        } else if rule.starts_with("- ") {
            (FilterAction::Exclude, &rule[2..])
        } else if rule.starts_with("P ") {
            (FilterAction::Protect, &rule[2..])
        } else if rule.starts_with("R ") {
            (FilterAction::Risk, &rule[2..])
        } else if rule.starts_with("H ") {
            (FilterAction::Hide, &rule[2..])
        } else if rule.starts_with("S ") {
            (FilterAction::Show, &rule[2..])
        } else {
            return Err(ParseError::InvalidAction);
        };

        let anchored = pattern.starts_with('/');
        let directory_only = pattern.ends_with('/');
        let clean_pattern = pattern
            .trim_start_matches('/')
            .trim_end_matches('/');

        let glob = Glob::new(clean_pattern)?;

        Ok(Self {
            action,
            pattern: glob.compile_matcher(),
            anchored,
            directory_only,
            negated: false,
        })
    }

    pub fn matches(&self, path: &Path, is_dir: bool) -> FilterResult {
        // Directory-only rules skip non-directories
        if self.directory_only && !is_dir {
            return FilterResult::NoMatch;
        }

        // Check if pattern matches
        let matches = if self.anchored {
            // Anchored: match from root
            self.pattern.is_match(path)
        } else {
            // Unanchored: match any path component
            path.ancestors()
                .any(|ancestor| self.pattern.is_match(ancestor))
        };

        if matches {
            match self.action {
                FilterAction::Include | FilterAction::Risk | FilterAction::Show => {
                    FilterResult::Include
                }
                FilterAction::Exclude | FilterAction::Protect | FilterAction::Hide => {
                    FilterResult::Exclude
                }
                FilterAction::Clear => FilterResult::NoMatch,
            }
        } else {
            FilterResult::NoMatch
        }
    }
}

pub struct FilterChain {
    rules: Vec<FilterRule>,
}

impl FilterChain {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn add_rule(&mut self, rule: FilterRule) {
        self.rules.push(rule);
    }

    /// Evaluate filter chain - first match wins
    pub fn evaluate(&self, path: &Path, is_dir: bool) -> FilterResult {
        for rule in &self.rules {
            match rule.matches(path, is_dir) {
                FilterResult::NoMatch => continue,
                result => return result,
            }
        }
        FilterResult::NoMatch  // Default: no explicit include/exclude
    }

    /// Load rules from .rsync-filter file
    pub fn load_merge_file(&mut self, path: &Path) -> io::Result<()> {
        let content = std::fs::read_to_string(path)?;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Ok(rule) = FilterRule::parse(line) {
                self.add_rule(rule);
            }
        }
        Ok(())
    }
}
```

### Daemon State Machine

Connection lifecycle uses explicit state transitions:

```rust
// Daemon state machine (crates/daemon/)
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub enum ConnectionState {
    /// Initial state: waiting for protocol greeting
    Greeting,

    /// Client selecting a module
    ModuleSelect,

    /// Authenticating for a specific module
    Authenticating {
        module: String,
        challenge: [u8; 16],
    },

    /// Active transfer in progress
    Transferring {
        module: String,
        read_only: bool,
        client_addr: std::net::SocketAddr,
    },

    /// Connection closing (graceful or error)
    Closing {
        reason: CloseReason,
    },
}

#[derive(Debug, Clone)]
pub enum CloseReason {
    Complete,
    ClientDisconnect,
    AuthFailure,
    ProtocolError(String),
    Timeout,
    ServerShutdown,
}

impl ConnectionState {
    /// Validate state transition
    pub fn can_transition_to(&self, next: &ConnectionState) -> bool {
        use ConnectionState::*;
        match (self, next) {
            (Greeting, ModuleSelect) => true,
            (ModuleSelect, Authenticating { .. }) => true,
            (ModuleSelect, Transferring { .. }) => true,  // Anonymous module
            (Authenticating { .. }, Transferring { .. }) => true,
            (Authenticating { .. }, Closing { .. }) => true,  // Auth failed
            (Transferring { .. }, Closing { .. }) => true,
            (_, Closing { .. }) => true,  // Can always close
            _ => false,
        }
    }

    pub fn transition(self, next: ConnectionState) -> Result<ConnectionState, StateError> {
        if self.can_transition_to(&next) {
            Ok(next)
        } else {
            Err(StateError::InvalidTransition {
                from: format!("{:?}", self),
                to: format!("{:?}", next),
            })
        }
    }
}

/// Daemon server with graceful shutdown
pub struct DaemonServer {
    config: DaemonConfig,
    shutdown_tx: broadcast::Sender<()>,
    connection_limit: tokio::sync::Semaphore,
}

impl DaemonServer {
    pub fn new(config: DaemonConfig, max_connections: usize) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            config,
            shutdown_tx,
            connection_limit: tokio::sync::Semaphore::new(max_connections),
        }
    }

    /// Signal graceful shutdown to all connections
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    /// Accept connections with graceful shutdown support
    pub async fn accept_loop(&self, listener: tokio::net::TcpListener) {
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        loop {
            tokio::select! {
                // Accept new connection
                result = listener.accept() => {
                    match result {
                        Ok((socket, addr)) => {
                            // Acquire connection permit
                            let permit = match self.connection_limit.try_acquire() {
                                Ok(p) => p,
                                Err(_) => {
                                    // At connection limit, reject
                                    drop(socket);
                                    continue;
                                }
                            };

                            let shutdown_rx = self.shutdown_tx.subscribe();
                            tokio::spawn(async move {
                                let _permit = permit;  // Hold until connection ends
                                handle_connection(socket, addr, shutdown_rx).await;
                            });
                        }
                        Err(e) => {
                            eprintln!("Accept error: {}", e);
                        }
                    }
                }

                // Shutdown signal received
                _ = shutdown_rx.recv() => {
                    break;
                }
            }
        }
    }
}
```

### Protocol Multiplexing Details

Message framing with protocol-specific tags:

```rust
// Protocol multiplexing (crates/protocol/)

/// Message tags mirror upstream rsync MSG_* constants
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageTag {
    Data = 0,           // MSG_DATA - raw file data
    ErrorXfer = 1,      // MSG_ERROR_XFER - transfer error
    Info = 2,           // MSG_INFO - informational message
    Error = 3,          // MSG_ERROR - fatal error
    Warning = 4,        // MSG_WARNING - warning message
    ErrorSocket = 5,    // MSG_ERROR_SOCKET - socket error
    Log = 6,            // MSG_LOG - log message
    ClientInfo = 7,     // MSG_CLIENT_INFO - client info
    ErrorUtf8 = 8,      // MSG_ERROR_UTF8 - UTF-8 error
    Redo = 9,           // MSG_REDO - redo file
    Flist = 20,         // MSG_FLIST - file list data
    FlistEof = 21,      // MSG_FLIST_EOF - end of file list
    IoError = 22,       // MSG_IO_ERROR - I/O error code
    NoDel = 23,         // MSG_NODEL - no delete
    Sum = 24,           // MSG_SUM - checksum data
    DelStats = 25,      // MSG_DELETED_STATS - deletion stats
    Success = 100,      // MSG_SUCCESS - success marker
    Deleted = 101,      // MSG_DELETED - file deleted
    NoSend = 102,       // MSG_NO_SEND - don't send file
}

impl MessageTag {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Data),
            1 => Some(Self::ErrorXfer),
            2 => Some(Self::Info),
            3 => Some(Self::Error),
            // ... etc
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self,
            Self::ErrorXfer |
            Self::Error |
            Self::ErrorSocket |
            Self::ErrorUtf8 |
            Self::IoError
        )
    }

    pub fn is_info(&self) -> bool {
        matches!(self, Self::Info | Self::Warning | Self::Log | Self::ClientInfo)
    }
}

/// Multiplexed message frame
pub struct MultiplexFrame {
    pub tag: MessageTag,
    pub payload: Vec<u8>,
}

impl MultiplexFrame {
    /// Encode frame for wire transmission
    /// Format: [tag:1][len:3][payload:len]
    pub fn encode(&self) -> Vec<u8> {
        let len = self.payload.len() as u32;
        let header = ((self.tag as u32) << 24) | (len & 0x00FFFFFF);

        let mut buf = Vec::with_capacity(4 + self.payload.len());
        buf.extend_from_slice(&header.to_le_bytes());
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Decode frame from wire format
    pub fn decode(data: &[u8]) -> Result<(Self, usize), DecodeError> {
        if data.len() < 4 {
            return Err(DecodeError::Incomplete);
        }

        let header = u32::from_le_bytes(data[..4].try_into().unwrap());
        let tag_byte = ((header >> 24) & 0xFF) as u8;
        let len = (header & 0x00FFFFFF) as usize;

        let tag = MessageTag::from_byte(tag_byte)
            .ok_or(DecodeError::InvalidTag(tag_byte))?;

        if data.len() < 4 + len {
            return Err(DecodeError::Incomplete);
        }

        let payload = data[4..4 + len].to_vec();
        Ok((Self { tag, payload }, 4 + len))
    }
}
```

### Incremental Recursion

Protocol 30+ supports incremental file list transfer:

```rust
// Incremental recursion (crates/protocol/)

/// Incremental recursion state
pub struct IncrementalState {
    /// Directories pending expansion
    pending_dirs: VecDeque<FileIndex>,
    /// Current phase
    phase: IncRecursePhase,
    /// Files received so far
    file_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncRecursePhase {
    /// Initial file list exchange
    Initial,
    /// Receiving incremental updates
    Incremental,
    /// All directories expanded
    Complete,
}

impl IncrementalState {
    pub fn new() -> Self {
        Self {
            pending_dirs: VecDeque::new(),
            phase: IncRecursePhase::Initial,
            file_count: 0,
        }
    }

    /// Queue directory for later expansion
    pub fn queue_directory(&mut self, index: FileIndex) {
        self.pending_dirs.push_back(index);
    }

    /// Get next directory to expand (generator side)
    pub fn next_pending(&mut self) -> Option<FileIndex> {
        self.pending_dirs.pop_front()
    }

    /// Check if incremental recursion is complete
    pub fn is_complete(&self) -> bool {
        self.phase == IncRecursePhase::Complete
    }

    /// Transition to next phase
    pub fn advance_phase(&mut self) {
        self.phase = match self.phase {
            IncRecursePhase::Initial => IncRecursePhase::Incremental,
            IncRecursePhase::Incremental if self.pending_dirs.is_empty() => {
                IncRecursePhase::Complete
            }
            other => other,
        };
    }
}
```

### Enhanced Error Handling

Error types with path context and exit code mapping:

```rust
// Error handling (crates/core/)
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RsyncError {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("protocol error: {message}")]
    Protocol { message: String },

    #[error("authentication failed for module '{module}'")]
    AuthFailed { module: String },

    #[error("permission denied: {path}")]
    PermissionDenied { path: PathBuf },

    #[error("file not found: {path}")]
    NotFound { path: PathBuf },

    #[error("partial transfer: {transferred}/{total} files")]
    PartialTransfer { transferred: usize, total: usize },

    #[error("timeout after {seconds}s")]
    Timeout { seconds: u64 },

    #[error("remote error: {message}")]
    Remote { message: String },
}

impl RsyncError {
    /// Map to upstream rsync exit codes
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Io { source, .. } => {
                match source.kind() {
                    io::ErrorKind::NotFound => 23,      // RERR_FILEIO
                    io::ErrorKind::PermissionDenied => 23,
                    io::ErrorKind::ConnectionRefused => 10,  // RERR_SOCKETIO
                    _ => 12,  // RERR_STREAMIO
                }
            }
            Self::Protocol { .. } => 12,       // RERR_STREAMIO
            Self::AuthFailed { .. } => 5,      // RERR_UNAUTHORIZED
            Self::PermissionDenied { .. } => 23,  // RERR_FILEIO
            Self::NotFound { .. } => 23,       // RERR_FILEIO
            Self::PartialTransfer { .. } => 24,  // RERR_PARTIAL
            Self::Timeout { .. } => 30,        // RERR_TIMEOUT
            Self::Remote { .. } => 12,         // RERR_STREAMIO
        }
    }
}

/// Exit code constants (matching upstream rsync)
pub mod exit_codes {
    pub const OK: i32 = 0;
    pub const SYNTAX: i32 = 1;          // RERR_SYNTAX
    pub const PROTOCOL: i32 = 2;        // RERR_PROTOCOL
    pub const FILESELECT: i32 = 3;      // RERR_FILESELECT
    pub const UNSUPPORTED: i32 = 4;     // RERR_UNSUPPORTED
    pub const UNAUTHORIZED: i32 = 5;    // RERR_UNAUTHORIZED
    pub const SOCKETIO: i32 = 10;       // RERR_SOCKETIO
    pub const FILEIO: i32 = 11;         // RERR_FILEIO
    pub const STREAMIO: i32 = 12;       // RERR_STREAMIO
    pub const MESSAGEIO: i32 = 13;      // RERR_MESSAGEIO
    pub const IPC: i32 = 14;            // RERR_IPC
    pub const CRASHED: i32 = 15;        // RERR_CRASHED
    pub const MALLOC: i32 = 21;         // RERR_MALLOC
    pub const PARTIAL_TRANSFER: i32 = 23; // RERR_PARTIAL
    pub const VANISHED: i32 = 24;       // RERR_VANISHED
    pub const DEL_LIMIT: i32 = 25;      // RERR_DEL_LIMIT
    pub const TIMEOUT: i32 = 30;        // RERR_TIMEOUT
    pub const CONTIMEOUT: i32 = 35;     // RERR_CONTIMEOUT
}

/// Extension trait for adding path context to I/O errors
pub trait IoResultExt<T> {
    fn with_path(self, path: &Path) -> Result<T, RsyncError>;
}

impl<T> IoResultExt<T> for std::io::Result<T> {
    fn with_path(self, path: &Path) -> Result<T, RsyncError> {
        self.map_err(|source| RsyncError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

// Usage example
fn read_file(path: &Path) -> Result<Vec<u8>, RsyncError> {
    std::fs::read(path).with_path(path)
}
```

### Enhanced Testing Patterns

Test fixtures with wire capture for protocol debugging:

```rust
// Test utilities (tests/common/)
use tempfile::TempDir;
use std::sync::{Arc, Mutex};

/// Reusable test fixture with source/destination directories
pub struct TestFixture {
    pub temp_dir: TempDir,
    pub source: PathBuf,
    pub dest: PathBuf,
}

impl TestFixture {
    pub fn new() -> io::Result<Self> {
        let temp_dir = TempDir::new()?;
        let source = temp_dir.path().join("source");
        let dest = temp_dir.path().join("dest");

        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&dest)?;

        Ok(Self { temp_dir, source, dest })
    }

    /// Create a test file with given content
    pub fn create_file(&self, rel_path: &str, content: &[u8]) -> io::Result<PathBuf> {
        let path = self.source.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, content)?;
        Ok(path)
    }

    /// Create a test directory
    pub fn create_dir(&self, rel_path: &str) -> io::Result<PathBuf> {
        let path = self.source.join(rel_path);
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Assert source and destination have identical content
    pub fn assert_synced(&self) -> io::Result<()> {
        assert_dirs_equal(&self.source, &self.dest)
    }
}

/// Recursive directory comparison
pub fn assert_dirs_equal(a: &Path, b: &Path) -> io::Result<()> {
    let a_entries: Vec<_> = walkdir::WalkDir::new(a)
        .into_iter()
        .filter_map(Result::ok)
        .collect();

    for entry in &a_entries {
        let rel = entry.path().strip_prefix(a).unwrap();
        let b_path = b.join(rel);

        assert!(b_path.exists(), "Missing in dest: {:?}", rel);

        if entry.file_type().is_file() {
            let a_content = std::fs::read(entry.path())?;
            let b_content = std::fs::read(&b_path)?;
            assert_eq!(
                a_content, b_content,
                "Content mismatch: {:?}",
                rel
            );
        }
    }

    Ok(())
}

/// Wire capture for protocol debugging
pub struct WireCapture {
    sent: Arc<Mutex<Vec<u8>>>,
    received: Arc<Mutex<Vec<u8>>>,
}

impl WireCapture {
    pub fn new() -> Self {
        Self {
            sent: Arc::new(Mutex::new(Vec::new())),
            received: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn record_send(&self, data: &[u8]) {
        self.sent.lock().unwrap().extend_from_slice(data);
    }

    pub fn record_recv(&self, data: &[u8]) {
        self.received.lock().unwrap().extend_from_slice(data);
    }

    /// Get captured data as hex dump for debugging
    pub fn sent_hex(&self) -> String {
        hex_dump(&self.sent.lock().unwrap())
    }

    pub fn received_hex(&self) -> String {
        hex_dump(&self.received.lock().unwrap())
    }

    /// Compare against golden file
    pub fn assert_matches_golden(&self, golden_path: &Path) -> io::Result<()> {
        let expected = std::fs::read(golden_path)?;
        let actual = self.sent.lock().unwrap();
        assert_eq!(&*actual, &expected, "Wire data mismatch");
        Ok(())
    }
}

fn hex_dump(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .chunks(32)
        .map(|chunk| chunk.join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Integration test runner
pub struct IntegrationTest {
    fixture: TestFixture,
    capture: WireCapture,
}

impl IntegrationTest {
    pub fn setup() -> io::Result<Self> {
        Ok(Self {
            fixture: TestFixture::new()?,
            capture: WireCapture::new(),
        })
    }

    pub fn run_transfer(&mut self) -> Result<TransferStats, RsyncError> {
        // Configure client with wire capture
        let config = CoreConfig::builder()
            .source(&self.fixture.source)
            .dest(&self.fixture.dest)
            .wire_capture(self.capture.clone())
            .build()?;

        core::run_client(config)
    }
}
```

### SIMD Detection Caching

Runtime feature detection is cached to avoid repeated `is_x86_feature_detected!` calls:

```rust
// SIMD detection caching (crates/checksums/src/rolling/)
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimdLevel {
    Scalar,
    Sse2,
    Avx2,
    Neon,
}

static DETECTED_SIMD: OnceLock<SimdLevel> = OnceLock::new();

pub fn detect_simd_level() -> SimdLevel {
    *DETECTED_SIMD.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                return SimdLevel::Avx2;
            }
            if is_x86_feature_detected!("sse2") {
                return SimdLevel::Sse2;
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            // NEON is always available on aarch64
            return SimdLevel::Neon;
        }

        SimdLevel::Scalar
    })
}

/// Dispatch to appropriate SIMD implementation
pub fn accumulate_chunk(s1: &mut u32, s2: &mut u32, data: &[u8]) {
    match detect_simd_level() {
        SimdLevel::Avx2 => unsafe { avx2_accumulate(s1, s2, data) },
        SimdLevel::Sse2 => unsafe { sse2_accumulate(s1, s2, data) },
        SimdLevel::Neon => unsafe { neon_accumulate(s1, s2, data) },
        SimdLevel::Scalar => scalar_accumulate(s1, s2, data),
    }
}
```

### Complete DeltaGenerator with Search State

Full delta generation with match tracking:

```rust
// Delta generator (crates/engine/src/delta/)
use std::collections::HashMap;

/// Block signature for delta matching
#[derive(Debug, Clone)]
pub struct BlockSignature {
    pub index: usize,          // Block index in basis file
    pub weak: u32,             // Rolling checksum
    pub strong: [u8; 16],      // Truncated strong hash
    pub offset: u64,           // Byte offset in basis file
}

/// Index for O(1) weak checksum lookup with collision handling
pub struct SignatureIndex {
    /// Map from weak checksum to list of matching block indices
    weak_map: HashMap<u32, Vec<usize>>,
    /// All signatures for strong hash verification
    signatures: Vec<BlockSignature>,
    /// Block size used for signatures
    block_size: usize,
}

impl SignatureIndex {
    pub fn new(signatures: Vec<BlockSignature>, block_size: usize) -> Self {
        let mut weak_map: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, sig) in signatures.iter().enumerate() {
            weak_map.entry(sig.weak).or_default().push(i);
        }
        Self { weak_map, signatures, block_size }
    }

    /// Find matching block, verifying with strong hash
    pub fn find_match(&self, weak: u32, data: &[u8], strong_hasher: &impl StrongDigest) -> Option<&BlockSignature> {
        let candidates = self.weak_map.get(&weak)?;

        // Compute strong hash only if we have candidates
        let strong = strong_hasher.digest(data);
        let strong_prefix: [u8; 16] = strong.as_ref()[..16].try_into().ok()?;

        // Check each candidate
        for &idx in candidates {
            if self.signatures[idx].strong == strong_prefix {
                return Some(&self.signatures[idx]);
            }
        }
        None
    }
}

/// Delta generator state machine
pub struct DeltaGenerator<'a> {
    new_data: &'a [u8],
    index: &'a SignatureIndex,
    rolling: RollingChecksum,
    pos: usize,
    literal_start: usize,
    block_size: usize,
}

impl<'a> DeltaGenerator<'a> {
    pub fn new(new_data: &'a [u8], index: &'a SignatureIndex) -> Self {
        let block_size = index.block_size;
        let mut rolling = RollingChecksum::new();

        // Initialize with first block if possible
        if new_data.len() >= block_size {
            rolling.update(&new_data[..block_size]);
        }

        Self {
            new_data,
            index,
            rolling,
            pos: 0,
            literal_start: 0,
            block_size,
        }
    }

    /// Generate all delta operations
    pub fn generate(mut self, strong_hasher: &impl StrongDigest) -> Vec<DeltaOp> {
        let mut ops = Vec::new();

        while self.pos + self.block_size <= self.new_data.len() {
            let weak = self.rolling.digest();
            let block_data = &self.new_data[self.pos..self.pos + self.block_size];

            if let Some(sig) = self.index.find_match(weak, block_data, strong_hasher) {
                // Emit accumulated literal data
                if self.literal_start < self.pos {
                    ops.push(DeltaOp::Literal(
                        self.new_data[self.literal_start..self.pos].to_vec()
                    ));
                }

                // Emit copy instruction
                ops.push(DeltaOp::Copy {
                    offset: sig.offset,
                    length: self.block_size as u32,
                });

                // Jump past matched block
                self.pos += self.block_size;
                self.literal_start = self.pos;

                // Reset rolling checksum for next block
                if self.pos + self.block_size <= self.new_data.len() {
                    self.rolling = RollingChecksum::new();
                    self.rolling.update(&self.new_data[self.pos..self.pos + self.block_size]);
                }
            } else {
                // No match - roll forward one byte
                if self.pos + self.block_size < self.new_data.len() {
                    self.rolling.roll(
                        self.new_data[self.pos],
                        self.new_data[self.pos + self.block_size]
                    );
                }
                self.pos += 1;
            }
        }

        // Emit final literal data
        if self.literal_start < self.new_data.len() {
            ops.push(DeltaOp::Literal(
                self.new_data[self.literal_start..].to_vec()
            ));
        }

        ops
    }
}
```

### Multiplex Reader and Writer

Separate reader/writer with buffering for bidirectional streams:

```rust
// Multiplex I/O (crates/protocol/src/multiplex/)
use std::io::{self, BufReader, BufWriter, Read, Write};

/// Buffered multiplexed reader
pub struct MultiplexReader<R> {
    inner: BufReader<R>,
    active: bool,
    pending_data: Vec<u8>,
}

impl<R: Read> MultiplexReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::with_capacity(64 * 1024, reader),
            active: false,
            pending_data: Vec::new(),
        }
    }

    /// Activate multiplexed mode (after protocol negotiation)
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// Read next frame, handling out-of-band messages
    pub fn read_frame(&mut self) -> io::Result<MultiplexFrame> {
        if !self.active {
            // Non-multiplexed: read raw data
            let mut buf = vec![0u8; 4096];
            let n = self.inner.read(&mut buf)?;
            buf.truncate(n);
            return Ok(MultiplexFrame {
                tag: MessageTag::Data,
                payload: buf,
            });
        }

        loop {
            // Read 4-byte header
            let mut header = [0u8; 4];
            self.inner.read_exact(&mut header)?;

            let header_val = u32::from_le_bytes(header);
            let tag_byte = ((header_val >> 24) & 0xFF) as u8;
            let len = (header_val & 0x00FFFFFF) as usize;

            let tag = MessageTag::from_byte(tag_byte)
                .ok_or_else(|| io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("invalid message tag: {}", tag_byte)
                ))?;

            // Read payload
            let mut payload = vec![0u8; len];
            self.inner.read_exact(&mut payload)?;

            // Handle out-of-band messages
            if tag.is_info() {
                // Log info messages but continue reading
                log_message(tag, &payload);
                continue;
            }

            if tag.is_error() {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    String::from_utf8_lossy(&payload).into_owned()
                ));
            }

            return Ok(MultiplexFrame { tag, payload });
        }
    }
}

/// Buffered multiplexed writer
pub struct MultiplexWriter<W> {
    inner: BufWriter<W>,
    active: bool,
}

impl<W: Write> MultiplexWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            inner: BufWriter::with_capacity(64 * 1024, writer),
            active: false,
        }
    }

    /// Activate multiplexed mode
    pub fn activate(&mut self) {
        self.active = true;
    }

    /// Write data frame
    pub fn write_data(&mut self, data: &[u8]) -> io::Result<()> {
        self.write_frame(MessageTag::Data, data)
    }

    /// Write arbitrary frame
    pub fn write_frame(&mut self, tag: MessageTag, payload: &[u8]) -> io::Result<()> {
        if !self.active {
            // Non-multiplexed: write raw
            return self.inner.write_all(payload);
        }

        // Encode header
        let len = payload.len() as u32;
        if len > 0x00FFFFFF {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "payload too large for multiplex frame"
            ));
        }

        let header = ((tag as u32) << 24) | len;
        self.inner.write_all(&header.to_le_bytes())?;
        self.inner.write_all(payload)?;
        Ok(())
    }

    /// Flush buffered data
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
```

### FileListWalker Iterator Implementation

Complete file list walker with sorting and filtering:

```rust
// File list walker (crates/walk/)
use std::cmp::Ordering;
use walkdir::WalkDir;

/// Iterator over files in rsync transfer order
pub struct FileListWalker {
    entries: std::vec::IntoIter<FileEntry>,
    filter: Option<FilterChain>,
}

impl FileListWalker {
    pub fn new(root: &Path, recursive: bool) -> io::Result<Self> {
        let mut entries = Vec::new();

        let walker = if recursive {
            WalkDir::new(root).follow_links(false)
        } else {
            WalkDir::new(root).max_depth(1).follow_links(false)
        };

        for entry in walker {
            let entry = entry.map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            let metadata = entry.metadata()?;

            let file_entry = FileEntry::from_walkdir(&entry, &metadata, root)?;
            entries.push(file_entry);
        }

        // Sort in rsync order: directories before files, then lexicographic
        entries.sort_by(|a, b| {
            match (a.file_type == FileType::Directory, b.file_type == FileType::Directory) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => a.path.cmp(&b.path),
            }
        });

        Ok(Self {
            entries: entries.into_iter(),
            filter: None,
        })
    }

    pub fn with_filter(mut self, filter: FilterChain) -> Self {
        self.filter = Some(filter);
        self
    }
}

impl Iterator for FileListWalker {
    type Item = io::Result<FileEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let entry = self.entries.next()?;

            // Apply filter if present
            if let Some(ref filter) = self.filter {
                let is_dir = entry.file_type == FileType::Directory;
                match filter.evaluate(&entry.path, is_dir) {
                    FilterResult::Exclude => continue,
                    FilterResult::Include | FilterResult::NoMatch => {}
                }
            }

            return Some(Ok(entry));
        }
    }
}

impl FileEntry {
    fn from_walkdir(entry: &walkdir::DirEntry, metadata: &std::fs::Metadata, root: &Path) -> io::Result<Self> {
        let path = entry.path().strip_prefix(root)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
            .to_path_buf();

        let file_type = if metadata.is_dir() {
            FileType::Directory
        } else if metadata.is_symlink() {
            FileType::Symlink
        } else {
            FileType::Regular
        };

        #[cfg(unix)]
        let (mode, uid, gid) = {
            use std::os::unix::fs::MetadataExt;
            (metadata.mode(), metadata.uid(), metadata.gid())
        };

        #[cfg(not(unix))]
        let (mode, uid, gid) = (0o644, 0, 0);

        Ok(Self {
            path,
            file_type,
            size: metadata.len(),
            mtime: metadata.modified()?,
            mode,
            uid,
            gid,
            link_target: None,
            device_info: None,
        })
    }
}
```

### Varint Encoding Patterns

Protocol-specific variable-length integer encoding:

```rust
// Varint encoding (crates/protocol/src/varint.rs)

/// Write variable-length integer (protocol 30+)
pub fn write_varint<W: Write>(writer: &mut W, mut value: u64) -> io::Result<usize> {
    let mut buf = [0u8; 10];
    let mut len = 0;

    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;

        if value == 0 {
            buf[len] = byte;
            len += 1;
            break;
        } else {
            buf[len] = byte | 0x80;  // Set continuation bit
            len += 1;
        }
    }

    writer.write_all(&buf[..len])?;
    Ok(len)
}

/// Read variable-length integer
pub fn read_varint<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut value: u64 = 0;
    let mut shift = 0;

    loop {
        let mut byte = [0u8];
        reader.read_exact(&mut byte)?;

        let b = byte[0];
        value |= ((b & 0x7F) as u64) << shift;

        if b & 0x80 == 0 {
            break;
        }

        shift += 7;
        if shift >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "varint overflow"
            ));
        }
    }

    Ok(value)
}

/// Write signed varint (zigzag encoding)
pub fn write_svarint<W: Write>(writer: &mut W, value: i64) -> io::Result<usize> {
    // Zigzag encode: (value << 1) ^ (value >> 63)
    let encoded = ((value << 1) ^ (value >> 63)) as u64;
    write_varint(writer, encoded)
}

/// Read signed varint
pub fn read_svarint<R: Read>(reader: &mut R) -> io::Result<i64> {
    let encoded = read_varint(reader)?;
    // Zigzag decode
    Ok(((encoded >> 1) as i64) ^ -((encoded & 1) as i64))
}

/// NDX encoding for protocol 30+
pub fn write_ndx<W: Write>(writer: &mut W, ndx: i32) -> io::Result<usize> {
    if ndx >= 0 && ndx < 0xFE {
        writer.write_all(&[ndx as u8])?;
        Ok(1)
    } else if ndx < 0 || ndx == NDX_DONE {
        writer.write_all(&[0xFF])?;
        Ok(1)
    } else {
        writer.write_all(&[0xFE])?;
        let bytes = (ndx as u32).to_le_bytes();
        writer.write_all(&bytes)?;
        Ok(5)
    }
}

pub const NDX_DONE: i32 = -1;
```

### Daemon Authentication Challenge-Response

MD5-based authentication for daemon modules:

```rust
// Daemon authentication (crates/daemon/)
use md5::{Md5, Digest};
use rand::RngCore;

/// Generate authentication challenge
pub fn generate_challenge() -> [u8; 16] {
    let mut challenge = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut challenge);
    challenge
}

/// Compute authentication response
pub fn compute_response(challenge: &[u8], password: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(challenge);
    hasher.update(password.as_bytes());
    let result = hasher.finalize();
    base64_encode(&result)
}

/// Verify client response
pub fn verify_response(challenge: &[u8], password: &str, response: &str) -> bool {
    let expected = compute_response(challenge, password);
    constant_time_eq(expected.as_bytes(), response.as_bytes())
}

/// Constant-time comparison to prevent timing attacks
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Base64 encode for wire format
fn base64_encode(data: &[u8]) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    STANDARD.encode(data)
}

/// Authentication state machine
pub struct AuthState {
    challenge: [u8; 16],
    module: String,
    attempts: u32,
    max_attempts: u32,
}

impl AuthState {
    pub fn new(module: String) -> Self {
        Self {
            challenge: generate_challenge(),
            module,
            attempts: 0,
            max_attempts: 3,
        }
    }

    pub fn challenge_string(&self) -> String {
        base64_encode(&self.challenge)
    }

    pub fn verify(&mut self, response: &str, secrets: &SecretsFile) -> AuthResult {
        self.attempts += 1;

        if self.attempts > self.max_attempts {
            return AuthResult::TooManyAttempts;
        }

        match secrets.get_password(&self.module) {
            Some(password) => {
                if verify_response(&self.challenge, password, response) {
                    AuthResult::Success
                } else {
                    AuthResult::InvalidPassword
                }
            }
            None => AuthResult::NoSuchModule,
        }
    }
}

pub enum AuthResult {
    Success,
    InvalidPassword,
    NoSuchModule,
    TooManyAttempts,
}
```

### Systemd Notification Integration

Service readiness notification for daemon mode:

```rust
// Systemd integration (crates/daemon/)
#[cfg(feature = "sd-notify")]

/// Notify systemd of daemon state
pub mod systemd {
    use std::env;
    use std::io::{self, Write};
    use std::os::unix::net::UnixDatagram;

    /// Send notification to systemd
    pub fn notify(state: &str) -> io::Result<()> {
        let socket_path = match env::var("NOTIFY_SOCKET") {
            Ok(path) => path,
            Err(_) => return Ok(()),  // Not running under systemd
        };

        let socket = UnixDatagram::unbound()?;

        // Handle abstract socket (starts with @)
        let addr = if socket_path.starts_with('@') {
            format!("\0{}", &socket_path[1..])
        } else {
            socket_path
        };

        socket.send_to(state.as_bytes(), addr)?;
        Ok(())
    }

    /// Signal service is ready
    pub fn ready() -> io::Result<()> {
        notify("READY=1")
    }

    /// Signal service is stopping
    pub fn stopping() -> io::Result<()> {
        notify("STOPPING=1")
    }

    /// Update service status
    pub fn status(msg: &str) -> io::Result<()> {
        notify(&format!("STATUS={}", msg))
    }

    /// Signal watchdog heartbeat
    pub fn watchdog() -> io::Result<()> {
        notify("WATCHDOG=1")
    }

    /// Notify with main PID (for forking daemons)
    pub fn mainpid(pid: u32) -> io::Result<()> {
        notify(&format!("MAINPID={}", pid))
    }
}

/// Daemon with systemd integration
pub struct SystemdDaemon {
    server: DaemonServer,
    watchdog_interval: Option<Duration>,
}

impl SystemdDaemon {
    pub async fn run(self, listener: TcpListener) -> io::Result<()> {
        // Parse watchdog interval from environment
        let watchdog_usec = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|s| s.parse::<u64>().ok());

        // Notify ready
        #[cfg(feature = "sd-notify")]
        systemd::ready()?;

        // Start watchdog task if configured
        if let Some(usec) = watchdog_usec {
            let interval = Duration::from_micros(usec / 2);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    #[cfg(feature = "sd-notify")]
                    let _ = systemd::watchdog();
                }
            });
        }

        // Run accept loop
        self.server.accept_loop(listener).await;

        // Notify stopping
        #[cfg(feature = "sd-notify")]
        systemd::stopping()?;

        Ok(())
    }
}
```

### Protocol Test Harness for Daemon Integration

Complete test harness for daemon protocol testing:

```rust
// Protocol test harness (tests/common/)
use std::process::{Child, Command, Stdio};
use std::net::TcpStream;
use std::time::Duration;

/// Test harness for daemon integration tests
pub struct ProtocolTestHarness {
    daemon: Child,
    port: u16,
    config_dir: TempDir,
}

impl ProtocolTestHarness {
    /// Start daemon with test configuration
    pub fn start() -> io::Result<Self> {
        let config_dir = TempDir::new()?;
        let port = find_free_port()?;

        // Create test module directory
        let module_path = config_dir.path().join("testmodule");
        std::fs::create_dir_all(&module_path)?;

        // Write test config
        let config_path = config_dir.path().join("rsyncd.conf");
        std::fs::write(&config_path, format!(r#"
[testmod]
path = {}
read only = no
use chroot = no
"#, module_path.display()))?;

        // Start daemon
        let daemon = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
            .args(&[
                "--daemon",
                "--no-detach",
                "--port", &port.to_string(),
                "--config", config_path.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;

        // Wait for daemon to be ready
        let harness = Self { daemon, port, config_dir };
        harness.wait_for_ready(Duration::from_secs(5))?;

        Ok(harness)
    }

    fn wait_for_ready(&self, timeout: Duration) -> io::Result<()> {
        let start = std::time::Instant::now();
        loop {
            if TcpStream::connect(("127.0.0.1", self.port)).is_ok() {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "daemon did not start in time"
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Get connection URL for rsync client
    pub fn url(&self, module: &str) -> String {
        format!("rsync://127.0.0.1:{}/{}/", self.port, module)
    }

    /// Connect to daemon and return raw stream
    pub fn connect(&self) -> io::Result<TcpStream> {
        TcpStream::connect(("127.0.0.1", self.port))
    }

    /// Create test files in module
    pub fn create_file(&self, rel_path: &str, content: &[u8]) -> io::Result<()> {
        let path = self.config_dir.path().join("testmodule").join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
    }
}

impl Drop for ProtocolTestHarness {
    fn drop(&mut self) {
        // Gracefully stop daemon
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// Find an available TCP port
fn find_free_port() -> io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

// Example test using harness
#[test]
fn test_daemon_file_transfer() -> io::Result<()> {
    let harness = ProtocolTestHarness::start()?;

    // Create source file
    harness.create_file("test.txt", b"Hello, rsync!")?;

    // Run transfer
    let output = Command::new("rsync")
        .args(&["-v", &harness.url("testmod"), "/tmp/dest/"])
        .output()?;

    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string("/tmp/dest/test.txt")?,
        "Hello, rsync!"
    );

    Ok(())
}
```

### Compression Level Negotiation

Protocol-aware compression negotiation:

```rust
// Compression negotiation (crates/compress/)

/// Compression algorithm identifiers (matching upstream)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionAlgorithm {
    None = 0,
    Zlib = 1,
    Lz4 = 2,
    Zstd = 3,
}

/// Negotiate compression algorithm and level
pub struct CompressionNegotiator {
    local_algorithms: Vec<CompressionAlgorithm>,
    local_level: i32,
}

impl CompressionNegotiator {
    pub fn new(algorithms: Vec<CompressionAlgorithm>, level: i32) -> Self {
        Self {
            local_algorithms: algorithms,
            local_level: level.clamp(-1, 22),  // zstd max level
        }
    }

    /// Negotiate with remote peer
    pub fn negotiate(&self, remote_algorithms: &[CompressionAlgorithm]) -> Option<CompressionConfig> {
        // Find first mutually supported algorithm
        for &algo in &self.local_algorithms {
            if remote_algorithms.contains(&algo) && algo != CompressionAlgorithm::None {
                return Some(CompressionConfig {
                    algorithm: algo,
                    level: self.effective_level(algo),
                });
            }
        }
        None
    }

    fn effective_level(&self, algo: CompressionAlgorithm) -> i32 {
        match algo {
            CompressionAlgorithm::Zlib => self.local_level.clamp(1, 9),
            CompressionAlgorithm::Lz4 => 0,  // LZ4 doesn't use levels
            CompressionAlgorithm::Zstd => self.local_level.clamp(1, 22),
            CompressionAlgorithm::None => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompressionConfig {
    pub algorithm: CompressionAlgorithm,
    pub level: i32,
}

impl CompressionConfig {
    /// Create compressor based on negotiated config
    pub fn create_compressor(&self) -> Box<dyn Compressor> {
        match self.algorithm {
            CompressionAlgorithm::Zlib => {
                Box::new(ZlibCompressor::new(self.level as u32))
            }
            CompressionAlgorithm::Lz4 => {
                Box::new(Lz4Compressor::new())
            }
            CompressionAlgorithm::Zstd => {
                Box::new(ZstdCompressor::new(self.level))
            }
            CompressionAlgorithm::None => {
                Box::new(NoopCompressor)
            }
        }
    }
}

/// Compressor trait for algorithm abstraction
pub trait Compressor: Send + Sync {
    fn compress(&mut self, input: &[u8], output: &mut Vec<u8>) -> io::Result<()>;
    fn flush(&mut self, output: &mut Vec<u8>) -> io::Result<()>;
}
```

### Quick Start Guide

Basic usage examples for library integration:

```rust
// Quick Start: Basic file synchronization (crates/core/)
use core::{CoreConfig, run_client};
use logging::Format;

// Example 1: Local directory sync
fn sync_local_directories() -> Result<(), Box<dyn std::error::Error>> {
    let config = CoreConfig::builder()
        .source("/path/to/source/")
        .destination("/path/to/dest/")
        .recursive(true)
        .times(true)           // Preserve modification times
        .perms(true)           // Preserve permissions
        .build()?;

    run_client(config, Format::default())?;
    Ok(())
}

// Example 2: Remote sync via rsync:// protocol
fn sync_from_daemon() -> Result<(), Box<dyn std::error::Error>> {
    let config = CoreConfig::builder()
        .source("rsync://server.example.com/module/")
        .destination("/local/path/")
        .recursive(true)
        .compress(true)        // Enable compression
        .progress(true)        // Show progress
        .build()?;

    run_client(config, Format::default())?;
    Ok(())
}

// Example 3: Push to remote daemon
fn push_to_daemon() -> Result<(), Box<dyn std::error::Error>> {
    let config = CoreConfig::builder()
        .source("/local/path/")
        .destination("rsync://server.example.com/module/")
        .recursive(true)
        .delete(true)          // Delete extraneous files
        .build()?;

    run_client(config, Format::default())?;
    Ok(())
}

// Example 4: SSH-based remote sync
fn sync_via_ssh() -> Result<(), Box<dyn std::error::Error>> {
    let config = CoreConfig::builder()
        .source("user@host:/remote/path/")
        .destination("/local/path/")
        .recursive(true)
        .rsh("ssh -p 2222")    // Custom SSH command
        .build()?;

    run_client(config, Format::default())?;
    Ok(())
}

// Example 5: Dry-run with verbose output
fn preview_sync() -> Result<(), Box<dyn std::error::Error>> {
    let config = CoreConfig::builder()
        .source("/path/to/source/")
        .destination("/path/to/dest/")
        .recursive(true)
        .dry_run(true)         // Don't make changes
        .verbose(2)            // Detailed output
        .itemize_changes(true) // Show itemized changes
        .build()?;

    run_client(config, Format::default())?;
    Ok(())
}
```

### Memory-Mapped Checksumming

Efficient checksum computation for large files using memory mapping:

```rust
// Memory-mapped checksumming (crates/checksums/)
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::io;

/// Memory-mapped file for efficient checksum computation
pub struct MappedFile {
    mmap: Mmap,
    offset: usize,
}

impl MappedFile {
    /// Open file with memory mapping
    pub fn open(path: &std::path::Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self { mmap, offset: 0 })
    }

    /// Open with offset for partial mapping
    pub fn open_range(
        path: &std::path::Path,
        offset: u64,
        len: usize,
    ) -> io::Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe {
            MmapOptions::new()
                .offset(offset)
                .len(len)
                .map(&file)?
        };
        Ok(Self { mmap, offset: 0 })
    }

    /// Get slice for block at index
    pub fn block(&self, index: usize, block_size: usize) -> Option<&[u8]> {
        let start = index * block_size;
        if start >= self.mmap.len() {
            return None;
        }
        let end = std::cmp::min(start + block_size, self.mmap.len());
        Some(&self.mmap[start..end])
    }

    /// Compute block signatures efficiently
    pub fn compute_signatures(
        &self,
        block_size: usize,
        checksum: &ChecksumAlgorithm,
    ) -> Vec<BlockSignature> {
        let mut signatures = Vec::new();
        let mut offset = 0;

        while offset < self.mmap.len() {
            let end = std::cmp::min(offset + block_size, self.mmap.len());
            let block = &self.mmap[offset..end];

            // Compute rolling checksum
            let rolling = RollingChecksum::compute(block);

            // Compute strong checksum
            let strong = checksum.hash(block);

            signatures.push(BlockSignature {
                index: signatures.len() as u32,
                rolling: rolling.value(),
                strong,
                length: block.len() as u32,
            });

            offset = end;
        }

        signatures
    }

    /// Iterate over blocks
    pub fn blocks(&self, block_size: usize) -> BlockIterator<'_> {
        BlockIterator {
            data: &self.mmap,
            block_size,
            offset: 0,
        }
    }
}

/// Iterator over file blocks
pub struct BlockIterator<'a> {
    data: &'a [u8],
    block_size: usize,
    offset: usize,
}

impl<'a> Iterator for BlockIterator<'a> {
    type Item = (usize, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.data.len() {
            return None;
        }

        let start = self.offset;
        let end = std::cmp::min(start + self.block_size, self.data.len());
        let index = start / self.block_size;

        self.offset = end;
        Some((index, &self.data[start..end]))
    }
}
```

### Parallel Directory Walking

Efficient parallel traversal using rayon:

```rust
// Parallel directory walking (crates/walk/)
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use crossbeam_channel::{Sender, bounded};

/// Parallel directory walker with work stealing
pub struct ParallelWalker {
    root: PathBuf,
    filters: Arc<FilterChain>,
    max_depth: Option<usize>,
}

impl ParallelWalker {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            filters: Arc::new(FilterChain::default()),
            max_depth: None,
        }
    }

    pub fn with_filters(mut self, filters: FilterChain) -> Self {
        self.filters = Arc::new(filters);
        self
    }

    pub fn max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Walk directory tree in parallel, sending entries to channel
    pub fn walk_parallel(&self, sender: Sender<FileEntry>) -> io::Result<()> {
        self.walk_dir(&self.root, 0, &sender)
    }

    fn walk_dir(
        &self,
        dir: &Path,
        depth: usize,
        sender: &Sender<FileEntry>,
    ) -> io::Result<()> {
        // Check depth limit
        if let Some(max) = self.max_depth {
            if depth > max {
                return Ok(());
            }
        }

        // Read directory entries
        let entries: Vec<_> = std::fs::read_dir(dir)?
            .filter_map(|e| e.ok())
            .collect();

        // Process entries in parallel
        entries.par_iter().try_for_each(|entry| {
            let path = entry.path();
            let rel_path = path.strip_prefix(&self.root)
                .unwrap_or(&path)
                .to_path_buf();

            // Apply filters
            match self.filters.check(&rel_path, entry.file_type().ok()) {
                FilterResult::Include => {}
                FilterResult::Exclude => return Ok(()),
                FilterResult::Hide => return Ok(()),
            }

            // Get metadata
            let metadata = entry.metadata()?;

            // Create file entry
            let file_entry = FileEntry {
                path: rel_path,
                size: metadata.len(),
                mtime: metadata.modified().ok(),
                mode: Self::get_mode(&metadata),
                file_type: Self::get_file_type(&metadata),
            };

            // Send entry
            let _ = sender.send(file_entry);

            // Recurse into directories
            if metadata.is_dir() {
                self.walk_dir(&path, depth + 1, sender)?;
            }

            Ok::<_, io::Error>(())
        })
    }

    #[cfg(unix)]
    fn get_mode(metadata: &std::fs::Metadata) -> u32 {
        use std::os::unix::fs::MetadataExt;
        metadata.mode()
    }

    #[cfg(not(unix))]
    fn get_mode(_metadata: &std::fs::Metadata) -> u32 {
        0o644  // Default mode on non-Unix
    }

    fn get_file_type(metadata: &std::fs::Metadata) -> FileType {
        if metadata.is_file() {
            FileType::Regular
        } else if metadata.is_dir() {
            FileType::Directory
        } else if metadata.file_type().is_symlink() {
            FileType::Symlink
        } else {
            FileType::Special
        }
    }
}

/// Usage example with parallel collection
pub fn collect_file_list(root: &Path) -> io::Result<Vec<FileEntry>> {
    let (sender, receiver) = bounded(1000);

    let walker = ParallelWalker::new(root);

    // Spawn walker in background
    let handle = std::thread::spawn(move || {
        walker.walk_parallel(sender)
    });

    // Collect entries
    let entries: Vec<_> = receiver.iter().collect();

    // Wait for walker to complete
    handle.join().unwrap()?;

    Ok(entries)
}
```

### Zero-Copy Delta Application

Efficient delta application with minimal memory copies:

```rust
// Zero-copy delta application (crates/engine/)
use std::io::{self, Read, Write, Seek, SeekFrom};

/// Zero-copy delta applicator
pub struct ZeroCopyDelta<'a, R, W> {
    basis: &'a mut R,
    output: &'a mut W,
    basis_offset: u64,
    output_offset: u64,
    copy_buffer: Vec<u8>,
}

impl<'a, R: Read + Seek, W: Write> ZeroCopyDelta<'a, R, W> {
    pub fn new(basis: &'a mut R, output: &'a mut W) -> Self {
        Self {
            basis,
            output,
            basis_offset: 0,
            output_offset: 0,
            copy_buffer: vec![0u8; 64 * 1024],  // 64KB buffer
        }
    }

    /// Apply delta tokens to produce output
    pub fn apply<I: Iterator<Item = DeltaToken>>(&mut self, tokens: I) -> io::Result<u64> {
        for token in tokens {
            match token {
                DeltaToken::Copy { offset, length } => {
                    self.apply_copy(offset, length)?;
                }
                DeltaToken::Literal(data) => {
                    self.apply_literal(&data)?;
                }
            }
        }
        Ok(self.output_offset)
    }

    fn apply_copy(&mut self, offset: u64, length: u32) -> io::Result<()> {
        // Only seek if not at expected position
        if self.basis_offset != offset {
            self.basis.seek(SeekFrom::Start(offset))?;
            self.basis_offset = offset;
        }

        let mut remaining = length as usize;

        while remaining > 0 {
            let to_read = std::cmp::min(remaining, self.copy_buffer.len());
            let n = self.basis.read(&mut self.copy_buffer[..to_read])?;

            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "basis file truncated"
                ));
            }

            self.output.write_all(&self.copy_buffer[..n])?;
            self.basis_offset += n as u64;
            self.output_offset += n as u64;
            remaining -= n;
        }

        Ok(())
    }

    fn apply_literal(&mut self, data: &[u8]) -> io::Result<()> {
        self.output.write_all(data)?;
        self.output_offset += data.len() as u64;
        Ok(())
    }
}

/// Delta token types
#[derive(Debug, Clone)]
pub enum DeltaToken {
    /// Copy from basis file
    Copy { offset: u64, length: u32 },
    /// Literal data (new content)
    Literal(Vec<u8>),
}

/// Delta token reader from wire format
pub struct DeltaTokenReader<R> {
    reader: R,
    block_size: u32,
}

impl<R: Read> DeltaTokenReader<R> {
    pub fn new(reader: R, block_size: u32) -> Self {
        Self { reader, block_size }
    }
}

impl<R: Read> Iterator for DeltaTokenReader<R> {
    type Item = io::Result<DeltaToken>;

    fn next(&mut self) -> Option<Self::Item> {
        // Read token header
        let mut header = [0u8; 4];
        match self.reader.read_exact(&mut header) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return None,
            Err(e) => return Some(Err(e)),
        }

        let token = i32::from_le_bytes(header);

        if token == 0 {
            // End of delta
            return None;
        } else if token > 0 {
            // Literal data of `token` bytes
            let mut data = vec![0u8; token as usize];
            if let Err(e) = self.reader.read_exact(&mut data) {
                return Some(Err(e));
            }
            Some(Ok(DeltaToken::Literal(data)))
        } else {
            // Copy from basis: -token is block index
            let block_index = (-token - 1) as u64;
            let offset = block_index * self.block_size as u64;
            Some(Ok(DeltaToken::Copy {
                offset,
                length: self.block_size,
            }))
        }
    }
}
```

### XattrHandler Implementation

Extended attribute preservation:

```rust
// Extended attribute handling (crates/metadata/)
#[cfg(unix)]
use std::ffi::OsStr;
use std::path::Path;
use std::io;

/// Extended attribute handler
pub struct XattrHandler {
    /// Preserve user namespace xattrs
    user: bool,
    /// Preserve system namespace xattrs (requires privileges)
    system: bool,
    /// Preserve security namespace (SELinux, etc.)
    security: bool,
    /// Preserve trusted namespace (requires CAP_SYS_ADMIN)
    trusted: bool,
}

impl XattrHandler {
    pub fn new() -> Self {
        Self {
            user: true,
            system: false,
            security: false,
            trusted: false,
        }
    }

    /// Enable all namespaces (requires privileges)
    pub fn all_namespaces(mut self) -> Self {
        self.system = true;
        self.security = true;
        self.trusted = true;
        self
    }

    /// Check if namespace should be preserved
    fn should_preserve(&self, name: &OsStr) -> bool {
        let name_bytes = name.as_encoded_bytes();

        if name_bytes.starts_with(b"user.") {
            self.user
        } else if name_bytes.starts_with(b"system.") {
            self.system
        } else if name_bytes.starts_with(b"security.") {
            self.security
        } else if name_bytes.starts_with(b"trusted.") {
            self.trusted
        } else {
            false
        }
    }

    /// Read all xattrs from file
    #[cfg(target_os = "linux")]
    pub fn read_xattrs(&self, path: &Path) -> io::Result<Vec<Xattr>> {
        use std::os::unix::ffi::OsStrExt;

        let mut xattrs = Vec::new();

        // List xattr names
        let names = xattr::list(path)?;

        for name in names {
            if !self.should_preserve(&name) {
                continue;
            }

            // Get xattr value
            if let Some(value) = xattr::get(path, &name)? {
                xattrs.push(Xattr {
                    name: name.as_bytes().to_vec(),
                    value,
                });
            }
        }

        Ok(xattrs)
    }

    /// Write xattrs to file
    #[cfg(target_os = "linux")]
    pub fn write_xattrs(&self, path: &Path, xattrs: &[Xattr]) -> io::Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        for xattr_entry in xattrs {
            let name = OsString::from_vec(xattr_entry.name.clone());

            if !self.should_preserve(&name) {
                continue;
            }

            xattr::set(path, &name, &xattr_entry.value)?;
        }

        Ok(())
    }

    /// Copy xattrs from source to destination
    #[cfg(target_os = "linux")]
    pub fn copy_xattrs(&self, src: &Path, dst: &Path) -> io::Result<()> {
        let xattrs = self.read_xattrs(src)?;
        self.write_xattrs(dst, &xattrs)
    }
}

/// Extended attribute entry
#[derive(Debug, Clone)]
pub struct Xattr {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

impl Xattr {
    /// Encode for wire transfer
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Name length (varint)
        let name_len = self.name.len() as u32;
        buf.extend_from_slice(&name_len.to_le_bytes());
        buf.extend_from_slice(&self.name);

        // Value length (varint)
        let value_len = self.value.len() as u32;
        buf.extend_from_slice(&value_len.to_le_bytes());
        buf.extend_from_slice(&self.value);

        buf
    }

    /// Decode from wire format
    pub fn decode(data: &[u8]) -> io::Result<(Self, usize)> {
        if data.len() < 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "xattr too short"
            ));
        }

        let mut offset = 0;

        // Read name
        let name_len = u32::from_le_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
        offset += 4;
        let name = data[offset..offset+name_len].to_vec();
        offset += name_len;

        // Read value
        let value_len = u32::from_le_bytes(data[offset..offset+4].try_into().unwrap()) as usize;
        offset += 4;
        let value = data[offset..offset+value_len].to_vec();
        offset += value_len;

        Ok((Self { name, value }, offset))
    }
}
```

### FileListCodec Wire Format

Complete file list encoding/decoding for wire transfer:

```rust
// File list wire codec (crates/flist/)
use std::io::{self, Read, Write};

/// File list wire codec
pub struct FileListCodec {
    protocol_version: u8,
    preserve_links: bool,
    preserve_devices: bool,
    preserve_specials: bool,
    preserve_uid: bool,
    preserve_gid: bool,
    preserve_acls: bool,
    preserve_xattrs: bool,
}

impl FileListCodec {
    pub fn new(protocol_version: u8) -> Self {
        Self {
            protocol_version,
            preserve_links: false,
            preserve_devices: false,
            preserve_specials: false,
            preserve_uid: false,
            preserve_gid: false,
            preserve_acls: false,
            preserve_xattrs: false,
        }
    }

    /// Encode file entry for wire transfer
    pub fn encode_entry<W: Write>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        prev: Option<&FileEntry>,
    ) -> io::Result<()> {
        // Compute transmit flags
        let mut xflags = TransmitFlags::empty();

        // Path delta encoding
        let (path_prefix_len, path_suffix) = if let Some(prev) = prev {
            self.compute_path_delta(&entry.path, &prev.path)
        } else {
            (0, entry.path.as_os_str().as_encoded_bytes())
        };

        if path_prefix_len > 0 {
            xflags |= TransmitFlags::SAME_NAME;
        }

        // Check for same mode, uid, gid, mtime
        if let Some(prev) = prev {
            if entry.mode == prev.mode {
                xflags |= TransmitFlags::SAME_MODE;
            }
            if entry.uid == prev.uid {
                xflags |= TransmitFlags::SAME_UID;
            }
            if entry.gid == prev.gid {
                xflags |= TransmitFlags::SAME_GID;
            }
            if entry.mtime == prev.mtime {
                xflags |= TransmitFlags::SAME_TIME;
            }
        }

        // Write flags
        let flags_byte = xflags.bits() as u8;
        if xflags.bits() > 0xFF {
            writer.write_all(&[flags_byte | 0x80])?;
            writer.write_all(&[(xflags.bits() >> 8) as u8])?;
        } else {
            writer.write_all(&[flags_byte])?;
        }

        // Write path
        if xflags.contains(TransmitFlags::SAME_NAME) {
            writer.write_all(&[path_prefix_len as u8])?;
        }
        self.write_path_bytes(writer, path_suffix)?;

        // Write size
        self.write_file_size(writer, entry.size)?;

        // Write mtime (unless same as previous)
        if !xflags.contains(TransmitFlags::SAME_TIME) {
            self.write_mtime(writer, entry.mtime)?;
        }

        // Write mode (unless same as previous)
        if !xflags.contains(TransmitFlags::SAME_MODE) {
            writer.write_all(&entry.mode.to_le_bytes())?;
        }

        // Write uid/gid if preserving
        if self.preserve_uid && !xflags.contains(TransmitFlags::SAME_UID) {
            self.write_varint(writer, entry.uid as u64)?;
        }
        if self.preserve_gid && !xflags.contains(TransmitFlags::SAME_GID) {
            self.write_varint(writer, entry.gid as u64)?;
        }

        // Write symlink target if applicable
        if entry.is_symlink() && self.preserve_links {
            if let Some(ref target) = entry.link_target {
                let target_bytes = target.as_os_str().as_encoded_bytes();
                self.write_varint(writer, target_bytes.len() as u64)?;
                writer.write_all(target_bytes)?;
            }
        }

        // Write device info if applicable
        if entry.is_device() && (self.preserve_devices || self.preserve_specials) {
            if let Some(ref dev) = entry.device_info {
                writer.write_all(&dev.major.to_le_bytes())?;
                writer.write_all(&dev.minor.to_le_bytes())?;
            }
        }

        Ok(())
    }

    /// Decode file entry from wire format
    pub fn decode_entry<R: Read>(
        &self,
        reader: &mut R,
        prev: Option<&FileEntry>,
    ) -> io::Result<Option<FileEntry>> {
        // Read flags byte
        let mut flags_buf = [0u8; 1];
        reader.read_exact(&mut flags_buf)?;

        // Check for end marker
        if flags_buf[0] == 0 {
            return Ok(None);
        }

        let mut xflags = flags_buf[0] as u16;

        // Read extended flags if present
        if xflags & 0x80 != 0 {
            xflags &= !0x80;
            reader.read_exact(&mut flags_buf)?;
            xflags |= (flags_buf[0] as u16) << 8;
        }

        let xflags = TransmitFlags::from_bits_truncate(xflags);

        // Read path
        let path = self.read_path(reader, xflags, prev)?;

        // Read size
        let size = self.read_file_size(reader)?;

        // Read mtime
        let mtime = if xflags.contains(TransmitFlags::SAME_TIME) {
            prev.map(|p| p.mtime).unwrap_or(0)
        } else {
            self.read_mtime(reader)?
        };

        // Read mode
        let mode = if xflags.contains(TransmitFlags::SAME_MODE) {
            prev.map(|p| p.mode).unwrap_or(0o644)
        } else {
            let mut mode_buf = [0u8; 4];
            reader.read_exact(&mut mode_buf)?;
            u32::from_le_bytes(mode_buf)
        };

        // Read uid/gid
        let uid = if self.preserve_uid && !xflags.contains(TransmitFlags::SAME_UID) {
            self.read_varint(reader)? as u32
        } else {
            prev.map(|p| p.uid).unwrap_or(0)
        };

        let gid = if self.preserve_gid && !xflags.contains(TransmitFlags::SAME_GID) {
            self.read_varint(reader)? as u32
        } else {
            prev.map(|p| p.gid).unwrap_or(0)
        };

        Ok(Some(FileEntry {
            path,
            size,
            mtime,
            mode,
            uid,
            gid,
            link_target: None,  // Read separately if symlink
            device_info: None,  // Read separately if device
        }))
    }

    fn write_file_size<W: Write>(&self, writer: &mut W, size: u64) -> io::Result<()> {
        if self.protocol_version >= 30 {
            self.write_varint(writer, size)
        } else {
            // Protocol 28-29: 4-byte LE, or 12-byte for large files
            if size <= 0x7FFFFFFF {
                writer.write_all(&(size as u32).to_le_bytes())
            } else {
                writer.write_all(&0xFFFFFFFFu32.to_le_bytes())?;
                writer.write_all(&size.to_le_bytes())
            }
        }
    }

    fn read_file_size<R: Read>(&self, reader: &mut R) -> io::Result<u64> {
        if self.protocol_version >= 30 {
            self.read_varint(reader)
        } else {
            let mut buf = [0u8; 4];
            reader.read_exact(&mut buf)?;
            let val = u32::from_le_bytes(buf);
            if val != 0xFFFFFFFF {
                Ok(val as u64)
            } else {
                let mut buf = [0u8; 8];
                reader.read_exact(&mut buf)?;
                Ok(u64::from_le_bytes(buf))
            }
        }
    }

    fn write_mtime<W: Write>(&self, writer: &mut W, mtime: i64) -> io::Result<()> {
        if self.protocol_version >= 30 {
            self.write_svarint(writer, mtime)
        } else {
            writer.write_all(&(mtime as u32).to_le_bytes())
        }
    }

    fn read_mtime<R: Read>(&self, reader: &mut R) -> io::Result<i64> {
        if self.protocol_version >= 30 {
            self.read_svarint(reader)
        } else {
            let mut buf = [0u8; 4];
            reader.read_exact(&mut buf)?;
            Ok(u32::from_le_bytes(buf) as i64)
        }
    }

    // Helper methods for path encoding, varint, etc.
    fn compute_path_delta<'a>(&self, path: &'a Path, prev: &Path) -> (usize, &'a [u8]) {
        let path_bytes = path.as_os_str().as_encoded_bytes();
        let prev_bytes = prev.as_os_str().as_encoded_bytes();

        let common = path_bytes.iter()
            .zip(prev_bytes.iter())
            .take_while(|(a, b)| a == b)
            .count();

        (common, &path_bytes[common..])
    }

    fn write_path_bytes<W: Write>(&self, writer: &mut W, path: &[u8]) -> io::Result<()> {
        if path.len() < 0x80 {
            writer.write_all(&[path.len() as u8])?;
        } else {
            writer.write_all(&[0x80 | (path.len() & 0x7F) as u8])?;
            writer.write_all(&[(path.len() >> 7) as u8])?;
        }
        writer.write_all(path)
    }

    fn read_path<R: Read>(
        &self,
        reader: &mut R,
        xflags: TransmitFlags,
        prev: Option<&FileEntry>,
    ) -> io::Result<PathBuf> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let prefix_len = if xflags.contains(TransmitFlags::SAME_NAME) {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0] as usize
        } else {
            0
        };

        // Read path length
        let mut len_buf = [0u8; 1];
        reader.read_exact(&mut len_buf)?;
        let suffix_len = if len_buf[0] & 0x80 != 0 {
            let low = (len_buf[0] & 0x7F) as usize;
            reader.read_exact(&mut len_buf)?;
            low | ((len_buf[0] as usize) << 7)
        } else {
            len_buf[0] as usize
        };

        // Build path
        let mut path_bytes = Vec::with_capacity(prefix_len + suffix_len);

        if prefix_len > 0 {
            if let Some(prev) = prev {
                let prev_bytes = prev.path.as_os_str().as_encoded_bytes();
                path_bytes.extend_from_slice(&prev_bytes[..prefix_len]);
            }
        }

        // Read suffix
        let mut suffix = vec![0u8; suffix_len];
        reader.read_exact(&mut suffix)?;
        path_bytes.extend_from_slice(&suffix);

        Ok(PathBuf::from(OsString::from_vec(path_bytes)))
    }

    fn write_varint<W: Write>(&self, writer: &mut W, mut value: u64) -> io::Result<()> {
        loop {
            if value < 0x80 {
                writer.write_all(&[value as u8])?;
                return Ok(());
            }
            writer.write_all(&[0x80 | (value & 0x7F) as u8])?;
            value >>= 7;
        }
    }

    fn read_varint<R: Read>(&self, reader: &mut R) -> io::Result<u64> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            value |= ((buf[0] & 0x7F) as u64) << shift;
            if buf[0] & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
        }
    }

    fn write_svarint<W: Write>(&self, writer: &mut W, value: i64) -> io::Result<()> {
        let encoded = ((value << 1) ^ (value >> 63)) as u64;
        self.write_varint(writer, encoded)
    }

    fn read_svarint<R: Read>(&self, reader: &mut R) -> io::Result<i64> {
        let encoded = self.read_varint(reader)?;
        Ok(((encoded >> 1) as i64) ^ -((encoded & 1) as i64))
    }
}

bitflags::bitflags! {
    /// Transmit flags for file list entries
    pub struct TransmitFlags: u16 {
        const SAME_MODE = 0x0002;
        const EXTENDED_FLAGS = 0x0004;
        const SAME_UID = 0x0008;
        const SAME_GID = 0x0010;
        const SAME_NAME = 0x0020;
        const LONG_NAME = 0x0040;
        const SAME_TIME = 0x0080;
        const SAME_RDEV_MAJOR = 0x0100;
        const HLINKED = 0x0200;
        const USER_NAME_FOLLOWS = 0x0400;
        const GROUP_NAME_FOLLOWS = 0x0800;
        const HLINK_FIRST = 0x1000;
        const IO_ERROR_ENDLIST = 0x1000;
        const MOD_NSEC = 0x2000;
    }
}
```

### Checksum Streaming Patterns

Streaming checksum computation for large files:

```rust
// Checksum streaming (crates/checksums/)
use std::io::{self, Read};

/// Streaming checksum computer
pub struct StreamingChecksum<H: StrongHasher> {
    hasher: H,
    bytes_processed: u64,
    block_size: usize,
    block_checksums: Vec<u32>,  // Rolling checksums per block
}

impl<H: StrongHasher> StreamingChecksum<H> {
    pub fn new(hasher: H, block_size: usize) -> Self {
        Self {
            hasher,
            bytes_processed: 0,
            block_size,
            block_checksums: Vec::new(),
        }
    }

    /// Process data from reader, computing checksums incrementally
    pub fn process<R: Read>(&mut self, reader: &mut R) -> io::Result<()> {
        let mut buffer = vec![0u8; self.block_size];

        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }

            let chunk = &buffer[..n];

            // Update strong hash
            self.hasher.update(chunk);

            // Compute rolling checksum for complete blocks
            if n == self.block_size {
                let rolling = RollingChecksum::compute(chunk);
                self.block_checksums.push(rolling.value());
            }

            self.bytes_processed += n as u64;
        }

        Ok(())
    }

    /// Finalize and get file signature
    pub fn finalize(self) -> FileSignature {
        FileSignature {
            strong_hash: self.hasher.finalize(),
            block_checksums: self.block_checksums,
            file_size: self.bytes_processed,
            block_size: self.block_size as u32,
        }
    }
}

/// Strong hasher trait for different algorithms
pub trait StrongHasher: Send {
    fn update(&mut self, data: &[u8]);
    fn finalize(self) -> Vec<u8>;
}

/// MD5 hasher implementation
pub struct Md5Hasher {
    state: md5::Md5,
}

impl Md5Hasher {
    pub fn new() -> Self {
        use md5::Digest;
        Self { state: md5::Md5::new() }
    }
}

impl StrongHasher for Md5Hasher {
    fn update(&mut self, data: &[u8]) {
        use md5::Digest;
        self.state.update(data);
    }

    fn finalize(self) -> Vec<u8> {
        use md5::Digest;
        self.state.finalize().to_vec()
    }
}

/// XXHash64 hasher (protocol 30+)
pub struct XxHasher {
    state: xxhash_rust::xxh3::Xxh3,
}

impl XxHasher {
    pub fn new() -> Self {
        Self { state: xxhash_rust::xxh3::Xxh3::new() }
    }
}

impl StrongHasher for XxHasher {
    fn update(&mut self, data: &[u8]) {
        self.state.update(data);
    }

    fn finalize(self) -> Vec<u8> {
        self.state.digest128().to_le_bytes().to_vec()
    }
}

/// File signature containing all checksums
#[derive(Debug, Clone)]
pub struct FileSignature {
    pub strong_hash: Vec<u8>,
    pub block_checksums: Vec<u32>,
    pub file_size: u64,
    pub block_size: u32,
}

impl FileSignature {
    /// Create hasher appropriate for protocol version
    pub fn create_hasher(protocol_version: u8) -> Box<dyn StrongHasher> {
        if protocol_version >= 30 {
            Box::new(XxHasher::new())
        } else {
            Box::new(Md5Hasher::new())
        }
    }
}
```

### Progress Callback Integration

Progress reporting for transfer operations:

```rust
// Progress callback (crates/core/)
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Progress callback trait
pub trait ProgressCallback: Send + Sync {
    /// Called when a file transfer starts
    fn file_started(&self, path: &Path, size: u64);

    /// Called periodically during transfer
    fn progress(&self, bytes_transferred: u64, total_bytes: u64);

    /// Called when a file transfer completes
    fn file_completed(&self, path: &Path, bytes: u64, duration: Duration);

    /// Called when transfer encounters an error
    fn file_error(&self, path: &Path, error: &io::Error);

    /// Called with overall stats at end
    fn transfer_complete(&self, stats: &TransferStats);
}

/// Transfer statistics
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    pub files_total: u64,
    pub files_transferred: u64,
    pub files_skipped: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub bytes_matched: u64,   // Data matched via delta
    pub bytes_literal: u64,   // Data sent as literals
    pub duration: Duration,
}

impl TransferStats {
    pub fn speedup(&self) -> f64 {
        let total = self.bytes_matched + self.bytes_literal;
        if self.bytes_sent + self.bytes_received > 0 {
            total as f64 / (self.bytes_sent + self.bytes_received) as f64
        } else {
            1.0
        }
    }
}

/// Default progress reporter (writes to stderr)
pub struct DefaultProgressReporter {
    start: Instant,
    last_update: std::sync::Mutex<Instant>,
    update_interval: Duration,
    current_file: std::sync::Mutex<Option<PathBuf>>,
}

impl DefaultProgressReporter {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last_update: std::sync::Mutex::new(now),
            update_interval: Duration::from_millis(100),
            current_file: std::sync::Mutex::new(None),
        }
    }
}

impl ProgressCallback for DefaultProgressReporter {
    fn file_started(&self, path: &Path, size: u64) {
        *self.current_file.lock().unwrap() = Some(path.to_path_buf());
        eprintln!("{} ({} bytes)", path.display(), size);
    }

    fn progress(&self, bytes_transferred: u64, total_bytes: u64) {
        let mut last = self.last_update.lock().unwrap();
        let now = Instant::now();

        if now.duration_since(*last) >= self.update_interval {
            *last = now;

            let elapsed = now.duration_since(self.start);
            let rate = if elapsed.as_secs_f64() > 0.0 {
                bytes_transferred as f64 / elapsed.as_secs_f64()
            } else {
                0.0
            };

            let percent = if total_bytes > 0 {
                (bytes_transferred * 100) / total_bytes
            } else {
                0
            };

            eprint!(
                "\r{:>3}% {:>10} {:>10}/s",
                percent,
                format_bytes(bytes_transferred),
                format_bytes(rate as u64)
            );
        }
    }

    fn file_completed(&self, _path: &Path, _bytes: u64, _duration: Duration) {
        eprintln!();  // Clear progress line
    }

    fn file_error(&self, path: &Path, error: &io::Error) {
        eprintln!("\nError: {}: {}", path.display(), error);
    }

    fn transfer_complete(&self, stats: &TransferStats) {
        eprintln!();
        eprintln!(
            "sent {} bytes  received {} bytes  {:.2} bytes/sec",
            stats.bytes_sent,
            stats.bytes_received,
            (stats.bytes_sent + stats.bytes_received) as f64 / stats.duration.as_secs_f64()
        );
        eprintln!(
            "total size is {}  speedup is {:.2}",
            format_bytes(stats.bytes_matched + stats.bytes_literal),
            stats.speedup()
        );
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2}M", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2}K", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}

/// Silent progress (for programmatic use)
pub struct SilentProgress;

impl ProgressCallback for SilentProgress {
    fn file_started(&self, _: &Path, _: u64) {}
    fn progress(&self, _: u64, _: u64) {}
    fn file_completed(&self, _: &Path, _: u64, _: Duration) {}
    fn file_error(&self, _: &Path, _: &io::Error) {}
    fn transfer_complete(&self, _: &TransferStats) {}
}
```

### Sparse File Detection and Writing

Efficient sparse file handling:

```rust
// Sparse file handling (crates/engine/)
use std::io::{self, Read, Write, Seek, SeekFrom};
use std::fs::File;

/// Sparse file writer that creates holes for zero regions
pub struct SparseWriter {
    file: File,
    position: u64,
    pending_zeros: u64,
    min_hole_size: u64,
}

impl SparseWriter {
    pub fn new(file: File) -> Self {
        Self {
            file,
            position: 0,
            pending_zeros: 0,
            min_hole_size: 4096,  // Minimum hole size (page size)
        }
    }

    pub fn with_min_hole_size(mut self, size: u64) -> Self {
        self.min_hole_size = size;
        self
    }

    /// Write data, detecting zero runs and creating holes
    pub fn write_sparse(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut offset = 0;

        while offset < data.len() {
            // Find length of zero run at current position
            let zero_len = self.zero_run_length(&data[offset..]);

            if zero_len > 0 {
                self.pending_zeros += zero_len as u64;
                offset += zero_len;
                continue;
            }

            // Find length of non-zero run
            let data_len = self.data_run_length(&data[offset..]);

            // Flush pending zeros if enough accumulated
            self.maybe_create_hole()?;

            // Write non-zero data
            self.file.write_all(&data[offset..offset + data_len])?;
            self.position += data_len as u64;
            offset += data_len;
        }

        Ok(data.len())
    }

    fn zero_run_length(&self, data: &[u8]) -> usize {
        // Use 16-byte chunks for faster comparison
        let mut len = 0;

        // Check 16-byte aligned chunks
        while len + 16 <= data.len() {
            let chunk: [u8; 16] = data[len..len+16].try_into().unwrap();
            if u128::from_ne_bytes(chunk) != 0 {
                break;
            }
            len += 16;
        }

        // Check remaining bytes
        while len < data.len() && data[len] == 0 {
            len += 1;
        }

        len
    }

    fn data_run_length(&self, data: &[u8]) -> usize {
        // Find next significant zero run
        for (i, window) in data.windows(16).enumerate() {
            if window.iter().all(|&b| b == 0) {
                return i;
            }
        }
        data.len()
    }

    fn maybe_create_hole(&mut self) -> io::Result<()> {
        if self.pending_zeros >= self.min_hole_size {
            // Create hole by seeking past zeros
            self.file.seek(SeekFrom::Current(self.pending_zeros as i64))?;
            self.position += self.pending_zeros;
        } else if self.pending_zeros > 0 {
            // Write zeros if not enough for a hole
            let zeros = vec![0u8; self.pending_zeros as usize];
            self.file.write_all(&zeros)?;
            self.position += self.pending_zeros;
        }
        self.pending_zeros = 0;
        Ok(())
    }

    /// Finalize file, handling trailing zeros
    pub fn finalize(mut self) -> io::Result<u64> {
        if self.pending_zeros > 0 {
            // Truncate file to create trailing hole
            self.file.set_len(self.position + self.pending_zeros)?;
            self.position += self.pending_zeros;
        }
        Ok(self.position)
    }
}

/// Detect if data is sparse (contains large zero regions)
pub fn detect_sparse_regions(data: &[u8], threshold: usize) -> Vec<SparseRegion> {
    let mut regions = Vec::new();
    let mut offset = 0;

    while offset < data.len() {
        // Check for zero run
        let zero_start = offset;
        while offset < data.len() && data[offset] == 0 {
            offset += 1;
        }

        let zero_len = offset - zero_start;
        if zero_len >= threshold {
            regions.push(SparseRegion::Hole {
                offset: zero_start,
                length: zero_len,
            });
        } else if zero_len > 0 {
            regions.push(SparseRegion::Data {
                offset: zero_start,
                length: zero_len,
            });
        }

        // Find data run
        let data_start = offset;
        while offset < data.len() && data[offset] != 0 {
            offset += 1;
        }

        if offset > data_start {
            regions.push(SparseRegion::Data {
                offset: data_start,
                length: offset - data_start,
            });
        }
    }

    regions
}

#[derive(Debug, Clone)]
pub enum SparseRegion {
    Data { offset: usize, length: usize },
    Hole { offset: usize, length: usize },
}
```

### Symlink and Hardlink Handling

Link creation and preservation:

```rust
// Link handling (crates/metadata/)
use std::path::{Path, PathBuf};
use std::io;
use std::collections::HashMap;

/// Hardlink manager for detecting and creating hardlinks
pub struct HardlinkManager {
    /// Map from (dev, inode) to first occurrence path
    seen: HashMap<(u64, u64), PathBuf>,
    /// Preserve hardlinks option
    preserve: bool,
}

impl HardlinkManager {
    pub fn new(preserve: bool) -> Self {
        Self {
            seen: HashMap::new(),
            preserve,
        }
    }

    /// Check if file is hardlinked to a previously seen file
    #[cfg(unix)]
    pub fn check_hardlink(&mut self, path: &Path, metadata: &std::fs::Metadata) -> Option<&Path> {
        use std::os::unix::fs::MetadataExt;

        if !self.preserve || metadata.nlink() <= 1 {
            return None;
        }

        let key = (metadata.dev(), metadata.ino());

        if let Some(first) = self.seen.get(&key) {
            Some(first.as_path())
        } else {
            self.seen.insert(key, path.to_path_buf());
            None
        }
    }

    /// Create hardlink at destination
    #[cfg(unix)]
    pub fn create_hardlink(src: &Path, dst: &Path) -> io::Result<()> {
        std::fs::hard_link(src, dst)
    }
}

/// Symlink handling
pub struct SymlinkHandler {
    /// Follow symlinks (dereference)
    dereference: bool,
    /// Copy symlinks as symlinks
    preserve_links: bool,
    /// Copy unsafe symlinks
    copy_unsafe: bool,
    /// Base path for determining safe symlinks
    base_path: PathBuf,
}

impl SymlinkHandler {
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            dereference: false,
            preserve_links: true,
            copy_unsafe: false,
            base_path: base_path.into(),
        }
    }

    pub fn dereference(mut self, value: bool) -> Self {
        self.dereference = value;
        self
    }

    pub fn copy_unsafe(mut self, value: bool) -> Self {
        self.copy_unsafe = value;
        self
    }

    /// Read symlink target
    pub fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::read_link(path)
    }

    /// Check if symlink is "safe" (points within base directory)
    pub fn is_safe_symlink(&self, link_path: &Path, target: &Path) -> bool {
        // Resolve the target relative to the link's parent
        let resolved = if target.is_absolute() {
            target.to_path_buf()
        } else {
            let parent = link_path.parent().unwrap_or(Path::new("."));
            parent.join(target)
        };

        // Canonicalize to check if within base
        match resolved.canonicalize() {
            Ok(canonical) => canonical.starts_with(&self.base_path),
            Err(_) => false,  // Can't resolve, treat as unsafe
        }
    }

    /// Create symlink at destination
    #[cfg(unix)]
    pub fn create_symlink(&self, target: &Path, link_path: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link_path)
    }

    #[cfg(windows)]
    pub fn create_symlink(&self, target: &Path, link_path: &Path) -> io::Result<()> {
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link_path)
        } else {
            std::os::windows::fs::symlink_file(target, link_path)
        }
    }

    /// Handle symlink during transfer
    pub fn handle_symlink(
        &self,
        src_path: &Path,
        dst_path: &Path,
        target: &Path,
    ) -> io::Result<SymlinkAction> {
        if self.dereference {
            return Ok(SymlinkAction::Dereference);
        }

        if !self.preserve_links {
            return Ok(SymlinkAction::Skip);
        }

        if !self.copy_unsafe && !self.is_safe_symlink(src_path, target) {
            return Ok(SymlinkAction::SkipUnsafe);
        }

        // Create symlink at destination
        if dst_path.exists() || dst_path.symlink_metadata().is_ok() {
            std::fs::remove_file(dst_path)?;
        }

        self.create_symlink(target, dst_path)?;
        Ok(SymlinkAction::Created)
    }
}

#[derive(Debug)]
pub enum SymlinkAction {
    /// Follow symlink and transfer target
    Dereference,
    /// Skip symlink entirely
    Skip,
    /// Skip because symlink points outside tree
    SkipUnsafe,
    /// Created symlink at destination
    Created,
}
```

### Device and Special File Support

Handling of device nodes and special files:

```rust
// Device and special file handling (crates/metadata/)
use std::path::Path;
use std::io;

/// Device information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceInfo {
    pub major: u32,
    pub minor: u32,
    pub device_type: DeviceType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Block,
    Character,
}

impl DeviceInfo {
    #[cfg(unix)]
    pub fn from_metadata(metadata: &std::fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let file_type = metadata.file_type();
        let device_type = if file_type.is_block_device() {
            DeviceType::Block
        } else if file_type.is_char_device() {
            DeviceType::Character
        } else {
            return None;
        };

        let rdev = metadata.rdev();
        Some(Self {
            major: unsafe { libc::major(rdev) } as u32,
            minor: unsafe { libc::minor(rdev) } as u32,
            device_type,
        })
    }

    #[cfg(unix)]
    pub fn to_rdev(&self) -> u64 {
        unsafe { libc::makedev(self.major, self.minor) }
    }
}

/// Special file handler
pub struct SpecialFileHandler {
    preserve_devices: bool,
    preserve_specials: bool,
}

impl SpecialFileHandler {
    pub fn new() -> Self {
        Self {
            preserve_devices: false,
            preserve_specials: false,
        }
    }

    pub fn preserve_devices(mut self, value: bool) -> Self {
        self.preserve_devices = value;
        self
    }

    pub fn preserve_specials(mut self, value: bool) -> Self {
        self.preserve_specials = value;
        self
    }

    /// Create device node at destination (requires CAP_MKNOD)
    #[cfg(unix)]
    pub fn create_device(&self, path: &Path, dev: &DeviceInfo, mode: u32) -> io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes())?;

        let dev_type = match dev.device_type {
            DeviceType::Block => libc::S_IFBLK,
            DeviceType::Character => libc::S_IFCHR,
        };

        let result = unsafe {
            libc::mknod(
                c_path.as_ptr(),
                dev_type | (mode & 0o7777),
                dev.to_rdev(),
            )
        };

        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Create FIFO (named pipe)
    #[cfg(unix)]
    pub fn create_fifo(&self, path: &Path, mode: u32) -> io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes())?;

        let result = unsafe {
            libc::mkfifo(c_path.as_ptr(), mode & 0o7777)
        };

        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Create socket node
    #[cfg(unix)]
    pub fn create_socket(&self, path: &Path) -> io::Result<()> {
        use std::os::unix::net::UnixListener;

        // Create and immediately close to leave socket node
        let listener = UnixListener::bind(path)?;
        drop(listener);
        Ok(())
    }

    /// Handle special file during transfer
    pub fn handle_special(
        &self,
        metadata: &std::fs::Metadata,
        dst_path: &Path,
        mode: u32,
    ) -> io::Result<SpecialAction> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;

            let file_type = metadata.file_type();

            if file_type.is_block_device() || file_type.is_char_device() {
                if !self.preserve_devices {
                    return Ok(SpecialAction::Skip);
                }

                if let Some(dev) = DeviceInfo::from_metadata(metadata) {
                    self.create_device(dst_path, &dev, mode)?;
                    return Ok(SpecialAction::Created);
                }
            }

            if file_type.is_fifo() {
                if !self.preserve_specials {
                    return Ok(SpecialAction::Skip);
                }
                self.create_fifo(dst_path, mode)?;
                return Ok(SpecialAction::Created);
            }

            if file_type.is_socket() {
                if !self.preserve_specials {
                    return Ok(SpecialAction::Skip);
                }
                self.create_socket(dst_path)?;
                return Ok(SpecialAction::Created);
            }
        }

        Ok(SpecialAction::Skip)
    }
}

#[derive(Debug)]
pub enum SpecialAction {
    Skip,
    Created,
}
```

### Batch Mode Operations

Batch file for offline transfer:

```rust
// Batch mode (crates/core/)
use std::io::{self, Read, Write, BufReader, BufWriter};
use std::fs::File;
use std::path::Path;

/// Batch file writer for recording transfer operations
pub struct BatchWriter {
    writer: BufWriter<File>,
    protocol_version: u8,
}

impl BatchWriter {
    pub fn create(path: &Path, protocol_version: u8) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);

        // Write batch file header
        writer.write_all(b"RSYNC_BATCH")?;
        writer.write_all(&[protocol_version])?;

        Ok(Self { writer, protocol_version })
    }

    /// Write file list to batch
    pub fn write_file_list(&mut self, entries: &[FileEntry]) -> io::Result<()> {
        // Write count
        let count = entries.len() as u32;
        self.writer.write_all(&count.to_le_bytes())?;

        // Write each entry
        let codec = FileListCodec::new(self.protocol_version);
        let mut prev: Option<&FileEntry> = None;

        for entry in entries {
            codec.encode_entry(&mut self.writer, entry, prev)?;
            prev = Some(entry);
        }

        // Write end marker
        self.writer.write_all(&[0])?;

        Ok(())
    }

    /// Write delta data for a file
    pub fn write_delta(&mut self, file_index: u32, delta: &[u8]) -> io::Result<()> {
        // Write file index
        self.writer.write_all(&file_index.to_le_bytes())?;

        // Write delta length
        let len = delta.len() as u32;
        self.writer.write_all(&len.to_le_bytes())?;

        // Write delta data
        self.writer.write_all(delta)?;

        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        // Write end marker
        self.writer.write_all(&[0xFF, 0xFF, 0xFF, 0xFF])?;
        self.writer.flush()
    }
}

/// Batch file reader for applying recorded operations
pub struct BatchReader {
    reader: BufReader<File>,
    protocol_version: u8,
    file_list: Vec<FileEntry>,
}

impl BatchReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        // Read and verify header
        let mut header = [0u8; 11];
        reader.read_exact(&mut header)?;

        if &header[..11] != b"RSYNC_BATCH" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid batch file header"
            ));
        }

        let mut version = [0u8; 1];
        reader.read_exact(&mut version)?;

        Ok(Self {
            reader,
            protocol_version: version[0],
            file_list: Vec::new(),
        })
    }

    /// Read file list from batch
    pub fn read_file_list(&mut self) -> io::Result<&[FileEntry]> {
        // Read count
        let mut count_buf = [0u8; 4];
        self.reader.read_exact(&mut count_buf)?;
        let count = u32::from_le_bytes(count_buf) as usize;

        let codec = FileListCodec::new(self.protocol_version);
        let mut prev: Option<&FileEntry> = None;

        self.file_list.reserve(count);

        for _ in 0..count {
            if let Some(entry) = codec.decode_entry(&mut self.reader, prev)? {
                self.file_list.push(entry);
                prev = self.file_list.last();
            }
        }

        Ok(&self.file_list)
    }

    /// Read next delta operation
    pub fn read_delta(&mut self) -> io::Result<Option<(u32, Vec<u8>)>> {
        // Read file index
        let mut idx_buf = [0u8; 4];
        self.reader.read_exact(&mut idx_buf)?;
        let file_index = u32::from_le_bytes(idx_buf);

        // Check for end marker
        if file_index == 0xFFFFFFFF {
            return Ok(None);
        }

        // Read delta length
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        // Read delta data
        let mut delta = vec![0u8; len];
        self.reader.read_exact(&mut delta)?;

        Ok(Some((file_index, delta)))
    }
}
```

### Redo Queue for Failed Transfers

Handling files that fail on first attempt:

```rust
// Redo queue (crates/core/)
use std::collections::VecDeque;

/// Queue for files that failed and need retry
pub struct RedoQueue {
    entries: VecDeque<RedoEntry>,
    max_retries: u32,
}

#[derive(Debug, Clone)]
pub struct RedoEntry {
    pub file_index: u32,
    pub reason: RedoReason,
    pub attempts: u32,
}

#[derive(Debug, Clone)]
pub enum RedoReason {
    /// Basis file changed during transfer
    BasisChanged,
    /// Checksum mismatch
    ChecksumMismatch,
    /// Temporary I/O error
    IoError(String),
    /// File vanished during transfer
    Vanished,
}

impl RedoQueue {
    pub fn new(max_retries: u32) -> Self {
        Self {
            entries: VecDeque::new(),
            max_retries,
        }
    }

    /// Add file to redo queue
    pub fn queue(&mut self, file_index: u32, reason: RedoReason) {
        // Check if already queued
        if self.entries.iter().any(|e| e.file_index == file_index) {
            return;
        }

        self.entries.push_back(RedoEntry {
            file_index,
            reason,
            attempts: 0,
        });
    }

    /// Get next file to retry
    pub fn pop(&mut self) -> Option<RedoEntry> {
        self.entries.pop_front()
    }

    /// Re-queue entry with incremented attempt count
    pub fn requeue(&mut self, mut entry: RedoEntry) -> bool {
        entry.attempts += 1;

        if entry.attempts >= self.max_retries {
            return false;  // Exceeded max retries
        }

        self.entries.push_back(entry);
        true
    }

    /// Check if queue has entries
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get number of pending entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Drain all entries (for error reporting)
    pub fn drain(&mut self) -> impl Iterator<Item = RedoEntry> + '_ {
        self.entries.drain(..)
    }
}

/// File transfer with redo support
pub struct TransferWithRedo {
    redo_queue: RedoQueue,
}

impl TransferWithRedo {
    pub fn new() -> Self {
        Self {
            redo_queue: RedoQueue::new(3),
        }
    }

    /// Process file list with automatic retries
    pub fn process_files<F>(&mut self, files: &[FileEntry], mut transfer_fn: F) -> TransferResult
    where
        F: FnMut(u32, &FileEntry) -> Result<(), TransferError>,
    {
        let mut stats = TransferResult::default();

        // First pass: transfer all files
        for (index, entry) in files.iter().enumerate() {
            match transfer_fn(index as u32, entry) {
                Ok(()) => stats.transferred += 1,
                Err(e) => {
                    if e.is_retriable() {
                        self.redo_queue.queue(index as u32, e.into());
                    } else {
                        stats.failed += 1;
                    }
                }
            }
        }

        // Redo pass: retry failed files
        while let Some(entry) = self.redo_queue.pop() {
            let file = &files[entry.file_index as usize];

            match transfer_fn(entry.file_index, file) {
                Ok(()) => stats.transferred += 1,
                Err(e) => {
                    if e.is_retriable() && self.redo_queue.requeue(entry) {
                        // Will retry again
                    } else {
                        stats.failed += 1;
                    }
                }
            }
        }

        stats
    }
}

#[derive(Default)]
pub struct TransferResult {
    pub transferred: u32,
    pub failed: u32,
}

pub struct TransferError {
    pub kind: TransferErrorKind,
    pub message: String,
}

pub enum TransferErrorKind {
    BasisChanged,
    ChecksumMismatch,
    IoError,
    Vanished,
    PermissionDenied,
    DiskFull,
}

impl TransferError {
    pub fn is_retriable(&self) -> bool {
        matches!(
            self.kind,
            TransferErrorKind::BasisChanged |
            TransferErrorKind::ChecksumMismatch |
            TransferErrorKind::IoError |
            TransferErrorKind::Vanished
        )
    }
}

impl From<TransferError> for RedoReason {
    fn from(e: TransferError) -> Self {
        match e.kind {
            TransferErrorKind::BasisChanged => RedoReason::BasisChanged,
            TransferErrorKind::ChecksumMismatch => RedoReason::ChecksumMismatch,
            TransferErrorKind::IoError => RedoReason::IoError(e.message),
            TransferErrorKind::Vanished => RedoReason::Vanished,
            _ => RedoReason::IoError(e.message),
        }
    }
}
```

### Connection Lifecycle Management

TCP and SSH connection handling:

```rust
// Connection lifecycle (crates/transport/)
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Connection state machine
pub struct Connection {
    stream: Box<dyn ReadWrite>,
    state: ConnectionPhase,
    timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPhase {
    /// Initial connection established
    Connected,
    /// Protocol version exchanged
    Negotiated,
    /// Module selected (daemon mode)
    ModuleSelected,
    /// Authentication complete
    Authenticated,
    /// File list exchange
    FileList,
    /// Data transfer active
    Transferring,
    /// Transfer complete, closing
    Closing,
    /// Connection closed
    Closed,
}

impl Connection {
    /// Connect to rsync daemon
    pub fn connect_daemon<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;

        Ok(Self {
            stream: Box::new(stream),
            state: ConnectionPhase::Connected,
            timeout: Some(Duration::from_secs(60)),
        })
    }

    /// Connect via SSH
    pub fn connect_ssh(host: &str, user: Option<&str>, command: &str) -> io::Result<Self> {
        use std::process::{Command, Stdio};

        let mut cmd = Command::new("ssh");

        if let Some(u) = user {
            cmd.arg("-l").arg(u);
        }

        cmd.arg(host)
           .arg("--")
           .arg(command)
           .stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .stderr(Stdio::inherit());

        let child = cmd.spawn()?;

        let stdin = child.stdin.expect("stdin");
        let stdout = child.stdout.expect("stdout");

        Ok(Self {
            stream: Box::new(SshStream { stdin, stdout }),
            state: ConnectionPhase::Connected,
            timeout: None,
        })
    }

    /// Set I/O timeout
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
        if let Some(tcp) = self.stream.as_tcp() {
            let _ = tcp.set_read_timeout(timeout);
            let _ = tcp.set_write_timeout(timeout);
        }
    }

    /// Transition to next phase
    pub fn advance(&mut self, phase: ConnectionPhase) -> io::Result<()> {
        // Validate transition
        let valid = match (self.state, phase) {
            (ConnectionPhase::Connected, ConnectionPhase::Negotiated) => true,
            (ConnectionPhase::Negotiated, ConnectionPhase::ModuleSelected) => true,
            (ConnectionPhase::Negotiated, ConnectionPhase::FileList) => true,
            (ConnectionPhase::ModuleSelected, ConnectionPhase::Authenticated) => true,
            (ConnectionPhase::ModuleSelected, ConnectionPhase::FileList) => true,
            (ConnectionPhase::Authenticated, ConnectionPhase::FileList) => true,
            (ConnectionPhase::FileList, ConnectionPhase::Transferring) => true,
            (ConnectionPhase::Transferring, ConnectionPhase::Closing) => true,
            (_, ConnectionPhase::Closing) => true,
            (ConnectionPhase::Closing, ConnectionPhase::Closed) => true,
            _ => false,
        };

        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid transition {:?} -> {:?}", self.state, phase)
            ));
        }

        self.state = phase;
        Ok(())
    }

    /// Get current phase
    pub fn phase(&self) -> ConnectionPhase {
        self.state
    }

    /// Graceful shutdown
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.state = ConnectionPhase::Closing;
        if let Some(tcp) = self.stream.as_tcp() {
            tcp.shutdown(std::net::Shutdown::Both)?;
        }
        self.state = ConnectionPhase::Closed;
        Ok(())
    }
}

/// SSH stream wrapper
struct SshStream {
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
}

impl Read for SshStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Write for SshStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

/// Trait for connection streams
pub trait ReadWrite: Read + Write + Send {
    fn as_tcp(&self) -> Option<&TcpStream> { None }
}

impl ReadWrite for TcpStream {
    fn as_tcp(&self) -> Option<&TcpStream> { Some(self) }
}

impl ReadWrite for SshStream {}
```

### ACL Handler Implementation

POSIX ACL preservation:

```rust
// ACL handling (crates/metadata/)
#[cfg(target_os = "linux")]
use std::path::Path;
use std::io;

/// ACL entry type
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AclEntryType {
    User,
    Group,
    Other,
    Mask,
    NamedUser(u32),   // uid
    NamedGroup(u32),  // gid
}

/// ACL permission bits
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclPermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl AclPermissions {
    pub fn from_mode(mode: u8) -> Self {
        Self {
            read: mode & 4 != 0,
            write: mode & 2 != 0,
            execute: mode & 1 != 0,
        }
    }

    pub fn to_mode(&self) -> u8 {
        (if self.read { 4 } else { 0 })
            | (if self.write { 2 } else { 0 })
            | (if self.execute { 1 } else { 0 })
    }
}

/// ACL entry
#[derive(Debug, Clone)]
pub struct AclEntry {
    pub entry_type: AclEntryType,
    pub perms: AclPermissions,
}

/// ACL handler for reading/writing POSIX ACLs
pub struct AclHandler {
    preserve: bool,
}

impl AclHandler {
    pub fn new(preserve: bool) -> Self {
        Self { preserve }
    }

    /// Read ACL from file
    #[cfg(target_os = "linux")]
    pub fn read_acl(&self, path: &Path) -> io::Result<Vec<AclEntry>> {
        if !self.preserve {
            return Ok(Vec::new());
        }

        // Use system calls to read ACL
        // This is a simplified representation
        let metadata = std::fs::metadata(path)?;
        let mode = Self::get_mode(&metadata);

        // Convert standard mode to base ACL entries
        Ok(vec![
            AclEntry {
                entry_type: AclEntryType::User,
                perms: AclPermissions::from_mode((mode >> 6) as u8 & 7),
            },
            AclEntry {
                entry_type: AclEntryType::Group,
                perms: AclPermissions::from_mode((mode >> 3) as u8 & 7),
            },
            AclEntry {
                entry_type: AclEntryType::Other,
                perms: AclPermissions::from_mode(mode as u8 & 7),
            },
        ])
    }

    /// Write ACL to file
    #[cfg(target_os = "linux")]
    pub fn write_acl(&self, path: &Path, entries: &[AclEntry]) -> io::Result<()> {
        if !self.preserve || entries.is_empty() {
            return Ok(());
        }

        // Build mode from base ACL entries
        let mut mode: u32 = 0;

        for entry in entries {
            match entry.entry_type {
                AclEntryType::User => mode |= (entry.perms.to_mode() as u32) << 6,
                AclEntryType::Group => mode |= (entry.perms.to_mode() as u32) << 3,
                AclEntryType::Other => mode |= entry.perms.to_mode() as u32,
                _ => {} // Named entries handled separately
            }
        }

        // Apply base permissions
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;

        Ok(())
    }

    /// Encode ACL for wire transfer
    pub fn encode_acl(&self, entries: &[AclEntry]) -> Vec<u8> {
        let mut buf = Vec::new();

        // Entry count
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());

        for entry in entries {
            // Entry type tag
            let tag = match &entry.entry_type {
                AclEntryType::User => 0x01,
                AclEntryType::Group => 0x02,
                AclEntryType::Other => 0x03,
                AclEntryType::Mask => 0x04,
                AclEntryType::NamedUser(_) => 0x05,
                AclEntryType::NamedGroup(_) => 0x06,
            };
            buf.push(tag);

            // ID for named entries
            if let AclEntryType::NamedUser(id) | AclEntryType::NamedGroup(id) = entry.entry_type {
                buf.extend_from_slice(&id.to_le_bytes());
            }

            // Permissions
            buf.push(entry.perms.to_mode());
        }

        buf
    }

    /// Decode ACL from wire format
    pub fn decode_acl(&self, data: &[u8]) -> io::Result<Vec<AclEntry>> {
        if data.len() < 2 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ACL too short"));
        }

        let count = u16::from_le_bytes([data[0], data[1]]) as usize;
        let mut entries = Vec::with_capacity(count);
        let mut offset = 2;

        for _ in 0..count {
            if offset >= data.len() {
                break;
            }

            let tag = data[offset];
            offset += 1;

            let entry_type = match tag {
                0x01 => AclEntryType::User,
                0x02 => AclEntryType::Group,
                0x03 => AclEntryType::Other,
                0x04 => AclEntryType::Mask,
                0x05 => {
                    let id = u32::from_le_bytes(data[offset..offset+4].try_into().unwrap());
                    offset += 4;
                    AclEntryType::NamedUser(id)
                }
                0x06 => {
                    let id = u32::from_le_bytes(data[offset..offset+4].try_into().unwrap());
                    offset += 4;
                    AclEntryType::NamedGroup(id)
                }
                _ => continue,
            };

            let perms = AclPermissions::from_mode(data[offset]);
            offset += 1;

            entries.push(AclEntry { entry_type, perms });
        }

        Ok(entries)
    }

    #[cfg(unix)]
    fn get_mode(metadata: &std::fs::Metadata) -> u32 {
        use std::os::unix::fs::MetadataExt;
        metadata.mode()
    }
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
```

### FileTime Nanosecond Precision

High-precision time handling:

```rust
// FileTime with nanosecond precision (crates/metadata/)
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// File modification time with nanosecond precision
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileTime {
    /// Seconds since Unix epoch
    pub secs: i64,
    /// Nanoseconds (0-999_999_999)
    pub nsecs: u32,
}

impl FileTime {
    /// Create from seconds only (legacy mode)
    pub fn from_secs(secs: i64) -> Self {
        Self { secs, nsecs: 0 }
    }

    /// Create from seconds and nanoseconds
    pub fn new(secs: i64, nsecs: u32) -> Self {
        Self {
            secs,
            nsecs: nsecs.min(999_999_999),
        }
    }

    /// Create from SystemTime
    pub fn from_system_time(time: SystemTime) -> Self {
        match time.duration_since(UNIX_EPOCH) {
            Ok(d) => Self::new(d.as_secs() as i64, d.subsec_nanos()),
            Err(e) => {
                let d = e.duration();
                Self::new(-(d.as_secs() as i64), d.subsec_nanos())
            }
        }
    }

    /// Convert to SystemTime
    pub fn to_system_time(&self) -> SystemTime {
        if self.secs >= 0 {
            UNIX_EPOCH + Duration::new(self.secs as u64, self.nsecs)
        } else {
            UNIX_EPOCH - Duration::new((-self.secs) as u64, self.nsecs)
        }
    }

    /// Check if times are equal within tolerance
    pub fn equals_within(&self, other: &Self, tolerance_ns: u32) -> bool {
        let self_ns = self.secs as i128 * 1_000_000_000 + self.nsecs as i128;
        let other_ns = other.secs as i128 * 1_000_000_000 + other.nsecs as i128;
        (self_ns - other_ns).abs() <= tolerance_ns as i128
    }

    /// Encode for wire (protocol 31+: includes nanoseconds)
    pub fn encode(&self, protocol_version: u8) -> Vec<u8> {
        let mut buf = Vec::new();

        if protocol_version >= 31 {
            // 8-byte seconds + 4-byte nanoseconds
            buf.extend_from_slice(&self.secs.to_le_bytes());
            buf.extend_from_slice(&self.nsecs.to_le_bytes());
        } else {
            // Legacy: 4-byte seconds only
            buf.extend_from_slice(&(self.secs as u32).to_le_bytes());
        }

        buf
    }

    /// Decode from wire
    pub fn decode(data: &[u8], protocol_version: u8) -> io::Result<Self> {
        use std::io;

        if protocol_version >= 31 {
            if data.len() < 12 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "time too short"));
            }
            let secs = i64::from_le_bytes(data[..8].try_into().unwrap());
            let nsecs = u32::from_le_bytes(data[8..12].try_into().unwrap());
            Ok(Self::new(secs, nsecs))
        } else {
            if data.len() < 4 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "time too short"));
            }
            let secs = u32::from_le_bytes(data[..4].try_into().unwrap()) as i64;
            Ok(Self::from_secs(secs))
        }
    }
}

/// Apply mtime to file
#[cfg(unix)]
pub fn set_file_time(path: &std::path::Path, mtime: FileTime) -> io::Result<()> {
    use std::os::unix::fs::UtimesExt;

    let times = [
        // atime = mtime for rsync compatibility
        libc::timespec {
            tv_sec: mtime.secs,
            tv_nsec: mtime.nsecs as i64,
        },
        // mtime
        libc::timespec {
            tv_sec: mtime.secs,
            tv_nsec: mtime.nsecs as i64,
        },
    ];

    std::fs::File::open(path)?.set_times(times)?;
    Ok(())
}
```

### UID/GID Name Mapping

Username and group resolution:

```rust
// UID/GID mapping (crates/metadata/)
use std::collections::HashMap;
use std::sync::RwLock;

/// UID/GID mapper with caching
pub struct IdMapper {
    uid_cache: RwLock<HashMap<u32, String>>,
    gid_cache: RwLock<HashMap<u32, String>>,
    name_to_uid: RwLock<HashMap<String, u32>>,
    name_to_gid: RwLock<HashMap<String, u32>>,
    numeric_ids: bool,
}

impl IdMapper {
    pub fn new(numeric_ids: bool) -> Self {
        Self {
            uid_cache: RwLock::new(HashMap::new()),
            gid_cache: RwLock::new(HashMap::new()),
            name_to_uid: RwLock::new(HashMap::new()),
            name_to_gid: RwLock::new(HashMap::new()),
            numeric_ids,
        }
    }

    /// Get username for UID
    #[cfg(unix)]
    pub fn uid_to_name(&self, uid: u32) -> Option<String> {
        if self.numeric_ids {
            return None;
        }

        // Check cache
        if let Some(name) = self.uid_cache.read().ok()?.get(&uid) {
            return Some(name.clone());
        }

        // Look up in passwd
        let name = unsafe {
            let pw = libc::getpwuid(uid);
            if pw.is_null() {
                return None;
            }
            std::ffi::CStr::from_ptr((*pw).pw_name)
                .to_string_lossy()
                .into_owned()
        };

        // Cache result
        self.uid_cache.write().ok()?.insert(uid, name.clone());
        Some(name)
    }

    /// Get UID for username
    #[cfg(unix)]
    pub fn name_to_uid(&self, name: &str) -> Option<u32> {
        // Check cache
        if let Some(&uid) = self.name_to_uid.read().ok()?.get(name) {
            return Some(uid);
        }

        // Look up in passwd
        let c_name = std::ffi::CString::new(name).ok()?;
        let uid = unsafe {
            let pw = libc::getpwnam(c_name.as_ptr());
            if pw.is_null() {
                return None;
            }
            (*pw).pw_uid
        };

        // Cache result
        self.name_to_uid.write().ok()?.insert(name.to_string(), uid);
        Some(uid)
    }

    /// Get group name for GID
    #[cfg(unix)]
    pub fn gid_to_name(&self, gid: u32) -> Option<String> {
        if self.numeric_ids {
            return None;
        }

        // Check cache
        if let Some(name) = self.gid_cache.read().ok()?.get(&gid) {
            return Some(name.clone());
        }

        // Look up in group
        let name = unsafe {
            let gr = libc::getgrgid(gid);
            if gr.is_null() {
                return None;
            }
            std::ffi::CStr::from_ptr((*gr).gr_name)
                .to_string_lossy()
                .into_owned()
        };

        // Cache result
        self.gid_cache.write().ok()?.insert(gid, name.clone());
        Some(name)
    }

    /// Get GID for group name
    #[cfg(unix)]
    pub fn name_to_gid(&self, name: &str) -> Option<u32> {
        // Check cache
        if let Some(&gid) = self.name_to_gid.read().ok()?.get(name) {
            return Some(gid);
        }

        // Look up in group
        let c_name = std::ffi::CString::new(name).ok()?;
        let gid = unsafe {
            let gr = libc::getgrnam(c_name.as_ptr());
            if gr.is_null() {
                return None;
            }
            (*gr).gr_gid
        };

        // Cache result
        self.name_to_gid.write().ok()?.insert(name.to_string(), gid);
        Some(gid)
    }

    /// Build ID mapping table for transfer
    pub fn build_mapping(&self, entries: &[FileEntry]) -> IdMappingTable {
        let mut uid_table = HashMap::new();
        let mut gid_table = HashMap::new();

        for entry in entries {
            if let Some(name) = self.uid_to_name(entry.uid) {
                uid_table.entry(entry.uid).or_insert(name);
            }
            if let Some(name) = self.gid_to_name(entry.gid) {
                gid_table.entry(entry.gid).or_insert(name);
            }
        }

        IdMappingTable { uid_table, gid_table }
    }
}

/// ID mapping table for remote transfer
#[derive(Debug, Clone)]
pub struct IdMappingTable {
    pub uid_table: HashMap<u32, String>,
    pub gid_table: HashMap<u32, String>,
}

impl IdMappingTable {
    /// Encode for wire transfer
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // UID table
        buf.extend_from_slice(&(self.uid_table.len() as u16).to_le_bytes());
        for (&uid, name) in &self.uid_table {
            buf.extend_from_slice(&uid.to_le_bytes());
            buf.push(name.len() as u8);
            buf.extend_from_slice(name.as_bytes());
        }

        // GID table
        buf.extend_from_slice(&(self.gid_table.len() as u16).to_le_bytes());
        for (&gid, name) in &self.gid_table {
            buf.extend_from_slice(&gid.to_le_bytes());
            buf.push(name.len() as u8);
            buf.extend_from_slice(name.as_bytes());
        }

        buf
    }
}
```

### Backup Suffix Handling

Backup file creation patterns:

```rust
// Backup handling (crates/engine/)
use std::path::{Path, PathBuf};
use std::io;
use chrono::Local;

/// Backup suffix generator
pub struct BackupHandler {
    /// Suffix to append (default: "~")
    suffix: String,
    /// Use directory for backups
    backup_dir: Option<PathBuf>,
    /// Delete backups after transfer
    delete_after: bool,
}

impl BackupHandler {
    pub fn new() -> Self {
        Self {
            suffix: "~".to_string(),
            backup_dir: None,
            delete_after: false,
        }
    }

    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = suffix.into();
        self
    }

    pub fn with_backup_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.backup_dir = Some(dir.into());
        self
    }

    /// Generate backup path for file
    pub fn backup_path(&self, original: &Path) -> PathBuf {
        if let Some(ref backup_dir) = self.backup_dir {
            // Backup to separate directory
            let file_name = original.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            backup_dir.join(format!("{}{}", file_name, self.suffix))
        } else {
            // Backup in same directory
            let mut backup = original.to_path_buf();
            let file_name = backup.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            backup.set_file_name(format!("{}{}", file_name, self.suffix));
            backup
        }
    }

    /// Generate numbered backup path (for multiple backups)
    pub fn backup_path_numbered(&self, original: &Path) -> PathBuf {
        let base = self.backup_path(original);
        let mut n = 1;

        loop {
            let numbered = PathBuf::from(format!("{}.{}", base.display(), n));
            if !numbered.exists() {
                return numbered;
            }
            n += 1;
            if n > 9999 {
                // Fallback to timestamp
                return self.backup_path_timestamped(original);
            }
        }
    }

    /// Generate timestamped backup path
    pub fn backup_path_timestamped(&self, original: &Path) -> PathBuf {
        let timestamp = Local::now().format("%Y%m%d_%H%M%S");
        let base = self.backup_path(original);
        PathBuf::from(format!("{}.{}", base.display(), timestamp))
    }

    /// Create backup of file
    pub fn create_backup(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        if !path.exists() {
            return Ok(None);
        }

        let backup = self.backup_path(path);

        // Ensure backup directory exists
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Rename original to backup
        std::fs::rename(path, &backup)?;

        Ok(Some(backup))
    }

    /// Create backup by copying (preserves original)
    pub fn create_backup_copy(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        if !path.exists() {
            return Ok(None);
        }

        let backup = self.backup_path_numbered(path);

        // Ensure backup directory exists
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Copy original to backup
        std::fs::copy(path, &backup)?;

        Ok(Some(backup))
    }
}
```

### Partial Transfer Resume

Resume interrupted transfers:

```rust
// Partial transfer (crates/engine/)
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

/// Partial transfer manager
pub struct PartialTransfer {
    /// Directory for partial files
    partial_dir: Option<PathBuf>,
    /// Suffix for partial files
    suffix: String,
    /// Delay before removing partial file (seconds)
    delay: u32,
}

impl PartialTransfer {
    pub fn new() -> Self {
        Self {
            partial_dir: None,
            suffix: ".partial".to_string(),
            delay: 0,
        }
    }

    pub fn with_partial_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.partial_dir = Some(dir.into());
        self
    }

    /// Get path for partial file
    pub fn partial_path(&self, dest: &Path) -> PathBuf {
        if let Some(ref partial_dir) = self.partial_dir {
            let file_name = dest.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            partial_dir.join(format!("{}{}", file_name, self.suffix))
        } else {
            let mut partial = dest.to_path_buf();
            let file_name = partial.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            partial.set_file_name(format!("{}{}", file_name, self.suffix));
            partial
        }
    }

    /// Check for existing partial file and return resume offset
    pub fn check_resume(&self, dest: &Path, expected_size: u64) -> io::Result<Option<u64>> {
        let partial = self.partial_path(dest);

        if !partial.exists() {
            return Ok(None);
        }

        let metadata = std::fs::metadata(&partial)?;
        let partial_size = metadata.len();

        // Don't resume if partial is larger than expected
        if partial_size >= expected_size {
            std::fs::remove_file(&partial)?;
            return Ok(None);
        }

        Ok(Some(partial_size))
    }

    /// Open partial file for resume (creates if needed)
    pub fn open_partial(&self, dest: &Path, resume_offset: Option<u64>) -> io::Result<File> {
        let partial = self.partial_path(dest);

        // Ensure directory exists
        if let Some(parent) = partial.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(&partial)?;

        if let Some(offset) = resume_offset {
            file.seek(SeekFrom::Start(offset))?;
        }

        Ok(file)
    }

    /// Finalize partial transfer (rename to destination)
    pub fn finalize(&self, dest: &Path) -> io::Result<()> {
        let partial = self.partial_path(dest);

        if partial.exists() {
            std::fs::rename(&partial, dest)?;
        }

        Ok(())
    }

    /// Clean up failed partial transfer
    pub fn cleanup(&self, dest: &Path) -> io::Result<()> {
        let partial = self.partial_path(dest);

        if partial.exists() {
            if self.delay > 0 {
                std::thread::sleep(std::time::Duration::from_secs(self.delay as u64));
            }
            std::fs::remove_file(&partial)?;
        }

        Ok(())
    }
}
```

### Exclude Pattern Syntax

Pattern matching reference:

```rust
// Exclude pattern matching (crates/filters/)
use std::path::Path;

/// Pattern type for include/exclude rules
#[derive(Debug, Clone)]
pub enum Pattern {
    /// Simple glob pattern (e.g., "*.txt")
    Glob(String),
    /// Path-based pattern (e.g., "/foo/bar")
    Path(String),
    /// Directory pattern (trailing slash)
    Directory(String),
    /// Anchored pattern (starts with /)
    Anchored(String),
    /// Double-star pattern (e.g., "**/foo")
    DoubleStar(String),
    /// Negation pattern (starts with !)
    Negation(Box<Pattern>),
    /// Character class (e.g., "[abc]")
    CharClass(String),
}

impl Pattern {
    /// Parse pattern string into Pattern
    pub fn parse(s: &str) -> Self {
        let s = s.trim();

        // Check for negation
        if let Some(rest) = s.strip_prefix('!') {
            return Pattern::Negation(Box::new(Pattern::parse(rest)));
        }

        // Check for anchored pattern
        if s.starts_with('/') {
            if s.ends_with('/') {
                return Pattern::Directory(s.to_string());
            }
            return Pattern::Anchored(s.to_string());
        }

        // Check for double-star
        if s.contains("**") {
            return Pattern::DoubleStar(s.to_string());
        }

        // Check for directory-only
        if s.ends_with('/') {
            return Pattern::Directory(s.to_string());
        }

        // Check for character class
        if s.contains('[') && s.contains(']') {
            return Pattern::CharClass(s.to_string());
        }

        // Simple glob or path
        if s.contains('/') {
            Pattern::Path(s.to_string())
        } else {
            Pattern::Glob(s.to_string())
        }
    }

    /// Match pattern against path
    pub fn matches(&self, path: &Path, is_dir: bool) -> bool {
        let path_str = path.to_string_lossy();

        match self {
            Pattern::Glob(pat) => {
                glob_match(pat, path.file_name()
                    .map(|n| n.to_string_lossy().as_ref())
                    .unwrap_or(""))
            }
            Pattern::Path(pat) => {
                glob_match(pat, &path_str)
            }
            Pattern::Directory(pat) => {
                if !is_dir {
                    return false;
                }
                let pat_trimmed = pat.trim_end_matches('/');
                glob_match(pat_trimmed, &path_str)
            }
            Pattern::Anchored(pat) => {
                let pat_trimmed = pat.trim_start_matches('/');
                glob_match(pat_trimmed, &path_str)
            }
            Pattern::DoubleStar(pat) => {
                double_star_match(pat, &path_str)
            }
            Pattern::Negation(inner) => {
                !inner.matches(path, is_dir)
            }
            Pattern::CharClass(pat) => {
                glob_match(pat, &path_str)
            }
        }
    }
}

/// Simple glob matching
fn glob_match(pattern: &str, text: &str) -> bool {
    let mut pat_chars = pattern.chars().peekable();
    let mut text_chars = text.chars().peekable();

    while let Some(p) = pat_chars.next() {
        match p {
            '*' => {
                // Match zero or more characters
                if pat_chars.peek().is_none() {
                    return true;
                }

                // Try matching at each position
                while text_chars.peek().is_some() {
                    let remaining_pat: String = pat_chars.clone().collect();
                    let remaining_text: String = text_chars.clone().collect();
                    if glob_match(&remaining_pat, &remaining_text) {
                        return true;
                    }
                    text_chars.next();
                }
                return false;
            }
            '?' => {
                // Match exactly one character
                if text_chars.next().is_none() {
                    return false;
                }
            }
            '[' => {
                // Character class
                let t = match text_chars.next() {
                    Some(c) => c,
                    None => return false,
                };

                let mut matched = false;
                let mut negated = false;
                let mut first = true;

                while let Some(c) = pat_chars.next() {
                    if c == ']' && !first {
                        break;
                    }
                    if c == '!' && first {
                        negated = true;
                        first = false;
                        continue;
                    }
                    first = false;

                    // Check for range
                    if pat_chars.peek() == Some(&'-') {
                        pat_chars.next();
                        if let Some(end) = pat_chars.next() {
                            if t >= c && t <= end {
                                matched = true;
                            }
                        }
                    } else if t == c {
                        matched = true;
                    }
                }

                if negated {
                    matched = !matched;
                }
                if !matched {
                    return false;
                }
            }
            c => {
                // Literal character
                match text_chars.next() {
                    Some(t) if t == c => {}
                    _ => return false,
                }
            }
        }
    }

    // Pattern exhausted, text should be too
    text_chars.next().is_none()
}

/// Double-star matching (matches across directories)
fn double_star_match(pattern: &str, text: &str) -> bool {
    // Split on ** and match segments
    let parts: Vec<&str> = pattern.split("**").collect();

    if parts.len() == 1 {
        return glob_match(pattern, text);
    }

    let mut pos = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        let part = part.trim_matches('/');

        if i == 0 {
            // First part must match at start
            if !text.starts_with(part) {
                return false;
            }
            pos = part.len();
        } else if i == parts.len() - 1 {
            // Last part must match at end
            if !text[pos..].ends_with(part) {
                return false;
            }
        } else {
            // Middle parts can match anywhere
            if let Some(idx) = text[pos..].find(part) {
                pos += idx + part.len();
            } else {
                return false;
            }
        }
    }

    true
}
```

### Inplace Update Mode

Update files in place without temp files:

```rust
// Inplace update (crates/engine/)
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write, Seek, SeekFrom};
use std::path::Path;

/// Inplace file updater
pub struct InplaceUpdater {
    file: File,
    position: u64,
    original_size: u64,
}

impl InplaceUpdater {
    /// Open file for inplace update
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;

        let metadata = file.metadata()?;
        let original_size = metadata.len();

        Ok(Self {
            file,
            position: 0,
            original_size,
        })
    }

    /// Read from current position
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read(buf)
    }

    /// Write at current position
    pub fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(data)?;
        self.position = offset + data.len() as u64;
        Ok(())
    }

    /// Apply delta token
    pub fn apply_delta(&mut self, token: &DeltaToken) -> io::Result<()> {
        match token {
            DeltaToken::Copy { offset, length } => {
                // For inplace, we may need to buffer if source/dest overlap
                let dest_start = self.position;
                let src_start = *offset;
                let len = *length as u64;

                if Self::ranges_overlap(src_start, len, dest_start, len) {
                    // Overlapping: need to buffer
                    let mut buf = vec![0u8; *length as usize];
                    self.read_at(src_start, &mut buf)?;
                    self.write_at(dest_start, &buf)?;
                } else {
                    // Non-overlapping: direct copy
                    let mut buf = vec![0u8; 8192.min(*length as usize)];
                    let mut remaining = *length as usize;
                    let mut src_pos = src_start;

                    while remaining > 0 {
                        let to_read = remaining.min(buf.len());
                        let n = self.read_at(src_pos, &mut buf[..to_read])?;
                        self.write_at(self.position, &buf[..n])?;
                        src_pos += n as u64;
                        remaining -= n;
                    }
                }
            }
            DeltaToken::Literal(data) => {
                self.write_at(self.position, data)?;
            }
        }
        Ok(())
    }

    /// Finalize: truncate or extend file to final size
    pub fn finalize(self, final_size: u64) -> io::Result<()> {
        self.file.set_len(final_size)?;
        self.file.sync_all()
    }

    fn ranges_overlap(a_start: u64, a_len: u64, b_start: u64, b_len: u64) -> bool {
        let a_end = a_start + a_len;
        let b_end = b_start + b_len;
        a_start < b_end && b_start < a_end
    }
}
```

### Delete Modes Implementation

Various deletion strategies:

```rust
// Delete modes (crates/engine/)
use std::path::Path;
use std::collections::HashSet;

/// Delete mode configuration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    /// No deletion
    None,
    /// Delete before transfer (--delete-before)
    Before,
    /// Delete during transfer (--delete-during, default for --delete)
    During,
    /// Delete after transfer (--delete-after)
    After,
    /// Delete excluded files (--delete-excluded)
    Excluded,
    /// Delete only on receiving side (--delete-delay)
    Delay,
}

/// Deletion manager
pub struct DeleteManager {
    mode: DeleteMode,
    max_delete: Option<u32>,
    deleted_count: u32,
    ignore_errors: bool,
    force: bool,
    excluded_files: HashSet<String>,
}

impl DeleteManager {
    pub fn new(mode: DeleteMode) -> Self {
        Self {
            mode,
            max_delete: None,
            deleted_count: 0,
            ignore_errors: false,
            force: false,
            excluded_files: HashSet::new(),
        }
    }

    pub fn max_delete(mut self, max: u32) -> Self {
        self.max_delete = Some(max);
        self
    }

    pub fn force(mut self, value: bool) -> Self {
        self.force = value;
        self
    }

    /// Check if we've reached max delete limit
    fn at_limit(&self) -> bool {
        self.max_delete.map(|m| self.deleted_count >= m).unwrap_or(false)
    }

    /// Delete files not in source list
    pub fn delete_extraneous(
        &mut self,
        dest_dir: &Path,
        source_files: &HashSet<String>,
    ) -> io::Result<Vec<PathBuf>> {
        let mut deleted = Vec::new();

        for entry in walkdir::WalkDir::new(dest_dir)
            .min_depth(1)
            .contents_first(true)  // Delete contents before directories
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) if self.ignore_errors => continue,
                Err(e) => return Err(e.into()),
            };

            let rel_path = entry.path()
                .strip_prefix(dest_dir)
                .unwrap()
                .to_string_lossy()
                .into_owned();

            // Skip if in source
            if source_files.contains(&rel_path) {
                continue;
            }

            // Check for excluded (--delete-excluded)
            if self.mode != DeleteMode::Excluded
                && self.excluded_files.contains(&rel_path)
            {
                continue;
            }

            // Check max delete limit
            if self.at_limit() {
                break;
            }

            // Delete
            let path = entry.path();
            let result = if path.is_dir() {
                std::fs::remove_dir(path)
            } else {
                std::fs::remove_file(path)
            };

            match result {
                Ok(()) => {
                    self.deleted_count += 1;
                    deleted.push(path.to_path_buf());
                }
                Err(_) if self.ignore_errors => continue,
                Err(e) => return Err(e),
            }
        }

        Ok(deleted)
    }
}

use walkdir;
use std::path::PathBuf;
use std::io;
```

### Itemize Changes Output

Detailed change reporting:

```rust
// Itemize changes (crates/logging/)

/// Itemize change flags (matches rsync output format)
#[derive(Debug, Clone, Default)]
pub struct ItemizeFlags {
    /// Update type: < sent, > received, c created, . unchanged
    pub update_type: char,
    /// File type: f file, d directory, L symlink, D device, S special
    pub file_type: char,
    /// Checksum differs
    pub checksum: bool,
    /// Size changed
    pub size: bool,
    /// Modification time changed
    pub mtime: bool,
    /// Permissions changed
    pub perms: bool,
    /// Owner changed
    pub owner: bool,
    /// Group changed
    pub group: bool,
    /// ACL changed
    pub acl: bool,
    /// Extended attributes changed
    pub xattr: bool,
}

impl ItemizeFlags {
    /// Create for new file
    pub fn new_file(file_type: char) -> Self {
        Self {
            update_type: '>',
            file_type,
            checksum: true,
            size: true,
            mtime: true,
            perms: true,
            owner: true,
            group: true,
            ..Default::default()
        }
    }

    /// Create for unchanged file
    pub fn unchanged(file_type: char) -> Self {
        Self {
            update_type: '.',
            file_type,
            ..Default::default()
        }
    }

    /// Format as rsync-style itemize string
    pub fn format(&self) -> String {
        format!(
            "{}{}{}{}{}{}{}{}{}{}{}",
            self.update_type,
            self.file_type,
            if self.checksum { 'c' } else { '.' },
            if self.size { 's' } else { '.' },
            if self.mtime { 't' } else { '.' },
            if self.perms { 'p' } else { '.' },
            if self.owner { 'o' } else { '.' },
            if self.group { 'g' } else { '.' },
            '.', // reserved
            if self.acl { 'a' } else { '.' },
            if self.xattr { 'x' } else { '.' },
        )
    }

    /// Parse from rsync-style string
    pub fn parse(s: &str) -> Option<Self> {
        let chars: Vec<char> = s.chars().collect();
        if chars.len() < 11 {
            return None;
        }

        Some(Self {
            update_type: chars[0],
            file_type: chars[1],
            checksum: chars[2] == 'c',
            size: chars[3] == 's',
            mtime: chars[4] == 't',
            perms: chars[5] == 'p',
            owner: chars[6] == 'o',
            group: chars[7] == 'g',
            acl: chars[9] == 'a',
            xattr: chars[10] == 'x',
        })
    }
}

/// Compare source and destination for itemize output
pub fn compute_itemize(
    src: &FileEntry,
    dst: Option<&FileEntry>,
    is_new: bool,
    checksum_differs: bool,
) -> ItemizeFlags {
    let file_type = match src.file_type {
        FileType::Regular => 'f',
        FileType::Directory => 'd',
        FileType::Symlink => 'L',
        FileType::Device => 'D',
        FileType::Special => 'S',
    };

    if is_new || dst.is_none() {
        return ItemizeFlags::new_file(file_type);
    }

    let dst = dst.unwrap();

    ItemizeFlags {
        update_type: if checksum_differs { '>' } else { '.' },
        file_type,
        checksum: checksum_differs,
        size: src.size != dst.size,
        mtime: src.mtime != dst.mtime,
        perms: src.mode != dst.mode,
        owner: src.uid != dst.uid,
        group: src.gid != dst.gid,
        acl: false,  // Set by ACL comparison
        xattr: false,  // Set by xattr comparison
    }
}
```

### Fuzzy Matching for Basis Files

Find similar files for better delta:

```rust
// Fuzzy matching (crates/engine/)
use std::path::{Path, PathBuf};
use std::collections::HashMap;

/// Fuzzy basis file finder
pub struct FuzzyMatcher {
    /// Map from file size to paths
    size_index: HashMap<u64, Vec<PathBuf>>,
    /// Size tolerance for matching
    size_tolerance: f64,
}

impl FuzzyMatcher {
    pub fn new() -> Self {
        Self {
            size_index: HashMap::new(),
            size_tolerance: 0.1,  // 10% size difference allowed
        }
    }

    /// Build index from destination files
    pub fn build_index(&mut self, dest_dir: &Path) -> io::Result<()> {
        for entry in walkdir::WalkDir::new(dest_dir)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            if let Ok(metadata) = entry.metadata() {
                let size = metadata.len();
                self.size_index
                    .entry(size)
                    .or_default()
                    .push(entry.path().to_path_buf());
            }
        }
        Ok(())
    }

    /// Find best basis file for source
    pub fn find_basis(&self, src: &FileEntry, dest_dir: &Path) -> Option<PathBuf> {
        let target_size = src.size;

        // First check exact size matches
        if let Some(candidates) = self.size_index.get(&target_size) {
            if let Some(best) = self.best_match(src, candidates) {
                return Some(best);
            }
        }

        // Check similar sizes
        let min_size = (target_size as f64 * (1.0 - self.size_tolerance)) as u64;
        let max_size = (target_size as f64 * (1.0 + self.size_tolerance)) as u64;

        let mut candidates = Vec::new();
        for (&size, paths) in &self.size_index {
            if size >= min_size && size <= max_size {
                candidates.extend(paths.iter().cloned());
            }
        }

        self.best_match(src, &candidates)
    }

    /// Select best match from candidates
    fn best_match(&self, src: &FileEntry, candidates: &[PathBuf]) -> Option<PathBuf> {
        if candidates.is_empty() {
            return None;
        }

        // Score each candidate
        let src_name = src.path.file_name()?.to_string_lossy();

        let mut best: Option<(PathBuf, u32)> = None;

        for candidate in candidates {
            let cand_name = candidate.file_name()?.to_string_lossy();

            let score = self.similarity_score(&src_name, &cand_name);

            match &best {
                None => best = Some((candidate.clone(), score)),
                Some((_, best_score)) if score > *best_score => {
                    best = Some((candidate.clone(), score));
                }
                _ => {}
            }
        }

        best.map(|(path, _)| path)
    }

    /// Compute similarity score between filenames
    fn similarity_score(&self, a: &str, b: &str) -> u32 {
        // Simple Levenshtein-based scoring
        let a_chars: Vec<char> = a.chars().collect();
        let b_chars: Vec<char> = b.chars().collect();

        let mut score = 0u32;

        // Bonus for matching extension
        if let (Some(a_ext), Some(b_ext)) = (
            a.rsplit('.').next(),
            b.rsplit('.').next(),
        ) {
            if a_ext == b_ext {
                score += 100;
            }
        }

        // Bonus for common prefix
        let common_prefix = a_chars.iter()
            .zip(b_chars.iter())
            .take_while(|(x, y)| x == y)
            .count();
        score += (common_prefix * 10) as u32;

        // Bonus for common suffix
        let common_suffix = a_chars.iter().rev()
            .zip(b_chars.iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        score += (common_suffix * 5) as u32;

        score
    }
}
```

### TransferOptions Configuration

Complete transfer configuration combining all option categories:

```rust
/// Complete transfer options configuration
#[derive(Debug, Clone, Default)]
pub struct TransferOptions {
    /// File selection options
    pub selection: SelectionOptions,
    /// Preservation options
    pub preservation: PreservationOptions,
    /// Transfer behavior options
    pub behavior: BehaviorOptions,
    /// Output and logging options
    pub output: OutputOptions,
    /// Network options
    pub network: NetworkOptions,
}

#[derive(Debug, Clone, Default)]
pub struct SelectionOptions {
    /// Recurse into directories
    pub recursive: bool,
    /// Follow symlinks in source
    pub copy_links: bool,
    /// Copy symlinks as symlinks
    pub links: bool,
    /// Skip files based on checksum, not mod-time/size
    pub checksum: bool,
    /// Only update files that are newer on sender
    pub update: bool,
    /// Skip files that match in size
    pub size_only: bool,
    /// Ignore files that exist on receiver
    pub ignore_existing: bool,
    /// Delete extraneous files from destination
    pub delete: DeleteMode,
    /// Maximum file size to transfer
    pub max_size: Option<u64>,
    /// Minimum file size to transfer
    pub min_size: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct BehaviorOptions {
    /// Number of retries for failed transfers
    pub retries: u32,
    /// Timeout for I/O operations (seconds)
    pub timeout: u32,
    /// Block size for delta algorithm
    pub block_size: Option<u32>,
    /// Dry run - don't make changes
    pub dry_run: bool,
    /// Use whole-file transfers (no delta)
    pub whole_file: bool,
    /// Update in place (risky)
    pub inplace: bool,
    /// Append data to shorter files
    pub append: bool,
    /// Append with verification
    pub append_verify: bool,
}

impl TransferOptions {
    /// Build from CLI arguments
    pub fn from_args(args: &Args) -> Self {
        Self {
            selection: SelectionOptions {
                recursive: args.recursive,
                copy_links: args.copy_links,
                links: args.links,
                checksum: args.checksum,
                update: args.update,
                size_only: args.size_only,
                ignore_existing: args.ignore_existing,
                delete: args.delete_mode(),
                max_size: args.max_size,
                min_size: args.min_size,
            },
            preservation: PreservationOptions::from_args(args),
            behavior: BehaviorOptions {
                retries: args.retries.unwrap_or(0),
                timeout: args.timeout.unwrap_or(0),
                block_size: args.block_size,
                dry_run: args.dry_run,
                whole_file: args.whole_file,
                inplace: args.inplace,
                append: args.append,
                append_verify: args.append_verify,
            },
            output: OutputOptions::from_args(args),
            network: NetworkOptions::from_args(args),
        }
    }

    /// Check if this is a whole-file transfer (no delta)
    pub fn is_whole_file(&self) -> bool {
        self.behavior.whole_file
    }

    /// Check if this is a local-only transfer
    pub fn is_local(&self) -> bool {
        self.network.transport == TransportType::Local
    }
}
```

### PreservationOptions Configuration

Options controlling which file attributes to preserve:

```rust
/// File attribute preservation options
#[derive(Debug, Clone, Default)]
pub struct PreservationOptions {
    /// Preserve modification times
    pub times: bool,
    /// Preserve modification times on directories
    pub omit_dir_times: bool,
    /// Preserve modification times on symlinks
    pub omit_link_times: bool,
    /// Preserve permissions
    pub perms: bool,
    /// Preserve owner
    pub owner: bool,
    /// Preserve group
    pub group: bool,
    /// Preserve device files (super-user only)
    pub devices: bool,
    /// Preserve special files
    pub specials: bool,
    /// Preserve ACLs (implies perms)
    pub acls: bool,
    /// Preserve extended attributes
    pub xattrs: bool,
    /// Preserve hard links
    pub hard_links: bool,
    /// Preserve sparse files as sparse
    pub sparse: bool,
    /// Set destination permissions using umask
    pub executability: bool,
    /// Preserve create times (macOS)
    pub crtimes: bool,
    /// Preserve file flags (BSD)
    pub fileflags: bool,
}

impl PreservationOptions {
    /// Archive mode: -rlptgoD
    pub fn archive() -> Self {
        Self {
            times: true,
            perms: true,
            owner: true,
            group: true,
            devices: true,
            specials: true,
            ..Default::default()
        }
    }

    /// From CLI -a/--archive flag
    pub fn from_args(args: &Args) -> Self {
        let base = if args.archive {
            Self::archive()
        } else {
            Self::default()
        };

        Self {
            times: args.times || base.times,
            omit_dir_times: args.omit_dir_times,
            omit_link_times: args.omit_link_times,
            perms: args.perms || args.acls || base.perms,
            owner: args.owner || base.owner,
            group: args.group || base.group,
            devices: args.devices || base.devices,
            specials: args.specials || base.specials,
            acls: args.acls,
            xattrs: args.xattrs,
            hard_links: args.hard_links,
            sparse: args.sparse,
            executability: args.executability,
            crtimes: args.crtimes,
            fileflags: args.fileflags,
            ..base
        }
    }

    /// Check if any extended metadata preservation is enabled
    pub fn has_extended_metadata(&self) -> bool {
        self.acls || self.xattrs || self.crtimes || self.fileflags
    }

    /// Get the compat flags needed for this preservation config
    pub fn required_compat_flags(&self) -> CompatibilityFlags {
        let mut flags = CompatibilityFlags::empty();

        if self.acls {
            flags |= CompatibilityFlags::ACL_SUPPORT;
        }
        if self.xattrs {
            flags |= CompatibilityFlags::XATTR_SUPPORT;
        }
        if self.hard_links {
            flags |= CompatibilityFlags::HARDLINK_SPECIAL;
        }

        flags
    }
}
```

### I/O Timeout Configuration

Timeout handling for network and file operations:

```rust
use std::time::{Duration, Instant};

/// I/O timeout configuration
#[derive(Debug, Clone)]
pub struct IoTimeoutConfig {
    /// Overall transfer timeout (0 = no timeout)
    pub timeout: Duration,
    /// Contimeout for initial connection
    pub contimeout: Duration,
    /// Select timeout for blocking I/O
    pub select_timeout: Duration,
    /// Deadline for current operation
    deadline: Option<Instant>,
}

impl Default for IoTimeoutConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::ZERO,
            contimeout: Duration::from_secs(60),
            select_timeout: Duration::from_secs(60),
            deadline: None,
        }
    }
}

impl IoTimeoutConfig {
    /// Create from seconds value (rsync --timeout=N)
    pub fn from_seconds(secs: u32) -> Self {
        Self {
            timeout: Duration::from_secs(secs as u64),
            ..Default::default()
        }
    }

    /// Start tracking timeout for an operation
    pub fn start_operation(&mut self) {
        if !self.timeout.is_zero() {
            self.deadline = Some(Instant::now() + self.timeout);
        }
    }

    /// Check if current operation has timed out
    pub fn is_timed_out(&self) -> bool {
        self.deadline.map_or(false, |d| Instant::now() >= d)
    }

    /// Get remaining time until timeout
    pub fn remaining(&self) -> Option<Duration> {
        self.deadline.map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// Reset the deadline (called after successful I/O)
    pub fn reset_deadline(&mut self) {
        if !self.timeout.is_zero() {
            self.deadline = Some(Instant::now() + self.timeout);
        }
    }
}

/// Wrapper for I/O operations with timeout
pub struct TimeoutReader<R> {
    inner: R,
    config: IoTimeoutConfig,
}

impl<R: Read> TimeoutReader<R> {
    pub fn new(inner: R, config: IoTimeoutConfig) -> Self {
        Self { inner, config }
    }
}

impl<R: Read> Read for TimeoutReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.config.is_timed_out() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "I/O timeout exceeded"
            ));
        }

        let result = self.inner.read(buf);

        if result.is_ok() {
            self.config.reset_deadline();
        }

        result
    }
}
```

### ModuleConfig Parsing

Configuration file parsing for daemon modules:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Daemon module configuration
#[derive(Debug, Clone)]
pub struct ModuleConfig {
    /// Module name (section header)
    pub name: String,
    /// Base path for this module
    pub path: PathBuf,
    /// Comment shown in module listing
    pub comment: String,
    /// Whether module is read-only
    pub read_only: bool,
    /// Whether to list this module
    pub list: bool,
    /// Authentication users (user:group format)
    pub auth_users: Vec<String>,
    /// Path to secrets file
    pub secrets_file: Option<PathBuf>,
    /// Allowed/denied hosts
    pub hosts_allow: Vec<String>,
    pub hosts_deny: Vec<String>,
    /// UID to run as
    pub uid: Option<String>,
    /// GID to run as
    pub gid: Option<String>,
    /// Use chroot
    pub use_chroot: bool,
    /// Maximum connections
    pub max_connections: u32,
    /// Lock file for connection counting
    pub lock_file: Option<PathBuf>,
    /// Pre/post transfer commands
    pub pre_xfer_exec: Option<String>,
    pub post_xfer_exec: Option<String>,
    /// Include/exclude patterns
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Raw key-value pairs for unknown options
    pub extra: HashMap<String, String>,
}

impl Default for ModuleConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            path: PathBuf::new(),
            comment: String::new(),
            read_only: true,  // Safe default
            list: true,
            auth_users: Vec::new(),
            secrets_file: None,
            hosts_allow: Vec::new(),
            hosts_deny: Vec::new(),
            uid: None,
            gid: None,
            use_chroot: true,  // Safe default
            max_connections: 0,  // Unlimited
            lock_file: None,
            pre_xfer_exec: None,
            post_xfer_exec: None,
            include: Vec::new(),
            exclude: Vec::new(),
            extra: HashMap::new(),
        }
    }
}

/// Parser for rsyncd.conf format
pub struct ConfigParser;

impl ConfigParser {
    /// Parse configuration file
    pub fn parse_file(path: &Path) -> io::Result<Vec<ModuleConfig>> {
        let content = std::fs::read_to_string(path)?;
        Self::parse(&content)
    }

    /// Parse configuration from string
    pub fn parse(content: &str) -> io::Result<Vec<ModuleConfig>> {
        let mut modules = Vec::new();
        let mut current: Option<ModuleConfig> = None;
        let mut globals = HashMap::new();

        for line in content.lines() {
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            // Section header
            if line.starts_with('[') && line.ends_with(']') {
                // Save previous module
                if let Some(module) = current.take() {
                    modules.push(module);
                }

                let name = &line[1..line.len() - 1];
                if name != "global" {
                    let mut module = ModuleConfig::default();
                    module.name = name.to_string();
                    // Apply globals as defaults
                    for (key, value) in &globals {
                        Self::apply_option(&mut module, key, value);
                    }
                    current = Some(module);
                }
                continue;
            }

            // Key = value
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim().to_lowercase();
                let value = value.trim();

                if let Some(ref mut module) = current {
                    Self::apply_option(module, &key, value);
                } else {
                    // Global section
                    globals.insert(key, value.to_string());
                }
            }
        }

        // Save last module
        if let Some(module) = current {
            modules.push(module);
        }

        Ok(modules)
    }

    fn apply_option(module: &mut ModuleConfig, key: &str, value: &str) {
        match key {
            "path" => module.path = PathBuf::from(value),
            "comment" => module.comment = value.to_string(),
            "read only" => module.read_only = Self::parse_bool(value),
            "list" => module.list = Self::parse_bool(value),
            "auth users" => {
                module.auth_users = value.split(',')
                    .map(|s| s.trim().to_string())
                    .collect();
            }
            "secrets file" => module.secrets_file = Some(PathBuf::from(value)),
            "hosts allow" => {
                module.hosts_allow = value.split_whitespace()
                    .map(String::from)
                    .collect();
            }
            "hosts deny" => {
                module.hosts_deny = value.split_whitespace()
                    .map(String::from)
                    .collect();
            }
            "uid" => module.uid = Some(value.to_string()),
            "gid" => module.gid = Some(value.to_string()),
            "use chroot" => module.use_chroot = Self::parse_bool(value),
            "max connections" => {
                module.max_connections = value.parse().unwrap_or(0);
            }
            "lock file" => module.lock_file = Some(PathBuf::from(value)),
            "pre-xfer exec" => module.pre_xfer_exec = Some(value.to_string()),
            "post-xfer exec" => module.post_xfer_exec = Some(value.to_string()),
            "include" => module.include.push(value.to_string()),
            "exclude" => module.exclude.push(value.to_string()),
            _ => {
                module.extra.insert(key.to_string(), value.to_string());
            }
        }
    }

    fn parse_bool(value: &str) -> bool {
        matches!(
            value.to_lowercase().as_str(),
            "yes" | "true" | "1" | "on"
        )
    }
}
```

### LogFormat String Templating

Output format string parsing matching rsync's --out-format:

```rust
/// Log format token types
#[derive(Debug, Clone, PartialEq)]
pub enum FormatToken {
    /// Literal text
    Literal(String),
    /// %i - itemize changes
    Itemize,
    /// %n - filename (short)
    Filename,
    /// %L - symlink target
    SymlinkTarget,
    /// %f - full path
    FullPath,
    /// %b - bytes transferred
    BytesTransferred,
    /// %l - file length
    FileLength,
    /// %c - checksum (MD5)
    Checksum,
    /// %C - checksum (full)
    ChecksumFull,
    /// %o - operation (send/recv/del)
    Operation,
    /// %p - process ID
    ProcessId,
    /// %t - current time
    CurrentTime,
    /// %M - modification time
    ModTime,
    /// %U - uid
    Uid,
    /// %G - gid
    Gid,
    /// %B - permission bits
    PermBits,
    /// Custom format with width: %10n
    WithWidth { token: Box<FormatToken>, width: usize },
}

/// Log format parser
pub struct LogFormat {
    tokens: Vec<FormatToken>,
}

impl LogFormat {
    /// Parse format string
    pub fn parse(format: &str) -> Self {
        let mut tokens = Vec::new();
        let mut chars = format.chars().peekable();
        let mut literal = String::new();

        while let Some(c) = chars.next() {
            if c == '%' {
                // Flush literal
                if !literal.is_empty() {
                    tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
                }

                // Parse width if present
                let mut width = 0usize;
                while let Some(&digit) = chars.peek() {
                    if digit.is_ascii_digit() {
                        width = width * 10 + digit.to_digit(10).unwrap() as usize;
                        chars.next();
                    } else {
                        break;
                    }
                }

                // Parse format specifier
                let token = match chars.next() {
                    Some('i') => FormatToken::Itemize,
                    Some('n') => FormatToken::Filename,
                    Some('L') => FormatToken::SymlinkTarget,
                    Some('f') => FormatToken::FullPath,
                    Some('b') => FormatToken::BytesTransferred,
                    Some('l') => FormatToken::FileLength,
                    Some('c') => FormatToken::Checksum,
                    Some('C') => FormatToken::ChecksumFull,
                    Some('o') => FormatToken::Operation,
                    Some('p') => FormatToken::ProcessId,
                    Some('t') => FormatToken::CurrentTime,
                    Some('M') => FormatToken::ModTime,
                    Some('U') => FormatToken::Uid,
                    Some('G') => FormatToken::Gid,
                    Some('B') => FormatToken::PermBits,
                    Some('%') => FormatToken::Literal("%".to_string()),
                    Some(other) => FormatToken::Literal(format!("%{}", other)),
                    None => break,
                };

                if width > 0 {
                    tokens.push(FormatToken::WithWidth {
                        token: Box::new(token),
                        width,
                    });
                } else {
                    tokens.push(token);
                }
            } else {
                literal.push(c);
            }
        }

        if !literal.is_empty() {
            tokens.push(FormatToken::Literal(literal));
        }

        Self { tokens }
    }

    /// Format a file entry
    pub fn format(&self, entry: &FileEntry, stats: &TransferStats) -> String {
        let mut result = String::new();

        for token in &self.tokens {
            match token {
                FormatToken::Literal(s) => result.push_str(s),
                FormatToken::Filename => result.push_str(&entry.name),
                FormatToken::FullPath => result.push_str(&entry.path.display().to_string()),
                FormatToken::FileLength => {
                    result.push_str(&entry.size.to_string());
                }
                FormatToken::BytesTransferred => {
                    result.push_str(&stats.bytes_sent.to_string());
                }
                FormatToken::Operation => {
                    let op = if stats.is_sender { "send" } else { "recv" };
                    result.push_str(op);
                }
                FormatToken::Itemize => {
                    result.push_str(&entry.itemize_string());
                }
                FormatToken::WithWidth { token, width } => {
                    let formatted = self.format_single(token, entry, stats);
                    result.push_str(&format!("{:>width$}", formatted, width = *width));
                }
                // ... other tokens
                _ => {}
            }
        }

        result
    }

    fn format_single(&self, token: &FormatToken, entry: &FileEntry, stats: &TransferStats) -> String {
        match token {
            FormatToken::Filename => entry.name.clone(),
            FormatToken::FileLength => entry.size.to_string(),
            FormatToken::BytesTransferred => stats.bytes_sent.to_string(),
            _ => String::new(),
        }
    }
}

impl Default for LogFormat {
    fn default() -> Self {
        // Default format: "%o %i %n%L"
        Self::parse("%o %i %n%L")
    }
}
```

### ChecksumSeed Management

Seed generation and management for delta checksums:

```rust
use std::time::{SystemTime, UNIX_EPOCH};

/// Checksum seed manager
#[derive(Debug, Clone)]
pub struct ChecksumSeed {
    /// The actual seed value
    value: i32,
    /// Whether seed was explicitly set
    explicit: bool,
}

impl ChecksumSeed {
    /// Create from explicit value (--checksum-seed=N)
    pub fn explicit(value: i32) -> Self {
        Self {
            value,
            explicit: true,
        }
    }

    /// Generate seed from current time
    pub fn generate() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        // Use lower 31 bits of timestamp (matching upstream)
        let value = (now.as_secs() as i32) & 0x7FFFFFFF;

        Self {
            value,
            explicit: false,
        }
    }

    /// Get the seed value
    pub fn value(&self) -> i32 {
        self.value
    }

    /// Check if seed was explicitly set
    pub fn is_explicit(&self) -> bool {
        self.explicit
    }

    /// Encode seed for protocol exchange
    pub fn encode(&self) -> [u8; 4] {
        self.value.to_le_bytes()
    }

    /// Decode seed from protocol exchange
    pub fn decode(bytes: &[u8; 4]) -> Self {
        Self {
            value: i32::from_le_bytes(*bytes),
            explicit: false,
        }
    }
}

impl Default for ChecksumSeed {
    fn default() -> Self {
        Self::generate()
    }
}

/// Checksum configuration with seed
#[derive(Debug, Clone)]
pub struct ChecksumConfig {
    /// Seed for rolling checksum
    pub seed: ChecksumSeed,
    /// Block size for delta algorithm
    pub block_size: u32,
    /// Strong checksum algorithm
    pub checksum_type: ChecksumType,
    /// Whether to use file-level checksums
    pub always_checksum: bool,
}

impl ChecksumConfig {
    /// Apply seed to rolling checksum computation
    pub fn seeded_rolling(&self, data: &[u8]) -> u32 {
        let mut sum = RollingChecksum::new();
        sum.update(data);

        // Mix in seed (matches upstream rsync)
        let raw = sum.digest();
        raw.wrapping_add(self.seed.value() as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumType {
    /// MD4 (protocol < 30)
    Md4,
    /// MD5 (protocol >= 30)
    Md5,
    /// XXHash (protocol >= 31 with negotiation)
    XxHash,
    /// XXHash3 (protocol >= 32)
    XxHash3,
}

impl ChecksumType {
    /// Select based on protocol version
    pub fn for_protocol(version: u8) -> Self {
        match version {
            0..=29 => Self::Md4,
            30 => Self::Md5,
            31 => Self::XxHash,
            _ => Self::XxHash3,
        }
    }

    /// Get digest length in bytes
    pub fn digest_len(&self) -> usize {
        match self {
            Self::Md4 | Self::Md5 => 16,
            Self::XxHash => 8,
            Self::XxHash3 => 8,
        }
    }
}
```

### CopyDest Basis File Handling

Using --copy-dest to find basis files for delta transfers:

```rust
use std::path::{Path, PathBuf};

/// Copy-dest basis file finder
#[derive(Debug)]
pub struct CopyDestFinder {
    /// List of copy-dest directories
    dirs: Vec<PathBuf>,
    /// Whether to create hardlinks (--link-dest behavior)
    link_dest: bool,
    /// Cache of found basis files
    cache: HashMap<PathBuf, Option<PathBuf>>,
}

impl CopyDestFinder {
    /// Create from --copy-dest arguments
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            link_dest: false,
            cache: HashMap::new(),
        }
    }

    /// Create from --link-dest arguments (will create hardlinks)
    pub fn link_dest(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            link_dest: true,
            cache: HashMap::new(),
        }
    }

    /// Find basis file for a target path
    pub fn find_basis(&mut self, relative_path: &Path) -> Option<PathBuf> {
        // Check cache first
        if let Some(cached) = self.cache.get(relative_path) {
            return cached.clone();
        }

        // Search in copy-dest directories
        let result = self.search_basis(relative_path);
        self.cache.insert(relative_path.to_path_buf(), result.clone());
        result
    }

    fn search_basis(&self, relative_path: &Path) -> Option<PathBuf> {
        for dir in &self.dirs {
            let candidate = dir.join(relative_path);

            if candidate.exists() {
                // For link-dest, verify the file is linkable
                if self.link_dest {
                    if let Ok(meta) = candidate.metadata() {
                        // Can only hardlink regular files
                        if !meta.is_file() {
                            continue;
                        }
                    }
                }
                return Some(candidate);
            }
        }
        None
    }

    /// Check if we should create a hardlink instead of copying
    pub fn should_hardlink(&self, source_meta: &Metadata, target_path: &Path) -> Option<PathBuf> {
        if !self.link_dest {
            return None;
        }

        // Find matching file in link-dest
        let relative = target_path.file_name()?;
        let basis = self.find_basis(Path::new(relative))?;

        // Verify metadata matches (size, mtime)
        let basis_meta = std::fs::metadata(&basis).ok()?;

        if basis_meta.len() == source_meta.len() {
            // mtime comparison with tolerance
            if let (Ok(basis_mtime), Ok(source_mtime)) =
                (basis_meta.modified(), source_meta.modified())
            {
                let diff = if basis_mtime > source_mtime {
                    basis_mtime.duration_since(source_mtime)
                } else {
                    source_mtime.duration_since(basis_mtime)
                };

                if diff.map_or(false, |d| d.as_secs() < 2) {
                    return Some(basis);
                }
            }
        }

        None
    }

    /// Create hardlink or copy based on mode
    pub fn apply_basis(
        &self,
        basis: &Path,
        target: &Path,
    ) -> io::Result<BasisResult> {
        if self.link_dest {
            // Try to create hardlink
            match std::fs::hard_link(basis, target) {
                Ok(()) => return Ok(BasisResult::Linked),
                Err(e) if e.kind() == io::ErrorKind::CrossesDevices => {
                    // Fall through to copy
                }
                Err(e) => return Err(e),
            }
        }

        // Use as basis for delta transfer
        Ok(BasisResult::UseBasis(basis.to_path_buf()))
    }
}

/// Result of applying a basis file
#[derive(Debug)]
pub enum BasisResult {
    /// Created hardlink, no transfer needed
    Linked,
    /// Use this file as basis for delta
    UseBasis(PathBuf),
    /// No basis found, full transfer needed
    NoBasis,
}
```

### SafePathBuilder Pattern

Secure path construction preventing directory traversal:

```rust
use std::path::{Component, Path, PathBuf};

/// Safe path builder preventing directory traversal attacks
#[derive(Debug)]
pub struct SafePathBuilder {
    /// Base directory (root for all operations)
    base: PathBuf,
    /// Whether to allow absolute paths
    allow_absolute: bool,
    /// Whether to allow parent directory references
    allow_parent: bool,
}

impl SafePathBuilder {
    /// Create builder anchored at base directory
    pub fn new(base: PathBuf) -> Self {
        Self {
            base,
            allow_absolute: false,
            allow_parent: false,
        }
    }

    /// Allow absolute paths (still confined to base)
    pub fn allow_absolute(mut self, allow: bool) -> Self {
        self.allow_absolute = allow;
        self
    }

    /// Build safe path from user input
    pub fn build(&self, user_path: &str) -> Result<PathBuf, PathError> {
        let path = Path::new(user_path);

        // Reject absolute paths unless allowed
        if path.is_absolute() && !self.allow_absolute {
            return Err(PathError::AbsolutePath);
        }

        // Normalize and validate components
        let mut result = self.base.clone();
        let mut depth = 0i32;

        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    // Check for hidden files masquerading as normal
                    let name_str = name.to_string_lossy();
                    if name_str.contains('\0') {
                        return Err(PathError::NullByte);
                    }
                    result.push(name);
                    depth += 1;
                }
                Component::ParentDir => {
                    if !self.allow_parent {
                        return Err(PathError::ParentReference);
                    }
                    depth -= 1;
                    if depth < 0 {
                        return Err(PathError::EscapesBase);
                    }
                    result.pop();
                }
                Component::CurDir => {
                    // Skip "."
                }
                Component::RootDir | Component::Prefix(_) => {
                    if !self.allow_absolute {
                        return Err(PathError::AbsolutePath);
                    }
                    // For absolute paths, restart from base
                    result = self.base.clone();
                    depth = 0;
                }
            }
        }

        // Final validation: ensure result is under base
        if !result.starts_with(&self.base) {
            return Err(PathError::EscapesBase);
        }

        Ok(result)
    }

    /// Validate path without building
    pub fn validate(&self, user_path: &str) -> Result<(), PathError> {
        self.build(user_path).map(|_| ())
    }

    /// Join safely with existing path
    pub fn join(&self, base: &Path, relative: &str) -> Result<PathBuf, PathError> {
        // Ensure base is under our root
        if !base.starts_with(&self.base) {
            return Err(PathError::EscapesBase);
        }

        // Build relative path
        let relative_path = self.build(relative)?;

        // Combine paths
        let combined = base.join(
            relative_path.strip_prefix(&self.base).unwrap_or(&relative_path)
        );

        // Revalidate combined path
        if !combined.starts_with(&self.base) {
            return Err(PathError::EscapesBase);
        }

        Ok(combined)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// Absolute path when not allowed
    AbsolutePath,
    /// Parent directory reference (..) when not allowed
    ParentReference,
    /// Path escapes base directory
    EscapesBase,
    /// Null byte in path
    NullByte,
    /// Invalid UTF-8 in path
    InvalidUtf8,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AbsolutePath => write!(f, "absolute path not allowed"),
            Self::ParentReference => write!(f, "parent directory reference not allowed"),
            Self::EscapesBase => write!(f, "path escapes base directory"),
            Self::NullByte => write!(f, "null byte in path"),
            Self::InvalidUtf8 => write!(f, "invalid UTF-8 in path"),
        }
    }
}

impl std::error::Error for PathError {}
```

### WireReader/WireWriter Traits

Protocol-aware wire encoding traits:

```rust
use std::io::{self, Read, Write};

/// Wire protocol reader trait
pub trait WireReader: Read {
    /// Read a byte
    fn read_byte(&mut self) -> io::Result<u8> {
        let mut buf = [0u8; 1];
        self.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    /// Read little-endian i32
    fn read_i32_le(&mut self) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf))
    }

    /// Read little-endian u32
    fn read_u32_le(&mut self) -> io::Result<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    /// Read little-endian i64
    fn read_i64_le(&mut self) -> io::Result<i64> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf)?;
        Ok(i64::from_le_bytes(buf))
    }

    /// Read varint (7-bit variable length encoding)
    fn read_varint(&mut self) -> io::Result<u64> {
        let mut result = 0u64;
        let mut shift = 0u32;

        loop {
            let byte = self.read_byte()?;
            result |= ((byte & 0x7F) as u64) << shift;

            if byte & 0x80 == 0 {
                break;
            }

            shift += 7;
            if shift >= 64 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "varint overflow"
                ));
            }
        }

        Ok(result)
    }

    /// Read NDX (file index) based on protocol version
    fn read_ndx(&mut self, protocol: u8) -> io::Result<i32> {
        if protocol >= 30 {
            // Delta-encoded NDX
            self.read_varint().map(|v| v as i32)
        } else {
            // Fixed 4-byte encoding
            self.read_i32_le()
        }
    }

    /// Read length-prefixed bytes
    fn read_bytes(&mut self) -> io::Result<Vec<u8>> {
        let len = self.read_varint()? as usize;
        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Read length-prefixed string
    fn read_string(&mut self) -> io::Result<String> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e)
        })
    }
}

/// Wire protocol writer trait
pub trait WireWriter: Write {
    /// Write a byte
    fn write_byte(&mut self, b: u8) -> io::Result<()> {
        self.write_all(&[b])
    }

    /// Write little-endian i32
    fn write_i32_le(&mut self, v: i32) -> io::Result<()> {
        self.write_all(&v.to_le_bytes())
    }

    /// Write little-endian u32
    fn write_u32_le(&mut self, v: u32) -> io::Result<()> {
        self.write_all(&v.to_le_bytes())
    }

    /// Write little-endian i64
    fn write_i64_le(&mut self, v: i64) -> io::Result<()> {
        self.write_all(&v.to_le_bytes())
    }

    /// Write varint (7-bit variable length encoding)
    fn write_varint(&mut self, mut value: u64) -> io::Result<()> {
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;

            if value != 0 {
                byte |= 0x80;
            }

            self.write_byte(byte)?;

            if value == 0 {
                break;
            }
        }
        Ok(())
    }

    /// Write NDX (file index) based on protocol version
    fn write_ndx(&mut self, ndx: i32, protocol: u8) -> io::Result<()> {
        if protocol >= 30 {
            // Delta-encoded NDX
            self.write_varint(ndx as u64)
        } else {
            // Fixed 4-byte encoding
            self.write_i32_le(ndx)
        }
    }

    /// Write length-prefixed bytes
    fn write_bytes(&mut self, data: &[u8]) -> io::Result<()> {
        self.write_varint(data.len() as u64)?;
        self.write_all(data)
    }

    /// Write length-prefixed string
    fn write_string(&mut self, s: &str) -> io::Result<()> {
        self.write_bytes(s.as_bytes())
    }
}

// Blanket implementations
impl<R: Read> WireReader for R {}
impl<W: Write> WireWriter for W {}
```

### BlockAllocator Pattern

Memory-efficient block allocation for delta processing:

```rust
use std::sync::Arc;

/// Block allocator for delta processing
#[derive(Debug)]
pub struct BlockAllocator {
    /// Block size in bytes
    block_size: usize,
    /// Pool of reusable blocks
    pool: Vec<Vec<u8>>,
    /// Maximum pool size
    max_pool_size: usize,
    /// Statistics
    stats: AllocatorStats,
}

#[derive(Debug, Default)]
pub struct AllocatorStats {
    pub allocations: u64,
    pub deallocations: u64,
    pub pool_hits: u64,
    pub pool_misses: u64,
    pub bytes_allocated: u64,
}

impl BlockAllocator {
    /// Create allocator with given block size
    pub fn new(block_size: usize) -> Self {
        Self {
            block_size,
            pool: Vec::new(),
            max_pool_size: 64,
            stats: AllocatorStats::default(),
        }
    }

    /// Allocate a block
    pub fn allocate(&mut self) -> Block {
        self.stats.allocations += 1;

        let data = if let Some(mut block) = self.pool.pop() {
            self.stats.pool_hits += 1;
            block.clear();
            block.resize(self.block_size, 0);
            block
        } else {
            self.stats.pool_misses += 1;
            self.stats.bytes_allocated += self.block_size as u64;
            vec![0u8; self.block_size]
        };

        Block {
            data,
            size: 0,
        }
    }

    /// Return block to pool
    pub fn deallocate(&mut self, block: Block) {
        self.stats.deallocations += 1;

        if self.pool.len() < self.max_pool_size {
            self.pool.push(block.data);
        }
    }

    /// Get allocator statistics
    pub fn stats(&self) -> &AllocatorStats {
        &self.stats
    }

    /// Pre-allocate pool
    pub fn preallocate(&mut self, count: usize) {
        let to_alloc = count.min(self.max_pool_size).saturating_sub(self.pool.len());

        for _ in 0..to_alloc {
            self.pool.push(vec![0u8; self.block_size]);
            self.stats.bytes_allocated += self.block_size as u64;
        }
    }
}

/// A block from the allocator
#[derive(Debug)]
pub struct Block {
    data: Vec<u8>,
    /// Actual used size (may be less than capacity)
    size: usize,
}

impl Block {
    /// Get block data
    pub fn data(&self) -> &[u8] {
        &self.data[..self.size]
    }

    /// Get mutable block data
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data[..self.size]
    }

    /// Set used size
    pub fn set_size(&mut self, size: usize) {
        self.size = size.min(self.data.len());
    }

    /// Get capacity
    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    /// Fill from reader
    pub fn fill_from<R: Read>(&mut self, reader: &mut R) -> io::Result<usize> {
        let n = reader.read(&mut self.data)?;
        self.size = n;
        Ok(n)
    }
}

impl std::ops::Deref for Block {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.data()
    }
}

impl std::ops::DerefMut for Block {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data_mut()
    }
}
```

### TransferStatistics Tracking

Comprehensive transfer statistics matching rsync output:

```rust
use std::time::{Duration, Instant};

/// Transfer statistics
#[derive(Debug, Clone, Default)]
pub struct TransferStatistics {
    /// Total bytes sent
    pub bytes_sent: u64,
    /// Total bytes received
    pub bytes_received: u64,
    /// Literal data bytes (uncompressed)
    pub literal_data: u64,
    /// Matched data bytes (from basis)
    pub matched_data: u64,
    /// Number of files transferred
    pub files_transferred: u64,
    /// Number of files total (including skipped)
    pub files_total: u64,
    /// Number of files created
    pub files_created: u64,
    /// Number of files deleted
    pub files_deleted: u64,
    /// Number of directories created
    pub dirs_created: u64,
    /// Number of symlinks transferred
    pub symlinks: u64,
    /// Number of devices transferred
    pub devices: u64,
    /// Number of specials transferred
    pub specials: u64,
    /// Total file size
    pub total_size: u64,
    /// Transfer start time
    start_time: Option<Instant>,
    /// Transfer end time
    end_time: Option<Instant>,
}

impl TransferStatistics {
    /// Start timing
    pub fn start(&mut self) {
        self.start_time = Some(Instant::now());
    }

    /// Stop timing
    pub fn stop(&mut self) {
        self.end_time = Some(Instant::now());
    }

    /// Get elapsed time
    pub fn elapsed(&self) -> Duration {
        match (self.start_time, self.end_time) {
            (Some(start), Some(end)) => end.duration_since(start),
            (Some(start), None) => start.elapsed(),
            _ => Duration::ZERO,
        }
    }

    /// Calculate transfer rate (bytes/sec)
    pub fn transfer_rate(&self) -> f64 {
        let secs = self.elapsed().as_secs_f64();
        if secs > 0.0 {
            self.bytes_sent as f64 / secs
        } else {
            0.0
        }
    }

    /// Calculate speedup ratio
    pub fn speedup(&self) -> f64 {
        let transferred = self.literal_data + self.matched_data;
        if transferred > 0 {
            self.total_size as f64 / transferred as f64
        } else {
            0.0
        }
    }

    /// Format as rsync-style summary
    pub fn format_summary(&self) -> String {
        let elapsed = self.elapsed();
        let rate = self.transfer_rate();

        format!(
            r#"
Number of files: {} (reg: {}, dir: {}, link: {}, dev: {}, special: {})
Number of created files: {}
Number of deleted files: {}
Number of regular files transferred: {}
Total file size: {} bytes
Total transferred file size: {} bytes
Literal data: {} bytes
Matched data: {} bytes
File list size: unknown
Total bytes sent: {}
Total bytes received: {}
sent {} bytes  received {} bytes  {:.2} bytes/sec
total size is {}  speedup is {:.2}
"#,
            self.files_total,
            self.files_transferred,
            self.dirs_created,
            self.symlinks,
            self.devices,
            self.specials,
            self.files_created,
            self.files_deleted,
            self.files_transferred,
            self.total_size,
            self.literal_data + self.matched_data,
            self.literal_data,
            self.matched_data,
            self.bytes_sent,
            self.bytes_received,
            self.bytes_sent,
            self.bytes_received,
            rate,
            self.total_size,
            self.speedup(),
        )
    }

    /// Merge statistics from another instance
    pub fn merge(&mut self, other: &TransferStatistics) {
        self.bytes_sent += other.bytes_sent;
        self.bytes_received += other.bytes_received;
        self.literal_data += other.literal_data;
        self.matched_data += other.matched_data;
        self.files_transferred += other.files_transferred;
        self.files_total += other.files_total;
        self.files_created += other.files_created;
        self.files_deleted += other.files_deleted;
        self.dirs_created += other.dirs_created;
        self.symlinks += other.symlinks;
        self.devices += other.devices;
        self.specials += other.specials;
        self.total_size += other.total_size;
    }
}
```

### SocketOptions Configuration

TCP socket configuration for daemon connections:

```rust
use std::net::TcpStream;
use std::time::Duration;

/// Socket configuration options
#[derive(Debug, Clone)]
pub struct SocketOptions {
    /// TCP keep-alive
    pub keepalive: Option<Duration>,
    /// TCP nodelay (disable Nagle)
    pub nodelay: bool,
    /// Send buffer size
    pub send_buffer: Option<usize>,
    /// Receive buffer size
    pub recv_buffer: Option<usize>,
    /// Read timeout
    pub read_timeout: Option<Duration>,
    /// Write timeout
    pub write_timeout: Option<Duration>,
    /// Linger on close
    pub linger: Option<Duration>,
    /// Reuse address
    pub reuse_address: bool,
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            keepalive: Some(Duration::from_secs(60)),
            nodelay: true,  // rsync uses nodelay
            send_buffer: None,
            recv_buffer: None,
            read_timeout: None,
            write_timeout: None,
            linger: None,
            reuse_address: true,
        }
    }
}

impl SocketOptions {
    /// Apply options to a TCP stream
    pub fn apply(&self, stream: &TcpStream) -> io::Result<()> {
        // TCP nodelay
        stream.set_nodelay(self.nodelay)?;

        // Read/write timeouts
        stream.set_read_timeout(self.read_timeout)?;
        stream.set_write_timeout(self.write_timeout)?;

        // Platform-specific options
        #[cfg(unix)]
        self.apply_unix(stream)?;

        Ok(())
    }

    #[cfg(unix)]
    fn apply_unix(&self, stream: &TcpStream) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        let fd = stream.as_raw_fd();

        // Keep-alive
        if let Some(keepalive) = self.keepalive {
            unsafe {
                let optval: libc::c_int = 1;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_KEEPALIVE,
                    &optval as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );

                // Keep-alive interval
                let interval = keepalive.as_secs() as libc::c_int;
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_TCP,
                    libc::TCP_KEEPIDLE,
                    &interval as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        // Send buffer
        if let Some(size) = self.send_buffer {
            unsafe {
                let size = size as libc::c_int;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        // Receive buffer
        if let Some(size) = self.recv_buffer {
            unsafe {
                let size = size as libc::c_int;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        // Linger
        if let Some(linger_time) = self.linger {
            unsafe {
                let linger = libc::linger {
                    l_onoff: 1,
                    l_linger: linger_time.as_secs() as libc::c_int,
                };
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    &linger as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                );
            }
        }

        Ok(())
    }

    /// Builder pattern
    pub fn with_keepalive(mut self, duration: Duration) -> Self {
        self.keepalive = Some(duration);
        self
    }

    pub fn with_timeout(mut self, duration: Duration) -> Self {
        self.read_timeout = Some(duration);
        self.write_timeout = Some(duration);
        self
    }

    pub fn with_buffer_sizes(mut self, send: usize, recv: usize) -> Self {
        self.send_buffer = Some(send);
        self.recv_buffer = Some(recv);
        self
    }
}
```

### CompareDest File Comparison

Using --compare-dest to skip unchanged files without transfer:

```rust
use std::path::{Path, PathBuf};
use std::fs::Metadata;

/// Compare-dest directory handler
#[derive(Debug)]
pub struct CompareDestHandler {
    /// List of compare-dest directories
    dirs: Vec<PathBuf>,
    /// Cache of comparison results
    cache: HashMap<PathBuf, CompareResult>,
}

#[derive(Debug, Clone)]
pub enum CompareResult {
    /// File matches in compare-dest, skip transfer
    Match(PathBuf),
    /// File differs, needs transfer
    Differs,
    /// File not found in any compare-dest
    NotFound,
}

impl CompareDestHandler {
    /// Create from --compare-dest arguments
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            cache: HashMap::new(),
        }
    }

    /// Check if file matches any compare-dest
    pub fn check(&mut self, relative_path: &Path, source_meta: &Metadata) -> CompareResult {
        // Check cache first
        if let Some(result) = self.cache.get(relative_path) {
            return result.clone();
        }

        let result = self.find_match(relative_path, source_meta);
        self.cache.insert(relative_path.to_path_buf(), result.clone());
        result
    }

    fn find_match(&self, relative_path: &Path, source_meta: &Metadata) -> CompareResult {
        for dir in &self.dirs {
            let candidate = dir.join(relative_path);

            if let Ok(dest_meta) = std::fs::metadata(&candidate) {
                if self.files_match(source_meta, &dest_meta) {
                    return CompareResult::Match(candidate);
                } else {
                    return CompareResult::Differs;
                }
            }
        }
        CompareResult::NotFound
    }

    fn files_match(&self, source: &Metadata, dest: &Metadata) -> bool {
        // Same size
        if source.len() != dest.len() {
            return false;
        }

        // Same mtime (with 2-second tolerance for FAT filesystems)
        if let (Ok(src_mtime), Ok(dst_mtime)) = (source.modified(), dest.modified()) {
            let diff = if src_mtime > dst_mtime {
                src_mtime.duration_since(dst_mtime)
            } else {
                dst_mtime.duration_since(src_mtime)
            };

            if diff.map_or(true, |d| d.as_secs() >= 2) {
                return false;
            }
        }

        true
    }

    /// Decide transfer action based on compare result
    pub fn transfer_action(&mut self, path: &Path, meta: &Metadata) -> TransferAction {
        match self.check(path, meta) {
            CompareResult::Match(_) => TransferAction::Skip,
            CompareResult::Differs => TransferAction::Transfer,
            CompareResult::NotFound => TransferAction::Transfer,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferAction {
    /// Skip file, matches compare-dest
    Skip,
    /// Transfer file
    Transfer,
}
```

### MaxDelete Limit Enforcement

Enforcing --max-delete limits during transfer:

```rust
use std::sync::atomic::{AtomicU64, Ordering};

/// Max delete limit enforcer
#[derive(Debug)]
pub struct MaxDeleteLimiter {
    /// Maximum deletions allowed (0 = unlimited)
    limit: u64,
    /// Current deletion count
    count: AtomicU64,
    /// Whether limit has been reached
    limit_reached: AtomicBool,
}

use std::sync::atomic::AtomicBool;

impl MaxDeleteLimiter {
    /// Create with limit (0 = unlimited)
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            count: AtomicU64::new(0),
            limit_reached: AtomicBool::new(false),
        }
    }

    /// Unlimited deletions
    pub fn unlimited() -> Self {
        Self::new(0)
    }

    /// Check if deletion is allowed and increment count
    pub fn try_delete(&self) -> bool {
        if self.limit == 0 {
            self.count.fetch_add(1, Ordering::Relaxed);
            return true;
        }

        loop {
            let current = self.count.load(Ordering::Acquire);
            if current >= self.limit {
                self.limit_reached.store(true, Ordering::Release);
                return false;
            }

            if self.count.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ).is_ok() {
                return true;
            }
        }
    }

    /// Get current deletion count
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Check if limit was reached
    pub fn limit_reached(&self) -> bool {
        self.limit_reached.load(Ordering::Acquire)
    }

    /// Get remaining deletions allowed
    pub fn remaining(&self) -> Option<u64> {
        if self.limit == 0 {
            None // Unlimited
        } else {
            Some(self.limit.saturating_sub(self.count.load(Ordering::Relaxed)))
        }
    }

    /// Reset counter (for testing)
    pub fn reset(&self) {
        self.count.store(0, Ordering::Release);
        self.limit_reached.store(false, Ordering::Release);
    }
}

/// Delete manager with max-delete support
pub struct DeleteManager {
    limiter: MaxDeleteLimiter,
    mode: DeleteMode,
    force: bool,
    stats: DeleteStats,
}

#[derive(Debug, Default)]
pub struct DeleteStats {
    pub files_deleted: u64,
    pub dirs_deleted: u64,
    pub bytes_freed: u64,
    pub skipped_due_to_limit: u64,
}

impl DeleteManager {
    pub fn new(limit: u64, mode: DeleteMode, force: bool) -> Self {
        Self {
            limiter: MaxDeleteLimiter::new(limit),
            mode,
            force,
            stats: DeleteStats::default(),
        }
    }

    /// Attempt to delete a file
    pub fn delete_file(&mut self, path: &Path) -> io::Result<bool> {
        if !self.limiter.try_delete() {
            self.stats.skipped_due_to_limit += 1;
            return Ok(false);
        }

        let meta = std::fs::metadata(path)?;
        std::fs::remove_file(path)?;

        self.stats.files_deleted += 1;
        self.stats.bytes_freed += meta.len();
        Ok(true)
    }

    /// Attempt to delete a directory
    pub fn delete_dir(&mut self, path: &Path) -> io::Result<bool> {
        if !self.limiter.try_delete() {
            self.stats.skipped_due_to_limit += 1;
            return Ok(false);
        }

        if self.force {
            std::fs::remove_dir_all(path)?;
        } else {
            std::fs::remove_dir(path)?;
        }

        self.stats.dirs_deleted += 1;
        Ok(true)
    }

    pub fn stats(&self) -> &DeleteStats {
        &self.stats
    }
}
```

### BwlimitSchedule Time-Based Throttling

Time-of-day bandwidth limit scheduling:

```rust
use std::time::{Duration, SystemTime};

/// Bandwidth limit schedule entry
#[derive(Debug, Clone)]
pub struct BwlimitScheduleEntry {
    /// Start time (seconds from midnight)
    pub start: u32,
    /// End time (seconds from midnight)
    pub end: u32,
    /// Bandwidth limit in bytes/sec (0 = unlimited)
    pub limit: u64,
    /// Days of week (bitmask: 1=Sun, 2=Mon, 4=Tue, ...)
    pub days: u8,
}

/// Bandwidth limit scheduler
#[derive(Debug)]
pub struct BwlimitScheduler {
    /// Schedule entries
    entries: Vec<BwlimitScheduleEntry>,
    /// Default limit when no schedule matches
    default_limit: u64,
}

impl BwlimitScheduler {
    pub fn new(default_limit: u64) -> Self {
        Self {
            entries: Vec::new(),
            default_limit,
        }
    }

    /// Add schedule entry
    pub fn add_entry(&mut self, entry: BwlimitScheduleEntry) {
        self.entries.push(entry);
    }

    /// Parse schedule string (rsync format: "HH:MM-HH:MM/LIMIT")
    pub fn parse_schedule(schedule: &str) -> Option<BwlimitScheduleEntry> {
        // Format: "09:00-17:00/1000K" or "Mon-Fri/09:00-17:00/500K"
        let parts: Vec<&str> = schedule.split('/').collect();

        if parts.len() < 2 {
            return None;
        }

        let (days, time_range, limit_str) = if parts.len() == 3 {
            (Self::parse_days(parts[0]), parts[1], parts[2])
        } else {
            (0x7F, parts[0], parts[1]) // All days
        };

        let (start, end) = Self::parse_time_range(time_range)?;
        let limit = Self::parse_limit(limit_str)?;

        Some(BwlimitScheduleEntry {
            start,
            end,
            limit,
            days,
        })
    }

    fn parse_days(s: &str) -> u8 {
        let mut days = 0u8;
        for part in s.split(',') {
            match part.trim().to_lowercase().as_str() {
                "sun" | "sunday" => days |= 1,
                "mon" | "monday" => days |= 2,
                "tue" | "tuesday" => days |= 4,
                "wed" | "wednesday" => days |= 8,
                "thu" | "thursday" => days |= 16,
                "fri" | "friday" => days |= 32,
                "sat" | "saturday" => days |= 64,
                "weekdays" => days |= 0x3E, // Mon-Fri
                "weekends" => days |= 0x41, // Sat-Sun
                _ => {}
            }
        }
        days
    }

    fn parse_time_range(s: &str) -> Option<(u32, u32)> {
        let (start_str, end_str) = s.split_once('-')?;

        let start = Self::parse_time(start_str)?;
        let end = Self::parse_time(end_str)?;

        Some((start, end))
    }

    fn parse_time(s: &str) -> Option<u32> {
        let (hour_str, min_str) = s.trim().split_once(':')?;
        let hour: u32 = hour_str.parse().ok()?;
        let min: u32 = min_str.parse().ok()?;

        if hour >= 24 || min >= 60 {
            return None;
        }

        Some(hour * 3600 + min * 60)
    }

    fn parse_limit(s: &str) -> Option<u64> {
        let s = s.trim().to_uppercase();
        let (num_str, multiplier) = if s.ends_with('K') {
            (&s[..s.len()-1], 1024u64)
        } else if s.ends_with('M') {
            (&s[..s.len()-1], 1024 * 1024)
        } else if s.ends_with('G') {
            (&s[..s.len()-1], 1024 * 1024 * 1024)
        } else {
            (s.as_str(), 1)
        };

        num_str.parse::<u64>().ok().map(|n| n * multiplier)
    }

    /// Get current bandwidth limit based on time
    pub fn current_limit(&self) -> u64 {
        let now = SystemTime::now();
        let secs_since_midnight = self.seconds_since_midnight(now);
        let day_of_week = self.day_of_week(now);

        for entry in &self.entries {
            // Check if current day matches
            if entry.days & (1 << day_of_week) == 0 {
                continue;
            }

            // Check if current time is in range
            let in_range = if entry.start <= entry.end {
                secs_since_midnight >= entry.start && secs_since_midnight < entry.end
            } else {
                // Wraps midnight
                secs_since_midnight >= entry.start || secs_since_midnight < entry.end
            };

            if in_range {
                return entry.limit;
            }
        }

        self.default_limit
    }

    fn seconds_since_midnight(&self, time: SystemTime) -> u32 {
        // Simplified: get seconds since epoch, mod 86400
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| (d.as_secs() % 86400) as u32)
            .unwrap_or(0)
    }

    fn day_of_week(&self, time: SystemTime) -> u8 {
        // Unix epoch was Thursday (4), so: (days_since_epoch + 4) % 7
        time.duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| ((d.as_secs() / 86400 + 4) % 7) as u8)
            .unwrap_or(0)
    }
}
```

### PasswordFile Secrets Handling

Secure password file reading for daemon authentication:

```rust
use std::path::Path;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;

/// Password file handler
#[derive(Debug)]
pub struct PasswordFile {
    /// Path to secrets file
    path: PathBuf,
    /// Cached entries
    entries: HashMap<String, String>,
    /// Whether file has been validated
    validated: bool,
}

#[derive(Debug)]
pub enum PasswordFileError {
    /// File not found
    NotFound,
    /// File has insecure permissions
    InsecurePermissions(u32),
    /// Parse error
    ParseError(String),
    /// I/O error
    IoError(io::Error),
}

impl PasswordFile {
    /// Load password file with permission checking
    pub fn load(path: &Path) -> Result<Self, PasswordFileError> {
        // Check file exists
        if !path.exists() {
            return Err(PasswordFileError::NotFound);
        }

        // Check permissions (must be 0600 or stricter)
        #[cfg(unix)]
        {
            let meta = fs::metadata(path).map_err(PasswordFileError::IoError)?;
            let mode = meta.permissions().mode() & 0o777;

            if mode & 0o077 != 0 {
                return Err(PasswordFileError::InsecurePermissions(mode));
            }
        }

        // Read and parse
        let content = fs::read_to_string(path).map_err(PasswordFileError::IoError)?;
        let entries = Self::parse_content(&content)?;

        Ok(Self {
            path: path.to_path_buf(),
            entries,
            validated: true,
        })
    }

    fn parse_content(content: &str) -> Result<HashMap<String, String>, PasswordFileError> {
        let mut entries = HashMap::new();

        for (line_num, line) in content.lines().enumerate() {
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Format: username:password
            if let Some((user, pass)) = line.split_once(':') {
                let user = user.trim();
                let pass = pass.trim();

                if user.is_empty() {
                    return Err(PasswordFileError::ParseError(
                        format!("Empty username on line {}", line_num + 1)
                    ));
                }

                entries.insert(user.to_string(), pass.to_string());
            } else {
                return Err(PasswordFileError::ParseError(
                    format!("Invalid format on line {}: expected 'user:password'", line_num + 1)
                ));
            }
        }

        Ok(entries)
    }

    /// Look up password for user
    pub fn get_password(&self, username: &str) -> Option<&str> {
        self.entries.get(username).map(String::as_str)
    }

    /// Verify password for user
    pub fn verify(&self, username: &str, password: &str) -> bool {
        self.entries.get(username)
            .map(|stored| stored == password)
            .unwrap_or(false)
    }

    /// List all usernames
    pub fn usernames(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Reload file from disk
    pub fn reload(&mut self) -> Result<(), PasswordFileError> {
        let reloaded = Self::load(&self.path)?;
        self.entries = reloaded.entries;
        Ok(())
    }
}

impl std::fmt::Display for PasswordFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "password file not found"),
            Self::InsecurePermissions(mode) => {
                write!(f, "password file has insecure permissions {:04o} (must be 0600)", mode)
            }
            Self::ParseError(msg) => write!(f, "password file parse error: {}", msg),
            Self::IoError(e) => write!(f, "password file I/O error: {}", e),
        }
    }
}

impl std::error::Error for PasswordFileError {}
```

### HostsAllow/Deny Access Control

IP-based access control for daemon:

```rust
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Host access control entry
#[derive(Debug, Clone)]
pub enum HostPattern {
    /// Exact IP address
    Exact(IpAddr),
    /// CIDR network (address + prefix length)
    Network(IpAddr, u8),
    /// Hostname pattern (may contain *)
    Hostname(String),
    /// All hosts
    All,
}

/// Host access controller
#[derive(Debug)]
pub struct HostAccessController {
    /// Allow patterns (checked first)
    allow: Vec<HostPattern>,
    /// Deny patterns
    deny: Vec<HostPattern>,
    /// Default action when no pattern matches
    default_allow: bool,
}

impl HostAccessController {
    /// Create with default deny
    pub fn new() -> Self {
        Self {
            allow: Vec::new(),
            deny: Vec::new(),
            default_allow: false,
        }
    }

    /// Add allow pattern
    pub fn allow(&mut self, pattern: HostPattern) {
        self.allow.push(pattern);
    }

    /// Add deny pattern
    pub fn deny(&mut self, pattern: HostPattern) {
        self.deny.push(pattern);
    }

    /// Parse pattern string
    pub fn parse_pattern(s: &str) -> Option<HostPattern> {
        let s = s.trim();

        if s == "*" || s.eq_ignore_ascii_case("all") {
            return Some(HostPattern::All);
        }

        // Try CIDR notation (e.g., "192.168.1.0/24")
        if let Some((addr_str, prefix_str)) = s.split_once('/') {
            if let (Ok(addr), Ok(prefix)) = (addr_str.parse::<IpAddr>(), prefix_str.parse::<u8>()) {
                let max_prefix = if addr.is_ipv4() { 32 } else { 128 };
                if prefix <= max_prefix {
                    return Some(HostPattern::Network(addr, prefix));
                }
            }
        }

        // Try exact IP
        if let Ok(addr) = s.parse::<IpAddr>() {
            return Some(HostPattern::Exact(addr));
        }

        // Treat as hostname pattern
        Some(HostPattern::Hostname(s.to_string()))
    }

    /// Check if host is allowed
    pub fn is_allowed(&self, addr: &IpAddr, hostname: Option<&str>) -> bool {
        // Check deny list first
        for pattern in &self.deny {
            if self.matches(pattern, addr, hostname) {
                // Check if also in allow list (allow overrides deny)
                for allow_pattern in &self.allow {
                    if self.matches(allow_pattern, addr, hostname) {
                        return true;
                    }
                }
                return false;
            }
        }

        // Check allow list
        if !self.allow.is_empty() {
            for pattern in &self.allow {
                if self.matches(pattern, addr, hostname) {
                    return true;
                }
            }
            return false;
        }

        self.default_allow
    }

    fn matches(&self, pattern: &HostPattern, addr: &IpAddr, hostname: Option<&str>) -> bool {
        match pattern {
            HostPattern::All => true,
            HostPattern::Exact(pattern_addr) => addr == pattern_addr,
            HostPattern::Network(network, prefix) => {
                self.in_network(addr, network, *prefix)
            }
            HostPattern::Hostname(pattern) => {
                hostname.map_or(false, |h| self.hostname_matches(h, pattern))
            }
        }
    }

    fn in_network(&self, addr: &IpAddr, network: &IpAddr, prefix: u8) -> bool {
        match (addr, network) {
            (IpAddr::V4(a), IpAddr::V4(n)) => {
                let mask = if prefix >= 32 {
                    u32::MAX
                } else {
                    u32::MAX << (32 - prefix)
                };
                (u32::from(*a) & mask) == (u32::from(*n) & mask)
            }
            (IpAddr::V6(a), IpAddr::V6(n)) => {
                let a_bytes = a.octets();
                let n_bytes = n.octets();
                let full_bytes = (prefix / 8) as usize;
                let remaining_bits = prefix % 8;

                // Compare full bytes
                if a_bytes[..full_bytes] != n_bytes[..full_bytes] {
                    return false;
                }

                // Compare remaining bits
                if remaining_bits > 0 && full_bytes < 16 {
                    let mask = 0xFF << (8 - remaining_bits);
                    if (a_bytes[full_bytes] & mask) != (n_bytes[full_bytes] & mask) {
                        return false;
                    }
                }

                true
            }
            _ => false, // IPv4/IPv6 mismatch
        }
    }

    fn hostname_matches(&self, hostname: &str, pattern: &str) -> bool {
        if pattern.contains('*') {
            // Simple glob matching
            let parts: Vec<&str> = pattern.split('*').collect();
            let mut pos = 0;

            for (i, part) in parts.iter().enumerate() {
                if part.is_empty() {
                    continue;
                }

                if let Some(found) = hostname[pos..].find(part) {
                    if i == 0 && found != 0 {
                        return false; // Must match at start
                    }
                    pos += found + part.len();
                } else {
                    return false;
                }
            }

            // If pattern doesn't end with *, must match to end
            !parts.last().map_or(false, |p| !p.is_empty()) || pos == hostname.len()
        } else {
            hostname.eq_ignore_ascii_case(pattern)
        }
    }
}

impl Default for HostAccessController {
    fn default() -> Self {
        Self::new()
    }
}
```

### FilterMerge Dir-Merge Parsing

Parsing .rsync-filter and per-directory filter files:

```rust
use std::path::Path;

/// Filter merge file types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeType {
    /// Include rules from file
    Merge,
    /// Include rules, remove file from transfer
    DirMerge,
    /// Exclude merge (negate patterns)
    ExcludeMerge,
}

/// Filter merge handler
#[derive(Debug)]
pub struct FilterMergeHandler {
    /// Stack of active filter files
    filter_stack: Vec<FilterFileState>,
    /// Global filters (from command line)
    global_filters: Vec<FilterRule>,
    /// Per-directory filter filename
    dir_filter_name: String,
}

#[derive(Debug)]
struct FilterFileState {
    /// Directory containing the filter file
    directory: PathBuf,
    /// Rules from this file
    rules: Vec<FilterRule>,
    /// Whether to exclude the filter file itself
    exclude_self: bool,
}

impl FilterMergeHandler {
    pub fn new() -> Self {
        Self {
            filter_stack: Vec::new(),
            global_filters: Vec::new(),
            dir_filter_name: ".rsync-filter".to_string(),
        }
    }

    /// Set per-directory filter filename
    pub fn set_dir_filter_name(&mut self, name: &str) {
        self.dir_filter_name = name.to_string();
    }

    /// Add global filter rule
    pub fn add_global(&mut self, rule: FilterRule) {
        self.global_filters.push(rule);
    }

    /// Enter directory, loading any filter files
    pub fn enter_directory(&mut self, dir: &Path) -> io::Result<()> {
        let filter_path = dir.join(&self.dir_filter_name);

        if filter_path.exists() {
            let rules = self.parse_filter_file(&filter_path)?;
            self.filter_stack.push(FilterFileState {
                directory: dir.to_path_buf(),
                rules,
                exclude_self: true,
            });
        }

        Ok(())
    }

    /// Leave directory, popping filter state
    pub fn leave_directory(&mut self, dir: &Path) {
        // Pop all filter states for this directory
        while let Some(state) = self.filter_stack.last() {
            if state.directory == dir {
                self.filter_stack.pop();
            } else {
                break;
            }
        }
    }

    /// Check if path matches any filter
    pub fn check(&self, path: &Path, is_dir: bool) -> FilterResult {
        // Check global filters first
        for rule in &self.global_filters {
            if let Some(result) = rule.matches(path, is_dir) {
                return result;
            }
        }

        // Check directory filters (most recent first)
        for state in self.filter_stack.iter().rev() {
            // Skip filter file itself if configured
            if state.exclude_self {
                if let Some(name) = path.file_name() {
                    if name == self.dir_filter_name.as_str() {
                        continue;
                    }
                }
            }

            for rule in &state.rules {
                if let Some(result) = rule.matches(path, is_dir) {
                    return result;
                }
            }
        }

        FilterResult::NoMatch
    }

    fn parse_filter_file(&self, path: &Path) -> io::Result<Vec<FilterRule>> {
        let content = std::fs::read_to_string(path)?;
        let mut rules = Vec::new();

        for line in content.lines() {
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if let Some(rule) = FilterRule::parse(line) {
                rules.push(rule);
            }
        }

        Ok(rules)
    }

    /// Parse merge directive (.: or . filename)
    pub fn parse_merge_directive(&self, line: &str) -> Option<(MergeType, String)> {
        let line = line.trim();

        if line.starts_with(". ") {
            Some((MergeType::Merge, line[2..].trim().to_string()))
        } else if line.starts_with(": ") {
            Some((MergeType::DirMerge, line[2..].trim().to_string()))
        } else if line.starts_with("- ") && line.contains("merge") {
            // Exclude merge
            Some((MergeType::ExcludeMerge, line[2..].trim().to_string()))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterResult {
    /// Include the file
    Include,
    /// Exclude the file
    Exclude,
    /// Hide the file (exclude from transfer and listing)
    Hide,
    /// No rule matched
    NoMatch,
}
```

### TempFileManager Atomic Operations

Temporary file handling for safe atomic writes:

```rust
use std::path::{Path, PathBuf};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};

/// Temporary file manager for atomic operations
#[derive(Debug)]
pub struct TempFileManager {
    /// Directory for temp files
    temp_dir: Option<PathBuf>,
    /// Prefix for temp filenames
    prefix: String,
    /// Active temp files (for cleanup)
    active_files: Vec<PathBuf>,
    /// Whether to use hidden files (.filename.XXXXXX)
    use_hidden: bool,
}

impl TempFileManager {
    /// Create manager using same directory as target
    pub fn new() -> Self {
        Self {
            temp_dir: None,
            prefix: ".".to_string(),
            active_files: Vec::new(),
            use_hidden: true,
        }
    }

    /// Create manager with specific temp directory
    pub fn with_temp_dir(dir: PathBuf) -> Self {
        Self {
            temp_dir: Some(dir),
            prefix: ".".to_string(),
            active_files: Vec::new(),
            use_hidden: true,
        }
    }

    /// Create temp file for target path
    pub fn create_temp(&mut self, target: &Path) -> io::Result<TempFile> {
        let temp_path = self.generate_temp_path(target)?;

        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;

        self.active_files.push(temp_path.clone());

        Ok(TempFile {
            file,
            temp_path,
            target_path: target.to_path_buf(),
            committed: false,
        })
    }

    fn generate_temp_path(&self, target: &Path) -> io::Result<PathBuf> {
        let parent = self.temp_dir.as_ref()
            .or_else(|| target.parent().map(Path::to_path_buf).as_ref())
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));

        let filename = target.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("temp");

        // Generate unique suffix
        let mut rng_bytes = [0u8; 6];
        getrandom::getrandom(&mut rng_bytes).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, e)
        })?;

        let suffix: String = rng_bytes.iter()
            .map(|b| format!("{:02x}", b))
            .collect();

        let temp_name = if self.use_hidden {
            format!("{}{}.{}", self.prefix, filename, suffix)
        } else {
            format!("{}.{}", filename, suffix)
        };

        Ok(parent.join(temp_name))
    }

    /// Clean up all active temp files
    pub fn cleanup(&mut self) {
        for path in self.active_files.drain(..) {
            let _ = fs::remove_file(&path);
        }
    }

    /// Remove temp file from tracking (after commit)
    pub fn untrack(&mut self, path: &Path) {
        self.active_files.retain(|p| p != path);
    }
}

impl Drop for TempFileManager {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// A temporary file that can be atomically renamed
pub struct TempFile {
    file: File,
    temp_path: PathBuf,
    target_path: PathBuf,
    committed: bool,
}

impl TempFile {
    /// Get the temporary path
    pub fn temp_path(&self) -> &Path {
        &self.temp_path
    }

    /// Get the target path
    pub fn target_path(&self) -> &Path {
        &self.target_path
    }

    /// Commit the temp file (rename to target)
    pub fn commit(mut self) -> io::Result<()> {
        self.file.sync_all()?;
        fs::rename(&self.temp_path, &self.target_path)?;
        self.committed = true;
        Ok(())
    }

    /// Abort and remove temp file
    pub fn abort(mut self) -> io::Result<()> {
        drop(self.file);
        fs::remove_file(&self.temp_path)?;
        self.committed = true; // Prevent double cleanup
        Ok(())
    }
}

impl Write for TempFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}
```

### ChecksumCache Persistence

Caching file checksums to disk for faster incremental syncs:

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Checksum cache entry
#[derive(Debug, Clone)]
pub struct ChecksumCacheEntry {
    /// File modification time when checksum was computed
    pub mtime: SystemTime,
    /// File size when checksum was computed
    pub size: u64,
    /// Cached checksum
    pub checksum: Vec<u8>,
}

/// Persistent checksum cache
#[derive(Debug)]
pub struct ChecksumCache {
    /// Cache file path
    path: PathBuf,
    /// In-memory cache
    entries: HashMap<PathBuf, ChecksumCacheEntry>,
    /// Whether cache has been modified
    dirty: bool,
    /// Maximum cache entries
    max_entries: usize,
}

impl ChecksumCache {
    /// Create or load cache from file
    pub fn open(path: &Path) -> io::Result<Self> {
        let entries = if path.exists() {
            Self::load_entries(path)?
        } else {
            HashMap::new()
        };

        Ok(Self {
            path: path.to_path_buf(),
            entries,
            dirty: false,
            max_entries: 100_000,
        })
    }

    /// Get cached checksum if still valid
    pub fn get(&self, file_path: &Path, current_meta: &Metadata) -> Option<&[u8]> {
        let entry = self.entries.get(file_path)?;

        // Validate cache entry
        if entry.size != current_meta.len() {
            return None;
        }

        if let Ok(mtime) = current_meta.modified() {
            if entry.mtime != mtime {
                return None;
            }
        }

        Some(&entry.checksum)
    }

    /// Store checksum in cache
    pub fn put(&mut self, file_path: &Path, meta: &Metadata, checksum: Vec<u8>) {
        let entry = ChecksumCacheEntry {
            mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            size: meta.len(),
            checksum,
        };

        self.entries.insert(file_path.to_path_buf(), entry);
        self.dirty = true;

        // Evict old entries if cache is too large
        if self.entries.len() > self.max_entries {
            self.evict_oldest();
        }
    }

    /// Remove entry from cache
    pub fn invalidate(&mut self, file_path: &Path) {
        if self.entries.remove(file_path).is_some() {
            self.dirty = true;
        }
    }

    /// Save cache to disk
    pub fn save(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }

        let mut file = std::fs::File::create(&self.path)?;

        // Simple binary format
        for (path, entry) in &self.entries {
            let path_bytes = path.to_string_lossy().into_owned().into_bytes();

            // Path length (4 bytes) + path + mtime (8 bytes) + size (8 bytes) + checksum length (4 bytes) + checksum
            file.write_all(&(path_bytes.len() as u32).to_le_bytes())?;
            file.write_all(&path_bytes)?;

            let mtime_secs = entry.mtime
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            file.write_all(&mtime_secs.to_le_bytes())?;
            file.write_all(&entry.size.to_le_bytes())?;
            file.write_all(&(entry.checksum.len() as u32).to_le_bytes())?;
            file.write_all(&entry.checksum)?;
        }

        self.dirty = false;
        Ok(())
    }

    fn load_entries(path: &Path) -> io::Result<HashMap<PathBuf, ChecksumCacheEntry>> {
        let data = std::fs::read(path)?;
        let mut entries = HashMap::new();
        let mut pos = 0;

        while pos + 4 <= data.len() {
            // Read path
            let path_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + path_len > data.len() { break; }

            let file_path = PathBuf::from(String::from_utf8_lossy(&data[pos..pos+path_len]).into_owned());
            pos += path_len;

            // Read mtime and size
            if pos + 16 > data.len() { break; }
            let mtime_secs = u64::from_le_bytes(data[pos..pos+8].try_into().unwrap());
            pos += 8;
            let size = u64::from_le_bytes(data[pos..pos+8].try_into().unwrap());
            pos += 8;

            // Read checksum
            if pos + 4 > data.len() { break; }
            let checksum_len = u32::from_le_bytes(data[pos..pos+4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + checksum_len > data.len() { break; }

            let checksum = data[pos..pos+checksum_len].to_vec();
            pos += checksum_len;

            let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(mtime_secs);
            entries.insert(file_path, ChecksumCacheEntry { mtime, size, checksum });
        }

        Ok(entries)
    }

    fn evict_oldest(&mut self) {
        // Simple strategy: remove 10% of entries
        let to_remove = self.max_entries / 10;
        let keys: Vec<_> = self.entries.keys().take(to_remove).cloned().collect();
        for key in keys {
            self.entries.remove(&key);
        }
    }
}

impl Drop for ChecksumCache {
    fn drop(&mut self) {
        let _ = self.save();
    }
}
```

### HardlinkTracker Inode Mapping

Tracking hardlinks for proper preservation:

```rust
use std::collections::HashMap;
use std::path::PathBuf;

/// Hardlink tracker for preserving link structure
#[derive(Debug)]
pub struct HardlinkTracker {
    /// Map from (device, inode) to first seen path
    seen: HashMap<(u64, u64), PathBuf>,
    /// Map from path to link target
    links: HashMap<PathBuf, PathBuf>,
    /// Statistics
    stats: HardlinkStats,
}

#[derive(Debug, Default)]
pub struct HardlinkStats {
    pub links_preserved: u64,
    pub bytes_saved: u64,
}

impl HardlinkTracker {
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
            links: HashMap::new(),
            stats: HardlinkStats::default(),
        }
    }

    /// Check if file is a hardlink to a previously seen file
    #[cfg(unix)]
    pub fn check(&mut self, path: &Path, meta: &Metadata) -> HardlinkResult {
        use std::os::unix::fs::MetadataExt;

        // Only track files with nlink > 1
        if meta.nlink() <= 1 {
            return HardlinkResult::Unique;
        }

        let key = (meta.dev(), meta.ino());

        if let Some(first_path) = self.seen.get(&key) {
            // This is a hardlink to a previously seen file
            self.links.insert(path.to_path_buf(), first_path.clone());
            self.stats.links_preserved += 1;
            self.stats.bytes_saved += meta.len();
            HardlinkResult::LinkTo(first_path.clone())
        } else {
            // First time seeing this inode
            self.seen.insert(key, path.to_path_buf());
            HardlinkResult::FirstSeen
        }
    }

    #[cfg(not(unix))]
    pub fn check(&mut self, _path: &Path, _meta: &Metadata) -> HardlinkResult {
        HardlinkResult::Unique
    }

    /// Get link target for a path
    pub fn get_link_target(&self, path: &Path) -> Option<&Path> {
        self.links.get(path).map(PathBuf::as_path)
    }

    /// Get all paths that link to the given inode
    #[cfg(unix)]
    pub fn get_all_links(&self, dev: u64, ino: u64) -> Vec<&Path> {
        let key = (dev, ino);
        let mut result = Vec::new();

        if let Some(first) = self.seen.get(&key) {
            result.push(first.as_path());

            for (path, target) in &self.links {
                if target == first {
                    result.push(path.as_path());
                }
            }
        }

        result
    }

    /// Clear tracker state
    pub fn clear(&mut self) {
        self.seen.clear();
        self.links.clear();
    }

    /// Get statistics
    pub fn stats(&self) -> &HardlinkStats {
        &self.stats
    }
}

#[derive(Debug)]
pub enum HardlinkResult {
    /// File has no hardlinks
    Unique,
    /// First time seeing this inode
    FirstSeen,
    /// Hardlink to existing path
    LinkTo(PathBuf),
}

impl Default for HardlinkTracker {
    fn default() -> Self {
        Self::new()
    }
}
```

### DeviceNode Handling Pattern

Special file handling for device nodes:

```rust
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;

/// Device node types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    /// Character device
    Char,
    /// Block device
    Block,
}

/// Device node information
#[derive(Debug, Clone)]
pub struct DeviceNode {
    /// Device type
    pub device_type: DeviceType,
    /// Major device number
    pub major: u32,
    /// Minor device number
    pub minor: u32,
}

impl DeviceNode {
    /// Create from metadata
    #[cfg(unix)]
    pub fn from_metadata(meta: &Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;

        let file_type = meta.file_type();

        let device_type = if file_type.is_char_device() {
            DeviceType::Char
        } else if file_type.is_block_device() {
            DeviceType::Block
        } else {
            return None;
        };

        let rdev = meta.rdev();
        let major = ((rdev >> 8) & 0xFF) as u32;
        let minor = (rdev & 0xFF) as u32;

        Some(Self {
            device_type,
            major,
            minor,
        })
    }

    #[cfg(not(unix))]
    pub fn from_metadata(_meta: &Metadata) -> Option<Self> {
        None
    }

    /// Create device node at path (requires privileges)
    #[cfg(unix)]
    pub fn create(&self, path: &Path, mode: u32) -> io::Result<()> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null"))?;

        let dev = libc::makedev(self.major, self.minor);

        let file_mode = match self.device_type {
            DeviceType::Char => libc::S_IFCHR | mode,
            DeviceType::Block => libc::S_IFBLK | mode,
        };

        let result = unsafe {
            libc::mknod(c_path.as_ptr(), file_mode as libc::mode_t, dev)
        };

        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(unix))]
    pub fn create(&self, _path: &Path, _mode: u32) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "device nodes not supported on this platform"
        ))
    }

    /// Encode for wire protocol
    pub fn encode(&self) -> [u8; 9] {
        let mut buf = [0u8; 9];
        buf[0] = match self.device_type {
            DeviceType::Char => b'c',
            DeviceType::Block => b'b',
        };
        buf[1..5].copy_from_slice(&self.major.to_le_bytes());
        buf[5..9].copy_from_slice(&self.minor.to_le_bytes());
        buf
    }

    /// Decode from wire protocol
    pub fn decode(buf: &[u8; 9]) -> Option<Self> {
        let device_type = match buf[0] {
            b'c' => DeviceType::Char,
            b'b' => DeviceType::Block,
            _ => return None,
        };

        let major = u32::from_le_bytes(buf[1..5].try_into().ok()?);
        let minor = u32::from_le_bytes(buf[5..9].try_into().ok()?);

        Some(Self { device_type, major, minor })
    }
}

/// Device node handler
#[derive(Debug)]
pub struct DeviceHandler {
    /// Whether to preserve devices
    preserve_devices: bool,
    /// Whether running as root
    is_root: bool,
}

impl DeviceHandler {
    pub fn new(preserve_devices: bool) -> Self {
        #[cfg(unix)]
        let is_root = unsafe { libc::geteuid() == 0 };
        #[cfg(not(unix))]
        let is_root = false;

        Self { preserve_devices, is_root }
    }

    /// Check if we can handle device nodes
    pub fn can_create_devices(&self) -> bool {
        self.preserve_devices && self.is_root
    }

    /// Handle device node during transfer
    pub fn handle(&self, path: &Path, device: &DeviceNode, mode: u32) -> io::Result<()> {
        if !self.preserve_devices {
            return Ok(());
        }

        if !self.is_root {
            // Log warning but don't fail
            return Ok(());
        }

        device.create(path, mode)
    }
}
```

### FifoHandler Special Files

Named pipe (FIFO) handling:

```rust
/// FIFO (named pipe) handler
#[derive(Debug)]
pub struct FifoHandler {
    /// Whether to preserve FIFOs
    preserve_specials: bool,
}

impl FifoHandler {
    pub fn new(preserve_specials: bool) -> Self {
        Self { preserve_specials }
    }

    /// Check if file is a FIFO
    #[cfg(unix)]
    pub fn is_fifo(meta: &Metadata) -> bool {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_fifo()
    }

    #[cfg(not(unix))]
    pub fn is_fifo(_meta: &Metadata) -> bool {
        false
    }

    /// Create FIFO at path
    #[cfg(unix)]
    pub fn create(&self, path: &Path, mode: u32) -> io::Result<()> {
        if !self.preserve_specials {
            return Ok(());
        }

        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let c_path = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains null"))?;

        let result = unsafe {
            libc::mkfifo(c_path.as_ptr(), mode as libc::mode_t)
        };

        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(unix))]
    pub fn create(&self, _path: &Path, _mode: u32) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "FIFOs not supported on this platform"
        ))
    }
}

/// Socket file handler
#[derive(Debug)]
pub struct SocketHandler {
    preserve_specials: bool,
}

impl SocketHandler {
    pub fn new(preserve_specials: bool) -> Self {
        Self { preserve_specials }
    }

    /// Check if file is a socket
    #[cfg(unix)]
    pub fn is_socket(meta: &Metadata) -> bool {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_socket()
    }

    #[cfg(not(unix))]
    pub fn is_socket(_meta: &Metadata) -> bool {
        false
    }

    // Note: Unix sockets can't be meaningfully transferred
    // They're created by listening processes
}
```

### NumericIds UID/GID Mode

Using numeric IDs instead of name mapping:

```rust
/// Numeric ID mode handler
#[derive(Debug, Clone)]
pub struct NumericIdMode {
    /// Use numeric UIDs (--numeric-ids)
    pub numeric_ids: bool,
    /// UID mapping table (when not numeric)
    uid_map: HashMap<u32, u32>,
    /// GID mapping table (when not numeric)
    gid_map: HashMap<u32, u32>,
}

impl NumericIdMode {
    /// Create with numeric IDs enabled
    pub fn numeric() -> Self {
        Self {
            numeric_ids: true,
            uid_map: HashMap::new(),
            gid_map: HashMap::new(),
        }
    }

    /// Create with name mapping
    pub fn with_mapping() -> Self {
        Self {
            numeric_ids: false,
            uid_map: HashMap::new(),
            gid_map: HashMap::new(),
        }
    }

    /// Map source UID to destination UID
    pub fn map_uid(&self, source_uid: u32) -> u32 {
        if self.numeric_ids {
            source_uid
        } else {
            *self.uid_map.get(&source_uid).unwrap_or(&source_uid)
        }
    }

    /// Map source GID to destination GID
    pub fn map_gid(&self, source_gid: u32) -> u32 {
        if self.numeric_ids {
            source_gid
        } else {
            *self.gid_map.get(&source_gid).unwrap_or(&source_gid)
        }
    }

    /// Add UID mapping (source -> dest)
    pub fn add_uid_mapping(&mut self, source: u32, dest: u32) {
        self.uid_map.insert(source, dest);
    }

    /// Add GID mapping (source -> dest)
    pub fn add_gid_mapping(&mut self, source: u32, dest: u32) {
        self.gid_map.insert(source, dest);
    }

    /// Build mapping from name lookups
    #[cfg(unix)]
    pub fn build_uid_mapping(&mut self, source_name: &str, source_uid: u32) {
        // Look up name on destination system
        if let Some(dest_uid) = Self::lookup_uid_by_name(source_name) {
            self.uid_map.insert(source_uid, dest_uid);
        }
    }

    #[cfg(unix)]
    pub fn build_gid_mapping(&mut self, source_name: &str, source_gid: u32) {
        if let Some(dest_gid) = Self::lookup_gid_by_name(source_name) {
            self.gid_map.insert(source_gid, dest_gid);
        }
    }

    #[cfg(unix)]
    fn lookup_uid_by_name(name: &str) -> Option<u32> {
        use std::ffi::CString;

        let c_name = CString::new(name).ok()?;
        let pwd = unsafe { libc::getpwnam(c_name.as_ptr()) };

        if pwd.is_null() {
            None
        } else {
            Some(unsafe { (*pwd).pw_uid })
        }
    }

    #[cfg(unix)]
    fn lookup_gid_by_name(name: &str) -> Option<u32> {
        use std::ffi::CString;

        let c_name = CString::new(name).ok()?;
        let grp = unsafe { libc::getgrnam(c_name.as_ptr()) };

        if grp.is_null() {
            None
        } else {
            Some(unsafe { (*grp).gr_gid })
        }
    }
}

impl Default for NumericIdMode {
    fn default() -> Self {
        Self::with_mapping()
    }
}
```

### OneFileSystem Boundary Detection

Preventing cross-filesystem traversal:

```rust
/// One-filesystem boundary detector
#[derive(Debug)]
pub struct OneFileSystemHandler {
    /// Device IDs of allowed filesystems
    allowed_devices: HashSet<u64>,
    /// Whether enabled
    enabled: bool,
}

impl OneFileSystemHandler {
    /// Create disabled handler
    pub fn disabled() -> Self {
        Self {
            allowed_devices: HashSet::new(),
            enabled: false,
        }
    }

    /// Create enabled handler
    pub fn enabled() -> Self {
        Self {
            allowed_devices: HashSet::new(),
            enabled: true,
        }
    }

    /// Initialize with starting path
    #[cfg(unix)]
    pub fn init(&mut self, start_path: &Path) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        use std::os::unix::fs::MetadataExt;

        let meta = std::fs::metadata(start_path)?;
        self.allowed_devices.insert(meta.dev());
        Ok(())
    }

    #[cfg(not(unix))]
    pub fn init(&mut self, _start_path: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Check if path crosses filesystem boundary
    #[cfg(unix)]
    pub fn should_skip(&self, meta: &Metadata) -> bool {
        if !self.enabled {
            return false;
        }

        use std::os::unix::fs::MetadataExt;
        !self.allowed_devices.contains(&meta.dev())
    }

    #[cfg(not(unix))]
    pub fn should_skip(&self, _meta: &Metadata) -> bool {
        false
    }

    /// Add additional allowed device
    pub fn allow_device(&mut self, dev: u64) {
        self.allowed_devices.insert(dev);
    }
}

use std::collections::HashSet;

impl Default for OneFileSystemHandler {
    fn default() -> Self {
        Self::disabled()
    }
}
```

### IgnoreErrors Resilient Mode

Continuing past errors during transfer:

```rust
/// Error handling mode
#[derive(Debug, Clone)]
pub struct IgnoreErrorsHandler {
    /// Whether to ignore read errors
    ignore_errors: bool,
    /// Collected errors
    errors: Vec<TransferError>,
    /// Maximum errors before abort
    max_errors: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct TransferError {
    pub path: PathBuf,
    pub error: String,
    pub severity: ErrorSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorSeverity {
    /// Skippable error
    Warning,
    /// Significant but continuable
    Error,
    /// Must abort
    Fatal,
}

impl IgnoreErrorsHandler {
    /// Create with ignore-errors enabled
    pub fn ignoring() -> Self {
        Self {
            ignore_errors: true,
            errors: Vec::new(),
            max_errors: None,
        }
    }

    /// Create with strict error handling
    pub fn strict() -> Self {
        Self {
            ignore_errors: false,
            errors: Vec::new(),
            max_errors: None,
        }
    }

    /// Set maximum errors before abort
    pub fn with_max_errors(mut self, max: usize) -> Self {
        self.max_errors = Some(max);
        self
    }

    /// Handle an error, return whether to continue
    pub fn handle(&mut self, path: &Path, error: io::Error) -> bool {
        let severity = Self::classify_error(&error);

        self.errors.push(TransferError {
            path: path.to_path_buf(),
            error: error.to_string(),
            severity,
        });

        // Check if we should abort
        if severity == ErrorSeverity::Fatal {
            return false;
        }

        if !self.ignore_errors && severity == ErrorSeverity::Error {
            return false;
        }

        // Check max errors
        if let Some(max) = self.max_errors {
            if self.errors.len() >= max {
                return false;
            }
        }

        true
    }

    fn classify_error(error: &io::Error) -> ErrorSeverity {
        match error.kind() {
            // Skippable errors
            io::ErrorKind::NotFound => ErrorSeverity::Warning,
            io::ErrorKind::PermissionDenied => ErrorSeverity::Error,

            // Fatal errors
            io::ErrorKind::OutOfMemory => ErrorSeverity::Fatal,
            io::ErrorKind::StorageFull => ErrorSeverity::Fatal,

            // Default to Error
            _ => ErrorSeverity::Error,
        }
    }

    /// Get collected errors
    pub fn errors(&self) -> &[TransferError] {
        &self.errors
    }

    /// Check if any errors occurred
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Get error count by severity
    pub fn error_count(&self, severity: ErrorSeverity) -> usize {
        self.errors.iter().filter(|e| e.severity == severity).count()
    }

    /// Format error summary
    pub fn format_summary(&self) -> String {
        let warnings = self.error_count(ErrorSeverity::Warning);
        let errors = self.error_count(ErrorSeverity::Error);
        let fatal = self.error_count(ErrorSeverity::Fatal);

        format!(
            "{} warnings, {} errors, {} fatal",
            warnings, errors, fatal
        )
    }
}
```

### ForceDelete Safety Pattern

Safe deletion with --force handling:

```rust
/// Force delete handler with safety checks
#[derive(Debug)]
pub struct ForceDeleteHandler {
    /// Whether force is enabled
    force: bool,
    /// Whether to delete directories with contents
    delete_during: bool,
    /// Protected paths (never delete)
    protected: HashSet<PathBuf>,
    /// Statistics
    stats: ForceDeleteStats,
}

#[derive(Debug, Default)]
pub struct ForceDeleteStats {
    pub files_deleted: u64,
    pub dirs_deleted: u64,
    pub protected_skipped: u64,
    pub errors: u64,
}

impl ForceDeleteHandler {
    pub fn new(force: bool) -> Self {
        Self {
            force,
            delete_during: false,
            protected: HashSet::new(),
            stats: ForceDeleteStats::default(),
        }
    }

    /// Add path to protected list
    pub fn protect(&mut self, path: PathBuf) {
        self.protected.insert(path);
    }

    /// Check if path is protected
    pub fn is_protected(&self, path: &Path) -> bool {
        // Check exact match
        if self.protected.contains(path) {
            return true;
        }

        // Check if any ancestor is protected
        for ancestor in path.ancestors().skip(1) {
            if self.protected.contains(ancestor) {
                return true;
            }
        }

        false
    }

    /// Delete file or directory
    pub fn delete(&mut self, path: &Path) -> io::Result<bool> {
        // Check protection
        if self.is_protected(path) {
            self.stats.protected_skipped += 1;
            return Ok(false);
        }

        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => {
                self.stats.errors += 1;
                return Err(e);
            }
        };

        if meta.is_dir() {
            self.delete_directory(path)
        } else {
            self.delete_file(path)
        }
    }

    fn delete_file(&mut self, path: &Path) -> io::Result<bool> {
        match std::fs::remove_file(path) {
            Ok(()) => {
                self.stats.files_deleted += 1;
                Ok(true)
            }
            Err(e) => {
                self.stats.errors += 1;
                Err(e)
            }
        }
    }

    fn delete_directory(&mut self, path: &Path) -> io::Result<bool> {
        if self.force {
            // Force: remove even if not empty
            match std::fs::remove_dir_all(path) {
                Ok(()) => {
                    self.stats.dirs_deleted += 1;
                    Ok(true)
                }
                Err(e) => {
                    self.stats.errors += 1;
                    Err(e)
                }
            }
        } else {
            // Non-force: only remove empty directories
            match std::fs::remove_dir(path) {
                Ok(()) => {
                    self.stats.dirs_deleted += 1;
                    Ok(true)
                }
                Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
                    // Not an error in non-force mode
                    Ok(false)
                }
                Err(e) => {
                    self.stats.errors += 1;
                    Err(e)
                }
            }
        }
    }

    /// Recursively delete contents of directory
    pub fn delete_contents(&mut self, dir: &Path) -> io::Result<()> {
        if !self.force {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if self.is_protected(&path) {
                self.stats.protected_skipped += 1;
                continue;
            }

            if entry.file_type()?.is_dir() {
                self.delete_contents(&path)?;
                let _ = self.delete_directory(&path);
            } else {
                let _ = self.delete_file(&path);
            }
        }

        Ok(())
    }

    pub fn stats(&self) -> &ForceDeleteStats {
        &self.stats
    }
}

impl Default for ForceDeleteHandler {
    fn default() -> Self {
        Self::new(false)
    }
}
```

### RsyncRolling Checksum Implementation

Detailed implementation of rsync's rolling checksum (modified Adler32) matching
upstream C implementation for wire compatibility:

```rust
// crates/checksums/src/rolling.rs

/// Rsync's rolling checksum (modified Adler32)
///
/// This matches the upstream C implementation exactly for wire compatibility.
#[derive(Clone, Default)]
pub struct RsyncRolling {
    a: u32,  // sum of bytes
    b: u32,  // weighted sum
}

impl RsyncRolling {
    const CHAR_OFFSET: u32 = 0;  // rsync uses 0, original Adler uses 1

    pub fn new() -> Self {
        Self::default()
    }

    /// Compute checksum over data block
    pub fn compute(data: &[u8]) -> u32 {
        let mut rolling = Self::new();
        rolling.reset(data);
        rolling.digest()
    }

    pub fn reset(&mut self, data: &[u8]) {
        let mut a: u32 = 0;
        let mut b: u32 = 0;
        let len = data.len() as u32;

        for (i, &byte) in data.iter().enumerate() {
            let val = u32::from(byte) + Self::CHAR_OFFSET;
            a = a.wrapping_add(val);
            b = b.wrapping_add(val.wrapping_mul(len - i as u32));
        }

        self.a = a & 0xFFFF;
        self.b = b & 0xFFFF;
    }

    pub fn roll(&mut self, old_byte: u8, new_byte: u8, block_size: u32) {
        let old_val = u32::from(old_byte) + Self::CHAR_OFFSET;
        let new_val = u32::from(new_byte) + Self::CHAR_OFFSET;

        self.a = self.a.wrapping_sub(old_val).wrapping_add(new_val) & 0xFFFF;
        self.b = self.b
            .wrapping_sub(old_val.wrapping_mul(block_size))
            .wrapping_add(self.a) & 0xFFFF;
    }

    pub fn digest(&self) -> u32 {
        (self.b << 16) | self.a
    }
}
```

### Strong Checksum Adapters

Type-safe adapters for different strong checksum algorithms:

```rust
// crates/checksums/src/strong.rs

/// XXH3-64 strong checksum (rsync 3.x default)
#[derive(Default)]
pub struct Xxh3Strong {
    state: xxhash_rust::xxh3::Xxh3,
}

impl StrongChecksum for Xxh3Strong {
    type Output = [u8; 8];

    fn update(&mut self, data: &[u8]) {
        self.state.update(data);
    }

    fn finalize(self) -> Self::Output {
        self.state.digest().to_le_bytes()
    }
}

/// MD5 strong checksum (legacy/fallback)
#[derive(Default)]
pub struct Md5Strong {
    hasher: md5::Md5,
}

impl StrongChecksum for Md5Strong {
    type Output = [u8; 16];

    fn update(&mut self, data: &[u8]) {
        use md5::Digest;
        self.hasher.update(data);
    }

    fn finalize(self) -> Self::Output {
        use md5::Digest;
        self.hasher.finalize().into()
    }
}

/// MD4 strong checksum (protocol < 30)
#[derive(Default)]
pub struct Md4Strong {
    hasher: md4::Md4,
}

impl StrongChecksum for Md4Strong {
    type Output = [u8; 16];

    fn update(&mut self, data: &[u8]) {
        use md4::Digest;
        self.hasher.update(data);
    }

    fn finalize(self) -> Self::Output {
        use md4::Digest;
        self.hasher.finalize().into()
    }
}
```

### MsgTag Protocol Message Codes

Complete enumeration of rsync protocol message tags:

```rust
// crates/protocol/src/multiplex.rs

/// Message tags matching upstream rsync
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgTag {
    Data       = 0,    // MSG_DATA - file data
    ErrorXfer  = 1,    // MSG_ERROR_XFER
    Info       = 2,    // MSG_INFO
    Error      = 3,    // MSG_ERROR
    Warning    = 4,    // MSG_WARNING
    ErrorSocket= 5,    // MSG_ERROR_SOCKET
    Log        = 6,    // MSG_LOG
    Client     = 7,    // MSG_CLIENT
    ErrorUtf8  = 8,    // MSG_ERROR_UTF8
    Redo       = 9,    // MSG_REDO
    Flist      = 20,   // MSG_FLIST
    FlistEof   = 21,   // MSG_FLIST_EOF
    IoError    = 22,   // MSG_IO_ERROR
    Noop       = 42,   // MSG_NOOP
    Success    = 100,  // MSG_SUCCESS
    Deleted    = 101,  // MSG_DELETED
    NoSend     = 102,  // MSG_NO_SEND
}

impl TryFrom<u8> for MsgTag {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Data),
            1 => Ok(Self::ErrorXfer),
            2 => Ok(Self::Info),
            // ... other variants
            _ => Err(ProtocolError::InvalidTag(value)),
        }
    }
}
```

### MultiplexMsg and Transport Layer

Multiplexed message structure with transport configuration:

```rust
// crates/protocol/src/multiplex.rs

/// Multiplexed message
#[derive(Clone, Debug)]
pub struct MultiplexMsg {
    pub tag: MsgTag,
    pub payload: BytesMut,
}

impl MultiplexMsg {
    pub fn new(tag: MsgTag, payload: impl Into<BytesMut>) -> Self {
        Self { tag, payload: payload.into() }
    }

    pub fn data(payload: impl Into<BytesMut>) -> Self {
        Self::new(MsgTag::Data, payload)
    }

    pub fn info(message: &str) -> Self {
        Self::new(MsgTag::Info, BytesMut::from(message.as_bytes()))
    }
}

// crates/protocol/src/transport.rs

/// Configuration for transport layer
#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub read_timeout: Duration,
    pub write_timeout: Duration,
    pub max_message_size: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(300),
            write_timeout: Duration::from_secs(60),
            max_message_size: 16 * 1024 * 1024,
        }
    }
}

/// Bidirectional rsync transport over any async I/O
pub struct Transport<S> {
    framed: Framed<S, MultiplexCodec>,
    config: TransportConfig,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Transport<S> {
    pub fn new(stream: S, config: TransportConfig) -> Self {
        let codec = MultiplexCodec::with_max_size(config.max_message_size);
        Self {
            framed: Framed::new(stream, codec),
            config,
        }
    }

    /// Send a message with timeout
    pub async fn send(&mut self, msg: MultiplexMsg) -> Result<(), TransportError> {
        timeout(self.config.write_timeout, self.framed.send(msg))
            .await
            .map_err(|_| TransportError::WriteTimeout)?
            .map_err(TransportError::Protocol)
    }

    /// Receive next message with timeout
    pub async fn recv(&mut self) -> Result<MultiplexMsg, TransportError> {
        timeout(self.config.read_timeout, async {
            self.framed
                .next()
                .await
                .ok_or(TransportError::ConnectionClosed)?
        })
        .await
        .map_err(|_| TransportError::ReadTimeout)?
        .map_err(TransportError::Protocol)
    }
}
```

### StreamCompressor and StreamDecompressor

Decorator pattern for transparent compression stream wrapping:

```rust
// crates/protocol/src/compress.rs

/// Compression level matching rsync's -z levels
#[derive(Clone, Copy, Debug)]
pub struct CompressionLevel(u32);

impl CompressionLevel {
    pub const NONE: Self = Self(0);
    pub const FAST: Self = Self(1);
    pub const DEFAULT: Self = Self(6);
    pub const BEST: Self = Self(9);

    pub fn new(level: u32) -> Self {
        Self(level.min(9))
    }

    pub fn is_enabled(&self) -> bool {
        self.0 > 0
    }
}

/// Streaming compressor for outbound data
pub struct StreamCompressor<W: Write> {
    inner: Option<ZlibEncoder<W>>,
    passthrough: Option<W>,
}

impl<W: Write> StreamCompressor<W> {
    pub fn new(writer: W, level: CompressionLevel) -> Self {
        if level.is_enabled() {
            Self {
                inner: Some(ZlibEncoder::new(writer, level.into())),
                passthrough: None,
            }
        } else {
            Self {
                inner: None,
                passthrough: Some(writer),
            }
        }
    }

    /// Finish compression and return underlying writer
    pub fn finish(self) -> io::Result<W> {
        if let Some(encoder) = self.inner {
            encoder.finish()
        } else {
            Ok(self.passthrough.unwrap())
        }
    }
}

/// Streaming decompressor for inbound data
pub struct StreamDecompressor<R: Read> {
    inner: Option<ZlibDecoder<R>>,
    passthrough: Option<R>,
}

impl<R: Read> StreamDecompressor<R> {
    pub fn new(reader: R, compressed: bool) -> Self {
        if compressed {
            Self {
                inner: Some(ZlibDecoder::new(reader)),
                passthrough: None,
            }
        } else {
            Self {
                inner: None,
                passthrough: Some(reader),
            }
        }
    }
}

/// Token-based compression for rsync's block-aware compression
pub struct TokenCompressor {
    level: CompressionLevel,
    buffer: Vec<u8>,
}

impl TokenCompressor {
    pub fn new(level: CompressionLevel) -> Self {
        Self {
            level,
            buffer: Vec::with_capacity(32 * 1024),
        }
    }

    /// Compress a literal token
    pub fn compress_literal(&mut self, data: &[u8]) -> io::Result<Vec<u8>> {
        if !self.level.is_enabled() {
            return Ok(data.to_vec());
        }

        self.buffer.clear();
        let mut encoder = ZlibEncoder::new(&mut self.buffer, self.level.into());
        encoder.write_all(data)?;
        encoder.finish()?;

        // Only use compressed if smaller
        if self.buffer.len() < data.len() {
            Ok(self.buffer.clone())
        } else {
            Ok(data.to_vec())
        }
    }
}
```

### BandwidthLimiter with RateLimitedWriter

Extended bandwidth limiting with blocking mode and writer wrapper:

```rust
// crates/bandwidth/src/lib.rs

impl BandwidthLimiter {
    /// Create unlimited limiter
    pub fn unlimited() -> Self {
        Self {
            limiter: None,
            bytes_per_sec: 0,
        }
    }

    /// Create from rsync-style bwlimit value (KiB/s)
    pub fn from_kbps(kbps: u64) -> Self {
        if kbps == 0 {
            Self::unlimited()
        } else {
            Self::new(kbps * 1024)
        }
    }

    /// Synchronous acquisition (blocks thread)
    pub fn acquire_blocking(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }

        let Some(ref limiter) = self.limiter else {
            return;
        };

        let mut remaining = bytes;
        while remaining > 0 {
            let chunk = remaining.min(u32::MAX as usize);
            let cells = NonZeroU32::new(chunk as u32).unwrap();

            while limiter.check_n(cells).is_err() {
                std::thread::sleep(Duration::from_millis(10));
            }
            remaining -= chunk;
        }
    }

    /// Check if this limiter is rate-limited
    pub fn is_limited(&self) -> bool {
        self.limiter.is_some()
    }
}

/// Wrapper for rate-limited writes
pub struct RateLimitedWriter<W> {
    inner: W,
    limiter: BandwidthLimiter,
}

impl<W> RateLimitedWriter<W> {
    pub fn new(inner: W, limiter: BandwidthLimiter) -> Self {
        Self { inner, limiter }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: std::io::Write> std::io::Write for RateLimitedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.limiter.acquire_blocking(buf.len());
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
```

### Connection State Machine Methods

Detailed daemon connection state machine with handler methods:

```rust
// crates/daemon/src/connection.rs

impl Connection {
    async fn handle_greeting(&mut self, config: &DaemonConfig) -> Result<(), ConnectionError> {
        // Send server greeting
        let greeting = format!("@RSYNCD: {}.{}\n",
            config.protocol_version / 10,
            config.protocol_version % 10);

        self.transport.send(MultiplexMsg::data(
            BytesMut::from(greeting.as_bytes())
        )).await?;

        // Receive client greeting and parse version
        let response = self.transport.recv_data().await?;
        let greeting_str = String::from_utf8_lossy(&response);

        if let Some(version_str) = greeting_str.strip_prefix("@RSYNCD: ") {
            let version_str = version_str.trim();
            if let Some((major, minor)) = version_str.split_once('.') {
                let major: u32 = major.parse().unwrap_or(0);
                let minor: u32 = minor.parse().unwrap_or(0);
                self.protocol_version = (major * 10 + minor).min(config.protocol_version);
            }
        }

        self.state = ConnectionState::ModuleSelect;
        Ok(())
    }

    async fn handle_module_select(&mut self, config: &DaemonConfig) -> Result<(), ConnectionError> {
        let request = self.transport.recv_data().await?;
        let module_name = String::from_utf8_lossy(&request).trim().to_string();

        // Check for module listing request
        if module_name.is_empty() || module_name == "#list" {
            self.send_module_list(config).await?;
            self.state = ConnectionState::Closing;
            return Ok(());
        }

        // Validate module exists
        let Some(module) = config.modules.get(&module_name) else {
            let error = format!("@ERROR: Unknown module '{}'\n", module_name);
            self.transport.send(MultiplexMsg::data(
                BytesMut::from(error.as_bytes())
            )).await?;
            self.state = ConnectionState::Closing;
            return Ok(());
        };

        // Check if authentication needed
        if module.auth_users.is_some() {
            self.transport.send(MultiplexMsg::data(
                BytesMut::from("@RSYNCD: AUTHREQD\n")
            )).await?;
            self.state = ConnectionState::Authenticating { module: module_name };
        } else {
            self.transport.send(MultiplexMsg::data(
                BytesMut::from("@RSYNCD: OK\n")
            )).await?;
            self.state = ConnectionState::Transferring {
                module: module_name,
                read_only: module.read_only,
            };
        }

        Ok(())
    }

    async fn send_module_list(&mut self, config: &DaemonConfig) -> Result<(), ConnectionError> {
        for (name, module) in &config.modules {
            if !module.list {
                continue;
            }

            let line = if let Some(ref comment) = module.comment {
                format!("{}\t{}\n", name, comment)
            } else {
                format!("{}\n", name)
            };

            self.transport.send(MultiplexMsg::data(
                BytesMut::from(line.as_bytes())
            )).await?;
        }

        self.transport.send(MultiplexMsg::data(
            BytesMut::from("@RSYNCD: EXIT\n")
        )).await?;
        Ok(())
    }
}
```

### Server Concurrent Connection Handler

Daemon server with semaphore-based connection limiting and graceful shutdown:

```rust
// crates/daemon/src/server.rs

/// Daemon server
pub struct Server {
    config: Arc<DaemonConfig>,
    max_connections: usize,
}

impl Server {
    pub fn new(config: DaemonConfig) -> Self {
        Self {
            config: Arc::new(config),
            max_connections: 200,
        }
    }

    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
        self
    }

    /// Run the server, accepting connections until shutdown signal
    pub async fn run(self, addr: SocketAddr) -> Result<(), ServerError> {
        let listener = TcpListener::bind(addr).await?;
        tracing::info!(address = %addr, "daemon listening");

        // Connection limiting
        let semaphore = Arc::new(Semaphore::new(self.max_connections));

        // Graceful shutdown channel
        let (shutdown_tx, _) = broadcast::channel::<()>(1);

        // Spawn signal handler
        let shutdown_tx_clone = shutdown_tx.clone();
        tokio::spawn(async move {
            if let Ok(()) = tokio::signal::ctrl_c().await {
                tracing::info!("shutdown signal received");
                let _ = shutdown_tx_clone.send(());
            }
        });

        loop {
            let mut shutdown_rx = shutdown_tx.subscribe();

            tokio::select! {
                result = listener.accept() => {
                    let (socket, peer_addr) = result?;
                    let config = Arc::clone(&self.config);
                    let permit = Arc::clone(&semaphore);

                    tokio::spawn(async move {
                        // Acquire connection permit
                        let _permit = match permit.acquire().await {
                            Ok(p) => p,
                            Err(_) => return,
                        };

                        let connection = Connection::new(socket, peer_addr);
                        if let Err(e) = connection.run(&config).await {
                            tracing::error!(peer = %peer_addr, error = %e, "connection error");
                        }
                    });
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("shutting down");
                    break;
                }
            }
        }

        Ok(())
    }
}
```

### CLI Options Structures

Complete CLI option structures with effective options resolution:

```rust
// crates/cli/src/lib.rs

#[derive(Args, Debug)]
pub struct GlobalOptions {
    /// Run as daemon
    #[arg(long)]
    pub daemon: bool,

    /// Specify alternate rsyncd.conf file
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Increase verbosity
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress non-error messages
    #[arg(short, long)]
    pub quiet: bool,

    /// Show what would be transferred
    #[arg(short = 'n', long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct TransferOptions {
    /// Archive mode (-rlptgoD)
    #[arg(short, long)]
    pub archive: bool,

    /// Recurse into directories
    #[arg(short, long)]
    pub recursive: bool,

    /// Compress file data during transfer
    #[arg(short = 'z', long)]
    pub compress: bool,

    /// Set compression level (0-9)
    #[arg(long, value_name = "LEVEL", default_value = "6")]
    pub compress_level: u32,

    /// Limit I/O bandwidth (KBytes/sec)
    #[arg(long, value_name = "KBPS")]
    pub bwlimit: Option<u64>,

    /// Delete extraneous files from dest
    #[arg(long)]
    pub delete: bool,
}

#[derive(Args, Debug)]
pub struct OutputOptions {
    /// Show progress during transfer
    #[arg(long)]
    pub progress: bool,

    /// Give stats at end of transfer
    #[arg(long)]
    pub stats: bool,

    /// Output numbers in human-readable format
    #[arg(short = 'h', long)]
    pub human_readable: bool,
}

/// Resolved effective options after processing archive mode
#[derive(Clone, Debug, Default)]
pub struct EffectiveOptions {
    pub recursive: bool,
    pub links: bool,
    pub perms: bool,
    pub times: bool,
    pub owner: bool,
    pub group: bool,
    pub devices: bool,
    pub compress: bool,
    pub compress_level: u32,
    pub delete: bool,
    pub dry_run: bool,
    pub verbose: u8,
}

impl Cli {
    /// Expand archive mode into component flags
    pub fn effective_options(&self) -> EffectiveOptions {
        let mut opts = EffectiveOptions::default();

        if self.transfer.archive {
            opts.recursive = true;
            opts.links = true;
            opts.perms = true;
            opts.times = true;
            opts.owner = true;
            opts.group = true;
            opts.devices = true;
        }

        opts.compress = self.transfer.compress;
        opts.compress_level = self.transfer.compress_level;
        opts.delete = self.transfer.delete;
        opts.dry_run = self.global.dry_run;
        opts.verbose = self.global.verbose;

        opts
    }
}
```

### TransferProgress with Indicatif

Progress reporting with multi-progress bar support:

```rust
// crates/cli/src/progress.rs

/// Progress reporter for rsync transfers
pub struct TransferProgress {
    multi: MultiProgress,
    current_file: ProgressBar,
    overall: ProgressBar,
    total_bytes: u64,
    transferred_bytes: u64,
}

impl TransferProgress {
    pub fn new(total_files: u64, total_bytes: u64) -> Self {
        let multi = MultiProgress::new();

        let overall_style = ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} files ({percent}%) {msg}")
            .expect("valid template")
            .progress_chars("=>-");

        let file_style = ProgressStyle::default_bar()
            .template("  {spinner:.green} {wide_msg} [{bar:30.yellow/white}] {bytes}/{total_bytes} ({bytes_per_sec})")
            .expect("valid template")
            .progress_chars("=>-");

        let overall = multi.add(ProgressBar::new(total_files));
        overall.set_style(overall_style);
        overall.enable_steady_tick(Duration::from_millis(100));

        let current_file = multi.add(ProgressBar::new(0));
        current_file.set_style(file_style);

        Self {
            multi,
            current_file,
            overall,
            total_bytes,
            transferred_bytes: 0,
        }
    }

    /// Start tracking a new file
    pub fn start_file(&self, name: &str, size: u64) {
        self.current_file.set_length(size);
        self.current_file.set_position(0);
        self.current_file.set_message(name.to_string());
    }

    /// Update progress on current file
    pub fn update_file(&mut self, bytes: u64) {
        self.current_file.set_position(bytes);
        self.transferred_bytes += bytes;
    }

    /// Complete current file
    pub fn finish_file(&self) {
        self.current_file.finish_and_clear();
        self.overall.inc(1);
    }

    /// Complete all progress
    pub fn finish(&self) {
        self.current_file.finish_and_clear();
        self.overall.finish_with_message("done");
    }
}
```

### TransferStats with Speedup Calculation

Statistics collection with rsync-compatible summary formatting:

```rust
// crates/cli/src/progress.rs

/// Statistics collected during transfer
#[derive(Clone, Debug, Default)]
pub struct TransferStats {
    pub num_files: u64,
    pub num_transferred: u64,
    pub total_size: u64,
    pub transferred_size: u64,
    pub literal_data: u64,
    pub matched_data: u64,
}

impl TransferStats {
    /// Calculate speedup ratio (original / transferred)
    pub fn speedup(&self) -> f64 {
        if self.literal_data == 0 {
            return 0.0;
        }
        self.transferred_size as f64 / self.literal_data as f64
    }

    /// Format stats for display (rsync-compatible format)
    pub fn format_summary(&self) -> String {
        let speedup = self.speedup();
        format!(
            "Number of files: {}\n\
             Number of transferred files: {}\n\
             Total file size: {} bytes\n\
             Total transferred size: {} bytes\n\
             Literal data: {} bytes\n\
             Matched data: {} bytes\n\
             Speedup: {:.2}",
            self.num_files,
            self.num_transferred,
            self.total_size,
            self.transferred_size,
            self.literal_data,
            self.matched_data,
            speedup,
        )
    }
}
```

### TestFixture Integration Pattern

Comprehensive test fixture for integration testing:

```rust
// tests/integration/harness.rs

/// Test fixture for rsync integration tests
pub struct TestFixture {
    pub source_dir: PathBuf,
    pub dest_dir: PathBuf,
    _temp: TempDir,
}

impl TestFixture {
    /// Create new test fixture with source and destination directories
    pub fn new() -> Self {
        let temp = TempDir::new().expect("create temp dir");
        let source_dir = temp.path().join("source");
        let dest_dir = temp.path().join("dest");

        fs::create_dir_all(&source_dir).expect("create source dir");
        fs::create_dir_all(&dest_dir).expect("create dest dir");

        Self {
            source_dir,
            dest_dir,
            _temp: temp,
        }
    }

    /// Create file with content in source directory
    pub fn create_source_file(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self.source_dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(&path, content).expect("write file");
        path
    }

    /// Create file with random content
    pub fn create_random_file(&self, name: &str, size: usize) -> PathBuf {
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        self.create_source_file(name, &content)
    }

    /// Create directory structure
    pub fn create_tree(&self, files: &[(&str, &[u8])]) {
        for (name, content) in files {
            self.create_source_file(name, content);
        }
    }

    /// Compare source and destination directories
    pub fn assert_dirs_equal(&self) {
        assert_dirs_equal(&self.source_dir, &self.dest_dir);
    }

    /// Run oc-rsync with given arguments
    pub fn run_rsync(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_oc-rsync"));
        cmd.args(args)
            .arg(self.source_dir.to_str().unwrap())
            .arg(self.dest_dir.to_str().unwrap());
        cmd.output().expect("run rsync")
    }
}
```

### WireCapture for Compatibility Testing

Wire-level capture helper for protocol compatibility testing:

```rust
// tests/integration/harness.rs

/// Wire compatibility test helper
pub struct WireCapture {
    pub sent: Vec<u8>,
    pub received: Vec<u8>,
}

impl WireCapture {
    pub fn new() -> Self {
        Self {
            sent: Vec::new(),
            received: Vec::new(),
        }
    }

    /// Record sent bytes
    pub fn record_send(&mut self, data: &[u8]) {
        self.sent.extend_from_slice(data);
    }

    /// Record received bytes
    pub fn record_recv(&mut self, data: &[u8]) {
        self.received.extend_from_slice(data);
    }

    /// Compare captured bytes against upstream rsync capture
    pub fn assert_matches_upstream(&self, expected_sent: &[u8], expected_received: &[u8]) {
        assert_eq!(
            &self.sent, expected_sent,
            "sent bytes don't match upstream"
        );
        assert_eq!(
            &self.received, expected_received,
            "received bytes don't match upstream"
        );
    }

    /// Save capture to files for analysis
    pub fn save_to_files(&self, prefix: &str) -> io::Result<()> {
        fs::write(format!("{}_sent.bin", prefix), &self.sent)?;
        fs::write(format!("{}_recv.bin", prefix), &self.received)?;
        Ok(())
    }
}

// Example usage in tests
#[test]
fn test_protocol_wire_format() {
    let mut capture = WireCapture::new();

    // Perform transfer with capture enabled
    // ...

    // Compare against golden files
    let expected_sent = include_bytes!("golden/handshake_sent.bin");
    let expected_recv = include_bytes!("golden/handshake_recv.bin");
    capture.assert_matches_upstream(expected_sent, expected_recv);
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

## Testing Patterns

### 5.1 Integration Test Harness

The workspace uses a standardized test fixture pattern for integration tests:

```rust
// Example test setup pattern
use tempfile::TempDir;

fn setup_test_dirs() -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().expect("create temp dir");
    let source = temp.path().join("source");
    let dest = temp.path().join("dest");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    (temp, source, dest)
}

// Create test files
fn create_test_file(dir: &Path, name: &str, content: &[u8]) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, content).unwrap();
}
```

### 5.2 Wire Compatibility Testing

Golden byte tests ensure wire-level compatibility with upstream rsync:

- Golden files in `crates/protocol/tests/golden/` contain captured byte streams
- Tests compare encoded output against expected bytes exactly
- Any wire format change must update golden files and be reviewed carefully

```rust
// Golden test pattern
#[test]
fn test_handshake_matches_golden() {
    let expected = include_bytes!("golden/protocol32_handshake.bin");
    let actual = encode_handshake(protocol_version);
    assert_eq!(&actual[..], &expected[..]);
}
```

### 5.3 Property-Based Testing

Use property tests for algorithmic correctness:

```rust
// Rolling checksum property: roll matches full recomputation
#[test]
fn rolling_matches_full() {
    let data = b"Hello, rsync world!";
    let block_size = 5;

    let full = RollingChecksum::compute(&data[1..block_size + 1]);

    let mut rolling = RollingChecksum::new();
    rolling.update(&data[0..block_size]);
    rolling.roll(data[0], data[block_size]);

    assert_eq!(rolling.digest(), full);
}
```

### 5.4 Interop Testing

Test against multiple upstream rsync versions:

```bash
# Run interop tests
cargo nextest run -E 'test(interop)'

# Test with specific upstream version
target/interop/upstream-install/3.4.1/bin/rsync --version
```

**Tested configurations:**

| Test | Client | Server | Protocol |
|------|--------|--------|----------|
| Push to daemon | oc-rsync | upstream 3.4.1 | 32 |
| Pull from daemon | oc-rsync | upstream 3.4.1 | 32 |
| Serve to client | upstream 3.4.1 | oc-rsync | 32 |
| Receive from client | upstream 3.4.1 | oc-rsync | 32 |

### 5.5 Directory Comparison

After transfer tests, verify source and destination match:

```rust
// Recursive directory comparison
fn assert_dirs_equal(a: &Path, b: &Path) {
    let a_files: Vec<_> = walkdir::WalkDir::new(a)
        .into_iter()
        .filter_map(Result::ok)
        .collect();

    for entry in a_files {
        let rel = entry.path().strip_prefix(a).unwrap();
        let b_path = b.join(rel);
        assert!(b_path.exists(), "missing: {:?}", rel);

        if entry.file_type().is_file() {
            let a_content = std::fs::read(entry.path()).unwrap();
            let b_content = std::fs::read(&b_path).unwrap();
            assert_eq!(a_content, b_content, "content mismatch: {:?}", rel);
        }
    }
}
```

### 5.6 Environment Isolation

Tests that modify environment variables must use guards:

```rust
// EnvGuard pattern for test isolation
struct EnvGuard {
    key: String,
    original: Option<String>,
}

impl EnvGuard {
    fn set(key: &str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key: key.to_string(), original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(val) => std::env::set_var(&self.key, val),
            None => std::env::remove_var(&self.key),
        }
    }
}
```

---

## 6. Core Traits and Interfaces

### 6.1 RollingChecksum Trait

The rolling checksum trait abstracts over different rolling hash implementations:

```rust
// crates/checksums/src/rolling.rs

/// Trait for rolling checksum algorithms
pub trait RollingChecksum: Default + Clone {
    /// Update with a slice of bytes (initial window)
    fn update(&mut self, data: &[u8]);

    /// Get the current checksum value
    fn digest(&self) -> u32;

    /// Roll the window by one byte
    ///
    /// Removes `old_byte` from the beginning and adds `new_byte` at the end.
    fn roll(&mut self, old_byte: u8, new_byte: u8);

    /// Compute checksum for entire buffer (convenience method)
    fn compute(data: &[u8]) -> u32 {
        let mut hasher = Self::default();
        hasher.update(data);
        hasher.digest()
    }
}
```

### 6.2 StrongChecksum Trait

Strong checksums provide cryptographic-strength verification:

```rust
// crates/checksums/src/strong.rs

/// Trait for strong checksum algorithms (MD4, MD5, XXH3, etc.)
pub trait StrongChecksum: Default {
    /// The output type for this checksum
    type Output: AsRef<[u8]> + Clone;

    /// Add data to the running checksum
    fn update(&mut self, data: &[u8]);

    /// Finalize and return the checksum
    fn finalize(self) -> Self::Output;

    /// One-shot: compute checksum for entire buffer
    fn compute(data: &[u8]) -> Self::Output {
        let mut hasher = Self::default();
        hasher.update(data);
        hasher.finalize()
    }

    /// Length of the checksum output in bytes
    fn output_len() -> usize;
}
```

### 6.3 DeltaOp Enum

Delta operations represent the instructions in a delta file:

```rust
// crates/engine/src/delta/ops.rs

/// A delta operation (instruction for reconstructing target from basis)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeltaOp {
    /// Copy `len` bytes from basis file starting at `offset`
    Copy { offset: u64, len: u64 },

    /// Insert literal data (not in basis)
    Literal(Vec<u8>),

    /// End of delta stream
    End,
}

impl DeltaOp {
    /// Size of this operation when serialized
    pub fn wire_size(&self) -> usize {
        match self {
            DeltaOp::Copy { .. } => 1 + 4 + 4,  // token + offset + len
            DeltaOp::Literal(data) => 1 + 4 + data.len(),  // token + len + data
            DeltaOp::End => 4,  // zero token
        }
    }

    /// Whether this is a copy operation
    pub fn is_copy(&self) -> bool {
        matches!(self, DeltaOp::Copy { .. })
    }
}
```

---

## 7. Signature Generation and Delta Encoding

### 7.1 SignatureConfig

Configuration for signature generation:

```rust
// crates/checksums/src/signature.rs

/// Configuration for generating file signatures
#[derive(Debug, Clone)]
pub struct SignatureConfig {
    /// Block size for chunking (default: 700 bytes for small files, scaled up for larger)
    pub block_size: u32,

    /// Number of bytes from strong checksum to use (2-16, protocol dependent)
    pub strong_len: usize,

    /// Checksum seed for rolling hash (from protocol negotiation)
    pub checksum_seed: u32,

    /// Strong checksum algorithm to use
    pub strong_type: StrongChecksumType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrongChecksumType {
    Md4,      // Protocol < 30
    Md5,      // Protocol 30
    Xxh3,     // Protocol 31+
}

impl SignatureConfig {
    /// Create config for a file of given size
    pub fn for_file_size(file_size: u64, protocol_version: u8) -> Self {
        // Upstream rsync algorithm for block size selection
        let block_size = if file_size <= 0x40000 {
            700  // 64KB or less
        } else {
            // Scale up for larger files
            let blength = file_size / 10000;
            blength.min(131072).max(700) as u32
        };

        Self {
            block_size,
            strong_len: if protocol_version >= 31 { 16 } else { 2 },
            checksum_seed: 0,
            strong_type: match protocol_version {
                0..=29 => StrongChecksumType::Md4,
                30 => StrongChecksumType::Md5,
                _ => StrongChecksumType::Xxh3,
            },
        }
    }
}
```

### 7.2 Signature Generation

Generate signatures for delta comparison:

```rust
// crates/checksums/src/signature.rs

/// A block signature (rolling + strong checksum pair)
#[derive(Debug, Clone)]
pub struct BlockSignature {
    pub rolling: u32,
    pub strong: Vec<u8>,
    pub block_index: u32,
}

/// Generate signatures for a file
pub fn generate_signatures<R: Read>(
    reader: &mut R,
    config: &SignatureConfig,
) -> io::Result<Vec<BlockSignature>> {
    let mut signatures = Vec::new();
    let mut buffer = vec![0u8; config.block_size as usize];
    let mut block_index = 0u32;

    loop {
        let bytes_read = read_full_or_eof(reader, &mut buffer)?;
        if bytes_read == 0 {
            break;
        }

        let data = &buffer[..bytes_read];

        // Compute rolling checksum
        let rolling = RsyncRolling::compute(data);

        // Compute strong checksum (truncated to strong_len)
        let strong = match config.strong_type {
            StrongChecksumType::Md4 => compute_md4(data, config.checksum_seed),
            StrongChecksumType::Md5 => compute_md5(data, config.checksum_seed),
            StrongChecksumType::Xxh3 => compute_xxh3(data, config.checksum_seed),
        };

        signatures.push(BlockSignature {
            rolling,
            strong: strong[..config.strong_len].to_vec(),
            block_index,
        });

        block_index += 1;
    }

    Ok(signatures)
}
```

### 7.3 SignatureTable for Delta Generation

Hash table for O(1) rolling checksum lookup:

```rust
// crates/engine/src/delta/table.rs

use std::collections::HashMap;

/// Hash table mapping rolling checksums to potential block matches
pub struct SignatureTable {
    /// Map from rolling checksum to list of (block_index, strong_checksum)
    table: HashMap<u32, Vec<(u32, Vec<u8>)>>,
    block_size: u32,
}

impl SignatureTable {
    /// Build table from received signatures
    pub fn from_signatures(sigs: Vec<BlockSignature>, block_size: u32) -> Self {
        let mut table: HashMap<u32, Vec<(u32, Vec<u8>)>> = HashMap::new();

        for sig in sigs {
            table
                .entry(sig.rolling)
                .or_default()
                .push((sig.block_index, sig.strong));
        }

        Self { table, block_size }
    }

    /// Look up potential matches for a rolling checksum
    pub fn find_matches(&self, rolling: u32) -> Option<&[(u32, Vec<u8>)]> {
        self.table.get(&rolling).map(|v| v.as_slice())
    }

    pub fn block_size(&self) -> u32 {
        self.block_size
    }
}
```

### 7.4 Delta Generation Algorithm

The core delta generation with rolling window search:

```rust
// crates/engine/src/delta/generator.rs

/// Generate delta operations by comparing new file against signature table
pub fn generate_delta<R: Read>(
    reader: &mut R,
    table: &SignatureTable,
    config: &SignatureConfig,
) -> io::Result<Vec<DeltaOp>> {
    let block_size = table.block_size() as usize;
    let mut delta_ops = Vec::new();
    let mut literal_buffer = Vec::new();

    // Read entire file into memory for rolling window
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;

    if data.is_empty() {
        delta_ops.push(DeltaOp::End);
        return Ok(delta_ops);
    }

    let mut pos = 0;
    let mut rolling = RsyncRolling::new();

    // Initialize rolling checksum with first block
    let initial_len = block_size.min(data.len());
    rolling.update(&data[..initial_len]);

    while pos + block_size <= data.len() {
        let digest = rolling.digest();

        // Check for rolling checksum match
        if let Some(matches) = table.find_matches(digest) {
            let block_data = &data[pos..pos + block_size];
            let strong = compute_strong(block_data, config);

            // Verify with strong checksum
            if let Some((block_idx, _)) = matches.iter().find(|(_, s)| s == &strong) {
                // Flush any pending literal data
                if !literal_buffer.is_empty() {
                    delta_ops.push(DeltaOp::Literal(std::mem::take(&mut literal_buffer)));
                }

                // Emit copy operation
                delta_ops.push(DeltaOp::Copy {
                    offset: (*block_idx as u64) * (block_size as u64),
                    len: block_size as u64,
                });

                pos += block_size;

                // Re-initialize rolling checksum for next block
                if pos + block_size <= data.len() {
                    rolling = RsyncRolling::new();
                    rolling.update(&data[pos..pos + block_size]);
                }
                continue;
            }
        }

        // No match - add byte to literal buffer and roll window
        literal_buffer.push(data[pos]);

        if pos + block_size < data.len() {
            rolling.roll(data[pos], data[pos + block_size]);
        }
        pos += 1;
    }

    // Handle remaining bytes as literal
    literal_buffer.extend_from_slice(&data[pos..]);
    if !literal_buffer.is_empty() {
        delta_ops.push(DeltaOp::Literal(literal_buffer));
    }

    delta_ops.push(DeltaOp::End);
    Ok(delta_ops)
}
```

### 7.5 DeltaApplicator

Apply delta operations to reconstruct target file:

```rust
// crates/engine/src/delta/applicator.rs

/// Applies delta operations to a basis file to produce target
pub struct DeltaApplicator<R: Read + Seek, W: Write> {
    basis: R,
    output: W,
    stats: ApplyStats,
}

#[derive(Debug, Default)]
pub struct ApplyStats {
    pub bytes_copied: u64,
    pub bytes_literal: u64,
    pub copy_ops: u64,
    pub literal_ops: u64,
}

impl<R: Read + Seek, W: Write> DeltaApplicator<R, W> {
    pub fn new(basis: R, output: W) -> Self {
        Self {
            basis,
            output,
            stats: ApplyStats::default(),
        }
    }

    /// Apply a single delta operation
    pub fn apply_op(&mut self, op: &DeltaOp) -> io::Result<bool> {
        match op {
            DeltaOp::Copy { offset, len } => {
                self.basis.seek(io::SeekFrom::Start(*offset))?;
                let copied = io::copy(&mut self.basis.by_ref().take(*len), &mut self.output)?;
                self.stats.bytes_copied += copied;
                self.stats.copy_ops += 1;
                Ok(true)
            }
            DeltaOp::Literal(data) => {
                self.output.write_all(data)?;
                self.stats.bytes_literal += data.len() as u64;
                self.stats.literal_ops += 1;
                Ok(true)
            }
            DeltaOp::End => Ok(false),
        }
    }

    /// Apply all operations until End
    pub fn apply_all(&mut self, ops: &[DeltaOp]) -> io::Result<()> {
        for op in ops {
            if !self.apply_op(op)? {
                break;
            }
        }
        Ok(())
    }

    pub fn into_stats(self) -> ApplyStats {
        self.stats
    }
}

/// Convenience function to apply delta
pub fn apply_delta<R: Read + Seek, W: Write>(
    basis: R,
    output: W,
    ops: &[DeltaOp],
) -> io::Result<ApplyStats> {
    let mut applicator = DeltaApplicator::new(basis, output);
    applicator.apply_all(ops)?;
    Ok(applicator.into_stats())
}
```

---

## 8. Protocol Version Negotiation

### 8.1 ProtocolVersion Struct

```rust
// crates/protocol/src/version.rs

/// Protocol version with negotiation support
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProtocolVersion(u8);

impl ProtocolVersion {
    /// Current (newest) protocol version we support
    pub const CURRENT: Self = Self(32);

    /// Minimum protocol version we support
    pub const MIN_SUPPORTED: Self = Self(28);

    /// Protocol version 30 (introduces varints)
    pub const V30: Self = Self(30);

    /// Protocol version 31 (introduces XXH3)
    pub const V31: Self = Self(31);

    /// Create from raw version number
    pub fn new(version: u8) -> Option<Self> {
        if version >= Self::MIN_SUPPORTED.0 && version <= Self::CURRENT.0 {
            Some(Self(version))
        } else {
            None
        }
    }

    /// Get raw version number
    pub fn as_u8(self) -> u8 {
        self.0
    }

    /// Negotiate version with remote
    pub fn negotiate(remote: u8) -> Option<Self> {
        let negotiated = remote.min(Self::CURRENT.0);
        Self::new(negotiated)
    }

    /// Check if varints are used
    pub fn uses_varints(self) -> bool {
        self.0 >= 30
    }

    /// Check if XXH3 checksums are available
    pub fn uses_xxh3(self) -> bool {
        self.0 >= 31
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

### 8.2 Version Parsing from Greeting

```rust
// crates/protocol/src/greeting.rs

/// Parse protocol version from daemon greeting
pub fn version_from_greeting(greeting: &str) -> Option<ProtocolVersion> {
    // Greeting format: "@RSYNCD: 32.0"
    let stripped = greeting.strip_prefix("@RSYNCD: ")?;
    let major = stripped.split('.').next()?;
    let version: u8 = major.parse().ok()?;
    ProtocolVersion::new(version)
}

/// Generate greeting string for our version
pub fn make_greeting(version: ProtocolVersion) -> String {
    format!("@RSYNCD: {}.0\n", version.as_u8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_greeting() {
        assert_eq!(
            version_from_greeting("@RSYNCD: 32.0"),
            Some(ProtocolVersion::CURRENT)
        );
        assert_eq!(
            version_from_greeting("@RSYNCD: 30.0"),
            Some(ProtocolVersion::V30)
        );
        assert_eq!(version_from_greeting("@RSYNCD: 27.0"), None);
    }
}
```

---

## 9. File List Generation

### 9.1 FileListOptions

Configuration for directory traversal:

```rust
// crates/walk/src/options.rs

/// Options controlling file list generation
#[derive(Debug, Clone)]
pub struct FileListOptions {
    /// Follow symlinks when reading source
    pub follow_symlinks: bool,

    /// Include directories in file list
    pub include_directories: bool,

    /// Include special files (devices, sockets, etc.)
    pub include_specials: bool,

    /// Maximum recursion depth (None = unlimited)
    pub max_depth: Option<usize>,

    /// Cross filesystem boundaries
    pub cross_filesystems: bool,

    /// Include hidden files (starting with .)
    pub include_hidden: bool,

    /// Sort entries (required for protocol compliance)
    pub sort_entries: bool,
}

impl Default for FileListOptions {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            include_directories: true,
            include_specials: false,
            max_depth: None,
            cross_filesystems: false,
            include_hidden: true,
            sort_entries: true,  // Required for rsync protocol
        }
    }
}

impl FileListOptions {
    /// Options for recursive directory sync (-r)
    pub fn recursive() -> Self {
        Self::default()
    }

    /// Options for archive mode (-a)
    pub fn archive() -> Self {
        Self {
            include_specials: true,
            ..Self::default()
        }
    }
}
```

### 9.2 Directory Walking

```rust
// crates/walk/src/walker.rs

/// Walk a directory tree and generate file entries
pub fn walk_directory(
    root: &Path,
    options: &FileListOptions,
) -> io::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    walk_recursive(root, root, options, 0, &mut entries)?;

    if options.sort_entries {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
    }

    Ok(entries)
}

fn walk_recursive(
    root: &Path,
    current: &Path,
    options: &FileListOptions,
    depth: usize,
    entries: &mut Vec<FileEntry>,
) -> io::Result<()> {
    if let Some(max) = options.max_depth {
        if depth > max {
            return Ok(());
        }
    }

    let iter = std::fs::read_dir(current)?;

    for entry_result in iter {
        let entry = entry_result?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Skip hidden files if requested
        if !options.include_hidden && name_str.starts_with('.') {
            continue;
        }

        let metadata = if options.follow_symlinks {
            std::fs::metadata(&path)?
        } else {
            std::fs::symlink_metadata(&path)?
        };

        let relative = path.strip_prefix(root)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let file_entry = FileEntry {
            path: relative.to_path_buf(),
            mode: metadata.permissions().mode(),
            size: metadata.len(),
            mtime: metadata.modified()?.into(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            link_target: if metadata.is_symlink() {
                Some(std::fs::read_link(&path)?)
            } else {
                None
            },
        };

        if metadata.is_dir() {
            if options.include_directories {
                entries.push(file_entry);
            }
            if !options.cross_filesystems {
                // Check if we're crossing filesystem boundaries
                // (simplified - full impl checks device IDs)
            }
            walk_recursive(root, &path, options, depth + 1, entries)?;
        } else if metadata.is_file() {
            entries.push(file_entry);
        } else if options.include_specials {
            entries.push(file_entry);
        }
    }

    Ok(())
}
```

---

## 10. Atomic File Operations

### 10.1 AtomicFile Struct

Safe atomic file writes with temp file → sync → rename:

```rust
// crates/engine/src/atomic.rs

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Atomic file writer: writes to temp file, then renames on commit
pub struct AtomicFile {
    /// Final destination path
    target: PathBuf,
    /// Temporary file path
    temp_path: PathBuf,
    /// Open file handle
    file: Option<File>,
    /// Whether to sync before rename
    sync_on_commit: bool,
}

impl AtomicFile {
    /// Create a new atomic file writer
    pub fn new(target: impl Into<PathBuf>) -> io::Result<Self> {
        let target = target.into();
        let temp_path = Self::temp_path_for(&target);

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)?;

        Ok(Self {
            target,
            temp_path,
            file: Some(file),
            sync_on_commit: true,
        })
    }

    /// Generate temp path in same directory as target
    fn temp_path_for(target: &Path) -> PathBuf {
        let parent = target.parent().unwrap_or(Path::new("."));
        let file_name = target.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        let pid = std::process::id();
        parent.join(format!(".{}.{}.tmp", file_name, pid))
    }

    /// Disable sync before rename (faster but less safe)
    pub fn without_sync(mut self) -> Self {
        self.sync_on_commit = false;
        self
    }

    /// Write data to the temp file
    pub fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "file already committed"))?
            .write_all(data)
    }

    /// Commit: sync and rename temp to target
    pub fn commit(mut self) -> io::Result<()> {
        let file = self.file.take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "file already committed"))?;

        if self.sync_on_commit {
            file.sync_all()?;
        }
        drop(file);

        fs::rename(&self.temp_path, &self.target)?;
        Ok(())
    }

    /// Abort: remove temp file without committing
    pub fn abort(mut self) -> io::Result<()> {
        self.file.take();
        let _ = fs::remove_file(&self.temp_path);
        Ok(())
    }
}

impl Write for AtomicFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "file already committed"))?
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "file already committed"))?
            .flush()
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        // Clean up temp file if not committed
        if self.file.is_some() {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}
```

---

## 11. Filter System

### 11.1 FilterRule Struct

```rust
// crates/filters/src/rule.rs

/// A single filter rule
#[derive(Debug, Clone)]
pub struct FilterRule {
    /// The pattern to match (glob or path)
    pub pattern: String,

    /// Type of rule
    pub rule_type: RuleType,

    /// Pattern modifiers
    pub modifiers: RuleModifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleType {
    Include,
    Exclude,
    Merge,      // Read rules from file
    DirMerge,   // Per-directory merge file
    Hide,       // Hide from transfer
    Show,
    Protect,    // Protect from deletion
    Risk,
    Clear,      // Clear current rules
}

#[derive(Debug, Clone, Default)]
pub struct RuleModifiers {
    /// Match only directories
    pub dir_only: bool,
    /// Pattern is anchored to transfer root
    pub anchored: bool,
    /// Pattern should not match path components
    pub no_wildcards: bool,
    /// Case insensitive matching
    pub case_insensitive: bool,
    /// Perishable (can be removed if parent dir removed)
    pub perishable: bool,
}

impl FilterRule {
    /// Parse a filter rule from string (e.g., "+ *.rs", "- /build/")
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }

        let (rule_type, rest) = if line.starts_with("+ ") {
            (RuleType::Include, &line[2..])
        } else if line.starts_with("- ") {
            (RuleType::Exclude, &line[2..])
        } else if line.starts_with(". ") {
            (RuleType::Merge, &line[2..])
        } else if line.starts_with(": ") {
            (RuleType::DirMerge, &line[2..])
        } else {
            // Default to exclude
            (RuleType::Exclude, line)
        };

        let mut modifiers = RuleModifiers::default();
        let pattern = rest.to_string();

        // Check for directory-only pattern
        if pattern.ends_with('/') {
            modifiers.dir_only = true;
        }

        // Check for anchored pattern
        if pattern.starts_with('/') {
            modifiers.anchored = true;
        }

        Some(Self {
            pattern,
            rule_type,
            modifiers,
        })
    }
}
```

### 11.2 FilterSet and FilterChain

```rust
// crates/filters/src/set.rs

/// A set of filter rules applied in order
#[derive(Debug, Clone, Default)]
pub struct FilterSet {
    rules: Vec<FilterRule>,
}

impl FilterSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a rule to the set
    pub fn add(&mut self, rule: FilterRule) {
        self.rules.push(rule);
    }

    /// Test a path against all rules
    ///
    /// Returns Some(true) for include, Some(false) for exclude, None for no match
    pub fn test(&self, path: &Path, is_dir: bool) -> Option<bool> {
        for rule in &self.rules {
            if rule.modifiers.dir_only && !is_dir {
                continue;
            }

            if self.matches(&rule.pattern, path, &rule.modifiers) {
                return match rule.rule_type {
                    RuleType::Include | RuleType::Show => Some(true),
                    RuleType::Exclude | RuleType::Hide => Some(false),
                    _ => None,
                };
            }
        }
        None
    }

    fn matches(&self, pattern: &str, path: &Path, modifiers: &RuleModifiers) -> bool {
        let path_str = path.to_string_lossy();
        let pattern = pattern.trim_end_matches('/');

        // Simple glob matching (full impl uses proper glob crate)
        if pattern.contains('*') {
            glob_match(pattern, &path_str, modifiers.case_insensitive)
        } else if modifiers.anchored {
            path_str.starts_with(pattern.trim_start_matches('/'))
        } else {
            path_str.contains(pattern) ||
                path.file_name().map(|n| n.to_string_lossy().contains(pattern)).unwrap_or(false)
        }
    }
}

/// Chain of filter sets (supports per-directory rules)
#[derive(Debug, Default)]
pub struct FilterChain {
    /// Global rules
    global: FilterSet,
    /// Per-directory rule stacks
    dir_rules: Vec<(PathBuf, FilterSet)>,
}

impl FilterChain {
    /// Load filters from a file
    pub fn from_filter_file(path: &Path) -> io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut chain = Self::default();

        for line in content.lines() {
            if let Some(rule) = FilterRule::parse(line) {
                chain.global.add(rule);
            }
        }

        Ok(chain)
    }

    /// Test a path against all applicable rules
    pub fn test(&self, path: &Path, is_dir: bool) -> bool {
        // Check per-directory rules first (most specific)
        for (dir, rules) in self.dir_rules.iter().rev() {
            if path.starts_with(dir) {
                if let Some(result) = rules.test(path, is_dir) {
                    return result;
                }
            }
        }

        // Fall back to global rules
        self.global.test(path, is_dir).unwrap_or(true)
    }
}

fn glob_match(pattern: &str, text: &str, case_insensitive: bool) -> bool {
    let pattern = if case_insensitive { pattern.to_lowercase() } else { pattern.to_string() };
    let text = if case_insensitive { text.to_lowercase() } else { text.to_string() };

    // Simple glob: * matches anything, ** matches path separators
    let pattern = pattern.replace("**", "\x00");
    let pattern = pattern.replace("*", "[^/]*");
    let pattern = pattern.replace("\x00", ".*");

    regex::Regex::new(&format!("^{}$", pattern))
        .map(|re| re.is_match(&text))
        .unwrap_or(false)
}
```

---

## 12. Daemon Authentication

### 12.1 AuthMethod Enum

```rust
// crates/core/src/auth/method.rs

/// Authentication methods supported by daemon
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// No authentication required
    Anonymous,

    /// Challenge-response authentication
    Challenge {
        user: String,
        challenge: [u8; 16],
    },

    /// Plain password (for secrets file lookup)
    Password {
        user: String,
    },
}

impl AuthMethod {
    /// Determine auth method for a module
    pub fn for_module(config: &ModuleConfig, client_user: Option<&str>) -> Self {
        match (&config.auth_users, client_user) {
            (None, _) => AuthMethod::Anonymous,
            (Some(users), Some(user)) if users.contains(&user.to_string()) => {
                AuthMethod::Challenge {
                    user: user.to_string(),
                    challenge: rand::random(),
                }
            }
            (Some(_), Some(user)) => {
                AuthMethod::Password {
                    user: user.to_string(),
                }
            }
            (Some(_), None) => AuthMethod::Anonymous,
        }
    }

    /// Verify client response to challenge
    pub fn verify_response(&self, response: &[u8], secrets: &SecretsFile) -> bool {
        match self {
            AuthMethod::Challenge { user, challenge } => {
                if let Some(password) = secrets.lookup(user) {
                    let expected = compute_challenge_response(challenge, password);
                    constant_time_eq(&expected, response)
                } else {
                    false
                }
            }
            AuthMethod::Anonymous => true,
            AuthMethod::Password { user } => {
                secrets.lookup(user).is_some()
            }
        }
    }
}

fn compute_challenge_response(challenge: &[u8; 16], password: &str) -> [u8; 16] {
    use md4::{Md4, Digest};
    let mut hasher = Md4::new();
    hasher.update(challenge);
    hasher.update(password.as_bytes());
    let result = hasher.finalize();
    let mut output = [0u8; 16];
    output.copy_from_slice(&result);
    output
}
```

### 12.2 ConnectionState Machine

```rust
// crates/daemon/src/connection.rs

/// States in the daemon connection lifecycle
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Initial state - waiting for client version
    AwaitingGreeting,

    /// Version exchanged - waiting for module selection
    ModuleSelect {
        client_version: ProtocolVersion,
    },

    /// Module selected - authentication in progress
    Authenticating {
        module: String,
        auth_method: AuthMethod,
    },

    /// Authenticated - transfer in progress
    Transferring {
        module: String,
        is_sender: bool,
    },

    /// Closing connection
    Closing {
        error: Option<String>,
    },

    /// Connection closed
    Closed,
}

impl ConnectionState {
    /// Get allowed transitions from current state
    pub fn allowed_transitions(&self) -> Vec<ConnectionState> {
        match self {
            ConnectionState::AwaitingGreeting => vec![
                ConnectionState::ModuleSelect { client_version: ProtocolVersion::CURRENT },
                ConnectionState::Closing { error: None },
            ],
            ConnectionState::ModuleSelect { .. } => vec![
                ConnectionState::Authenticating {
                    module: String::new(),
                    auth_method: AuthMethod::Anonymous,
                },
                ConnectionState::Closing { error: None },
            ],
            ConnectionState::Authenticating { module, .. } => vec![
                ConnectionState::Transferring {
                    module: module.clone(),
                    is_sender: true,
                },
                ConnectionState::Closing { error: None },
            ],
            ConnectionState::Transferring { .. } => vec![
                ConnectionState::Closing { error: None },
            ],
            ConnectionState::Closing { .. } => vec![
                ConnectionState::Closed,
            ],
            ConnectionState::Closed => vec![],
        }
    }
}
```

---

## 13. Metadata Operations

### 13.1 MetadataOptions

```rust
// crates/metadata/src/options.rs

/// Options controlling metadata preservation
#[derive(Debug, Clone)]
pub struct MetadataOptions {
    /// Preserve file permissions (-p)
    pub preserve_permissions: bool,

    /// Preserve owner and group (-o, -g)
    pub preserve_ownership: bool,

    /// Preserve modification times (-t)
    pub preserve_times: bool,

    /// Preserve extended attributes (-X)
    pub preserve_xattrs: bool,

    /// Preserve ACLs (-A)
    pub preserve_acls: bool,

    /// Preserve device and special files (-D)
    pub preserve_devices: bool,

    /// Numeric IDs instead of names
    pub numeric_ids: bool,

    /// Default permissions for new files
    pub default_file_mode: u32,

    /// Default permissions for new directories
    pub default_dir_mode: u32,
}

impl Default for MetadataOptions {
    fn default() -> Self {
        Self {
            preserve_permissions: false,
            preserve_ownership: false,
            preserve_times: false,
            preserve_xattrs: false,
            preserve_acls: false,
            preserve_devices: false,
            numeric_ids: false,
            default_file_mode: 0o644,
            default_dir_mode: 0o755,
        }
    }
}

impl MetadataOptions {
    /// Archive mode (-a): recursive + preserve all
    pub fn archive() -> Self {
        Self {
            preserve_permissions: true,
            preserve_ownership: true,
            preserve_times: true,
            preserve_devices: true,
            ..Self::default()
        }
    }
}
```

### 13.2 Metadata Application

```rust
// crates/metadata/src/apply.rs

use std::os::unix::fs::{PermissionsExt, chown};

/// Apply metadata to a file
pub fn apply_metadata(
    path: &Path,
    entry: &FileEntry,
    options: &MetadataOptions,
) -> io::Result<()> {
    // Permissions
    if options.preserve_permissions {
        let perms = std::fs::Permissions::from_mode(entry.mode);
        std::fs::set_permissions(path, perms)?;
    }

    // Ownership (requires root)
    if options.preserve_ownership {
        let uid = if options.numeric_ids {
            entry.uid
        } else {
            // Map name to local UID (simplified)
            entry.uid
        };
        let gid = if options.numeric_ids {
            entry.gid
        } else {
            entry.gid
        };

        // Note: chown requires nix crate on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::chown;
            let _ = chown(path, Some(uid), Some(gid));
        }
    }

    // Modification time
    if options.preserve_times {
        let mtime = filetime::FileTime::from_system_time(entry.mtime);
        filetime::set_file_mtime(path, mtime)?;
    }

    // Extended attributes
    if options.preserve_xattrs {
        apply_xattrs(path, &entry.xattrs)?;
    }

    // ACLs
    if options.preserve_acls {
        apply_acls(path, &entry.acl)?;
    }

    Ok(())
}

fn apply_xattrs(path: &Path, xattrs: &[(String, Vec<u8>)]) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        for (name, value) in xattrs {
            xattr::set(path, name, value)?;
        }
    }
    Ok(())
}

fn apply_acls(path: &Path, acl: &Option<Vec<u8>>) -> io::Result<()> {
    // ACL application is platform-specific
    // Uses exacl crate on Linux/macOS
    if let Some(_acl_data) = acl {
        #[cfg(target_os = "linux")]
        {
            // exacl::setfacl(path, acl_data)?;
        }
    }
    Ok(())
}
```

### 13.3 XattrHandler Trait

```rust
// crates/metadata/src/xattr.rs

/// Trait for extended attribute operations
pub trait XattrHandler {
    /// List all extended attributes on a path
    fn list(&self, path: &Path) -> io::Result<Vec<String>>;

    /// Get an extended attribute value
    fn get(&self, path: &Path, name: &str) -> io::Result<Option<Vec<u8>>>;

    /// Set an extended attribute
    fn set(&self, path: &Path, name: &str, value: &[u8]) -> io::Result<()>;

    /// Remove an extended attribute
    fn remove(&self, path: &Path, name: &str) -> io::Result<()>;
}

/// Default implementation using xattr crate
#[derive(Debug, Default)]
pub struct DefaultXattrHandler;

impl XattrHandler for DefaultXattrHandler {
    fn list(&self, path: &Path) -> io::Result<Vec<String>> {
        #[cfg(target_os = "linux")]
        {
            xattr::list(path)
                .map(|iter| iter.filter_map(|r| r.ok()).map(|n| n.to_string_lossy().into_owned()).collect())
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(Vec::new())
        }
    }

    fn get(&self, path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
        #[cfg(target_os = "linux")]
        {
            match xattr::get(path, name) {
                Ok(val) => Ok(val),
                Err(e) if e.raw_os_error() == Some(libc::ENODATA) => Ok(None),
                Err(e) => Err(io::Error::new(io::ErrorKind::Other, e)),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(None)
        }
    }

    fn set(&self, path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            xattr::set(path, name, value)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(())
        }
    }

    fn remove(&self, path: &Path, name: &str) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            xattr::remove(path, name)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(())
        }
    }
}
```

### 13.4 AclHandler Trait

```rust
// crates/metadata/src/acl.rs

/// Trait for ACL operations
pub trait AclHandler {
    /// Get ACL for a path
    fn get(&self, path: &Path) -> io::Result<Option<Vec<u8>>>;

    /// Set ACL on a path
    fn set(&self, path: &Path, acl: &[u8]) -> io::Result<()>;
}

/// Default ACL handler using exacl crate
#[derive(Debug, Default)]
pub struct DefaultAclHandler;

impl AclHandler for DefaultAclHandler {
    fn get(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        #[cfg(target_os = "linux")]
        {
            // exacl::getfacl returns structured ACL
            // Serialize to bytes for wire transfer
            match exacl::getfacl(path, None) {
                Ok(acl) => {
                    let serialized = serialize_acl(&acl);
                    Ok(Some(serialized))
                }
                Err(_) => Ok(None),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(None)
        }
    }

    fn set(&self, path: &Path, acl: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let acl = deserialize_acl(acl)?;
            exacl::setfacl(&[path], &acl, None)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(())
        }
    }
}
```

---

## 14. Performance Considerations

### 14.1 Memory-Mapped I/O

For large files, memory-mapping improves performance:

```rust
// crates/engine/src/mmap.rs

use memmap2::{Mmap, MmapOptions};
use std::fs::File;

/// Memory-mapped file for efficient random access
pub struct MappedFile {
    mmap: Mmap,
}

impl MappedFile {
    /// Create a memory-mapped view of a file
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self { mmap })
    }

    /// Get file contents as a slice
    pub fn as_slice(&self) -> &[u8] {
        &self.mmap[..]
    }

    /// Get a specific range
    pub fn range(&self, offset: usize, len: usize) -> &[u8] {
        &self.mmap[offset..offset + len]
    }
}

impl AsRef<[u8]> for MappedFile {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}
```

### 14.2 Buffer Pool

Reuse buffers to reduce allocations:

```rust
// crates/engine/src/buffer.rs

use std::sync::Mutex;

/// Thread-safe buffer pool for reducing allocations
pub struct BufferPool {
    buffers: Mutex<Vec<Vec<u8>>>,
    buffer_size: usize,
    max_buffers: usize,
}

impl BufferPool {
    pub fn new(buffer_size: usize, max_buffers: usize) -> Self {
        Self {
            buffers: Mutex::new(Vec::with_capacity(max_buffers)),
            buffer_size,
            max_buffers,
        }
    }

    /// Acquire a buffer from the pool
    pub fn acquire(&self) -> PooledBuffer {
        let buffer = self.buffers.lock().unwrap().pop()
            .unwrap_or_else(|| vec![0u8; self.buffer_size]);
        PooledBuffer {
            buffer,
            pool: self,
        }
    }

    /// Return a buffer to the pool
    fn release(&self, mut buffer: Vec<u8>) {
        let mut buffers = self.buffers.lock().unwrap();
        if buffers.len() < self.max_buffers {
            buffer.clear();
            buffers.push(buffer);
        }
        // Otherwise, buffer is dropped
    }
}

/// RAII buffer that returns to pool on drop
pub struct PooledBuffer<'a> {
    buffer: Vec<u8>,
    pool: &'a BufferPool,
}

impl<'a> std::ops::Deref for PooledBuffer<'a> {
    type Target = Vec<u8>;
    fn deref(&self) -> &Self::Target {
        &self.buffer
    }
}

impl<'a> std::ops::DerefMut for PooledBuffer<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffer
    }
}

impl<'a> Drop for PooledBuffer<'a> {
    fn drop(&mut self) {
        let buffer = std::mem::take(&mut self.buffer);
        self.pool.release(buffer);
    }
}
```

### 14.3 Parallel Processing

Use rayon for parallel file processing:

```rust
// Parallel signature generation
use rayon::prelude::*;

pub fn generate_signatures_parallel(
    files: &[PathBuf],
    config: &SignatureConfig,
) -> Vec<(PathBuf, Vec<BlockSignature>)> {
    files.par_iter()
        .filter_map(|path| {
            let mut file = File::open(path).ok()?;
            let sigs = generate_signatures(&mut file, config).ok()?;
            Some((path.clone(), sigs))
        })
        .collect()
}
```

---

## 15. Common Integration Patterns

### 15.1 Complete Client Example

```rust
// Full client implementation example

use core::{run_client, CoreConfig};
use transport::Connection;

pub fn sync_directory(
    source: &Path,
    dest_url: &str,  // e.g., "rsync://host/module/"
) -> Result<TransferStats, CoreError> {
    // Parse destination URL
    let (host, module, path) = parse_rsync_url(dest_url)?;

    // Build configuration
    let config = CoreConfig::builder()
        .source(source)
        .destination(&format!("{}:{}", host, module))
        .recursive(true)
        .preserve_times(true)
        .compress(true)
        .build()?;

    // Run the transfer
    run_client(config, logging::Format::default())
}
```

### 15.2 Incremental Sync with Checksums

```rust
// Incremental sync using delta transfer

pub fn sync_file_incrementally(
    source: &Path,
    basis: &Path,
    dest: &Path,
) -> io::Result<u64> {
    // Generate signatures for basis file
    let config = SignatureConfig::for_file_size(
        std::fs::metadata(basis)?.len(),
        32  // protocol version
    );

    let mut basis_file = File::open(basis)?;
    let signatures = generate_signatures(&mut basis_file, &config)?;

    // Build signature table
    let table = SignatureTable::from_signatures(signatures, config.block_size);

    // Generate delta from source
    let mut source_file = File::open(source)?;
    let delta_ops = generate_delta(&mut source_file, &table, &config)?;

    // Apply delta to create destination
    let basis_file = File::open(basis)?;
    let dest_file = AtomicFile::new(dest)?;

    let stats = apply_delta(basis_file, dest_file, &delta_ops)?;
    dest_file.commit()?;

    Ok(stats.bytes_copied + stats.bytes_literal)
}
```

### 15.3 Daemon Embedding

```rust
// Embedding daemon in an application

use daemon::{DaemonConfig, Server, ModuleConfig};
use std::net::SocketAddr;

pub async fn run_embedded_daemon(
    bind_addr: SocketAddr,
    modules: Vec<ModuleConfig>,
) -> Result<(), DaemonError> {
    let config = DaemonConfig {
        address: bind_addr.ip().to_string(),
        port: bind_addr.port(),
        modules,
        max_connections: 10,
        timeout: Duration::from_secs(60),
        ..DaemonConfig::default()
    };

    let server = Server::new(Arc::new(config));
    server.run().await
}

// Usage
#[tokio::main]
async fn main() {
    let modules = vec![
        ModuleConfig {
            name: "data".to_string(),
            path: PathBuf::from("/srv/data"),
            read_only: true,
            ..ModuleConfig::default()
        },
    ];

    run_embedded_daemon("0.0.0.0:873".parse().unwrap(), modules).await.unwrap();
}
```

---

## 16. Build Configuration

### 16.1 Cargo Features

```toml
# Cargo.toml feature configuration

[features]
default = ["zlib", "xxh3"]

# Compression algorithms
zlib = ["flate2"]
zstd = ["zstd"]

# Checksum algorithms
xxh3 = ["xxhash-rust"]
md4 = ["md4"]

# ACL support (Linux only)
acl = ["exacl"]

# Extended attributes
xattr = ["xattr"]

# TLS support for daemon
tls = ["rustls", "tokio-rustls"]

# Full feature set
full = ["zlib", "zstd", "xxh3", "md4", "acl", "xattr", "tls"]
```

### 16.2 Conditional Compilation

```rust
// Feature-gated compression support

#[cfg(feature = "zstd")]
pub fn create_zstd_compressor(level: i32) -> Box<dyn Compressor> {
    Box::new(ZstdCompressor::new(level))
}

#[cfg(not(feature = "zstd"))]
pub fn create_zstd_compressor(_level: i32) -> Box<dyn Compressor> {
    panic!("zstd support not compiled in")
}

// Runtime feature detection
pub fn compression_available(algorithm: CompressionAlgorithm) -> bool {
    match algorithm {
        CompressionAlgorithm::Zlib => true,  // Always available
        #[cfg(feature = "zstd")]
        CompressionAlgorithm::Zstd => true,
        #[cfg(not(feature = "zstd"))]
        CompressionAlgorithm::Zstd => false,
    }
}
```

---

## Section 17: Design Principles for Library Integration

This section documents the core design principles applied throughout the
oc-rsync library architecture.

### 17.1 Design Principles Table

| Principle | Application | Benefit |
|-----------|-------------|---------|
| **Single Responsibility** | Each crate handles one concern (checksums, protocol, filters) | Easy testing, clear ownership |
| **Dependency Inversion** | Traits define interfaces, implementations are swappable | Testability, flexibility |
| **Strategy Pattern** | Checksum algorithms, compression codecs are interchangeable | Runtime algorithm selection |
| **Builder Pattern** | Complex configs use builders (`SignatureConfig`, `FilterChain`) | Ergonomic API, validation |
| **State Machine** | Connection states, transfer phases explicitly modeled | Correct protocol sequencing |

### 17.2 Single Responsibility Principle

```rust
// Each crate has a focused responsibility:
// - checksums: Rolling and strong checksum computation
// - protocol: Wire format encoding/decoding
// - filters: Include/exclude rule processing
// - engine: Delta generation and application
// - transport: Network I/O abstraction

// Bad: Monolithic struct doing everything
pub struct RsyncEngine {
    // checksums, protocol, filters, transport all mixed
}

// Good: Focused, composable components
pub struct DeltaGenerator<C: RollingChecksum, S: StrongChecksum> {
    rolling: C,
    strong: S,
    block_size: u32,
}

pub struct ProtocolCodec {
    version: ProtocolVersion,
}

pub struct FilterChain {
    rules: Vec<FilterRule>,
}
```

### 17.3 Dependency Inversion Principle

```rust
// High-level modules depend on abstractions, not concretions

// Abstract interface
pub trait Checksum {
    fn update(&mut self, data: &[u8]);
    fn finalize(&self) -> Vec<u8>;
}

// Concrete implementations
pub struct Md5Checksum { /* ... */ }
pub struct Xxh3Checksum { /* ... */ }

impl Checksum for Md5Checksum { /* ... */ }
impl Checksum for Xxh3Checksum { /* ... */ }

// High-level code depends on trait, not implementation
pub fn generate_signature<C: Checksum>(
    data: &[u8],
    checksum: &mut C,
) -> Signature {
    checksum.update(data);
    Signature { hash: checksum.finalize() }
}
```

### 17.4 Strategy Pattern Application

```rust
// Algorithms are interchangeable at runtime

pub enum ChecksumStrategy {
    Md4,
    Md5,
    Xxh3,
    Xxh128,
}

pub fn create_checksum(strategy: ChecksumStrategy) -> Box<dyn StrongChecksum> {
    match strategy {
        ChecksumStrategy::Md4 => Box::new(Md4Checksum::new()),
        ChecksumStrategy::Md5 => Box::new(Md5Checksum::new()),
        ChecksumStrategy::Xxh3 => Box::new(Xxh3Checksum::new()),
        ChecksumStrategy::Xxh128 => Box::new(Xxh128Checksum::new()),
    }
}

// Compression strategy
pub fn create_compressor(algorithm: CompressionAlgorithm) -> Box<dyn Compressor> {
    match algorithm {
        CompressionAlgorithm::Zlib => Box::new(ZlibCompressor::default()),
        CompressionAlgorithm::Zstd => Box::new(ZstdCompressor::default()),
        CompressionAlgorithm::None => Box::new(NoopCompressor),
    }
}
```

### 17.5 Builder Pattern for Configuration

```rust
// Complex configuration uses builders for ergonomic construction

pub struct TransferConfigBuilder {
    block_size: Option<u32>,
    checksum_type: Option<StrongChecksumType>,
    compression: Option<CompressionAlgorithm>,
    bandwidth_limit: Option<u64>,
    preserve_permissions: bool,
    preserve_times: bool,
}

impl TransferConfigBuilder {
    pub fn new() -> Self {
        Self {
            block_size: None,
            checksum_type: None,
            compression: None,
            bandwidth_limit: None,
            preserve_permissions: false,
            preserve_times: false,
        }
    }

    pub fn block_size(mut self, size: u32) -> Self {
        self.block_size = Some(size);
        self
    }

    pub fn checksum_type(mut self, checksum: StrongChecksumType) -> Self {
        self.checksum_type = Some(checksum);
        self
    }

    pub fn compression(mut self, algo: CompressionAlgorithm) -> Self {
        self.compression = Some(algo);
        self
    }

    pub fn bandwidth_limit(mut self, bytes_per_sec: u64) -> Self {
        self.bandwidth_limit = Some(bytes_per_sec);
        self
    }

    pub fn preserve_permissions(mut self) -> Self {
        self.preserve_permissions = true;
        self
    }

    pub fn preserve_times(mut self) -> Self {
        self.preserve_times = true;
        self
    }

    pub fn build(self) -> Result<TransferConfig, ConfigError> {
        Ok(TransferConfig {
            block_size: self.block_size.unwrap_or(DEFAULT_BLOCK_SIZE),
            checksum_type: self.checksum_type.unwrap_or(StrongChecksumType::Xxh3),
            compression: self.compression.unwrap_or(CompressionAlgorithm::None),
            bandwidth_limit: self.bandwidth_limit,
            preserve_permissions: self.preserve_permissions,
            preserve_times: self.preserve_times,
        })
    }
}
```

### 17.6 State Machine Pattern

```rust
// Protocol states are explicitly modeled to ensure correct sequencing

pub enum TransferPhase {
    /// Initial connection, version negotiation
    Handshake,
    /// Filter rules being exchanged
    FilterExchange,
    /// File list being transmitted
    FileListTransfer,
    /// Delta generation and transmission
    DeltaTransfer,
    /// Final statistics and cleanup
    Finalization,
    /// Transfer complete
    Complete,
}

impl TransferPhase {
    /// Valid transitions from this phase
    pub fn valid_transitions(&self) -> &[TransferPhase] {
        match self {
            TransferPhase::Handshake => &[TransferPhase::FilterExchange],
            TransferPhase::FilterExchange => &[TransferPhase::FileListTransfer],
            TransferPhase::FileListTransfer => &[TransferPhase::DeltaTransfer],
            TransferPhase::DeltaTransfer => &[TransferPhase::Finalization],
            TransferPhase::Finalization => &[TransferPhase::Complete],
            TransferPhase::Complete => &[],
        }
    }

    pub fn transition_to(&self, next: TransferPhase) -> Result<TransferPhase, ProtocolError> {
        if self.valid_transitions().contains(&next) {
            Ok(next)
        } else {
            Err(ProtocolError::InvalidStateTransition {
                from: self.clone(),
                to: next,
            })
        }
    }
}
```

---

## Section 18: Workspace Dependencies Configuration

This section documents the complete Cargo.toml workspace configuration for
library integration.

### 18.1 Root Cargo.toml Workspace Configuration

```toml
[workspace]
resolver = "2"
members = [
    "crates/checksums",
    "crates/protocol",
    "crates/filters",
    "crates/engine",
    "crates/transport",
    "crates/metadata",
    "crates/compress",
    "crates/bandwidth",
    "crates/logging",
    "crates/core",
    "crates/daemon",
    "crates/cli",
    "crates/flist",
    "crates/io",
    "crates/walk",
    "crates/branding",
    "xtask",
]

[workspace.metadata.oc_rsync]
branded_name = "oc-rsync"
version = "3.4.1-rust"
config_dir = "/etc/oc-rsyncd"
config_file = "oc-rsyncd.conf"
secrets_file = "oc-rsyncd.secrets"
repository = "https://github.com/oferchen/rsync"

[workspace.dependencies]
# Core async runtime
tokio = { version = "1.40", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec", "io"] }
futures = "0.3"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
toml = "0.8"

# Checksums
md-5 = "0.10"
md4 = "0.10"
sha1 = "0.10"
xxhash-rust = { version = "0.8", features = ["xxh3", "xxh64", "xxh32"] }

# Compression
flate2 = "1.0"
zstd = { version = "0.13", optional = true }

# CLI
clap = { version = "4.5", features = ["derive", "env", "wrap_help"] }

# Error handling
thiserror = "2.0"
anyhow = "1.0"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Progress display
indicatif = "0.17"

# Platform abstractions
nix = { version = "0.29", features = ["fs", "user", "process"] }
libc = "0.2"

# Testing
proptest = "1.5"
criterion = "0.5"
tempfile = "3.14"
rstest = "0.23"

# Network
socket2 = "0.5"

# Memory mapping
memmap2 = "0.9"

# Byte utilities
bytes = "1.8"
memchr = "2.7"
```

### 18.2 Individual Crate Dependencies

```toml
# crates/checksums/Cargo.toml
[package]
name = "checksums"
version = "0.1.0"
edition = "2021"

[dependencies]
md-5.workspace = true
md4.workspace = true
sha1.workspace = true
xxhash-rust.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
criterion.workspace = true

[[bench]]
name = "checksum_benchmarks"
harness = false
```

```toml
# crates/protocol/Cargo.toml
[package]
name = "protocol"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio.workspace = true
tokio-util.workspace = true
bytes.workspace = true
thiserror.workspace = true
tracing.workspace = true

[dev-dependencies]
proptest.workspace = true
tempfile.workspace = true
```

```toml
# crates/engine/Cargo.toml
[package]
name = "engine"
version = "0.1.0"
edition = "2021"

[dependencies]
checksums = { path = "../checksums" }
protocol = { path = "../protocol" }
memmap2.workspace = true
thiserror.workspace = true
tracing.workspace = true

[dev-dependencies]
tempfile.workspace = true
criterion.workspace = true

[[bench]]
name = "delta_benchmarks"
harness = false
```

### 18.3 Feature Flags Configuration

```toml
# crates/core/Cargo.toml
[package]
name = "core"
version = "0.1.0"
edition = "2021"

[features]
default = ["zlib"]
zlib = []
zstd = ["compress/zstd"]
acl = ["metadata/acl"]
xattr = ["metadata/xattr"]
full = ["zstd", "acl", "xattr"]

[dependencies]
checksums = { path = "../checksums" }
protocol = { path = "../protocol" }
engine = { path = "../engine" }
filters = { path = "../filters" }
transport = { path = "../transport" }
metadata = { path = "../metadata" }
compress = { path = "../compress" }
bandwidth = { path = "../bandwidth" }
logging = { path = "../logging" }
flist = { path = "../flist" }
walk = { path = "../walk" }
branding = { path = "../branding" }

tokio.workspace = true
thiserror.workspace = true
tracing.workspace = true
```

---

## Section 19: Implementation Roadmap

This section documents the phased implementation roadmap for library
integration work.

### 19.1 Phase 1: Foundation (Core Infrastructure)

**Objective**: Establish core checksum and delta infrastructure.

**Deliverables**:
- RollingChecksum trait with Adler32 and custom implementations
- StrongChecksum trait with MD4, MD5, XXH3, XXH128 implementations
- DeltaOp enum with Copy, Literal, End variants
- Signature generation API
- Delta generation API
- Delta application API
- Comprehensive unit tests for all components
- Benchmark suite for performance validation

**Key Tasks**:

```rust
// Task 1.1: Implement RollingChecksum trait
pub trait RollingChecksum: Default + Clone + Send + Sync {
    fn update(&mut self, data: &[u8]);
    fn digest(&self) -> u32;
    fn roll(&mut self, old_byte: u8, new_byte: u8);
    fn reset(&mut self);
}

// Task 1.2: Implement StrongChecksum trait
pub trait StrongChecksum: Send + Sync {
    fn update(&mut self, data: &[u8]);
    fn finalize(&self) -> Vec<u8>;
    fn output_size(&self) -> usize;
    fn reset(&mut self);
}

// Task 1.3: Implement signature generation
pub fn generate_signatures(
    data: &[u8],
    config: &SignatureConfig,
) -> Vec<BlockSignature>;

// Task 1.4: Implement delta generation
pub fn generate_delta<R: RollingChecksum, S: StrongChecksum>(
    basis: &[u8],
    target: &[u8],
    signatures: &SignatureTable,
    config: &DeltaConfig,
) -> Vec<DeltaOp>;

// Task 1.5: Implement delta application
pub struct DeltaApplicator { /* ... */ }
impl DeltaApplicator {
    pub fn apply(&mut self, op: DeltaOp) -> io::Result<()>;
    pub fn finish(self) -> io::Result<Vec<u8>>;
}
```

**Success Criteria**:
- All checksum implementations pass property-based tests
- Delta round-trip produces identical output
- Performance within 20% of C implementation benchmarks

### 19.2 Phase 2: Async I/O and Streaming

**Objective**: Add async I/O support and streaming interfaces.

**Deliverables**:
- Async stream wrappers for all I/O operations
- MultiplexCodec for protocol framing
- Streaming signature generation
- Streaming delta generation and application
- Backpressure handling
- Timeout management

**Key Tasks**:

```rust
// Task 2.1: Implement MultiplexCodec
pub struct MultiplexCodec {
    state: CodecState,
    max_frame_size: usize,
}

impl Decoder for MultiplexCodec {
    type Item = MessageFrame;
    type Error = ProtocolError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error>;
}

impl Encoder<MessageFrame> for MultiplexCodec {
    type Error = ProtocolError;

    fn encode(&mut self, item: MessageFrame, dst: &mut BytesMut) -> Result<(), Self::Error>;
}

// Task 2.2: Streaming signature generation
pub struct StreamingSignatureGenerator<R: AsyncRead> {
    reader: R,
    config: SignatureConfig,
    buffer: Vec<u8>,
}

impl<R: AsyncRead + Unpin> Stream for StreamingSignatureGenerator<R> {
    type Item = io::Result<BlockSignature>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>>;
}

// Task 2.3: Streaming delta application
pub struct StreamingDeltaApplicator<W: AsyncWrite> {
    writer: W,
    basis: MappedFile,
    state: ApplicatorState,
}

impl<W: AsyncWrite + Unpin> StreamingDeltaApplicator<W> {
    pub async fn apply(&mut self, op: DeltaOp) -> io::Result<()>;
    pub async fn finish(self) -> io::Result<()>;
}
```

**Success Criteria**:
- Async operations complete without blocking runtime
- Memory usage bounded regardless of file size
- Proper backpressure propagation

### 19.3 Phase 3: Protocol Implementation

**Objective**: Implement complete rsync protocol support.

**Deliverables**:
- Protocol version negotiation (28-32)
- Compatibility flags handling
- Filter list exchange
- File list encoding/decoding
- Statistics reporting
- Error message handling

**Key Tasks**:

```rust
// Task 3.1: Protocol negotiation
pub async fn negotiate_protocol<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    our_version: ProtocolVersion,
    is_sender: bool,
) -> Result<NegotiatedProtocol, ProtocolError>;

// Task 3.2: Compatibility flags
pub struct CompatibilityFlags {
    pub inc_recurse: bool,
    pub symlink_times: bool,
    pub symlink_iconv: bool,
    pub safe_flist: bool,
    pub avoid_xattr_optim: bool,
    pub fix_checksum_seed: bool,
    pub inplace_partial_dir: bool,
}

impl CompatibilityFlags {
    pub fn negotiate(
        local: Self,
        remote: Self,
        protocol_version: ProtocolVersion,
    ) -> Self;
}

// Task 3.3: File list wire format
pub struct FileListCodec {
    version: ProtocolVersion,
    compat_flags: CompatibilityFlags,
}

impl FileListCodec {
    pub fn encode_entry(&self, entry: &FileListEntry, dst: &mut BytesMut) -> io::Result<()>;
    pub fn decode_entry(&self, src: &mut BytesMut) -> io::Result<Option<FileListEntry>>;
}
```

**Success Criteria**:
- Interoperability with rsync 3.0.9, 3.1.3, 3.4.1
- All protocol versions (28-32) supported
- Golden byte tests pass for all wire formats

### 19.4 Phase 4: Integration and Testing

**Objective**: Complete integration with existing infrastructure.

**Deliverables**:
- Integration with daemon subsystem
- Integration with CLI frontend
- End-to-end transfer tests
- Interoperability test suite
- Performance regression tests

**Key Tasks**:

```rust
// Task 4.1: Core transfer API
pub async fn transfer(
    config: TransferConfig,
    source: TransferSource,
    destination: TransferDestination,
    progress: Option<ProgressReporter>,
) -> Result<TransferStats, TransferError>;

// Task 4.2: Daemon session handler
pub async fn handle_daemon_session(
    ctx: DaemonContext,
    stream: TcpStream,
) -> Result<(), DaemonError>;

// Task 4.3: CLI integration
pub fn execute_transfer(args: &TransferArgs) -> Result<ExitCode, CliError> {
    let config = TransferConfig::from_args(args)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let stats = runtime.block_on(transfer(config, source, dest, progress))?;
    Ok(ExitCode::SUCCESS)
}
```

**Success Criteria**:
- All existing tests continue to pass
- Interop tests pass with upstream rsync
- No regressions in transfer performance

### 19.5 Phase 5: Optimization and Polish

**Objective**: Performance optimization and production readiness.

**Deliverables**:
- SIMD-optimized checksum implementations
- Memory-mapped file handling optimization
- Connection pooling for daemon mode
- Comprehensive documentation
- Performance tuning guide

**Key Tasks**:

```rust
// Task 5.1: SIMD checksum optimization
#[cfg(target_arch = "x86_64")]
pub fn rolling_checksum_avx2(data: &[u8]) -> u32;

#[cfg(target_arch = "aarch64")]
pub fn rolling_checksum_neon(data: &[u8]) -> u32;

// Task 5.2: Memory mapping optimization
pub struct OptimizedFileReader {
    mmap: Option<MappedFile>,
    fallback: Option<BufReader<File>>,
    threshold: u64,
}

impl OptimizedFileReader {
    pub fn new(path: &Path, mmap_threshold: u64) -> io::Result<Self>;
}

// Task 5.3: Connection pooling
pub struct DaemonConnectionPool {
    connections: HashMap<SocketAddr, Vec<PooledConnection>>,
    max_per_host: usize,
    idle_timeout: Duration,
}
```

**Success Criteria**:
- Performance parity or better than C implementation
- Memory usage optimized for large transfers
- Documentation complete and accurate

---

## Section 20: Decision Checklist

This section provides verification checklists for code review and integration
decisions.

### 20.1 Pre-Integration Checklist

Before integrating new library functionality, verify:

- [ ] **API Design Review**
  - [ ] Traits follow Rust conventions (Send, Sync, Clone where appropriate)
  - [ ] Error types are specific and actionable
  - [ ] Builder pattern used for complex configuration
  - [ ] Public API has comprehensive rustdoc

- [ ] **Test Coverage**
  - [ ] Unit tests cover all public functions
  - [ ] Property-based tests for algorithmic correctness
  - [ ] Integration tests verify end-to-end behavior
  - [ ] Benchmark tests establish performance baseline

- [ ] **Compatibility**
  - [ ] Wire format matches upstream rsync exactly
  - [ ] Interop tests pass with rsync 3.0.9, 3.1.3, 3.4.1
  - [ ] Protocol negotiation handles all versions (28-32)
  - [ ] Error messages match upstream format

- [ ] **Performance**
  - [ ] No unnecessary allocations in hot paths
  - [ ] Buffer reuse where appropriate
  - [ ] SIMD optimizations have scalar fallbacks
  - [ ] Memory usage bounded for streaming operations

### 20.2 Code Review Checklist

```markdown
## Code Review: [Component Name]

### Correctness
- [ ] Logic matches upstream rsync behavior
- [ ] Edge cases handled (empty input, max values, errors)
- [ ] No panics in library code (use Result)
- [ ] Thread safety verified (Send/Sync bounds)

### Style
- [ ] Follows AGENTS.md conventions
- [ ] File header present (//! path/to/file.rs)
- [ ] Rustdoc on all public items
- [ ] No unnecessary comments

### Testing
- [ ] Tests cover happy path
- [ ] Tests cover error conditions
- [ ] Tests cover boundary conditions
- [ ] Property tests where applicable

### Performance
- [ ] Benchmarks added for hot paths
- [ ] No obvious performance issues
- [ ] Memory allocation minimized
- [ ] I/O operations batched

### Documentation
- [ ] README updated if needed
- [ ] AGENTS.md updated if needed
- [ ] Inline examples compile and run
```

### 20.3 Release Checklist

```markdown
## Release Checklist: v[X.Y.Z]

### Pre-Release
- [ ] All tests pass (`cargo nextest run --workspace --all-features`)
- [ ] Clippy clean (`cargo clippy --workspace --all-targets --all-features`)
- [ ] Documentation builds (`cargo xtask docs`)
- [ ] CHANGELOG updated
- [ ] Version bumped in Cargo.toml

### Compatibility
- [ ] Interop tests pass with upstream rsync versions
- [ ] Wire format golden tests pass
- [ ] API compatibility verified (no breaking changes for minor/patch)

### Performance
- [ ] Benchmark comparison with previous release
- [ ] No performance regressions > 5%
- [ ] Memory usage acceptable

### Documentation
- [ ] README reflects current state
- [ ] AGENTS.md up to date
- [ ] API documentation complete
- [ ] Migration guide if breaking changes
```

---

## Section 21: Risk Mitigation

This section documents risk mitigation strategies for library integration.

### 21.1 Risk Assessment Table

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| **Protocol incompatibility** | Medium | High | Extensive interop testing, golden byte tests |
| **Performance regression** | Medium | Medium | Benchmark suite, CI performance gates |
| **Memory safety issues** | Low | High | Fuzzing, property tests, careful unsafe review |
| **API instability** | Medium | Medium | Semver discipline, deprecation warnings |
| **Build complexity** | Low | Low | Workspace dependencies, feature flags |

### 21.2 Protocol Incompatibility Mitigation

```rust
// Strategy: Golden byte tests for all wire formats

#[test]
fn test_file_list_encoding_golden() {
    let entry = FileListEntry {
        path: PathBuf::from("test.txt"),
        size: 1024,
        mtime: 1234567890,
        mode: 0o644,
        // ...
    };

    let mut codec = FileListCodec::new(ProtocolVersion::V31);
    let mut buffer = BytesMut::new();
    codec.encode_entry(&entry, &mut buffer).unwrap();

    // Golden bytes captured from upstream rsync
    let expected = include_bytes!("golden/file_list_entry_v31.bin");
    assert_eq!(&buffer[..], expected);
}

// Strategy: Interop test matrix
#[rstest]
#[case("3.0.9")]
#[case("3.1.3")]
#[case("3.4.1")]
fn test_interop_with_upstream(#[case] version: &str) {
    let upstream_bin = format!("target/interop/upstream-install/{}/bin/rsync", version);
    // Run interop test...
}
```

### 21.3 Performance Regression Mitigation

```rust
// Strategy: Benchmark CI gate

// criterion benchmark
fn checksum_benchmark(c: &mut Criterion) {
    let data = vec![0u8; 1024 * 1024]; // 1MB

    c.bench_function("rolling_checksum_1mb", |b| {
        b.iter(|| {
            let mut checksum = RollingChecksum::new();
            checksum.update(&data);
            black_box(checksum.digest())
        })
    });
}

// CI script checks for regressions
// cargo bench -- --save-baseline main
// cargo bench -- --baseline main --threshold 1.1
```

### 21.4 Memory Safety Mitigation

```rust
// Strategy: Fuzzing for parser code

#[cfg(fuzzing)]
fuzz_target!(|data: &[u8]| {
    let mut codec = FileListCodec::new(ProtocolVersion::V31);
    let mut buffer = BytesMut::from(data);
    // Should never panic, only return errors
    let _ = codec.decode_entry(&mut buffer);
});

// Strategy: Property tests for round-trip
proptest! {
    #[test]
    fn file_list_roundtrip(entry in arb_file_list_entry()) {
        let mut codec = FileListCodec::new(ProtocolVersion::V31);
        let mut buffer = BytesMut::new();
        codec.encode_entry(&entry, &mut buffer)?;

        let decoded = codec.decode_entry(&mut buffer)?.unwrap();
        prop_assert_eq!(entry, decoded);
    }
}
```

---

## Section 22: Success Metrics

This section documents success metrics and targets for library integration.

### 22.1 Success Metrics Table

| Metric | Target | Measurement |
|--------|--------|-------------|
| **Test Coverage** | > 80% line coverage | `cargo llvm-cov` |
| **Interop Success Rate** | 100% with supported versions | CI interop matrix |
| **Performance vs C** | Within 20% of upstream | Benchmark comparison |
| **Memory Overhead** | < 10% vs upstream | Peak RSS measurement |
| **Build Time** | < 5 minutes clean build | CI timing |
| **API Documentation** | 100% public items | `cargo doc --document-private-items` |

### 22.2 Performance Benchmarks

```rust
// Target benchmarks (relative to upstream rsync)

// Checksum computation: within 10% of C
// - Rolling checksum: 2GB/s target
// - MD5: 500MB/s target
// - XXH3: 10GB/s target

// Delta generation: within 20% of C
// - Small files (<1MB): < 1ms
// - Large files (1GB): < 10s

// Transfer throughput: within 15% of C
// - Local: 500MB/s target
// - Network (1Gbps): 100MB/s target
```

### 22.3 Quality Gates

```yaml
# CI quality gates (must pass for merge)
quality_gates:
  - name: "All tests pass"
    command: "cargo nextest run --workspace --all-features"
    required: true

  - name: "No clippy warnings"
    command: "cargo clippy --workspace --all-targets --all-features -- -D warnings"
    required: true

  - name: "Documentation builds"
    command: "cargo xtask docs"
    required: true

  - name: "No placeholder code"
    command: "bash tools/no_placeholders.sh"
    required: true

  - name: "Interop tests pass"
    command: "cargo xtask interop"
    required: true

  - name: "Performance within threshold"
    command: "cargo bench -- --baseline main --threshold 1.2"
    required: false  # Warning only
```

---

## Section 23: Library to Rsync Feature Mapping

This section maps library components to rsync command-line features.

### 23.1 Feature Mapping Table

| Rsync Feature | Library Component | Crate | Status |
|---------------|-------------------|-------|--------|
| `-a, --archive` | Metadata preservation | `metadata` | Complete |
| `-r, --recursive` | Directory traversal | `walk` | Complete |
| `-v, --verbose` | Progress reporting | `logging` | Complete |
| `-z, --compress` | Stream compression | `compress` | Complete |
| `--checksum` | Strong checksum comparison | `checksums` | Complete |
| `-c, --checksum` | Skip based on checksum | `engine` | Complete |
| `--inplace` | In-place file update | `engine` | Complete |
| `--partial` | Partial transfer resume | `engine` | Complete |
| `-n, --dry-run` | Simulation mode | `core` | Complete |
| `--delete` | Deletion handling | `engine` | Complete |
| `--exclude` | Filter rules | `filters` | Complete |
| `--filter` | Complex filter rules | `filters` | Complete |
| `--bwlimit` | Bandwidth limiting | `bandwidth` | Complete |
| `-p, --perms` | Permission preservation | `metadata` | Complete |
| `-o, --owner` | Owner preservation | `metadata` | Complete |
| `-g, --group` | Group preservation | `metadata` | Complete |
| `-t, --times` | Time preservation | `metadata` | Complete |
| `-A, --acls` | ACL preservation | `metadata` | Feature-gated |
| `-X, --xattrs` | Extended attributes | `metadata` | Feature-gated |
| `-H, --hard-links` | Hard link handling | `engine` | Complete |
| `-l, --links` | Symlink handling | `engine` | Complete |
| `--daemon` | Daemon mode | `daemon` | Complete |
| `--protocol` | Protocol selection | `protocol` | Complete |

### 23.2 Protocol Version Feature Matrix

| Feature | Proto 28 | Proto 29 | Proto 30 | Proto 31 | Proto 32 |
|---------|:--------:|:--------:|:--------:|:--------:|:--------:|
| Basic transfer | ✓ | ✓ | ✓ | ✓ | ✓ |
| Incremental recursion | | ✓ | ✓ | ✓ | ✓ |
| Sender-side filtering | | | ✓ | ✓ | ✓ |
| Nanosecond mtimes | | | | ✓ | ✓ |
| 64-bit dev numbers | | | | ✓ | ✓ |
| XXH3/XXH128 checksums | | | | | ✓ |

### 23.3 Crate Dependency Graph

```
                    ┌─────────┐
                    │   cli   │
                    └────┬────┘
                         │
                    ┌────▼────┐
          ┌─────────┤  core   ├─────────┐
          │         └────┬────┘         │
          │              │              │
    ┌─────▼─────┐  ┌─────▼─────┐  ┌─────▼─────┐
    │  daemon   │  │  engine   │  │ transport │
    └─────┬─────┘  └─────┬─────┘  └─────┬─────┘
          │              │              │
    ┌─────▼─────────────▼──────────────▼─────┐
    │                protocol                 │
    └─────┬─────────────┬──────────────┬─────┘
          │             │              │
    ┌─────▼─────┐ ┌─────▼─────┐ ┌─────▼─────┐
    │ checksums │ │  filters  │ │ compress  │
    └───────────┘ └───────────┘ └───────────┘
```

---

## Section 24: Quick Reference

This section provides quick reference tables for common CLI options, exit
codes, environment variables, and default values.

### 24.1 Common CLI Options Summary

| Option | Short | Description |
|--------|-------|-------------|
| `--archive` | `-a` | Archive mode: `-rlptgoD` |
| `--recursive` | `-r` | Recurse into directories |
| `--verbose` | `-v` | Increase verbosity (can repeat) |
| `--compress` | `-z` | Compress during transfer |
| `--progress` | | Show progress during transfer |
| `--delete` | | Delete extraneous files from dest |
| `--dry-run` | `-n` | Show what would be transferred |
| `--checksum` | `-c` | Skip based on checksum, not mod-time/size |
| `--exclude` | | Exclude files matching PATTERN |
| `--include` | | Include files matching PATTERN |
| `--bwlimit` | | Limit bandwidth (KB/s) |
| `--partial` | | Keep partially transferred files |
| `--inplace` | | Update destination files in-place |
| `--daemon` | | Run as an rsync daemon |

### 24.2 Exit Codes Quick Reference

| Code | Name | Meaning |
|------|------|---------|
| 0 | `RERR_OK` | Success |
| 1 | `RERR_SYNTAX` | Syntax or usage error |
| 2 | `RERR_PROTOCOL` | Protocol incompatibility |
| 3 | `RERR_FILESELECT` | Errors selecting input/output files |
| 4 | `RERR_UNSUPPORTED` | Requested action not supported |
| 5 | `RERR_STARTCLIENT` | Error starting client-server protocol |
| 6 | `RERR_SOCKETIO` | Daemon unable to append to log-file |
| 10 | `RERR_FILEIO` | Error in socket I/O |
| 11 | `RERR_STREAMIO` | Error in file I/O |
| 12 | `RERR_MESSAGEIO` | Error in rsync protocol data stream |
| 13 | `RERR_IPC` | Errors with program diagnostics |
| 14 | `RERR_CRASHED` | Error in IPC code |
| 20 | `RERR_SIGNAL` | Received SIGUSR1 or SIGINT |
| 21 | `RERR_WAITCHILD` | Some error returned by waitpid() |
| 22 | `RERR_MALLOC` | Error allocating core memory buffers |
| 23 | `RERR_PARTIAL` | Partial transfer due to error |
| 24 | `RERR_VANISHED` | Some files vanished before transfer |
| 25 | `RERR_DEL_LIMIT` | Deletion limit exceeded |
| 30 | `RERR_TIMEOUT` | Timeout in data send/receive |
| 35 | `RERR_CONTIMEOUT` | Timeout waiting for daemon connection |

### 24.3 Environment Variables Quick Reference

| Variable | Purpose | Example |
|----------|---------|---------|
| `RSYNC_PASSWORD` | Password for daemon authentication | `export RSYNC_PASSWORD=secret` |
| `RSYNC_CONNECT_PROG` | Custom program for connections | `ssh -p 2222` |
| `RSYNC_RSH` | Remote shell command | `ssh -i ~/.ssh/id_rsa` |
| `RSYNC_PROXY` | Proxy server for connections | `proxy.example.com:8080` |
| `OC_RSYNC_CONFIG` | Config file path (daemon) | `/etc/oc-rsyncd/oc-rsyncd.conf` |
| `OC_RSYNC_LOG_LEVEL` | Logging verbosity | `debug`, `info`, `warn`, `error` |
| `OC_RSYNC_NO_COLOR` | Disable colored output | `1` |
| `OC_RSYNC_PROGRESS_STYLE` | Progress bar style | `bar`, `simple`, `none` |

### 24.4 Default Values Quick Reference

| Parameter | Default | Range/Options |
|-----------|---------|---------------|
| Block size | 700 bytes | 512 - 131072 |
| Strong checksum | XXH3 (proto 32+) | MD4, MD5, XXH3, XXH128 |
| Compression | zlib level 6 | 0-9 (zlib), 1-22 (zstd) |
| Timeout | 0 (none) | 0 - 86400 seconds |
| Bandwidth limit | 0 (none) | KB/s |
| Protocol version | 32 | 28 - 32 |
| Daemon port | 873 | 1 - 65535 |

---

## Section 25: CLI Options Reference

This section provides comprehensive documentation of all CLI options.

### 25.1 Transfer Mode Options

```text
-r, --recursive         Recurse into directories
-l, --links             Copy symlinks as symlinks
-L, --copy-links        Transform symlink into referent file/dir
-k, --copy-dirlinks     Transform symlink to dir into referent dir
-K, --keep-dirlinks     Treat symlinked dir on receiver as dir
-H, --hard-links        Preserve hard links
-p, --perms             Preserve permissions
-E, --executability     Preserve executability
-A, --acls              Preserve ACLs (implies -p)
-X, --xattrs            Preserve extended attributes
-o, --owner             Preserve owner (super-user only)
-g, --group             Preserve group
-t, --times             Preserve modification times
-O, --omit-dir-times    Omit directories from --times
-J, --omit-link-times   Omit symlinks from --times
-D                      Same as --devices --specials
    --devices           Preserve device files (super-user only)
    --specials          Preserve special files
-a, --archive           Archive mode (equals -rlptgoD)
```

### 25.2 Transfer Behavior Options

```text
-u, --update            Skip files that are newer on receiver
    --inplace           Update destination files in-place
    --append            Append data onto shorter files
    --append-verify     Append with old data in file checksum
-c, --checksum          Skip based on checksum, not mod-time/size
-W, --whole-file        Copy files whole (without delta-transfer)
    --no-whole-file     Always use delta-transfer algorithm
-x, --one-file-system   Don't cross filesystem boundaries
-S, --sparse            Handle sparse files efficiently
    --preallocate       Allocate dest files before writing
-n, --dry-run           Show what would be transferred
    --existing          Skip creating new files on receiver
    --ignore-existing   Skip updating files that exist on receiver
    --remove-source-files Remove synchronized files from sender
    --delete            Delete extraneous files from dest dirs
    --delete-before     Receiver deletes before transfer, not during
    --delete-during     Receiver deletes during transfer
    --delete-delay      Find deletions during, delete after
    --delete-after      Receiver deletes after transfer, not during
    --delete-excluded   Also delete excluded files from dest dirs
    --force             Force deletion of dirs even if not empty
    --max-delete=NUM    Don't delete more than NUM files
    --max-size=SIZE     Don't transfer any file larger than SIZE
    --min-size=SIZE     Don't transfer any file smaller than SIZE
    --partial           Keep partially transferred files
    --partial-dir=DIR   Put a partially transferred file into DIR
```

### 25.3 Filter Options

```text
    --exclude=PATTERN   Exclude files matching PATTERN
    --exclude-from=FILE Read exclude patterns from FILE
    --include=PATTERN   Include files matching PATTERN
    --include-from=FILE Read include patterns from FILE
    --files-from=FILE   Read list of source-file names from FILE
-F                      Same as --filter='dir-merge /.rsync-filter'
                        Repeated: --filter='- .rsync-filter'
    --filter=RULE       Add a file-filtering RULE
    --cvs-exclude       Auto-ignore files in the same way CVS does
```

### 25.4 Compression Options

```text
-z, --compress          Compress file data during transfer
    --compress-level=N  Explicitly set compression level (0-9)
    --compress-choice=S Choose compression algorithm (zlib, zstd)
    --skip-compress=LIST Skip compressing files with suffix in LIST
```

### 25.5 Output and Logging Options

```text
-v, --verbose           Increase verbosity
    --info=FLAGS        Fine-grained informational verbosity
    --debug=FLAGS       Fine-grained debug verbosity
    --msgs2stderr       Output messages to stderr for piping
-q, --quiet             Suppress non-error messages
    --no-motd           Suppress daemon-mode MOTD
-h, --human-readable    Output numbers in a human-readable format
    --progress          Show progress during transfer
-P                      Same as --partial --progress
-i, --itemize-changes   Output a change-summary for all updates
    --out-format=FORMAT Output updates using specified FORMAT
    --log-file=FILE     Log what we're doing to FILE
    --log-file-format=FMT Update logging format
    --stats             Give some file-transfer stats
-8, --8-bit-output      Leave high-bit chars unescaped in output
```

### 25.6 Daemon Mode Options

```text
    --daemon            Run as an rsync daemon
    --address=ADDRESS   Bind to ADDRESS
    --bwlimit=RATE      Limit socket I/O bandwidth
    --config=FILE       Specify alternate rsyncd.conf file
    --dparam=PARAM      Override daemon config parameter
    --no-detach         Do not detach from parent
    --port=PORT         Listen on alternate port number
    --sockopts=OPTIONS  Specify custom TCP options
```

### 25.7 Connection Options

```text
-e, --rsh=COMMAND       Specify remote shell to use
    --rsync-path=PROG   Specify rsync to run on remote machine
    --blocking-io       Use blocking I/O for remote shell
    --timeout=SECONDS   Set I/O timeout in seconds
    --contimeout=SECONDS Set connection timeout in seconds
-4, --ipv4              Prefer IPv4
-6, --ipv6              Prefer IPv6
```

---

## Section 26: Exit Codes Reference

This section provides complete documentation of all exit codes.

### 26.1 Exit Code Table

| Code | Constant | Description | Common Causes |
|------|----------|-------------|---------------|
| 0 | `RERR_OK` | Success | Normal completion |
| 1 | `RERR_SYNTAX` | Syntax or usage error | Invalid CLI arguments |
| 2 | `RERR_PROTOCOL` | Protocol incompatibility | Version mismatch |
| 3 | `RERR_FILESELECT` | Errors selecting I/O files | Permission denied, path not found |
| 4 | `RERR_UNSUPPORTED` | Requested action not supported | Feature not compiled in |
| 5 | `RERR_STARTCLIENT` | Error starting client-server protocol | Daemon connection failed |
| 6 | `RERR_SOCKETIO` | Daemon unable to append to log-file | Log file permissions |
| 10 | `RERR_FILEIO` | Error in socket I/O | Network issues |
| 11 | `RERR_STREAMIO` | Error in file I/O | Disk full, file permissions |
| 12 | `RERR_MESSAGEIO` | Error in rsync protocol data stream | Protocol corruption |
| 13 | `RERR_IPC` | Errors with program diagnostics | Internal error |
| 14 | `RERR_CRASHED` | Error in IPC code | Child process crashed |
| 20 | `RERR_SIGNAL` | Received SIGUSR1 or SIGINT | User interrupted |
| 21 | `RERR_WAITCHILD` | Some error returned by waitpid() | Child process error |
| 22 | `RERR_MALLOC` | Error allocating core memory buffers | Out of memory |
| 23 | `RERR_PARTIAL` | Partial transfer due to error | Some files failed |
| 24 | `RERR_VANISHED` | Some files vanished before transfer | Files deleted during transfer |
| 25 | `RERR_DEL_LIMIT` | Deletion limit exceeded | `--max-delete` limit |
| 30 | `RERR_TIMEOUT` | Timeout in data send/receive | Network timeout |
| 35 | `RERR_CONTIMEOUT` | Timeout waiting for daemon connection | Daemon unreachable |

### 26.2 Exit Code Handling

```rust
// Exit code constants
pub const RERR_OK: i32 = 0;
pub const RERR_SYNTAX: i32 = 1;
pub const RERR_PROTOCOL: i32 = 2;
pub const RERR_FILESELECT: i32 = 3;
pub const RERR_UNSUPPORTED: i32 = 4;
pub const RERR_STARTCLIENT: i32 = 5;
pub const RERR_SOCKETIO: i32 = 6;
pub const RERR_FILEIO: i32 = 10;
pub const RERR_STREAMIO: i32 = 11;
pub const RERR_MESSAGEIO: i32 = 12;
pub const RERR_IPC: i32 = 13;
pub const RERR_CRASHED: i32 = 14;
pub const RERR_SIGNAL: i32 = 20;
pub const RERR_WAITCHILD: i32 = 21;
pub const RERR_MALLOC: i32 = 22;
pub const RERR_PARTIAL: i32 = 23;
pub const RERR_VANISHED: i32 = 24;
pub const RERR_DEL_LIMIT: i32 = 25;
pub const RERR_TIMEOUT: i32 = 30;
pub const RERR_CONTIMEOUT: i32 = 35;

// Convert error type to exit code
impl From<TransferError> for ExitCode {
    fn from(err: TransferError) -> Self {
        let code = match err {
            TransferError::Syntax(_) => RERR_SYNTAX,
            TransferError::Protocol(_) => RERR_PROTOCOL,
            TransferError::FileSelect(_) => RERR_FILESELECT,
            TransferError::Unsupported(_) => RERR_UNSUPPORTED,
            TransferError::StartClient(_) => RERR_STARTCLIENT,
            TransferError::FileIo(_) => RERR_FILEIO,
            TransferError::StreamIo(_) => RERR_STREAMIO,
            TransferError::Timeout(_) => RERR_TIMEOUT,
            TransferError::Partial(_) => RERR_PARTIAL,
            TransferError::Vanished(_) => RERR_VANISHED,
            _ => RERR_CRASHED,
        };
        ExitCode::from(code as u8)
    }
}
```

---

## Section 27: Environment Variables Reference

This section documents all environment variables recognized by oc-rsync.

### 27.1 Authentication Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `RSYNC_PASSWORD` | Password for rsync daemon authentication | `export RSYNC_PASSWORD=mysecret` |
| `RSYNC_PASSWORD_FILE` | File containing the password | `/etc/rsync.pass` |
| `USER` | Username for authentication (fallback) | `rsyncuser` |

### 27.2 Connection Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `RSYNC_RSH` | Remote shell command | `ssh -o StrictHostKeyChecking=no` |
| `RSYNC_CONNECT_PROG` | Custom connection program | `nc -X 5 -x proxy:1080` |
| `RSYNC_PROXY` | HTTP proxy for rsync:// connections | `proxy.example.com:8080` |

### 27.3 Configuration Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `OC_RSYNC_CONFIG` | Daemon config file path | `/etc/oc-rsyncd/oc-rsyncd.conf` |
| `OC_RSYNC_SECRETS` | Secrets file path | `/etc/oc-rsyncd/oc-rsyncd.secrets` |
| `OC_RSYNC_LOG_FILE` | Log file path | `/var/log/oc-rsync.log` |
| `OC_RSYNC_PID_FILE` | PID file path | `/var/run/oc-rsyncd.pid` |

### 27.4 Runtime Variables

| Variable | Description | Example |
|----------|-------------|---------|
| `OC_RSYNC_LOG_LEVEL` | Logging verbosity | `debug`, `info`, `warn`, `error` |
| `OC_RSYNC_NO_COLOR` | Disable colored output | `1` or `true` |
| `OC_RSYNC_PROGRESS_STYLE` | Progress bar style | `bar`, `simple`, `none` |
| `OC_RSYNC_BUFFER_SIZE` | I/O buffer size in bytes | `131072` |
| `OC_RSYNC_MAX_ALLOC` | Maximum allocation size | `1073741824` (1GB) |

### 27.5 Environment Variable Usage

```rust
use std::env;

/// Get authentication password from environment
pub fn get_password() -> Option<String> {
    // Try RSYNC_PASSWORD first
    if let Ok(pass) = env::var("RSYNC_PASSWORD") {
        return Some(pass);
    }

    // Fall back to password file
    if let Ok(path) = env::var("RSYNC_PASSWORD_FILE") {
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content.trim().to_string());
        }
    }

    None
}

/// Get remote shell command
pub fn get_rsh() -> String {
    env::var("RSYNC_RSH").unwrap_or_else(|_| "ssh".to_string())
}

/// Get log level from environment
pub fn get_log_level() -> tracing::Level {
    match env::var("OC_RSYNC_LOG_LEVEL")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "trace" => tracing::Level::TRACE,
        "debug" => tracing::Level::DEBUG,
        "info" => tracing::Level::INFO,
        "warn" | "warning" => tracing::Level::WARN,
        "error" => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    }
}
```

---

## Section 28: Default Values Reference

This section documents all default values and their valid ranges.

### 28.1 Block Size Defaults

| Context | Default | Minimum | Maximum | Notes |
|---------|---------|---------|---------|-------|
| Small files (<2KB) | 512 | 512 | 512 | Fixed for small files |
| Medium files | 700 | 512 | 131072 | Computed based on file size |
| Large files (>64MB) | Computed | 512 | 131072 | sqrt(file_size) algorithm |
| Remote transfer | 700 | 512 | 131072 | Standard default |
| Local copy | N/A | N/A | N/A | No delta algorithm used |

```rust
/// Compute optimal block size for a given file size
pub fn compute_block_size(file_size: u64) -> u32 {
    const MIN_BLOCK_SIZE: u32 = 512;
    const MAX_BLOCK_SIZE: u32 = 131072;
    const DEFAULT_BLOCK_SIZE: u32 = 700;

    if file_size == 0 {
        return DEFAULT_BLOCK_SIZE;
    }

    // Use sqrt algorithm for large files
    let computed = (file_size as f64).sqrt() as u32;
    computed.clamp(MIN_BLOCK_SIZE, MAX_BLOCK_SIZE)
}
```

### 28.2 Checksum Defaults

| Protocol Version | Rolling Checksum | Strong Checksum | Strong Length |
|------------------|------------------|-----------------|---------------|
| 28-29 | Adler32 variant | MD4 | 16 bytes |
| 30 | Adler32 variant | MD5 | 16 bytes |
| 31 | Adler32 variant | MD5 | 16 bytes |
| 32 | Adler32 variant | XXH3 (64-bit) | 8 bytes |
| 32 (xxh128) | Adler32 variant | XXH128 | 16 bytes |

### 28.3 Compression Defaults

| Algorithm | Default Level | Range | Notes |
|-----------|---------------|-------|-------|
| zlib | 6 | 0-9 | 0 = no compression |
| zstd | 3 | 1-22 | Higher = more compression |

```rust
/// Default compression configuration
pub struct CompressionDefaults {
    pub zlib_level: i32,    // 6
    pub zstd_level: i32,    // 3
    pub skip_suffixes: &'static [&'static str],
}

impl Default for CompressionDefaults {
    fn default() -> Self {
        Self {
            zlib_level: 6,
            zstd_level: 3,
            skip_suffixes: &[
                "7z", "ace", "avi", "bz2", "deb", "gz", "iso", "jpeg", "jpg",
                "lz", "lz4", "lzma", "lzo", "mkv", "mov", "mp3", "mp4", "ogg",
                "png", "rar", "rpm", "rzip", "tbz", "tgz", "tlz", "txz", "xz",
                "z", "zip", "zst",
            ],
        }
    }
}
```

### 28.4 Timeout Defaults

| Timeout Type | Default | Range | Purpose |
|--------------|---------|-------|---------|
| I/O timeout | 0 (none) | 0-86400 | Data transfer timeout |
| Connection timeout | 0 (none) | 0-86400 | Initial connection timeout |
| Select timeout | 60 | 1-3600 | Internal poll timeout |
| Module timeout | 0 (none) | 0-86400 | Per-module daemon timeout |

### 28.5 Buffer Defaults

| Buffer | Default Size | Notes |
|--------|--------------|-------|
| I/O buffer | 32KB | Read/write operations |
| Checksum buffer | 16KB | Checksum computation |
| Compression buffer | 64KB | Compression operations |
| Socket buffer | 64KB | Network I/O |
| File list buffer | 1MB | File list encoding |

---

## Section 29: Protocol Wire Format Reference

This section documents the rsync protocol wire format.

### 29.1 Message Frame Format

```text
+----------------+----------------+------------------+
| Tag (1 byte)   | Length (3 bytes, LE) | Payload   |
+----------------+----------------+------------------+

Tag values:
  7 = MSG_DATA      - File data
  8 = MSG_ERROR     - Error message
  9 = MSG_INFO      - Informational message
 10 = MSG_LOG       - Log message
 11 = MSG_CLIENT    - Client message (protocol 31+)
 22 = MSG_ERROR_XFER - Transfer error (non-fatal)
 32 = MSG_ERROR_UTF8 - UTF-8 error (protocol 31+)
```

### 29.2 Message Types

| Tag | Name | Description | Direction |
|-----|------|-------------|-----------|
| 7 | `MSG_DATA` | Raw file data | Both |
| 8 | `MSG_ERROR` | Error message text | Server → Client |
| 9 | `MSG_INFO` | Info message text | Server → Client |
| 10 | `MSG_LOG` | Log message | Server → Client |
| 11 | `MSG_CLIENT` | Client message (proto 31+) | Client → Server |
| 22 | `MSG_ERROR_XFER` | Non-fatal transfer error | Server → Client |
| 32 | `MSG_ERROR_UTF8` | UTF-8 error (proto 31+) | Server → Client |

### 29.3 Varint Encoding

```rust
/// Write a variable-length integer (protocol 30+)
pub fn write_varint<W: Write>(writer: &mut W, mut value: u64) -> io::Result<()> {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            writer.write_all(&[byte])?;
            break;
        } else {
            writer.write_all(&[byte | 0x80])?;
        }
    }
    Ok(())
}

/// Read a variable-length integer (protocol 30+)
pub fn read_varint<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        value |= ((byte[0] & 0x7F) as u64) << shift;
        if byte[0] & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "varint too long"
            ));
        }
    }
    Ok(value)
}
```

### 29.4 File List Entry Format

```text
Protocol 30+:
+--------+--------+--------+--------+--------+...
| Flags  | Name   | Size   | Mtime  | Mode   | ...
| 1-2B   | varint | varint | varint | varint |
+--------+--------+--------+--------+--------+...

Flags (XMIT_*):
  0x0001 = XMIT_TOP_DIR
  0x0002 = XMIT_SAME_MODE
  0x0004 = XMIT_EXTENDED_FLAGS (read 2nd byte)
  0x0008 = XMIT_SAME_RDEV_MAJOR
  0x0010 = XMIT_SAME_UID
  0x0020 = XMIT_SAME_GID
  0x0040 = XMIT_SAME_NAME
  0x0080 = XMIT_LONG_NAME
  0x0100 = XMIT_SAME_TIME
  0x0200 = XMIT_SAME_RDEV_MINOR
  0x0400 = XMIT_HLINKED
  0x0800 = XMIT_SAME_DEV_pre30
  0x1000 = XMIT_USER_NAME_FOLLOWS
  0x2000 = XMIT_GROUP_NAME_FOLLOWS
  0x4000 = XMIT_HLINK_FIRST
  0x8000 = XMIT_IO_ERROR_ENDLIST
```

### 29.5 NDX (Index) Encoding

```rust
/// Protocol-version-aware NDX encoding
pub trait NdxCodec {
    fn write_ndx<W: Write>(&self, writer: &mut W, ndx: i32) -> io::Result<()>;
    fn read_ndx<R: Read>(&self, reader: &mut R) -> io::Result<i32>;
    fn write_ndx_done<W: Write>(&self, writer: &mut W) -> io::Result<()>;
}

/// Protocol 28-29: 4-byte little-endian
pub struct LegacyNdxCodec;

impl NdxCodec for LegacyNdxCodec {
    fn write_ndx<W: Write>(&self, writer: &mut W, ndx: i32) -> io::Result<()> {
        writer.write_all(&ndx.to_le_bytes())
    }

    fn read_ndx<R: Read>(&self, reader: &mut R) -> io::Result<i32> {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        Ok(i32::from_le_bytes(buf))
    }

    fn write_ndx_done<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.write_ndx(writer, -1)
    }
}

/// Protocol 30+: delta encoding
pub struct ModernNdxCodec {
    last_ndx: Cell<i32>,
}

impl NdxCodec for ModernNdxCodec {
    fn write_ndx<W: Write>(&self, writer: &mut W, ndx: i32) -> io::Result<()> {
        let diff = ndx - self.last_ndx.get();
        self.last_ndx.set(ndx);

        if diff >= 0 && diff < 0xFE {
            writer.write_all(&[diff as u8])
        } else if diff >= 0 && diff < 0x10000 {
            writer.write_all(&[0xFE])?;
            writer.write_all(&(diff as u16).to_le_bytes())
        } else {
            writer.write_all(&[0xFF])?;
            writer.write_all(&ndx.to_le_bytes())
        }
    }

    fn read_ndx<R: Read>(&self, reader: &mut R) -> io::Result<i32> {
        let mut buf = [0u8; 1];
        reader.read_exact(&mut buf)?;

        let ndx = match buf[0] {
            0..=0xFD => self.last_ndx.get() + buf[0] as i32,
            0xFE => {
                let mut buf2 = [0u8; 2];
                reader.read_exact(&mut buf2)?;
                self.last_ndx.get() + u16::from_le_bytes(buf2) as i32
            }
            0xFF => {
                let mut buf4 = [0u8; 4];
                reader.read_exact(&mut buf4)?;
                i32::from_le_bytes(buf4)
            }
        };

        self.last_ndx.set(ndx);
        Ok(ndx)
    }

    fn write_ndx_done<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&[0xFF])?;
        writer.write_all(&(-1i32).to_le_bytes())
    }
}
```

---

## Section 30: Daemon Configuration Reference

This section documents daemon configuration options.

### 30.1 Global Options

```ini
# Global daemon configuration (/etc/oc-rsyncd/oc-rsyncd.conf)

# Network settings
port = 873
address = 0.0.0.0

# Logging
log file = /var/log/oc-rsyncd.log
log format = %t %a %m %f %b

# Security
use chroot = yes
uid = nobody
gid = nobody
max connections = 10
lock file = /var/run/oc-rsyncd.lock

# Timeouts
timeout = 300

# MOTD
motd file = /etc/oc-rsyncd/motd
```

### 30.2 Module Options

| Option | Description | Default |
|--------|-------------|---------|
| `path` | Path to module root | Required |
| `comment` | Module description | Empty |
| `read only` | Disallow uploads | `yes` |
| `write only` | Disallow downloads | `no` |
| `list` | Include in module listing | `yes` |
| `uid` | Run as this user | `nobody` |
| `gid` | Run as this group | `nobody` |
| `auth users` | Allowed usernames | Empty (anonymous) |
| `secrets file` | Password file path | Empty |
| `hosts allow` | Allowed hosts | `*` |
| `hosts deny` | Denied hosts | Empty |
| `max connections` | Per-module connection limit | `0` (unlimited) |
| `timeout` | Per-module timeout | Global value |
| `exclude` | Default exclude patterns | Empty |
| `include` | Default include patterns | Empty |
| `exclude from` | Exclude patterns file | Empty |
| `include from` | Include patterns file | Empty |
| `use chroot` | Change root to module path | `yes` |
| `numeric ids` | Don't map uid/gid by name | `no` |
| `munge symlinks` | Protect against symlink attacks | `yes` in chroot |
| `charset` | Filename character set | UTF-8 |

### 30.3 Example Module Configuration

```ini
[backup]
    path = /data/backups
    comment = Backup storage
    read only = no
    auth users = backup_user, admin
    secrets file = /etc/oc-rsyncd/oc-rsyncd.secrets
    hosts allow = 10.0.0.0/8 192.168.0.0/16
    max connections = 5
    timeout = 600
    exclude = *.tmp .cache/

[public]
    path = /data/public
    comment = Public files
    read only = yes
    list = yes
    uid = www-data
    gid = www-data

[private]
    path = /data/private
    comment = Private - unlisted
    list = no
    auth users = admin
    secrets file = /etc/oc-rsyncd/oc-rsyncd.secrets
```

### 30.4 Secrets File Format

```text
# /etc/oc-rsyncd/oc-rsyncd.secrets
# Format: username:password
# File must be mode 0600

backup_user:s3cr3t_p4ssw0rd
admin:4dm1n_p4ss
readonly:r34d0nly
```

---

## Section 31: Filter Rules Reference

This section documents the filter rule syntax.

### 31.1 Filter Rule Syntax

```text
RULE := [MODIFIERS] PATTERN

Modifiers:
  -  exclude
  +  include
  .  merge file
  :  dir-merge file
  H  hide (like exclude, but affects only sender)
  S  show (like include, but affects only sender)
  P  protect (like exclude, but affects only receiver)
  R  risk (like include, but affects only receiver)
  !  clear current filter list

Pattern special characters:
  *      matches any path component, excluding /
  **     matches anything, including /
  ?      matches any single character except /
  [...]  character class
  [^...] negated character class
```

### 31.2 Filter Rule Examples

```text
# Exclude all .git directories
- .git/

# Exclude all .o files
- *.o

# Include only .rs files
+ *.rs
- *

# Exclude node_modules but include package.json
+ package.json
- node_modules/

# Complex pattern: include src/, exclude build/, include everything else
+ src/
+ src/**
- build/
- build/**
+ *
```

### 31.3 Filter Rule Implementation

```rust
/// Filter rule with modifier and pattern
pub struct FilterRule {
    pub modifier: FilterModifier,
    pub pattern: Pattern,
    pub flags: FilterFlags,
}

#[derive(Clone, Copy, PartialEq)]
pub enum FilterModifier {
    Exclude,      // -
    Include,      // +
    Merge,        // .
    DirMerge,     // :
    Hide,         // H
    Show,         // S
    Protect,      // P
    Risk,         // R
    Clear,        // !
}

pub struct FilterChain {
    rules: Vec<FilterRule>,
}

impl FilterChain {
    /// Check if a path matches any filter rule
    pub fn check(&self, path: &Path, is_dir: bool) -> FilterResult {
        for rule in &self.rules {
            if rule.pattern.matches(path, is_dir) {
                return match rule.modifier {
                    FilterModifier::Include | FilterModifier::Show | FilterModifier::Risk => {
                        FilterResult::Include
                    }
                    FilterModifier::Exclude | FilterModifier::Hide | FilterModifier::Protect => {
                        FilterResult::Exclude
                    }
                    _ => continue,
                };
            }
        }
        FilterResult::NoMatch
    }

    /// Parse a filter rule string
    pub fn parse_rule(rule: &str) -> Result<FilterRule, FilterError> {
        let rule = rule.trim();
        if rule.is_empty() || rule.starts_with('#') {
            return Err(FilterError::Empty);
        }

        let (modifier, pattern) = match rule.chars().next() {
            Some('-') => (FilterModifier::Exclude, &rule[1..]),
            Some('+') => (FilterModifier::Include, &rule[1..]),
            Some('.') => (FilterModifier::Merge, &rule[1..]),
            Some(':') => (FilterModifier::DirMerge, &rule[1..]),
            Some('!') => (FilterModifier::Clear, ""),
            _ => (FilterModifier::Exclude, rule), // Default to exclude
        };

        Ok(FilterRule {
            modifier,
            pattern: Pattern::new(pattern.trim())?,
            flags: FilterFlags::default(),
        })
    }
}
```

---

## Section 32: Error Messages Reference

This section documents error message formats and common errors.

### 32.1 Error Message Format

```text
rsync error: <description> (code <N>) at <file>:<line> [<role>=<version>]

Examples:
rsync error: some files could not be transferred (code 23) at main.c:1234 [sender=3.4.1-rust]
rsync error: timeout in data send/receive (code 30) at io.c:1234 [sender=3.4.1-rust]
```

### 32.2 Common Error Messages

| Message | Code | Cause | Solution |
|---------|------|-------|----------|
| `syntax or usage error` | 1 | Invalid command-line options | Check CLI syntax |
| `protocol incompatibility` | 2 | Version mismatch | Use `--protocol` |
| `errors selecting input/output files` | 3 | File access errors | Check permissions |
| `requested action not supported` | 4 | Missing feature | Enable feature flag |
| `error starting client-server protocol` | 5 | Daemon connection failed | Check daemon status |
| `error in socket I/O` | 10 | Network errors | Check connectivity |
| `error in file I/O` | 11 | Disk errors | Check disk space |
| `error in rsync protocol data stream` | 12 | Protocol corruption | Retry transfer |
| `timeout in data send/receive` | 30 | I/O timeout | Increase `--timeout` |
| `timeout waiting for daemon connection` | 35 | Connection timeout | Check firewall |

### 32.3 Error Message Implementation

```rust
/// Format an error message following upstream rsync conventions
pub fn format_error(
    description: &str,
    code: i32,
    role: Role,
    source_file: &str,
    source_line: u32,
) -> String {
    format!(
        "rsync error: {} (code {}) at {}:{} [{}={}]",
        description,
        code,
        source_file,
        source_line,
        role.as_str(),
        crate::VERSION
    )
}

#[derive(Clone, Copy)]
pub enum Role {
    Sender,
    Receiver,
    Generator,
    Server,
    Client,
    Daemon,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Sender => "sender",
            Role::Receiver => "receiver",
            Role::Generator => "generator",
            Role::Server => "server",
            Role::Client => "client",
            Role::Daemon => "daemon",
        }
    }
}
```

---

## Section 33: Troubleshooting Guide

This section provides troubleshooting steps for common issues.

### 33.1 Connection Issues

**Symptom**: `connection refused` or `timeout waiting for daemon`

**Diagnostic Steps**:
```bash
# Check daemon is running
pgrep -la oc-rsync

# Check daemon is listening
ss -tlnp | grep 873

# Test basic connectivity
nc -vz hostname 873

# Check firewall rules
iptables -L -n | grep 873
```

**Common Causes**:
- Daemon not running
- Firewall blocking port 873
- Wrong hostname/IP
- Daemon listening on different port

### 33.2 Authentication Issues

**Symptom**: `auth failed` or `password mismatch`

**Diagnostic Steps**:
```bash
# Check secrets file permissions
ls -la /etc/oc-rsyncd/oc-rsyncd.secrets
# Should be: -rw------- (0600)

# Verify username exists in secrets file
grep "^username:" /etc/oc-rsyncd/oc-rsyncd.secrets

# Check module auth users setting
grep -A5 "\[modulename\]" /etc/oc-rsyncd/oc-rsyncd.conf
```

**Common Causes**:
- Wrong password
- Username not in `auth users`
- Secrets file wrong permissions
- Secrets file format error

### 33.3 Permission Issues

**Symptom**: `permission denied` or `failed to open`

**Diagnostic Steps**:
```bash
# Check file ownership
ls -la /path/to/file

# Check effective uid/gid for daemon
id nobody

# Test access as daemon user
sudo -u nobody test -r /path/to/file && echo "readable" || echo "not readable"
```

**Common Causes**:
- Daemon running as wrong user
- File permissions too restrictive
- SELinux/AppArmor blocking access

### 33.4 Protocol Issues

**Symptom**: `protocol incompatibility` or `unexpected tag`

**Diagnostic Steps**:
```bash
# Check versions
oc-rsync --version
rsync --version

# Force specific protocol version
oc-rsync --protocol=30 source dest

# Enable verbose debugging
oc-rsync -vvv --debug=ALL source dest
```

**Common Causes**:
- Version mismatch
- Corrupted stream
- Incompatible options

### 33.5 Performance Issues

**Symptom**: Transfer slower than expected

**Diagnostic Steps**:
```bash
# Check network bandwidth
iperf3 -c hostname

# Profile transfer
oc-rsync --stats -v source dest

# Check compression overhead
time oc-rsync -z source dest
time oc-rsync source dest
```

**Common Causes**:
- Network bandwidth limit
- High compression overhead
- Small block size
- Many small files

---

## Section 34: Platform Notes

This section documents platform-specific behavior and requirements.

### 34.1 Linux Platform Notes

**Required Packages**:
```bash
# Debian/Ubuntu
apt install libacl1-dev libxattr1-dev

# RHEL/Fedora
dnf install libacl-devel libattr-devel
```

**Capabilities** (for non-root daemon):
```bash
# Allow binding to privileged ports
setcap 'cap_net_bind_service=+ep' /usr/bin/oc-rsync

# Allow setting file ownership
setcap 'cap_chown,cap_fowner=+ep' /usr/bin/oc-rsync
```

**Systemd Service**:
```ini
[Unit]
Description=oc-rsync daemon
After=network.target

[Service]
Type=notify
ExecStart=/usr/bin/oc-rsync --daemon --no-detach
Restart=on-failure
User=root
Group=root

[Install]
WantedBy=multi-user.target
```

### 34.2 macOS Platform Notes

**File System Limitations**:
- APFS doesn't support traditional Unix ACLs
- Extended attributes work but with different namespace
- Case-insensitive by default (can cause issues)

**Installation**:
```bash
# Homebrew
brew install oc-rsync
```

**Code Signing** (for Gatekeeper):
```bash
codesign --sign - --force --preserve-metadata=entitlements,requirements,flags,runtime /usr/local/bin/oc-rsync
```

### 34.3 Windows Platform Notes

**Path Handling**:
- Use forward slashes in paths
- Drive letters supported: `C:/Users/...`
- UNC paths supported: `//server/share/...`

**Limitations**:
- No Unix permissions (emulated via ACLs)
- No symlinks without developer mode
- No special files (devices, FIFOs)

**Installation**:
```powershell
# Windows Package Manager
winget install oc-rsync

# Or Scoop
scoop install oc-rsync
```

### 34.4 BSD Platform Notes

**FreeBSD Specific**:
```bash
# Install from ports
cd /usr/ports/net/oc-rsync && make install

# Or pkg
pkg install oc-rsync
```

**OpenBSD Specific**:
- Pledge support for sandboxing
- Unveil for filesystem restrictions

---

## Section 35: Performance Tuning Guide

This section provides performance optimization guidance.

### 35.1 Block Size Optimization

| File Size Range | Recommended Block Size | Rationale |
|-----------------|------------------------|-----------|
| < 1 KB | 512 | Minimum block size |
| 1 KB - 100 KB | 700 | Default, good balance |
| 100 KB - 1 MB | 1024-2048 | Reduce signature overhead |
| 1 MB - 100 MB | 4096-8192 | Fewer blocks to match |
| > 100 MB | 16384-65536 | Large file optimization |

```rust
/// Compute optimal block size based on file size
pub fn optimal_block_size(file_size: u64, available_memory: u64) -> u32 {
    // Target: ~1000 blocks per file for efficient matching
    let ideal = (file_size / 1000) as u32;

    // Clamp to valid range
    let clamped = ideal.clamp(512, 131072);

    // Ensure signature table fits in memory
    // Each signature entry is ~24 bytes
    let max_blocks = available_memory / 24;
    let max_block_size = (file_size / max_blocks.max(1)) as u32;

    clamped.min(max_block_size.max(512))
}
```

### 35.2 Compression Trade-offs

| Scenario | Recommendation | Rationale |
|----------|----------------|-----------|
| LAN transfer | No compression | CPU overhead > network savings |
| WAN transfer | zlib -6 | Good compression/speed balance |
| Slow WAN | zstd -19 | Maximum compression |
| Pre-compressed data | Skip compression | Already compressed |

### 35.3 Memory Optimization

```rust
/// Memory-efficient file processing
pub struct MemoryEfficientProcessor {
    /// Reuse buffers across operations
    read_buffer: Vec<u8>,
    checksum_buffer: Vec<u8>,
    /// Memory-map large files
    mmap_threshold: u64,
}

impl MemoryEfficientProcessor {
    pub fn new(buffer_size: usize, mmap_threshold: u64) -> Self {
        Self {
            read_buffer: Vec::with_capacity(buffer_size),
            checksum_buffer: Vec::with_capacity(buffer_size),
            mmap_threshold,
        }
    }

    pub fn process_file(&mut self, path: &Path) -> io::Result<FileSignature> {
        let metadata = fs::metadata(path)?;

        if metadata.len() > self.mmap_threshold {
            // Memory-map large files
            self.process_mmap(path)
        } else {
            // Read small files into buffer
            self.process_buffered(path)
        }
    }
}
```

### 35.4 I/O Optimization

| Optimization | Benefit | Implementation |
|--------------|---------|----------------|
| Vectored I/O | Reduce syscalls | `writev`/`readv` |
| Async I/O | Non-blocking | tokio runtime |
| Buffer pooling | Reduce allocation | `bytes::BytesMut` |
| Sparse files | Skip zero blocks | `SEEK_HOLE` |

### 35.5 Network Optimization

```rust
/// Configure socket for optimal transfer
pub fn configure_socket(socket: &TcpStream) -> io::Result<()> {
    // Enable TCP_NODELAY for low latency
    socket.set_nodelay(true)?;

    // Set buffer sizes for throughput
    let sock_ref = socket2::SockRef::from(socket);
    sock_ref.set_send_buffer_size(256 * 1024)?;
    sock_ref.set_recv_buffer_size(256 * 1024)?;

    // Enable TCP keepalive
    let keepalive = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10));
    sock_ref.set_tcp_keepalive(&keepalive)?;

    Ok(())
}
```

---

## Section 36: Security Reference

This section documents security considerations and best practices.

### 36.1 Authentication Methods

| Method | Security Level | Use Case |
|--------|----------------|----------|
| Anonymous | None | Public read-only data |
| Challenge-Response | Medium | Daemon authentication |
| SSH key | High | SSH transport |

### 36.2 Challenge-Response Authentication

```rust
/// Daemon challenge-response authentication
pub struct ChallengeAuth {
    challenge: [u8; 16],
}

impl ChallengeAuth {
    /// Generate a random challenge
    pub fn new() -> Self {
        let mut challenge = [0u8; 16];
        getrandom::getrandom(&mut challenge).expect("RNG failure");
        Self { challenge }
    }

    /// Compute expected response: MD4(password || challenge)
    pub fn compute_response(&self, password: &str) -> [u8; 16] {
        let mut hasher = Md4::new();
        hasher.update(password.as_bytes());
        hasher.update(&self.challenge);
        hasher.finalize().into()
    }

    /// Verify client response
    pub fn verify(&self, password: &str, response: &[u8; 16]) -> bool {
        let expected = self.compute_response(password);
        constant_time_eq(&expected, response)
    }
}
```

### 36.3 Secrets File Security

```bash
# Create secrets file with correct permissions
touch /etc/oc-rsyncd/oc-rsyncd.secrets
chmod 600 /etc/oc-rsyncd/oc-rsyncd.secrets
chown root:root /etc/oc-rsyncd/oc-rsyncd.secrets

# Verify permissions (must be exactly 0600)
stat -c "%a %U:%G" /etc/oc-rsyncd/oc-rsyncd.secrets
# Expected: 600 root:root
```

### 36.4 Chroot Security

```rust
/// Secure chroot setup
pub fn setup_chroot(module_path: &Path) -> io::Result<()> {
    // Validate path is absolute
    if !module_path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "chroot path must be absolute"
        ));
    }

    // Change root directory
    nix::unistd::chroot(module_path)?;
    std::env::set_current_dir("/")?;

    // Drop privileges
    let nobody = nix::unistd::User::from_name("nobody")?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "user 'nobody' not found"))?;

    nix::unistd::setgid(nobody.gid)?;
    nix::unistd::setuid(nobody.uid)?;

    Ok(())
}
```

### 36.5 Symlink Attack Prevention

```rust
/// Safe symlink handling to prevent directory traversal
pub fn safe_resolve_symlink(
    chroot: &Path,
    symlink: &Path,
) -> io::Result<PathBuf> {
    let target = fs::read_link(symlink)?;

    // Resolve relative to symlink's directory
    let resolved = if target.is_absolute() {
        chroot.join(target.strip_prefix("/").unwrap_or(&target))
    } else {
        symlink.parent().unwrap_or(Path::new("/")).join(&target)
    };

    // Canonicalize and verify still within chroot
    let canonical = resolved.canonicalize()?;
    if !canonical.starts_with(chroot) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "symlink escapes chroot"
        ));
    }

    Ok(canonical)
}
```

---

## Section 37: Upstream Compatibility Matrix

This section documents compatibility with upstream rsync versions.

### 37.1 Tested Upstream Versions

| Version | Release Date | Protocol | Status | Notes |
|---------|--------------|----------|--------|-------|
| 3.0.9 | 2011-09-23 | 30 | ✓ Full | Oldest supported |
| 3.1.3 | 2018-01-28 | 31 | ✓ Full | Widely deployed |
| 3.2.7 | 2022-10-20 | 31 | ✓ Full | Current stable |
| 3.4.1 | 2024-12-24 | 32 | ✓ Full | Latest release |

### 37.2 Feature Support Matrix

| Feature | 3.0.9 | 3.1.3 | 3.2.7 | 3.4.1 | oc-rsync |
|---------|:-----:|:-----:|:-----:|:-----:|:--------:|
| Basic transfer | ✓ | ✓ | ✓ | ✓ | ✓ |
| Delta transfer | ✓ | ✓ | ✓ | ✓ | ✓ |
| Compression (zlib) | ✓ | ✓ | ✓ | ✓ | ✓ |
| Compression (zstd) | | | ✓ | ✓ | ✓ |
| Incremental recursion | ✓ | ✓ | ✓ | ✓ | ✓ |
| ACLs | ✓ | ✓ | ✓ | ✓ | ✓ |
| Extended attributes | ✓ | ✓ | ✓ | ✓ | ✓ |
| XXH3 checksum | | | | ✓ | ✓ |
| XXH128 checksum | | | | ✓ | ✓ |
| Nanosecond times | | ✓ | ✓ | ✓ | ✓ |

### 37.3 Interoperability Testing

```rust
/// Interop test configuration
pub struct InteropTest {
    pub upstream_version: &'static str,
    pub protocol_version: u8,
    pub test_cases: &'static [TestCase],
}

pub static INTEROP_TESTS: &[InteropTest] = &[
    InteropTest {
        upstream_version: "3.0.9",
        protocol_version: 30,
        test_cases: &[
            TestCase::BasicTransfer,
            TestCase::DeltaTransfer,
            TestCase::Compression,
            TestCase::Permissions,
        ],
    },
    InteropTest {
        upstream_version: "3.1.3",
        protocol_version: 31,
        test_cases: &[
            TestCase::BasicTransfer,
            TestCase::DeltaTransfer,
            TestCase::Compression,
            TestCase::Permissions,
            TestCase::NanosecondTimes,
            TestCase::IncrementalRecursion,
        ],
    },
    InteropTest {
        upstream_version: "3.4.1",
        protocol_version: 32,
        test_cases: &[
            TestCase::BasicTransfer,
            TestCase::DeltaTransfer,
            TestCase::Compression,
            TestCase::CompressionZstd,
            TestCase::Permissions,
            TestCase::NanosecondTimes,
            TestCase::IncrementalRecursion,
            TestCase::Xxh3Checksum,
            TestCase::Xxh128Checksum,
        ],
    },
];
```

### 37.4 Known Compatibility Issues

| Issue | Affected Versions | Workaround |
|-------|-------------------|------------|
| MD4 deprecated | 3.2.7+ | Use `--checksum-choice=md5` |
| Old zlib format | < 3.1.0 | Use `--compress-choice=zlib` |
| Missing symlink times | < 3.1.0 | Use `--omit-link-times` |

---

## Section 38: Itemize Output Format

This section documents the itemize output format used with `-i/--itemize-changes`.

### 38.1 Itemize String Format

The itemize string consists of 11 characters in the format `YXcstpoguax`:

| Position | Character | Meaning |
|----------|-----------|---------|
| 0 | Y | Update type: `<` = sent, `>` = received, `c` = local change, `h` = hard link, `.` = not updated, `*` = message |
| 1 | X | File type: `f` = file, `d` = dir, `L` = symlink, `D` = device, `S` = special |
| 2 | c | Checksum differs (regular files), or value differs (symlink, device, special) |
| 3 | s | Size differs |
| 4 | t | Modification time differs |
| 5 | p | Permissions differ |
| 6 | o | Owner differs |
| 7 | g | Group differs |
| 8 | u | Reserved for future use |
| 9 | a | ACL differs |
| 10 | x | Extended attributes differ |

### 38.2 Itemize Examples

```text
>f..t......    - File received, modification time updated
>f.st......    - File received, size and time changed
.d..t......    - Directory, time updated
>f+++++++++    - New file received
*deleting      - File being deleted
cL..t......    - Symlink changed, time updated
hf..t......    - Hard link created, time updated
>fcs.......    - File received, checksum and size changed
```

### 38.3 Itemize Implementation

```rust
/// Itemize change flags
#[derive(Default)]
pub struct ItemizeFlags {
    pub update_type: UpdateType,
    pub file_type: FileType,
    pub checksum_changed: bool,
    pub size_changed: bool,
    pub time_changed: bool,
    pub perms_changed: bool,
    pub owner_changed: bool,
    pub group_changed: bool,
    pub acl_changed: bool,
    pub xattr_changed: bool,
}

#[derive(Default)]
pub enum UpdateType {
    #[default]
    NotUpdated,  // '.'
    Sent,        // '<'
    Received,    // '>'
    LocalChange, // 'c'
    HardLink,    // 'h'
    Message,     // '*'
}

#[derive(Default)]
pub enum FileType {
    #[default]
    File,      // 'f'
    Directory, // 'd'
    Symlink,   // 'L'
    Device,    // 'D'
    Special,   // 'S'
}

impl ItemizeFlags {
    pub fn to_string(&self) -> String {
        let mut s = String::with_capacity(11);

        s.push(match self.update_type {
            UpdateType::NotUpdated => '.',
            UpdateType::Sent => '<',
            UpdateType::Received => '>',
            UpdateType::LocalChange => 'c',
            UpdateType::HardLink => 'h',
            UpdateType::Message => '*',
        });

        s.push(match self.file_type {
            FileType::File => 'f',
            FileType::Directory => 'd',
            FileType::Symlink => 'L',
            FileType::Device => 'D',
            FileType::Special => 'S',
        });

        s.push(if self.checksum_changed { 'c' } else { '.' });
        s.push(if self.size_changed { 's' } else { '.' });
        s.push(if self.time_changed { 't' } else { '.' });
        s.push(if self.perms_changed { 'p' } else { '.' });
        s.push(if self.owner_changed { 'o' } else { '.' });
        s.push(if self.group_changed { 'g' } else { '.' });
        s.push('.'); // Reserved
        s.push(if self.acl_changed { 'a' } else { '.' });
        s.push(if self.xattr_changed { 'x' } else { '.' });

        s
    }
}
```

---

## Section 39: Info and Debug Flags Reference

This section documents the `--info` and `--debug` flag options.

### 39.1 Info Flags

| Flag | Description | Verbosity Level |
|------|-------------|-----------------|
| `BACKUP` | Backup file creation | -v |
| `COPY` | File copying | -v |
| `DEL` | File deletion | -v |
| `FLIST` | File list building | -vv |
| `MISC` | Miscellaneous messages | -v |
| `MOUNT` | Mount point messages | -vv |
| `NAME` | File name messages | -v |
| `PROGRESS` | Transfer progress | --progress |
| `SKIP` | Skipped files | -vv |
| `STATS` | Transfer statistics | --stats |
| `SYMSAFE` | Symlink safety messages | -vv |

### 39.2 Debug Flags

| Flag | Description | Debug Level |
|------|-------------|-------------|
| `ACL` | ACL processing | --debug=ACL |
| `BACKUP` | Backup operations | --debug=BACKUP |
| `BIND` | Socket binding | --debug=BIND |
| `CHDIR` | Directory changes | --debug=CHDIR |
| `CONNECT` | Connection details | --debug=CONNECT |
| `CMD` | Remote command | --debug=CMD |
| `DEL` | Deletion details | --debug=DEL |
| `DELTASUM` | Delta/checksum | --debug=DELTASUM |
| `DUP` | Duplicate handling | --debug=DUP |
| `EXIT` | Exit codes | --debug=EXIT |
| `FILTER` | Filter rules | --debug=FILTER |
| `FLIST` | File list | --debug=FLIST |
| `FUZZY` | Fuzzy matching | --debug=FUZZY |
| `GENR` | Generator | --debug=GENR |
| `HASH` | Hashtable | --debug=HASH |
| `HLINK` | Hard links | --debug=HLINK |
| `ICONV` | Charset conversion | --debug=ICONV |
| `IO` | I/O operations | --debug=IO |
| `NSTR` | Name string | --debug=NSTR |
| `OWN` | Owner mapping | --debug=OWN |
| `RECV` | Receiver | --debug=RECV |
| `SEND` | Sender | --debug=SEND |
| `TIME` | Timing | --debug=TIME |

### 39.3 Flag Parsing Implementation

```rust
/// Parse --info and --debug flags
pub fn parse_info_flags(flags: &str) -> InfoFlags {
    let mut result = InfoFlags::default();

    for flag in flags.split(',') {
        let (name, enabled) = if let Some(stripped) = flag.strip_prefix("no") {
            (stripped, false)
        } else {
            (flag, true)
        };

        match name.to_uppercase().as_str() {
            "BACKUP" => result.backup = enabled,
            "COPY" => result.copy = enabled,
            "DEL" => result.del = enabled,
            "FLIST" => result.flist = enabled,
            "MISC" => result.misc = enabled,
            "MOUNT" => result.mount = enabled,
            "NAME" => result.name = enabled,
            "PROGRESS" => result.progress = enabled,
            "SKIP" => result.skip = enabled,
            "STATS" => result.stats = enabled,
            "SYMSAFE" => result.symsafe = enabled,
            "ALL" => result = InfoFlags::all(),
            "NONE" => result = InfoFlags::default(),
            _ => {} // Ignore unknown flags
        }
    }

    result
}

pub fn parse_debug_flags(flags: &str) -> DebugFlags {
    let mut result = DebugFlags::default();

    for flag in flags.split(',') {
        let (name, level) = if let Some((n, l)) = flag.split_once('=') {
            (n, l.parse().unwrap_or(1))
        } else {
            (flag, 1)
        };

        match name.to_uppercase().as_str() {
            "ACL" => result.acl = level,
            "DELTASUM" => result.deltasum = level,
            "FILTER" => result.filter = level,
            "FLIST" => result.flist = level,
            "FUZZY" => result.fuzzy = level,
            "GENR" => result.genr = level,
            "HLINK" => result.hlink = level,
            "IO" => result.io = level,
            "RECV" => result.recv = level,
            "SEND" => result.send = level,
            "ALL" => result = DebugFlags::all_at_level(level),
            "NONE" => result = DebugFlags::default(),
            _ => {}
        }
    }

    result
}
```

---

## Section 40: Delta Wire Format

This section documents the delta transfer wire format.

### 40.1 Delta Token Format

```text
Token types:
  - Literal data: token < 0, followed by -token bytes of literal data
  - Block match: token > 0, represents block index (1-based)
  - End marker: token = 0

Wire format for tokens:
  Single byte (0x00-0x7F): Literal of length 1-127
  0x80 + 2 bytes: Literal of length 128-32895
  0x81 + 4 bytes: Literal of length 32896-4294967295
  0x82-0xFF + varint: Block match (index = token - 0x82)
```

### 40.2 Sum Header Format

```text
Protocol 30+:
+---------------+---------------+---------------+---------------+
| count (4B LE) | blength (4B)  | s2length (4B) | remainder (4B)|
+---------------+---------------+---------------+---------------+

count:     Number of blocks
blength:   Block length in bytes
s2length:  Strong checksum length (2-16)
remainder: Size of final partial block
```

### 40.3 Delta Wire Implementation

```rust
/// Delta token types
pub enum DeltaToken {
    /// Literal data bytes
    Literal(Vec<u8>),
    /// Block match by index (0-based internally, 1-based on wire)
    Match(u32),
    /// End of delta stream
    End,
}

/// Write a delta token to the wire
pub fn write_delta_token<W: Write>(writer: &mut W, token: &DeltaToken) -> io::Result<()> {
    match token {
        DeltaToken::Literal(data) => {
            let len = data.len();
            if len <= 127 {
                // Single byte length
                writer.write_all(&[len as u8])?;
            } else if len <= 32895 {
                // Two byte length
                let adjusted = (len - 128) as u16;
                writer.write_all(&[0x80])?;
                writer.write_all(&adjusted.to_le_bytes())?;
            } else {
                // Four byte length
                writer.write_all(&[0x81])?;
                writer.write_all(&(len as u32).to_le_bytes())?;
            }
            writer.write_all(data)?;
        }
        DeltaToken::Match(index) => {
            // Block matches are 1-based on wire
            let wire_index = index + 1;
            if wire_index < 0x7E {
                writer.write_all(&[(wire_index + 0x82) as u8])?;
            } else {
                writer.write_all(&[0xFF])?;
                write_varint(writer, wire_index as u64)?;
            }
        }
        DeltaToken::End => {
            writer.write_all(&[0x00])?;
        }
    }
    Ok(())
}

/// Read a delta token from the wire
pub fn read_delta_token<R: Read>(reader: &mut R) -> io::Result<DeltaToken> {
    let mut tag = [0u8; 1];
    reader.read_exact(&mut tag)?;

    match tag[0] {
        0x00 => Ok(DeltaToken::End),
        0x01..=0x7F => {
            // Short literal
            let mut data = vec![0u8; tag[0] as usize];
            reader.read_exact(&mut data)?;
            Ok(DeltaToken::Literal(data))
        }
        0x80 => {
            // Medium literal
            let mut len_bytes = [0u8; 2];
            reader.read_exact(&mut len_bytes)?;
            let len = u16::from_le_bytes(len_bytes) as usize + 128;
            let mut data = vec![0u8; len];
            reader.read_exact(&mut data)?;
            Ok(DeltaToken::Literal(data))
        }
        0x81 => {
            // Long literal
            let mut len_bytes = [0u8; 4];
            reader.read_exact(&mut len_bytes)?;
            let len = u32::from_le_bytes(len_bytes) as usize;
            let mut data = vec![0u8; len];
            reader.read_exact(&mut data)?;
            Ok(DeltaToken::Literal(data))
        }
        0x82..=0xFE => {
            // Short block match
            let index = (tag[0] - 0x82) as u32;
            Ok(DeltaToken::Match(index))
        }
        0xFF => {
            // Long block match
            let wire_index = read_varint(reader)? as u32;
            Ok(DeltaToken::Match(wire_index - 1))
        }
    }
}

/// Sum header structure
pub struct SumHead {
    pub count: u32,
    pub blength: u32,
    pub s2length: u32,
    pub remainder: u32,
}

impl SumHead {
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.count.to_le_bytes())?;
        writer.write_all(&self.blength.to_le_bytes())?;
        writer.write_all(&self.s2length.to_le_bytes())?;
        writer.write_all(&self.remainder.to_le_bytes())?;
        Ok(())
    }

    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf)?;
        Ok(Self {
            count: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            blength: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            s2length: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            remainder: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        })
    }
}
```

---

## Section 41: Statistics Wire Format

This section documents the transfer statistics wire format.

### 41.1 Statistics Structure

```text
Protocol 30+:
+------------------+------------------+------------------+
| total_read (8B)  | total_written (8B)| total_size (8B) |
+------------------+------------------+------------------+

Protocol 28-29:
+------------------+------------------+------------------+
| total_read (4B)  | total_written (4B)| total_size (4B) |
+------------------+------------------+------------------+
```

### 41.2 Statistics Implementation

```rust
/// Transfer statistics
pub struct TransferStats {
    pub total_read: u64,
    pub total_written: u64,
    pub total_size: u64,
    pub num_files: u64,
    pub num_transferred_files: u64,
    pub literal_data: u64,
    pub matched_data: u64,
    pub flist_buildtime: Duration,
    pub flist_xfertime: Duration,
}

impl TransferStats {
    /// Write statistics to wire (protocol-aware)
    pub fn write<W: Write>(&self, writer: &mut W, protocol: ProtocolVersion) -> io::Result<()> {
        if protocol.as_u8() >= 30 {
            writer.write_all(&self.total_read.to_le_bytes())?;
            writer.write_all(&self.total_written.to_le_bytes())?;
            writer.write_all(&self.total_size.to_le_bytes())?;
        } else {
            writer.write_all(&(self.total_read as u32).to_le_bytes())?;
            writer.write_all(&(self.total_written as u32).to_le_bytes())?;
            writer.write_all(&(self.total_size as u32).to_le_bytes())?;
        }
        Ok(())
    }

    /// Read statistics from wire (protocol-aware)
    pub fn read<R: Read>(reader: &mut R, protocol: ProtocolVersion) -> io::Result<Self> {
        let (total_read, total_written, total_size) = if protocol.as_u8() >= 30 {
            let mut buf = [0u8; 24];
            reader.read_exact(&mut buf)?;
            (
                u64::from_le_bytes(buf[0..8].try_into().unwrap()),
                u64::from_le_bytes(buf[8..16].try_into().unwrap()),
                u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            )
        } else {
            let mut buf = [0u8; 12];
            reader.read_exact(&mut buf)?;
            (
                u32::from_le_bytes(buf[0..4].try_into().unwrap()) as u64,
                u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64,
                u32::from_le_bytes(buf[8..12].try_into().unwrap()) as u64,
            )
        };

        Ok(Self {
            total_read,
            total_written,
            total_size,
            ..Default::default()
        })
    }

    /// Calculate speedup ratio
    pub fn speedup(&self) -> f64 {
        if self.total_written == 0 {
            0.0
        } else {
            self.total_size as f64 / self.total_written as f64
        }
    }

    /// Format for display
    pub fn format_summary(&self) -> String {
        format!(
            "sent {} bytes  received {} bytes  {:.2} bytes/sec\n\
             total size is {}  speedup is {:.2}",
            self.total_written,
            self.total_read,
            (self.total_read + self.total_written) as f64,
            self.total_size,
            self.speedup()
        )
    }
}
```

---

## Section 42: ACL Wire Format

This section documents the ACL (Access Control List) wire format.

### 42.1 ACL Wire Structure

```text
POSIX ACL format:
+----------+----------+----------+
| count    | entries  | mask     |
| (varint) | (N*entry)| (optional)|
+----------+----------+----------+

Entry format:
+----------+----------+----------+
| tag      | qualifier| perms    |
| (1 byte) | (varint) | (1 byte) |
+----------+----------+----------+

Tag values:
  1 = ACL_USER_OBJ   (owner)
  2 = ACL_USER       (named user)
  4 = ACL_GROUP_OBJ  (owning group)
  8 = ACL_GROUP      (named group)
 16 = ACL_MASK       (mask)
 32 = ACL_OTHER      (other)
```

### 42.2 ACL Implementation

```rust
/// ACL entry tag types
#[derive(Clone, Copy, PartialEq)]
pub enum AclTag {
    UserObj = 1,
    User = 2,
    GroupObj = 4,
    Group = 8,
    Mask = 16,
    Other = 32,
}

/// ACL entry permissions
#[derive(Clone, Copy, Default)]
pub struct AclPerms {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl AclPerms {
    pub fn to_byte(&self) -> u8 {
        let mut b = 0u8;
        if self.read { b |= 4; }
        if self.write { b |= 2; }
        if self.execute { b |= 1; }
        b
    }

    pub fn from_byte(b: u8) -> Self {
        Self {
            read: b & 4 != 0,
            write: b & 2 != 0,
            execute: b & 1 != 0,
        }
    }
}

/// Single ACL entry
pub struct AclEntry {
    pub tag: AclTag,
    pub qualifier: Option<u32>, // uid or gid for named entries
    pub perms: AclPerms,
}

/// Full ACL
pub struct Acl {
    pub entries: Vec<AclEntry>,
}

impl Acl {
    /// Write ACL to wire
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        write_varint(writer, self.entries.len() as u64)?;

        for entry in &self.entries {
            writer.write_all(&[entry.tag as u8])?;
            if let Some(qual) = entry.qualifier {
                write_varint(writer, qual as u64)?;
            }
            writer.write_all(&[entry.perms.to_byte()])?;
        }

        Ok(())
    }

    /// Read ACL from wire
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let count = read_varint(reader)? as usize;
        let mut entries = Vec::with_capacity(count);

        for _ in 0..count {
            let mut tag_byte = [0u8; 1];
            reader.read_exact(&mut tag_byte)?;

            let tag = match tag_byte[0] {
                1 => AclTag::UserObj,
                2 => AclTag::User,
                4 => AclTag::GroupObj,
                8 => AclTag::Group,
                16 => AclTag::Mask,
                32 => AclTag::Other,
                _ => return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid ACL tag"
                )),
            };

            let qualifier = match tag {
                AclTag::User | AclTag::Group => Some(read_varint(reader)? as u32),
                _ => None,
            };

            let mut perms_byte = [0u8; 1];
            reader.read_exact(&mut perms_byte)?;

            entries.push(AclEntry {
                tag,
                qualifier,
                perms: AclPerms::from_byte(perms_byte[0]),
            });
        }

        Ok(Self { entries })
    }
}
```

---

## Section 43: Extended Attributes Wire Format

This section documents the xattr (extended attributes) wire format.

### 43.1 Xattr Wire Structure

```text
Xattr list format:
+----------+----------------------------+
| count    | entries                    |
| (varint) | (N * entry)                |
+----------+----------------------------+

Entry format:
+----------+----------+----------+
| name_len | name     | value_len | value |
| (varint) | (bytes)  | (varint)  | (bytes)|
+----------+----------+----------+
```

### 43.2 Xattr Implementation

```rust
/// Extended attribute entry
pub struct XattrEntry {
    pub name: Vec<u8>,
    pub value: Vec<u8>,
}

/// Extended attributes collection
pub struct Xattrs {
    pub entries: Vec<XattrEntry>,
}

impl Xattrs {
    /// Write xattrs to wire
    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        write_varint(writer, self.entries.len() as u64)?;

        for entry in &self.entries {
            write_varint(writer, entry.name.len() as u64)?;
            writer.write_all(&entry.name)?;
            write_varint(writer, entry.value.len() as u64)?;
            writer.write_all(&entry.value)?;
        }

        Ok(())
    }

    /// Read xattrs from wire
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let count = read_varint(reader)? as usize;
        let mut entries = Vec::with_capacity(count);

        for _ in 0..count {
            let name_len = read_varint(reader)? as usize;
            let mut name = vec![0u8; name_len];
            reader.read_exact(&mut name)?;

            let value_len = read_varint(reader)? as usize;
            let mut value = vec![0u8; value_len];
            reader.read_exact(&mut value)?;

            entries.push(XattrEntry { name, value });
        }

        Ok(Self { entries })
    }

    /// Get namespace for attribute name
    pub fn get_namespace(name: &[u8]) -> Option<XattrNamespace> {
        if name.starts_with(b"user.") {
            Some(XattrNamespace::User)
        } else if name.starts_with(b"system.") {
            Some(XattrNamespace::System)
        } else if name.starts_with(b"security.") {
            Some(XattrNamespace::Security)
        } else if name.starts_with(b"trusted.") {
            Some(XattrNamespace::Trusted)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum XattrNamespace {
    User,
    System,
    Security,
    Trusted,
}
```

---

## Section 44: Batch Mode Reference

This section documents batch mode operation.

### 44.1 Batch Mode Options

| Option | Description |
|--------|-------------|
| `--write-batch=FILE` | Write batch file for later replay |
| `--read-batch=FILE` | Read and replay a batch file |
| `--only-write-batch=FILE` | Write batch without performing transfer |

### 44.2 Batch File Format

```text
Batch file structure:
+----------------+----------------+----------------+
| Header         | File list      | Delta data     |
+----------------+----------------+----------------+

Header (64 bytes):
+----------------+----------------+----------------+
| Magic (8B)     | Version (4B)   | Flags (4B)     |
+----------------+----------------+----------------+
| Checksum info  | Filter info    | Options        |
+----------------+----------------+----------------+
```

### 44.3 Batch Mode Implementation

```rust
/// Batch file header
pub struct BatchHeader {
    pub magic: [u8; 8],
    pub version: u32,
    pub flags: BatchFlags,
    pub checksum_seed: u32,
    pub protocol_version: u32,
}

impl BatchHeader {
    pub const MAGIC: [u8; 8] = *b"rsync\x00\x1b\x00";

    pub fn write<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.magic)?;
        writer.write_all(&self.version.to_le_bytes())?;
        writer.write_all(&self.flags.bits().to_le_bytes())?;
        writer.write_all(&self.checksum_seed.to_le_bytes())?;
        writer.write_all(&self.protocol_version.to_le_bytes())?;
        Ok(())
    }

    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;
        if magic != Self::MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid batch file magic"
            ));
        }

        let mut buf = [0u8; 16];
        reader.read_exact(&mut buf)?;

        Ok(Self {
            magic,
            version: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            flags: BatchFlags::from_bits_truncate(
                u32::from_le_bytes(buf[4..8].try_into().unwrap())
            ),
            checksum_seed: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            protocol_version: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        })
    }
}

bitflags::bitflags! {
    pub struct BatchFlags: u32 {
        const RECURSIVE = 0x0001;
        const PRESERVE_PERMS = 0x0002;
        const PRESERVE_TIMES = 0x0004;
        const PRESERVE_OWNER = 0x0008;
        const PRESERVE_GROUP = 0x0010;
        const PRESERVE_DEVICES = 0x0020;
        const PRESERVE_LINKS = 0x0040;
        const PRESERVE_HARD_LINKS = 0x0080;
    }
}
```

---

## Section 45: Log Format Variables Reference

This section documents log format variables for `--out-format` and `--log-file-format`.

### 45.1 Log Format Variables

| Variable | Description |
|----------|-------------|
| `%a` | Remote IP address |
| `%b` | Bytes transferred |
| `%B` | Permission bits (e.g., "rwxr-xr-x") |
| `%c` | Checksum bytes (when --checksum used) |
| `%C` | MD5 checksum (if available) |
| `%f` | Filename (long form) |
| `%G` | Group ID |
| `%h` | Remote host name |
| `%i` | Itemized changes string |
| `%l` | File length in bytes |
| `%L` | Symlink target (if symlink) |
| `%m` | Module name |
| `%M` | Modification time (YYYY/MM/DD-HH:MM:SS) |
| `%n` | Filename (short form) |
| `%o` | Operation: "send", "recv", or "del." |
| `%p` | Process ID |
| `%P` | Path of file (module-relative) |
| `%t` | Current date/time |
| `%u` | Authenticated username |
| `%U` | User ID |

### 45.2 Log Format Implementation

```rust
/// Format a log entry using format string
pub fn format_log_entry(
    format: &str,
    context: &LogContext,
) -> String {
    let mut result = String::new();
    let mut chars = format.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('a') => result.push_str(&context.remote_addr),
                Some('b') => result.push_str(&context.bytes_transferred.to_string()),
                Some('B') => result.push_str(&format_perms(context.mode)),
                Some('c') => result.push_str(&context.checksum_bytes.to_string()),
                Some('f') => result.push_str(&context.filename_long),
                Some('G') => result.push_str(&context.gid.to_string()),
                Some('h') => result.push_str(&context.remote_host),
                Some('i') => result.push_str(&context.itemize.to_string()),
                Some('l') => result.push_str(&context.file_length.to_string()),
                Some('L') => {
                    if let Some(ref target) = context.symlink_target {
                        result.push_str(" -> ");
                        result.push_str(target);
                    }
                }
                Some('m') => result.push_str(&context.module_name),
                Some('M') => result.push_str(&format_mtime(context.mtime)),
                Some('n') => result.push_str(&context.filename_short),
                Some('o') => result.push_str(context.operation.as_str()),
                Some('p') => result.push_str(&std::process::id().to_string()),
                Some('P') => result.push_str(&context.module_path),
                Some('t') => result.push_str(&format_timestamp(context.timestamp)),
                Some('u') => result.push_str(&context.username),
                Some('U') => result.push_str(&context.uid.to_string()),
                Some('%') => result.push('%'),
                Some(other) => {
                    result.push('%');
                    result.push(other);
                }
                None => result.push('%'),
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Log context for format variables
pub struct LogContext {
    pub remote_addr: String,
    pub remote_host: String,
    pub bytes_transferred: u64,
    pub checksum_bytes: u64,
    pub filename_long: String,
    pub filename_short: String,
    pub gid: u32,
    pub uid: u32,
    pub mode: u32,
    pub file_length: u64,
    pub symlink_target: Option<String>,
    pub module_name: String,
    pub module_path: String,
    pub mtime: i64,
    pub operation: Operation,
    pub username: String,
    pub timestamp: i64,
    pub itemize: ItemizeFlags,
}

pub enum Operation {
    Send,
    Recv,
    Delete,
}

impl Operation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::Send => "send",
            Operation::Recv => "recv",
            Operation::Delete => "del.",
        }
    }
}
```

---

## Section 46: Hard Link Wire Format

This section documents the hard link wire format.

### 46.1 Hard Link Structure

```text
Hard link info (protocol 30+):
+----------+----------+----------+
| dev      | inode    | link_ndx |
| (varint) | (varint) | (varint) |
+----------+----------+----------+

Hard link flags in XMIT:
  XMIT_HLINKED (0x0400)    - File has hard link info
  XMIT_HLINK_FIRST (0x4000) - First occurrence of this inode
```

### 46.2 Hard Link Implementation

```rust
/// Hard link information
pub struct HardLinkInfo {
    pub dev: u64,
    pub inode: u64,
    pub link_ndx: Option<i32>, // Index of first file with this inode
}

/// Hard link tracker for file list
pub struct HardLinkTracker {
    /// Map from (dev, inode) to file list index
    seen: HashMap<(u64, u64), i32>,
}

impl HardLinkTracker {
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    /// Check if this inode has been seen before
    pub fn check(&mut self, dev: u64, inode: u64, current_ndx: i32) -> HardLinkResult {
        match self.seen.entry((dev, inode)) {
            std::collections::hash_map::Entry::Occupied(e) => {
                HardLinkResult::LinkTo(*e.get())
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(current_ndx);
                HardLinkResult::First
            }
        }
    }
}

pub enum HardLinkResult {
    /// First occurrence of this inode
    First,
    /// Links to file at given index
    LinkTo(i32),
}

/// Write hard link info to wire
pub fn write_hlink_info<W: Write>(
    writer: &mut W,
    info: &HardLinkInfo,
    protocol: ProtocolVersion,
) -> io::Result<()> {
    if protocol.as_u8() >= 30 {
        write_varint(writer, info.dev)?;
        write_varint(writer, info.inode)?;
        if let Some(ndx) = info.link_ndx {
            write_varint(writer, ndx as u64)?;
        }
    } else {
        // Protocol 28-29: 4-byte dev, 4-byte inode
        writer.write_all(&(info.dev as u32).to_le_bytes())?;
        writer.write_all(&(info.inode as u32).to_le_bytes())?;
    }
    Ok(())
}

/// Read hard link info from wire
pub fn read_hlink_info<R: Read>(
    reader: &mut R,
    protocol: ProtocolVersion,
    is_first: bool,
) -> io::Result<HardLinkInfo> {
    let (dev, inode) = if protocol.as_u8() >= 30 {
        (read_varint(reader)?, read_varint(reader)?)
    } else {
        let mut buf = [0u8; 8];
        reader.read_exact(&mut buf)?;
        (
            u32::from_le_bytes(buf[0..4].try_into().unwrap()) as u64,
            u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64,
        )
    };

    let link_ndx = if !is_first && protocol.as_u8() >= 30 {
        Some(read_varint(reader)? as i32)
    } else {
        None
    };

    Ok(HardLinkInfo { dev, inode, link_ndx })
}
```

---

## Section 47: Symlink Wire Format

This section documents the symlink wire format.

### 47.1 Symlink Structure

```text
Symlink entry in file list:
+----------+------------+
| name     | target     |
| (string) | (string)   |
+----------+------------+

Target is stored after the regular file entry data.
Length is implicit from file list entry size field.
```

### 47.2 Symlink Implementation

```rust
/// Symlink entry
pub struct SymlinkEntry {
    pub path: PathBuf,
    pub target: PathBuf,
}

/// Write symlink target to wire
pub fn write_symlink_target<W: Write>(
    writer: &mut W,
    target: &Path,
    protocol: ProtocolVersion,
) -> io::Result<()> {
    let target_bytes = target.as_os_str().as_encoded_bytes();

    if protocol.as_u8() >= 30 {
        write_varint(writer, target_bytes.len() as u64)?;
    } else {
        writer.write_all(&(target_bytes.len() as u32).to_le_bytes())?;
    }
    writer.write_all(target_bytes)?;

    Ok(())
}

/// Read symlink target from wire
pub fn read_symlink_target<R: Read>(
    reader: &mut R,
    protocol: ProtocolVersion,
) -> io::Result<PathBuf> {
    let len = if protocol.as_u8() >= 30 {
        read_varint(reader)? as usize
    } else {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        u32::from_le_bytes(buf) as usize
    };

    let mut target = vec![0u8; len];
    reader.read_exact(&mut target)?;

    // Convert to PathBuf (platform-specific)
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&target)))
    }
    #[cfg(not(unix))]
    {
        Ok(PathBuf::from(String::from_utf8_lossy(&target).into_owned()))
    }
}

/// Symlink safety check (munge symlinks in chroot)
pub fn munge_symlink(target: &Path, chroot: &Path) -> PathBuf {
    // Prefix absolute symlinks with chroot path
    if target.is_absolute() {
        let relative = target.strip_prefix("/").unwrap_or(target);
        chroot.join(relative)
    } else {
        target.to_path_buf()
    }
}

/// Check if symlink would escape chroot
pub fn symlink_escapes_chroot(
    symlink_dir: &Path,
    target: &Path,
    chroot: &Path,
) -> bool {
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        symlink_dir.join(target)
    };

    // Normalize path
    match resolved.canonicalize() {
        Ok(canonical) => !canonical.starts_with(chroot),
        Err(_) => true, // Assume escape if can't resolve
    }
}
```

---

## Section 48: Device and Special File Wire Format

This section documents the device and special file wire format.

### 48.1 Device File Structure

```text
Device file entry:
+----------+----------+
| rdev_major | rdev_minor |
| (varint)   | (varint)   |
+----------+----------+

XMIT flags:
  XMIT_SAME_RDEV_MAJOR (0x0008) - Same major as previous
  XMIT_SAME_RDEV_MINOR (0x0200) - Same minor as previous (proto 28+)
```

### 48.2 Device Implementation

```rust
/// Device file information
pub struct DeviceInfo {
    pub major: u32,
    pub minor: u32,
}

impl DeviceInfo {
    /// Create from raw dev_t
    #[cfg(unix)]
    pub fn from_dev(dev: libc::dev_t) -> Self {
        Self {
            major: unsafe { libc::major(dev) as u32 },
            minor: unsafe { libc::minor(dev) as u32 },
        }
    }

    /// Convert to raw dev_t
    #[cfg(unix)]
    pub fn to_dev(&self) -> libc::dev_t {
        unsafe { libc::makedev(self.major, self.minor) }
    }
}

/// Write device info to wire
pub fn write_device_info<W: Write>(
    writer: &mut W,
    info: &DeviceInfo,
    prev: Option<&DeviceInfo>,
    protocol: ProtocolVersion,
) -> io::Result<u16> {
    let mut flags = 0u16;

    let write_major = prev.map_or(true, |p| p.major != info.major);
    let write_minor = prev.map_or(true, |p| p.minor != info.minor);

    if !write_major {
        flags |= 0x0008; // XMIT_SAME_RDEV_MAJOR
    }
    if !write_minor && protocol.as_u8() >= 28 {
        flags |= 0x0200; // XMIT_SAME_RDEV_MINOR
    }

    if write_major {
        if protocol.as_u8() >= 30 {
            write_varint(writer, info.major as u64)?;
        } else {
            writer.write_all(&info.major.to_le_bytes())?;
        }
    }

    if write_minor {
        if protocol.as_u8() >= 30 {
            write_varint(writer, info.minor as u64)?;
        } else {
            writer.write_all(&info.minor.to_le_bytes())?;
        }
    }

    Ok(flags)
}

/// Read device info from wire
pub fn read_device_info<R: Read>(
    reader: &mut R,
    flags: u16,
    prev: Option<&DeviceInfo>,
    protocol: ProtocolVersion,
) -> io::Result<DeviceInfo> {
    let major = if flags & 0x0008 != 0 {
        prev.map(|p| p.major).unwrap_or(0)
    } else if protocol.as_u8() >= 30 {
        read_varint(reader)? as u32
    } else {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        u32::from_le_bytes(buf)
    };

    let minor = if flags & 0x0200 != 0 && protocol.as_u8() >= 28 {
        prev.map(|p| p.minor).unwrap_or(0)
    } else if protocol.as_u8() >= 30 {
        read_varint(reader)? as u32
    } else {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        u32::from_le_bytes(buf)
    };

    Ok(DeviceInfo { major, minor })
}
```

---

## Section 49: Checksum Negotiation

This section documents checksum algorithm negotiation.

### 49.1 Checksum Negotiation Protocol

```text
Protocol 30+:
Client sends list of supported checksums
Server selects best match

Checksum list format:
+----------+------------------+
| count    | names            |
| (varint) | (NUL-separated)  |
+----------+------------------+

Supported algorithms:
  - md4     (protocol 28+, deprecated 32+)
  - md5     (protocol 30+)
  - xxh64   (protocol 32+)
  - xxh3    (protocol 32+, default)
  - xxh128  (protocol 32+)
```

### 49.2 Checksum Negotiation Implementation

```rust
/// Supported checksum algorithms
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChecksumAlgorithm {
    Md4,
    Md5,
    Xxh64,
    Xxh3,
    Xxh128,
}

impl ChecksumAlgorithm {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Md4 => "md4",
            Self::Md5 => "md5",
            Self::Xxh64 => "xxh64",
            Self::Xxh3 => "xxh3",
            Self::Xxh128 => "xxh128",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "md4" => Some(Self::Md4),
            "md5" => Some(Self::Md5),
            "xxh64" | "xxhash64" => Some(Self::Xxh64),
            "xxh3" | "xxhash3" => Some(Self::Xxh3),
            "xxh128" | "xxhash128" => Some(Self::Xxh128),
            _ => None,
        }
    }

    /// Get output size in bytes
    pub fn output_size(&self) -> usize {
        match self {
            Self::Md4 | Self::Md5 | Self::Xxh128 => 16,
            Self::Xxh64 | Self::Xxh3 => 8,
        }
    }

    /// Check if supported at given protocol version
    pub fn supported_at(&self, protocol: ProtocolVersion) -> bool {
        match self {
            Self::Md4 => protocol.as_u8() >= 28 && protocol.as_u8() < 32,
            Self::Md5 => protocol.as_u8() >= 30,
            Self::Xxh64 | Self::Xxh3 | Self::Xxh128 => protocol.as_u8() >= 32,
        }
    }
}

/// Negotiate checksum algorithm
pub fn negotiate_checksum(
    client_prefs: &[ChecksumAlgorithm],
    server_prefs: &[ChecksumAlgorithm],
    protocol: ProtocolVersion,
) -> ChecksumAlgorithm {
    // Find first client preference that server supports
    for alg in client_prefs {
        if alg.supported_at(protocol) && server_prefs.contains(alg) {
            return *alg;
        }
    }

    // Fall back to protocol default
    default_checksum(protocol)
}

/// Get default checksum for protocol version
pub fn default_checksum(protocol: ProtocolVersion) -> ChecksumAlgorithm {
    if protocol.as_u8() >= 32 {
        ChecksumAlgorithm::Xxh3
    } else if protocol.as_u8() >= 30 {
        ChecksumAlgorithm::Md5
    } else {
        ChecksumAlgorithm::Md4
    }
}
```

---

## Section 50: Compression Negotiation

This section documents compression algorithm negotiation.

### 50.1 Compression Negotiation Protocol

```text
Protocol 31+:
Client sends list of supported compression algorithms
Server selects best match

Compression list format:
+----------+------------------+
| count    | names            |
| (varint) | (NUL-separated)  |
+----------+------------------+

Supported algorithms:
  - zlib    (all versions)
  - zlibx   (protocol 31+, token-based)
  - zstd    (protocol 31+)
  - lz4     (protocol 31+, if compiled)
```

### 50.2 Compression Negotiation Implementation

```rust
/// Supported compression algorithms
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CompressionAlgorithm {
    None,
    Zlib,
    ZlibX,
    Zstd,
    Lz4,
}

impl CompressionAlgorithm {
    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
            Self::ZlibX => "zlibx",
            Self::Zstd => "zstd",
            Self::Lz4 => "lz4",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "none" => Some(Self::None),
            "zlib" => Some(Self::Zlib),
            "zlibx" => Some(Self::ZlibX),
            "zstd" => Some(Self::Zstd),
            "lz4" => Some(Self::Lz4),
            _ => None,
        }
    }

    /// Check if available at runtime
    pub fn available(&self) -> bool {
        match self {
            Self::None | Self::Zlib | Self::ZlibX => true,
            Self::Zstd => cfg!(feature = "zstd"),
            Self::Lz4 => cfg!(feature = "lz4"),
        }
    }
}

/// Negotiate compression algorithm
pub fn negotiate_compression(
    client_prefs: &[CompressionAlgorithm],
    server_prefs: &[CompressionAlgorithm],
    protocol: ProtocolVersion,
) -> CompressionAlgorithm {
    // Protocol 30 and below only support zlib
    if protocol.as_u8() < 31 {
        return if client_prefs.contains(&CompressionAlgorithm::Zlib)
            && server_prefs.contains(&CompressionAlgorithm::Zlib)
        {
            CompressionAlgorithm::Zlib
        } else {
            CompressionAlgorithm::None
        };
    }

    // Find first client preference that server supports
    for alg in client_prefs {
        if alg.available() && server_prefs.contains(alg) {
            return *alg;
        }
    }

    CompressionAlgorithm::None
}

/// Write compression preference list
pub fn write_compression_list<W: Write>(
    writer: &mut W,
    prefs: &[CompressionAlgorithm],
) -> io::Result<()> {
    let available: Vec<_> = prefs.iter().filter(|a| a.available()).collect();
    write_varint(writer, available.len() as u64)?;

    for (i, alg) in available.iter().enumerate() {
        writer.write_all(alg.name().as_bytes())?;
        if i < available.len() - 1 {
            writer.write_all(&[0])?; // NUL separator
        }
    }

    Ok(())
}
```

---

## Section 51: File List Sorting and Deduplication

This section documents file list ordering and deduplication.

### 51.1 File List Sort Order

```text
Sort order:
1. Directories before files (if --dirs-first)
2. Lexicographic by path components
3. Within same directory: alphabetical

Deduplication:
- Same path entries merged
- First occurrence wins for metadata
- Hard links tracked by (dev, inode)
```

### 51.2 File List Implementation

```rust
/// File list with sorting and deduplication
pub struct FileList {
    entries: Vec<FileListEntry>,
    sorted: bool,
    path_index: HashMap<PathBuf, usize>,
    inode_index: HashMap<(u64, u64), usize>,
}

impl FileList {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            sorted: false,
            path_index: HashMap::new(),
            inode_index: HashMap::new(),
        }
    }

    /// Add entry with deduplication
    pub fn add(&mut self, entry: FileListEntry) -> bool {
        // Check for duplicate path
        if self.path_index.contains_key(&entry.path) {
            return false;
        }

        // Check for hard link (same dev+inode)
        if let Some(dev) = entry.dev {
            if let Some(inode) = entry.inode {
                if let Some(&first_ndx) = self.inode_index.get(&(dev, inode)) {
                    // This is a hard link to an existing entry
                    self.entries[first_ndx].hard_link_count += 1;
                }
                self.inode_index.insert((dev, inode), self.entries.len());
            }
        }

        let ndx = self.entries.len();
        self.path_index.insert(entry.path.clone(), ndx);
        self.entries.push(entry);
        self.sorted = false;
        true
    }

    /// Sort file list
    pub fn sort(&mut self, dirs_first: bool) {
        if self.sorted {
            return;
        }

        self.entries.sort_by(|a, b| {
            if dirs_first {
                let a_is_dir = a.is_directory();
                let b_is_dir = b.is_directory();
                if a_is_dir != b_is_dir {
                    return if a_is_dir {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Greater
                    };
                }
            }
            a.path.cmp(&b.path)
        });

        // Rebuild index after sort
        self.path_index.clear();
        for (i, entry) in self.entries.iter().enumerate() {
            self.path_index.insert(entry.path.clone(), i);
        }

        self.sorted = true;
    }

    /// Get entry by index
    pub fn get(&self, ndx: usize) -> Option<&FileListEntry> {
        self.entries.get(ndx)
    }

    /// Find entry by path
    pub fn find(&self, path: &Path) -> Option<usize> {
        self.path_index.get(path).copied()
    }

    /// Iterator over entries
    pub fn iter(&self) -> impl Iterator<Item = &FileListEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
```

---

## Section 52: Incremental Recursion Protocol

This section documents the incremental recursion protocol.

### 52.1 Incremental Recursion Overview

```text
Protocol 29+ with incremental recursion:
- File list sent incrementally as directories are discovered
- Generator starts before complete file list received
- Allows early start on deep directory trees

Key protocol elements:
- FLIST_END marker (NDX_DONE = -1)
- FLIST_DONE_SENDING (protocol 31+)
- File list segments separated by NDX_FLIST_EOF
```

### 52.2 Incremental Recursion Implementation

```rust
/// Incremental file list receiver
pub struct IncrementalFileList {
    entries: Vec<FileListEntry>,
    complete: bool,
    current_segment: usize,
    segments_complete: Vec<bool>,
}

impl IncrementalFileList {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            complete: false,
            current_segment: 0,
            segments_complete: Vec::new(),
        }
    }

    /// Add entry from current segment
    pub fn add_entry(&mut self, entry: FileListEntry) {
        self.entries.push(entry);
    }

    /// Mark current segment as complete
    pub fn end_segment(&mut self) {
        if self.current_segment >= self.segments_complete.len() {
            self.segments_complete.resize(self.current_segment + 1, false);
        }
        self.segments_complete[self.current_segment] = true;
        self.current_segment += 1;
    }

    /// Mark entire file list as complete
    pub fn mark_complete(&mut self) {
        self.complete = true;
    }

    /// Check if we can start processing from given index
    pub fn can_process(&self, ndx: usize) -> bool {
        ndx < self.entries.len()
    }

    /// Check if file list is completely received
    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

/// File list segment markers
pub const NDX_DONE: i32 = -1;
pub const NDX_FLIST_EOF: i32 = -2;
pub const NDX_FLIST_OFFSET: i32 = -101;

/// Read incremental file list segment
pub fn read_flist_segment<R: Read>(
    reader: &mut R,
    flist: &mut IncrementalFileList,
    protocol: ProtocolVersion,
    codec: &dyn NdxCodec,
) -> io::Result<bool> {
    loop {
        let ndx = codec.read_ndx(reader)?;

        if ndx == NDX_DONE {
            // End of file list
            flist.mark_complete();
            return Ok(true);
        }

        if ndx == NDX_FLIST_EOF {
            // End of current segment
            flist.end_segment();
            return Ok(false);
        }

        // Read file entry
        let entry = read_file_entry(reader, protocol)?;
        flist.add_entry(entry);
    }
}
```

---

## Section 53: Generator/Receiver Coordination

This section documents the coordination between generator and receiver.

### 53.1 Phase Coordination

```text
Transfer phases:
1. File list exchange
2. Generator sends file indices (NDX values)
3. Sender sends delta data
4. Receiver applies deltas
5. Statistics exchange

Phase transitions marked by NDX_DONE (-1)
```

### 53.2 Coordination Implementation

```rust
/// Transfer phase state
#[derive(Clone, Copy, PartialEq)]
pub enum TransferPhase {
    FileList,
    Generation,
    Transfer,
    Redo,
    Statistics,
    Complete,
}

/// Coordinator for generator/receiver
pub struct TransferCoordinator {
    phase: TransferPhase,
    max_phase: u8,
    files_pending: Vec<i32>,
    files_completed: HashSet<i32>,
    redo_queue: Vec<i32>,
}

impl TransferCoordinator {
    pub fn new(protocol: ProtocolVersion) -> Self {
        // Protocol 29+ has max_phase=2 (redo phase)
        let max_phase = if protocol.as_u8() >= 29 { 2 } else { 1 };

        Self {
            phase: TransferPhase::FileList,
            max_phase,
            files_pending: Vec::new(),
            files_completed: HashSet::new(),
            redo_queue: Vec::new(),
        }
    }

    /// Advance to next phase
    pub fn advance_phase(&mut self) -> TransferPhase {
        self.phase = match self.phase {
            TransferPhase::FileList => TransferPhase::Generation,
            TransferPhase::Generation => TransferPhase::Transfer,
            TransferPhase::Transfer => {
                if self.max_phase >= 2 && !self.redo_queue.is_empty() {
                    TransferPhase::Redo
                } else {
                    TransferPhase::Statistics
                }
            }
            TransferPhase::Redo => TransferPhase::Statistics,
            TransferPhase::Statistics => TransferPhase::Complete,
            TransferPhase::Complete => TransferPhase::Complete,
        };
        self.phase
    }

    /// Add file to pending queue
    pub fn queue_file(&mut self, ndx: i32) {
        self.files_pending.push(ndx);
    }

    /// Mark file as completed
    pub fn complete_file(&mut self, ndx: i32, success: bool) {
        self.files_completed.insert(ndx);
        if !success && self.phase != TransferPhase::Redo {
            self.redo_queue.push(ndx);
        }
    }

    /// Get next file to process
    pub fn next_file(&mut self) -> Option<i32> {
        if self.phase == TransferPhase::Redo {
            self.redo_queue.pop()
        } else {
            self.files_pending.pop()
        }
    }

    /// Check if all files are processed
    pub fn is_complete(&self) -> bool {
        self.files_pending.is_empty()
            && self.redo_queue.is_empty()
            && self.phase == TransferPhase::Complete
    }
}
```

---

## Section 54: Partial Transfer Resume

This section documents partial transfer resume functionality.

### 54.1 Partial File Handling

```text
--partial: Keep partially transferred files
--partial-dir=DIR: Store partial files in DIR

Partial file naming:
- Regular: filename
- In partial-dir: .filename.XXXXXX (temp name)

Resume detection:
- Compare partial file checksum with basis file
- Generate delta from partial file if beneficial
```

### 54.2 Partial Resume Implementation

```rust
/// Partial transfer manager
pub struct PartialManager {
    partial_dir: Option<PathBuf>,
    keep_partial: bool,
}

impl PartialManager {
    pub fn new(partial: bool, partial_dir: Option<PathBuf>) -> Self {
        Self {
            partial_dir,
            keep_partial: partial,
        }
    }

    /// Get path for partial file
    pub fn partial_path(&self, dest: &Path) -> PathBuf {
        if let Some(ref dir) = self.partial_dir {
            let name = dest.file_name().unwrap_or_default();
            dir.join(format!(".{}.partial", name.to_string_lossy()))
        } else {
            dest.to_path_buf()
        }
    }

    /// Check if we can resume from existing partial
    pub fn can_resume(&self, dest: &Path) -> Option<PartialInfo> {
        let partial_path = self.partial_path(dest);

        if partial_path.exists() {
            let metadata = std::fs::metadata(&partial_path).ok()?;
            Some(PartialInfo {
                path: partial_path,
                size: metadata.len(),
            })
        } else {
            None
        }
    }

    /// Finalize partial file
    pub fn finalize(&self, partial_path: &Path, dest: &Path) -> io::Result<()> {
        if partial_path != dest {
            std::fs::rename(partial_path, dest)?;
        }
        Ok(())
    }

    /// Cleanup failed transfer
    pub fn cleanup(&self, partial_path: &Path) -> io::Result<()> {
        if !self.keep_partial {
            let _ = std::fs::remove_file(partial_path);
        }
        Ok(())
    }
}

pub struct PartialInfo {
    pub path: PathBuf,
    pub size: u64,
}

/// Decide whether to use partial file as basis
pub fn should_use_partial_as_basis(
    partial_size: u64,
    target_size: u64,
    block_size: u32,
) -> bool {
    // Use partial if it's at least 50% of target and large enough
    // to save significant transfer
    let threshold = target_size / 2;
    let min_useful = block_size as u64 * 10;

    partial_size >= threshold && partial_size >= min_useful
}
```

---

## Section 55: Backup and Deletion Modes

This section documents backup suffix handling and deletion modes.

### 55.1 Backup Options

```text
--backup: Make backups of changed files
--backup-dir=DIR: Store backups in DIR
--suffix=SUFFIX: Backup suffix (default: ~)

Backup naming:
- Simple: filename~
- With suffix: filename.SUFFIX
- In backup-dir: DIR/filename
```

### 55.2 Deletion Modes

```text
--delete: Delete extraneous files from dest
--delete-before: Delete before transfer
--delete-during: Delete during transfer (default)
--delete-delay: Find during, delete after
--delete-after: Delete after transfer
--delete-excluded: Delete excluded files too
--max-delete=N: Maximum N deletions
```

### 55.3 Backup and Deletion Implementation

```rust
/// Backup manager
pub struct BackupManager {
    enabled: bool,
    suffix: String,
    dir: Option<PathBuf>,
}

impl BackupManager {
    pub fn new(backup: bool, suffix: Option<String>, dir: Option<PathBuf>) -> Self {
        Self {
            enabled: backup,
            suffix: suffix.unwrap_or_else(|| "~".to_string()),
            dir,
        }
    }

    /// Create backup of file
    pub fn backup(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        if !self.enabled || !path.exists() {
            return Ok(None);
        }

        let backup_path = self.backup_path(path);

        // Create backup directory if needed
        if let Some(parent) = backup_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Rename or copy to backup
        std::fs::rename(path, &backup_path)?;

        Ok(Some(backup_path))
    }

    fn backup_path(&self, path: &Path) -> PathBuf {
        if let Some(ref dir) = self.dir {
            // Preserve relative path structure in backup dir
            let relative = path.strip_prefix("/").unwrap_or(path);
            dir.join(relative)
        } else {
            // Add suffix to filename
            let mut backup = path.as_os_str().to_owned();
            backup.push(&self.suffix);
            PathBuf::from(backup)
        }
    }
}

/// Deletion manager
pub struct DeletionManager {
    mode: DeletionMode,
    delete_excluded: bool,
    max_delete: Option<usize>,
    delete_count: usize,
    pending_deletions: Vec<PathBuf>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum DeletionMode {
    None,
    Before,
    During,
    Delay,
    After,
}

impl DeletionManager {
    pub fn new(mode: DeletionMode, delete_excluded: bool, max_delete: Option<usize>) -> Self {
        Self {
            mode,
            delete_excluded,
            max_delete,
            delete_count: 0,
            pending_deletions: Vec::new(),
        }
    }

    /// Queue a file for deletion
    pub fn queue_deletion(&mut self, path: PathBuf) -> bool {
        if let Some(max) = self.max_delete {
            if self.delete_count >= max {
                return false;
            }
        }

        match self.mode {
            DeletionMode::Before | DeletionMode::During => {
                self.delete_now(&path);
            }
            DeletionMode::Delay | DeletionMode::After => {
                self.pending_deletions.push(path);
            }
            DeletionMode::None => {}
        }
        true
    }

    /// Execute pending deletions
    pub fn execute_pending(&mut self) -> io::Result<usize> {
        let mut count = 0;
        for path in self.pending_deletions.drain(..) {
            if self.delete_now(&path) {
                count += 1;
            }
        }
        Ok(count)
    }

    fn delete_now(&mut self, path: &Path) -> bool {
        if let Some(max) = self.max_delete {
            if self.delete_count >= max {
                return false;
            }
        }

        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };

        if result.is_ok() {
            self.delete_count += 1;
            true
        } else {
            false
        }
    }
}
```

---

## Section 56: Fuzzy Matching Algorithm

The `--fuzzy` option enables basis file discovery when the exact file doesn't
exist at the destination. This allows delta transfer to work even when files
are renamed.

### Basis File Scoring

```rust
/// Score for fuzzy matching - lower is better
pub struct FuzzyScore {
    /// Exact name match (0 = exact, 1 = fuzzy)
    pub name_match: u8,
    /// Size difference as percentage
    pub size_diff_pct: u32,
    /// Directory depth difference
    pub depth_diff: u32,
    /// Name similarity (Levenshtein distance)
    pub name_distance: u32,
}

impl FuzzyScore {
    /// Calculate composite score for ranking
    pub fn composite(&self) -> u64 {
        (self.name_match as u64) << 48
            | (self.size_diff_pct as u64) << 32
            | (self.depth_diff as u64) << 16
            | (self.name_distance as u64)
    }
}

/// Find best basis file for delta transfer
pub fn find_fuzzy_basis(
    target: &FileEntry,
    candidates: &[FileEntry],
    fuzzy_level: u8,
) -> Option<&FileEntry> {
    let mut best: Option<(&FileEntry, FuzzyScore)> = None;

    for candidate in candidates {
        // Skip if size differs by more than 50%
        let size_ratio = if target.size > candidate.size {
            candidate.size as f64 / target.size as f64
        } else {
            target.size as f64 / candidate.size as f64
        };
        if size_ratio < 0.5 {
            continue;
        }

        let score = calculate_fuzzy_score(target, candidate, fuzzy_level);

        if let Some((_, ref best_score)) = best {
            if score.composite() < best_score.composite() {
                best = Some((candidate, score));
            }
        } else {
            best = Some((candidate, score));
        }
    }

    best.map(|(entry, _)| entry)
}

/// Calculate fuzzy match score between target and candidate
fn calculate_fuzzy_score(
    target: &FileEntry,
    candidate: &FileEntry,
    fuzzy_level: u8,
) -> FuzzyScore {
    let name_match = if target.name == candidate.name { 0 } else { 1 };

    let size_diff_pct = if target.size == 0 {
        0
    } else {
        ((target.size as i64 - candidate.size as i64).abs() * 100 / target.size as i64) as u32
    };

    let depth_diff = target.path.components().count()
        .abs_diff(candidate.path.components().count()) as u32;

    let name_distance = if fuzzy_level >= 2 {
        levenshtein_distance(&target.name, &candidate.name) as u32
    } else {
        if target.name == candidate.name { 0 } else { 1000 }
    };

    FuzzyScore {
        name_match,
        size_diff_pct,
        depth_diff,
        name_distance,
    }
}
```

### Fuzzy Levels

| Level | Flag | Behavior |
|-------|------|----------|
| 0 | (default) | No fuzzy matching |
| 1 | `-y` | Match by name in same directory |
| 2 | `-yy` | Match by name anywhere + name similarity |

---

## Section 57: I/O Buffer Management

Rsync uses layered buffering for efficient I/O operations.

### Buffer Architecture

```rust
/// I/O buffer configuration
pub struct IoBufferConfig {
    /// Size of read buffer (default 32KB)
    pub read_buf_size: usize,
    /// Size of write buffer (default 32KB)
    pub write_buf_size: usize,
    /// Size of file mapping window (default 256KB)
    pub map_window_size: usize,
    /// Whether to use direct I/O
    pub direct_io: bool,
}

impl Default for IoBufferConfig {
    fn default() -> Self {
        Self {
            read_buf_size: 32 * 1024,
            write_buf_size: 32 * 1024,
            map_window_size: 256 * 1024,
            direct_io: false,
        }
    }
}

/// Buffered reader with read-ahead
pub struct BufferedReader<R> {
    inner: R,
    buffer: Vec<u8>,
    pos: usize,
    filled: usize,
}

impl<R: Read> BufferedReader<R> {
    pub fn new(inner: R, buf_size: usize) -> Self {
        Self {
            inner,
            buffer: vec![0u8; buf_size],
            pos: 0,
            filled: 0,
        }
    }

    /// Read with buffering and potential read-ahead
    pub fn read_buffered(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.filled {
            // Buffer empty, refill
            self.filled = self.inner.read(&mut self.buffer)?;
            self.pos = 0;
            if self.filled == 0 {
                return Ok(0);
            }
        }

        let available = self.filled - self.pos;
        let to_copy = buf.len().min(available);
        buf[..to_copy].copy_from_slice(&self.buffer[self.pos..self.pos + to_copy]);
        self.pos += to_copy;
        Ok(to_copy)
    }

    /// Peek at buffered data without consuming
    pub fn peek(&self) -> &[u8] {
        &self.buffer[self.pos..self.filled]
    }
}

/// Buffered writer with flush control
pub struct BufferedWriter<W> {
    inner: W,
    buffer: Vec<u8>,
    pos: usize,
}

impl<W: Write> BufferedWriter<W> {
    pub fn new(inner: W, buf_size: usize) -> Self {
        Self {
            inner,
            buffer: vec![0u8; buf_size],
            pos: 0,
        }
    }

    /// Write with buffering
    pub fn write_buffered(&mut self, data: &[u8]) -> io::Result<usize> {
        let available = self.buffer.len() - self.pos;

        if data.len() <= available {
            // Fits in buffer
            self.buffer[self.pos..self.pos + data.len()].copy_from_slice(data);
            self.pos += data.len();
            Ok(data.len())
        } else if data.len() >= self.buffer.len() {
            // Larger than buffer, flush and write directly
            self.flush_buffer()?;
            self.inner.write_all(data)?;
            Ok(data.len())
        } else {
            // Flush and buffer
            self.flush_buffer()?;
            self.buffer[..data.len()].copy_from_slice(data);
            self.pos = data.len();
            Ok(data.len())
        }
    }

    fn flush_buffer(&mut self) -> io::Result<()> {
        if self.pos > 0 {
            self.inner.write_all(&self.buffer[..self.pos])?;
            self.pos = 0;
        }
        Ok(())
    }
}
```

---

## Section 58: Sparse File Handling

The `--sparse` option enables efficient handling of files with large zero regions.

### Sparse Detection and Writing

```rust
use std::io::{self, Read, Seek, SeekFrom, Write};

/// Sparse file writer that detects zero runs
pub struct SparseWriter<W> {
    inner: W,
    offset: u64,
    /// Minimum run of zeros to trigger seek (default 512)
    min_sparse_size: usize,
    /// Whether sparse writing is enabled
    sparse_enabled: bool,
}

impl<W: Write + Seek> SparseWriter<W> {
    pub fn new(inner: W, sparse_enabled: bool) -> Self {
        Self {
            inner,
            offset: 0,
            min_sparse_size: 512,
            sparse_enabled,
        }
    }

    /// Write data, seeking over zero regions if sparse enabled
    pub fn write_sparse(&mut self, data: &[u8]) -> io::Result<usize> {
        if !self.sparse_enabled {
            self.inner.write_all(data)?;
            self.offset += data.len() as u64;
            return Ok(data.len());
        }

        let mut pos = 0;
        while pos < data.len() {
            // Find start of zero run
            let zero_start = self.find_zero_run_start(&data[pos..]);

            if zero_start > 0 {
                // Write non-zero prefix
                self.inner.write_all(&data[pos..pos + zero_start])?;
                self.offset += zero_start as u64;
                pos += zero_start;
            }

            if pos >= data.len() {
                break;
            }

            // Find end of zero run
            let zero_len = self.find_zero_run_length(&data[pos..]);

            if zero_len >= self.min_sparse_size {
                // Seek over zero region
                self.offset += zero_len as u64;
                self.inner.seek(SeekFrom::Start(self.offset))?;
            } else if zero_len > 0 {
                // Write small zero run
                self.inner.write_all(&data[pos..pos + zero_len])?;
                self.offset += zero_len as u64;
            }
            pos += zero_len;
        }

        Ok(data.len())
    }

    fn find_zero_run_start(&self, data: &[u8]) -> usize {
        // Use u128 comparison for efficiency (16 bytes at a time)
        let mut pos = 0;

        // Check 16-byte chunks
        while pos + 16 <= data.len() {
            let chunk: [u8; 16] = data[pos..pos + 16].try_into().unwrap();
            if u128::from_ne_bytes(chunk) == 0 {
                return pos;
            }
            pos += 16;
        }

        // Check remaining bytes
        while pos < data.len() {
            if data[pos] == 0 {
                return pos;
            }
            pos += 1;
        }

        data.len()
    }

    fn find_zero_run_length(&self, data: &[u8]) -> usize {
        let mut len = 0;

        // Check 16-byte chunks
        while len + 16 <= data.len() {
            let chunk: [u8; 16] = data[len..len + 16].try_into().unwrap();
            if u128::from_ne_bytes(chunk) != 0 {
                break;
            }
            len += 16;
        }

        // Check remaining bytes
        while len < data.len() && data[len] == 0 {
            len += 1;
        }

        len
    }

    /// Finalize sparse file (may need to write final byte)
    pub fn finalize(mut self) -> io::Result<W> {
        if self.sparse_enabled && self.offset > 0 {
            // Ensure file has correct size by seeking to end
            self.inner.seek(SeekFrom::Start(self.offset))?;
        }
        Ok(self.inner)
    }
}

/// Check if file system supports sparse files
#[cfg(target_os = "linux")]
pub fn supports_sparse(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    if let Ok(meta) = std::fs::metadata(path) {
        // Check if blocks < size / block_size (indicates sparse support)
        let expected_blocks = (meta.len() + 511) / 512;
        meta.blocks() < expected_blocks as u64
    } else {
        true // Assume supported for new files
    }
}
```

### Hole Punching (Linux)

```rust
#[cfg(target_os = "linux")]
pub fn punch_hole(fd: std::os::unix::io::RawFd, offset: i64, length: i64) -> io::Result<()> {
    use libc::{fallocate, FALLOC_FL_PUNCH_HOLE, FALLOC_FL_KEEP_SIZE};

    let result = unsafe {
        fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, offset, length)
    };

    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
```

---

## Section 59: Rolling Checksum Mathematics

The rsync rolling checksum is based on Adler-32 with modifications for efficient
sliding window computation.

### Mathematical Foundation

The rolling checksum consists of two 16-bit components:

```
s1 = Σ(b[i]) mod M
s2 = Σ((n - i) × b[i]) mod M

checksum = s1 + (s2 << 16)
```

Where:
- `b[i]` is the i-th byte in the window
- `n` is the window size (block length)
- `M` is the modulus (65521 for standard Adler-32, 65536 for rsync)

### Rolling Update

When sliding the window by one byte:
- Remove old byte `b_old` from position 0
- Add new byte `b_new` at position n-1

```rust
/// Rolling checksum state
pub struct RollingChecksum {
    s1: u32,
    s2: u32,
    window_size: usize,
}

impl RollingChecksum {
    pub fn new(window_size: usize) -> Self {
        Self {
            s1: 0,
            s2: 0,
            window_size,
        }
    }

    /// Initialize from a block of data
    pub fn init(&mut self, data: &[u8]) {
        self.s1 = 0;
        self.s2 = 0;

        for (i, &byte) in data.iter().enumerate() {
            self.s1 = self.s1.wrapping_add(byte as u32);
            self.s2 = self.s2.wrapping_add((data.len() - i) as u32 * byte as u32);
        }
    }

    /// Roll the checksum by one byte
    ///
    /// Mathematical derivation:
    /// - s1_new = s1_old - b_old + b_new
    /// - s2_new = s2_old - n*b_old + s1_new
    pub fn roll(&mut self, old_byte: u8, new_byte: u8) {
        let n = self.window_size as u32;

        // Update s1: subtract old, add new
        self.s1 = self.s1.wrapping_sub(old_byte as u32).wrapping_add(new_byte as u32);

        // Update s2: subtract n*old_byte, add new s1
        self.s2 = self.s2
            .wrapping_sub(n.wrapping_mul(old_byte as u32))
            .wrapping_add(self.s1);
    }

    /// Roll by multiple bytes efficiently
    pub fn roll_many(&mut self, old_bytes: &[u8], new_bytes: &[u8]) {
        debug_assert_eq!(old_bytes.len(), new_bytes.len());

        for (&old, &new) in old_bytes.iter().zip(new_bytes.iter()) {
            self.roll(old, new);
        }
    }

    /// Get current checksum value
    pub fn digest(&self) -> u32 {
        self.s1.wrapping_add(self.s2 << 16)
    }

    /// Get s1 component (for hash table lookup)
    pub fn s1(&self) -> u16 {
        self.s1 as u16
    }

    /// Get s2 component (for verification)
    pub fn s2(&self) -> u16 {
        self.s2 as u16
    }
}
```

### SIMD-Accelerated Accumulation

```rust
#[cfg(target_arch = "x86_64")]
pub fn accumulate_avx2(data: &[u8]) -> (u32, u32) {
    use std::arch::x86_64::*;

    if !is_x86_feature_detected!("avx2") {
        return accumulate_scalar(data);
    }

    unsafe {
        let mut s1 = _mm256_setzero_si256();
        let mut s2 = _mm256_setzero_si256();

        // Process 32 bytes at a time
        let chunks = data.chunks_exact(32);
        let remainder = chunks.remainder();

        for chunk in chunks {
            let bytes = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);

            // Accumulate s1
            let zeros = _mm256_setzero_si256();
            let sum16 = _mm256_sad_epu8(bytes, zeros);
            s1 = _mm256_add_epi64(s1, sum16);

            // Accumulate weighted s2 (using position weights)
            // ... (complex SIMD weight multiplication)
        }

        // Horizontal sum and add remainder
        let s1_scalar = horizontal_sum_epi64(s1);
        let s2_scalar = horizontal_sum_epi64(s2);

        // Process remainder with scalar
        let (r1, r2) = accumulate_scalar(remainder);

        (s1_scalar.wrapping_add(r1), s2_scalar.wrapping_add(r2))
    }
}

fn accumulate_scalar(data: &[u8]) -> (u32, u32) {
    let mut s1: u32 = 0;
    let mut s2: u32 = 0;

    for (i, &byte) in data.iter().enumerate() {
        s1 = s1.wrapping_add(byte as u32);
        s2 = s2.wrapping_add((data.len() - i) as u32 * byte as u32);
    }

    (s1, s2)
}
```

---

## Section 60: Progress Output Format

Rsync provides detailed progress information during transfers.

### Progress Line Format

```
filename
     bytes  pct%  rate   eta
```

For incremental recursion:
```
filename
     bytes  pct%  rate   eta  xfr#N, ir-chk=M/T
```

or:
```
filename
     bytes  pct%  rate   eta  xfr#N, to-chk=M/T
```

### Progress Implementation

```rust
/// Progress display configuration
pub struct ProgressConfig {
    /// Show per-file progress
    pub per_file: bool,
    /// Show total progress
    pub total: bool,
    /// Use incremental recursion format
    pub incremental: bool,
    /// Output format (human readable or parseable)
    pub format: ProgressFormat,
}

#[derive(Clone, Copy)]
pub enum ProgressFormat {
    Human,
    Parseable,
}

/// Progress state for display
pub struct ProgressState {
    /// Current file being transferred
    pub current_file: PathBuf,
    /// Bytes transferred for current file
    pub file_bytes: u64,
    /// Total size of current file
    pub file_size: u64,
    /// Transfer rate in bytes/sec
    pub rate: u64,
    /// Estimated time remaining
    pub eta: Option<Duration>,
    /// Number of files transferred
    pub xfr_count: u32,
    /// Files remaining to check (incremental)
    pub ir_check: Option<(u32, u32)>,  // (remaining, total)
    /// Files remaining to transfer
    pub to_check: Option<(u32, u32)>,  // (remaining, total)
}

impl ProgressState {
    /// Format progress line for display
    pub fn format(&self, config: &ProgressConfig) -> String {
        let pct = if self.file_size > 0 {
            (self.file_bytes * 100 / self.file_size) as u32
        } else {
            100
        };

        let rate_str = format_rate(self.rate);
        let eta_str = self.eta.map_or("--:--".to_string(), format_duration);

        let mut line = format!(
            "{:>12}  {:>3}%  {:>10}  {}",
            format_size(self.file_bytes),
            pct,
            rate_str,
            eta_str
        );

        // Add incremental recursion info
        if let Some((remaining, total)) = self.ir_check {
            line.push_str(&format!("  xfr#{}, ir-chk={}/{}",
                self.xfr_count, remaining, total));
        } else if let Some((remaining, total)) = self.to_check {
            line.push_str(&format!("  xfr#{}, to-chk={}/{}",
                self.xfr_count, remaining, total));
        }

        line
    }
}

/// Format byte count for human display
fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1000.0 && unit_idx < UNITS.len() - 1 {
        size /= 1000.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

/// Format transfer rate
fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_size(bytes_per_sec))
}

/// Format duration as HH:MM:SS or MM:SS
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;

    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    } else {
        format!("{:02}:{:02}", mins, secs)
    }
}
```

---

## Section 61: Checksum Seed

The `--checksum-seed` option controls the seed used for rolling and strong checksums.

### Seed Generation and Usage

```rust
use std::time::{SystemTime, UNIX_EPOCH};

/// Checksum seed configuration
pub struct ChecksumSeed {
    value: u32,
}

impl ChecksumSeed {
    /// Create with explicit seed value
    pub fn new(seed: u32) -> Self {
        Self { value: seed }
    }

    /// Create time-based seed (default behavior)
    pub fn time_based() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self { value: secs as u32 }
    }

    /// Create deterministic seed for testing
    pub fn deterministic(seed: u32) -> Self {
        Self { value: seed }
    }

    /// Get seed value
    pub fn value(&self) -> u32 {
        self.value
    }

    /// Apply seed to rolling checksum initialization
    pub fn apply_to_rolling(&self, base_checksum: u32) -> u32 {
        base_checksum ^ self.value
    }

    /// Apply seed to strong checksum (MD4/MD5)
    pub fn apply_to_strong(&self, data: &[u8]) -> Vec<u8> {
        let mut input = Vec::with_capacity(4 + data.len());
        input.extend_from_slice(&self.value.to_le_bytes());
        input.extend_from_slice(data);
        input
    }
}

/// Protocol exchange for checksum seed
pub fn exchange_checksum_seed<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    is_sender: bool,
    explicit_seed: Option<u32>,
) -> io::Result<ChecksumSeed> {
    if is_sender {
        let seed = explicit_seed
            .map(ChecksumSeed::new)
            .unwrap_or_else(ChecksumSeed::time_based);

        writer.write_all(&seed.value().to_le_bytes())?;
        writer.flush()?;

        Ok(seed)
    } else {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        let seed = u32::from_le_bytes(buf);

        Ok(ChecksumSeed::new(seed))
    }
}
```

---

## Section 62: File Ordering Priority

Rsync processes files in a specific order for efficiency and correctness.

### Transfer Order

```rust
/// File transfer priority
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransferPriority {
    /// Directories first (for creation)
    Directory = 0,
    /// Regular files
    File = 1,
    /// Symbolic links
    Symlink = 2,
    /// Hard links (after their targets)
    HardLink = 3,
    /// Device/special files
    Device = 4,
}

/// File ordering for transfer
pub struct FileOrdering {
    /// Sort by priority then path
    pub by_priority: bool,
    /// Process directories depth-first
    pub depth_first: bool,
    /// Hard links after their targets
    pub hard_links_last: bool,
}

impl FileOrdering {
    /// Sort file list for optimal transfer
    pub fn sort(&self, files: &mut [FileEntry]) {
        files.sort_by(|a, b| {
            // First by priority
            let pa = self.priority(a);
            let pb = self.priority(b);

            match pa.cmp(&pb) {
                std::cmp::Ordering::Equal => {
                    // Then by path (lexicographic for determinism)
                    a.path.cmp(&b.path)
                }
                other => other,
            }
        });
    }

    fn priority(&self, entry: &FileEntry) -> TransferPriority {
        if entry.is_dir() {
            TransferPriority::Directory
        } else if entry.is_symlink() {
            TransferPriority::Symlink
        } else if entry.is_hardlink() {
            TransferPriority::HardLink
        } else if entry.is_device() || entry.is_special() {
            TransferPriority::Device
        } else {
            TransferPriority::File
        }
    }
}

/// Directory deletion order (reverse of creation)
pub fn deletion_order(files: &mut [FileEntry]) {
    // Directories last (deepest first for proper rmdir)
    files.sort_by(|a, b| {
        match (a.is_dir(), b.is_dir()) {
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (true, true) => {
                // Deeper directories first
                b.path.components().count().cmp(&a.path.components().count())
            }
            (false, false) => a.path.cmp(&b.path),
        }
    });
}
```

---

## Section 63: Module Listing Protocol

The daemon module listing uses a specific protocol format.

### Module List Exchange

```rust
/// Request module listing from daemon
pub fn request_module_list<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> io::Result<Vec<ModuleInfo>> {
    // Module listing is triggered by empty module name
    // or by listing the root path

    let mut modules = Vec::new();

    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }

        let line = line.trim();

        // End of listing markers
        if line.starts_with("@RSYNCD: EXIT") {
            break;
        }
        if line.starts_with("@ERROR") {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                line.to_string(),
            ));
        }

        // Parse module line: "module\tdescription"
        if let Some((name, desc)) = line.split_once('\t') {
            modules.push(ModuleInfo {
                name: name.to_string(),
                comment: desc.to_string(),
            });
        } else if !line.is_empty() && !line.starts_with('@') {
            modules.push(ModuleInfo {
                name: line.to_string(),
                comment: String::new(),
            });
        }
    }

    Ok(modules)
}

/// Module information
pub struct ModuleInfo {
    pub name: String,
    pub comment: String,
}

impl ModuleInfo {
    /// Format for display
    pub fn display(&self) -> String {
        if self.comment.is_empty() {
            self.name.clone()
        } else {
            format!("{:<20} {}", self.name, self.comment)
        }
    }
}

/// Daemon sends module list
pub fn send_module_list<W: Write>(
    writer: &mut W,
    modules: &[ModuleConfig],
) -> io::Result<()> {
    for module in modules {
        if !module.list {
            continue; // Hidden module
        }

        let line = if module.comment.is_empty() {
            format!("{}\n", module.name)
        } else {
            format!("{}\t{}\n", module.name, module.comment)
        };

        writer.write_all(line.as_bytes())?;
    }

    writer.write_all(b"@RSYNCD: EXIT\n")?;
    writer.flush()
}
```

---

## Section 64: Error Recovery Mechanisms

Rsync implements retry and redo mechanisms for handling transient failures.

### Redo Queue

```rust
/// Redo queue for failed transfers
pub struct RedoQueue {
    /// Files that need retrying
    entries: Vec<i32>,
    /// Maximum retry attempts per file
    max_retries: u32,
    /// Retry counts per file index
    retry_counts: HashMap<i32, u32>,
}

impl RedoQueue {
    pub fn new(max_retries: u32) -> Self {
        Self {
            entries: Vec::new(),
            max_retries,
            retry_counts: HashMap::new(),
        }
    }

    /// Add file to redo queue
    pub fn add(&mut self, ndx: i32) -> bool {
        let count = self.retry_counts.entry(ndx).or_insert(0);
        if *count >= self.max_retries {
            return false; // Max retries exceeded
        }

        *count += 1;
        self.entries.push(ndx);
        true
    }

    /// Get next file to redo
    pub fn pop(&mut self) -> Option<i32> {
        self.entries.pop()
    }

    /// Check if queue is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of files needing redo
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Error types that trigger redo
#[derive(Clone, Copy)]
pub enum RedoReason {
    /// File changed during transfer (vanished or modified)
    FileChanged,
    /// Checksum verification failed
    ChecksumMismatch,
    /// I/O error (may be transient)
    IoError,
    /// Basis file unavailable
    BasisUnavailable,
}

impl RedoReason {
    /// Whether this error should trigger redo
    pub fn should_redo(&self) -> bool {
        match self {
            RedoReason::FileChanged => true,
            RedoReason::ChecksumMismatch => true,
            RedoReason::IoError => true,
            RedoReason::BasisUnavailable => false, // Use whole-file
        }
    }
}

/// Handle transfer error and decide on redo
pub fn handle_transfer_error(
    ndx: i32,
    reason: RedoReason,
    queue: &mut RedoQueue,
) -> TransferAction {
    if reason.should_redo() && queue.add(ndx) {
        TransferAction::Redo
    } else {
        TransferAction::Skip
    }
}

pub enum TransferAction {
    /// Retry transfer in next phase
    Redo,
    /// Skip this file
    Skip,
}
```

---

## Section 65: Temp File Naming

Rsync uses temporary files during transfer for atomicity.

### Temp File Strategy

```rust
use std::path::{Path, PathBuf};

/// Temporary file naming strategy
pub struct TempFileNaming {
    /// Prefix for temp files
    prefix: String,
    /// Use PID in temp name
    use_pid: bool,
    /// Suffix pattern
    suffix: String,
}

impl Default for TempFileNaming {
    fn default() -> Self {
        Self {
            prefix: ".".to_string(),
            use_pid: true,
            suffix: ".~tmp~".to_string(),
        }
    }
}

impl TempFileNaming {
    /// Generate temp file path for target
    pub fn temp_path(&self, target: &Path) -> PathBuf {
        let file_name = target.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");

        let temp_name = if self.use_pid {
            format!("{}{}.{}{}",
                self.prefix,
                file_name,
                std::process::id(),
                self.suffix)
        } else {
            format!("{}{}{}", self.prefix, file_name, self.suffix)
        };

        target.with_file_name(temp_name)
    }

    /// Check if path is a temp file
    pub fn is_temp_file(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with(&self.prefix) && n.ends_with(&self.suffix))
            .unwrap_or(false)
    }
}

/// Atomic file commit
pub fn atomic_commit(temp: &Path, target: &Path) -> io::Result<()> {
    // Sync temp file first
    let file = std::fs::File::open(temp)?;
    file.sync_all()?;
    drop(file);

    // Atomic rename
    std::fs::rename(temp, target)
}

/// Temp file cleanup on error
pub struct TempFileGuard {
    path: PathBuf,
    committed: bool,
}

impl TempFileGuard {
    pub fn new(path: PathBuf) -> Self {
        Self { path, committed: false }
    }

    pub fn commit(mut self) -> io::Result<()> {
        self.committed = true;
        Ok(())
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
```

---

## Section 66: Symlink Safety Options

Options for safe handling of symbolic links.

### Symlink Safety Implementation

```rust
use std::path::{Path, PathBuf};

/// Symlink handling mode
#[derive(Clone, Copy)]
pub enum SymlinkMode {
    /// Skip all symlinks
    Skip,
    /// Copy symlinks as symlinks
    Preserve,
    /// Follow symlinks, copy target
    Follow,
    /// Copy as symlink only if safe
    Safe,
    /// Copy unsafe links as files
    CopyUnsafe,
}

/// Check if symlink target is safe (within transfer root)
pub fn is_safe_symlink(link: &Path, target: &Path, root: &Path) -> bool {
    // Resolve the symlink relative to its location
    let link_dir = link.parent().unwrap_or(Path::new("."));
    let resolved = link_dir.join(target);

    // Canonicalize both paths
    let resolved = match resolved.canonicalize() {
        Ok(p) => p,
        Err(_) => return false, // Can't resolve = unsafe
    };

    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Check if resolved path is within root
    resolved.starts_with(&root)
}

/// Handle symlink according to mode
pub fn handle_symlink(
    link: &Path,
    target: &Path,
    mode: SymlinkMode,
    root: &Path,
) -> SymlinkAction {
    match mode {
        SymlinkMode::Skip => SymlinkAction::Skip,

        SymlinkMode::Preserve => SymlinkAction::CreateSymlink(target.to_path_buf()),

        SymlinkMode::Follow => SymlinkAction::CopyTarget,

        SymlinkMode::Safe => {
            if is_safe_symlink(link, target, root) {
                SymlinkAction::CreateSymlink(target.to_path_buf())
            } else {
                SymlinkAction::Skip
            }
        }

        SymlinkMode::CopyUnsafe => {
            if is_safe_symlink(link, target, root) {
                SymlinkAction::CreateSymlink(target.to_path_buf())
            } else {
                SymlinkAction::CopyTarget
            }
        }
    }
}

pub enum SymlinkAction {
    /// Skip this symlink
    Skip,
    /// Create symlink with given target
    CreateSymlink(PathBuf),
    /// Copy the target file instead
    CopyTarget,
}

/// Munge symlink for safe daemon transfer
pub fn munge_symlink(target: &Path, munge_links: bool) -> PathBuf {
    if !munge_links {
        return target.to_path_buf();
    }

    // Prefix symlink target to prevent escaping chroot
    let target_str = target.to_string_lossy();

    if target_str.starts_with('/') {
        PathBuf::from(format!("/rsyncd-munged{}", target_str))
    } else if target_str.starts_with("..") {
        PathBuf::from(format!("rsyncd-munged/{}", target_str))
    } else {
        target.to_path_buf()
    }
}
```

---

## Section 67: Delay Updates Mode

The `--delay-updates` option batches file updates for atomicity.

### Delay Updates Implementation

```rust
use std::path::{Path, PathBuf};
use std::collections::HashMap;

/// Delayed update manager
pub struct DelayedUpdates {
    /// Pending updates: temp_path -> final_path
    pending: HashMap<PathBuf, PathBuf>,
    /// Temp directory for delayed files
    temp_dir: PathBuf,
}

impl DelayedUpdates {
    pub fn new(base_dir: &Path) -> io::Result<Self> {
        let temp_dir = base_dir.join(".~tmp~");
        std::fs::create_dir_all(&temp_dir)?;

        Ok(Self {
            pending: HashMap::new(),
            temp_dir,
        })
    }

    /// Get temp path for delayed file
    pub fn temp_path(&self, final_path: &Path) -> PathBuf {
        let name = final_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");

        // Use unique suffix for each file
        let unique = format!("{}.{}", name, self.pending.len());
        self.temp_dir.join(unique)
    }

    /// Register pending update
    pub fn register(&mut self, temp: PathBuf, final_path: PathBuf) {
        self.pending.insert(temp, final_path);
    }

    /// Commit all pending updates atomically
    pub fn commit_all(&mut self) -> io::Result<usize> {
        let mut count = 0;

        for (temp, final_path) in self.pending.drain() {
            // Ensure parent directory exists
            if let Some(parent) = final_path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // Atomic rename
            std::fs::rename(&temp, &final_path)?;
            count += 1;
        }

        // Remove temp directory
        let _ = std::fs::remove_dir(&self.temp_dir);

        Ok(count)
    }

    /// Abort all pending updates
    pub fn abort(&mut self) {
        for (temp, _) in self.pending.drain() {
            let _ = std::fs::remove_file(temp);
        }
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

impl Drop for DelayedUpdates {
    fn drop(&mut self) {
        // Clean up on unexpected termination
        self.abort();
    }
}
```

---

## Section 68: Fake Super Mode

The `--fake-super` option stores root-only attributes in extended attributes.

### Fake Super Implementation

```rust
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// Extended attribute name for fake super data
const FAKE_SUPER_XATTR: &str = "user.rsync.%stat";

/// Fake super metadata encoding
#[derive(Debug, Clone)]
pub struct FakeSuperMeta {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u64,
}

impl FakeSuperMeta {
    /// Encode for xattr storage
    pub fn encode(&self) -> String {
        format!(
            "{:o} {:o},{:o} {:o}:{:o}",
            self.mode,
            (self.rdev >> 8) & 0xfff,
            self.rdev & 0xff,
            self.uid,
            self.gid
        )
    }

    /// Decode from xattr value
    pub fn decode(value: &str) -> Option<Self> {
        let parts: Vec<&str> = value.split_whitespace().collect();
        if parts.len() < 3 {
            return None;
        }

        let mode = u32::from_str_radix(parts[0], 8).ok()?;

        let rdev_parts: Vec<&str> = parts[1].split(',').collect();
        let rdev = if rdev_parts.len() == 2 {
            let major = u64::from_str_radix(rdev_parts[0], 8).ok()?;
            let minor = u64::from_str_radix(rdev_parts[1], 8).ok()?;
            (major << 8) | minor
        } else {
            0
        };

        let id_parts: Vec<&str> = parts[2].split(':').collect();
        let (uid, gid) = if id_parts.len() == 2 {
            (
                u32::from_str_radix(id_parts[0], 8).ok()?,
                u32::from_str_radix(id_parts[1], 8).ok()?,
            )
        } else {
            (0, 0)
        };

        Some(Self { mode, uid, gid, rdev })
    }
}

/// Store fake super attributes
#[cfg(target_os = "linux")]
pub fn store_fake_super(path: &Path, meta: &FakeSuperMeta) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_cstr = CString::new(path.as_os_str().as_bytes())?;
    let name_cstr = CString::new(FAKE_SUPER_XATTR)?;
    let value = meta.encode();

    let result = unsafe {
        libc::lsetxattr(
            path_cstr.as_ptr(),
            name_cstr.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
            0,
        )
    };

    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Retrieve fake super attributes
#[cfg(target_os = "linux")]
pub fn retrieve_fake_super(path: &Path) -> io::Result<Option<FakeSuperMeta>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_cstr = CString::new(path.as_os_str().as_bytes())?;
    let name_cstr = CString::new(FAKE_SUPER_XATTR)?;

    // Get size first
    let size = unsafe {
        libc::lgetxattr(
            path_cstr.as_ptr(),
            name_cstr.as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };

    if size < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENODATA) {
            return Ok(None);
        }
        return Err(err);
    }

    let mut buffer = vec![0u8; size as usize];
    let result = unsafe {
        libc::lgetxattr(
            path_cstr.as_ptr(),
            name_cstr.as_ptr(),
            buffer.as_mut_ptr() as *mut libc::c_void,
            size as usize,
        )
    };

    if result < 0 {
        return Err(io::Error::last_os_error());
    }

    let value = String::from_utf8_lossy(&buffer[..result as usize]);
    Ok(FakeSuperMeta::decode(&value))
}
```

---

## Section 69: Character Encoding

The `--iconv` option enables character set conversion.

### Iconv Implementation

```rust
use std::io::{self, Read, Write};

/// Character encoding conversion context
pub struct IconvContext {
    from_code: String,
    to_code: String,
    #[cfg(feature = "iconv")]
    converter: Option<iconv::Iconv>,
}

impl IconvContext {
    /// Create new conversion context
    pub fn new(from_code: &str, to_code: &str) -> io::Result<Self> {
        #[cfg(feature = "iconv")]
        {
            let converter = iconv::Iconv::new(to_code, from_code)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

            Ok(Self {
                from_code: from_code.to_string(),
                to_code: to_code.to_string(),
                converter: Some(converter),
            })
        }

        #[cfg(not(feature = "iconv"))]
        {
            Ok(Self {
                from_code: from_code.to_string(),
                to_code: to_code.to_string(),
            })
        }
    }

    /// Convert string between encodings
    pub fn convert(&self, input: &str) -> io::Result<String> {
        #[cfg(feature = "iconv")]
        if let Some(ref converter) = self.converter {
            let output = converter.convert(input.as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            return String::from_utf8(output)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }

        // Fallback: assume UTF-8 compatible
        Ok(input.to_string())
    }

    /// Convert path component
    pub fn convert_path(&self, path: &Path) -> io::Result<PathBuf> {
        let converted = self.convert(&path.to_string_lossy())?;
        Ok(PathBuf::from(converted))
    }
}

/// Parse --iconv option value
pub fn parse_iconv_option(value: &str) -> io::Result<(String, String)> {
    // Format: LOCAL,REMOTE or LOCAL (with UTF-8 as remote default)
    let parts: Vec<&str> = value.split(',').collect();

    match parts.as_slice() {
        [local] => Ok((local.to_string(), "UTF-8".to_string())),
        [local, remote] => Ok((local.to_string(), remote.to_string())),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid --iconv format. Use: LOCAL or LOCAL,REMOTE",
        )),
    }
}

/// Handle non-UTF8 filenames with --8-bit-output
pub fn sanitize_filename(name: &[u8], eight_bit: bool) -> String {
    if eight_bit {
        // Pass through non-ASCII bytes
        String::from_utf8_lossy(name).into_owned()
    } else {
        // Escape non-printable characters
        name.iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    (b as char).to_string()
                } else {
                    format!("\\#{:03o}", b)
                }
            })
            .collect()
    }
}
```

---

## Section 70: Daemon Exec Hooks

Pre and post transfer execution hooks.

### Exec Hook Implementation

```rust
use std::process::{Command, Stdio};
use std::path::Path;
use std::collections::HashMap;

/// Daemon execution hooks
pub struct DaemonHooks {
    pub pre_xfer: Option<String>,
    pub post_xfer: Option<String>,
    pub early_exec: Option<String>,
}

/// Hook execution result
pub struct HookResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl DaemonHooks {
    /// Execute pre-transfer hook
    pub fn run_pre_xfer(&self, env: &HookEnv) -> io::Result<Option<HookResult>> {
        self.run_hook(&self.pre_xfer, env)
    }

    /// Execute post-transfer hook
    pub fn run_post_xfer(&self, env: &HookEnv) -> io::Result<Option<HookResult>> {
        self.run_hook(&self.post_xfer, env)
    }

    /// Execute early hook (before chroot)
    pub fn run_early(&self, env: &HookEnv) -> io::Result<Option<HookResult>> {
        self.run_hook(&self.early_exec, env)
    }

    fn run_hook(
        &self,
        cmd: &Option<String>,
        env: &HookEnv,
    ) -> io::Result<Option<HookResult>> {
        let cmd = match cmd {
            Some(c) => c,
            None => return Ok(None),
        };

        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .envs(env.to_env_vars())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()?;

        Ok(Some(HookResult {
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }))
    }
}

/// Environment for hook execution
pub struct HookEnv {
    pub rsync_module_name: String,
    pub rsync_module_path: PathBuf,
    pub rsync_host_addr: String,
    pub rsync_host_name: String,
    pub rsync_user_name: Option<String>,
    pub rsync_pid: u32,
    pub rsync_request: String,
    pub rsync_arg_count: usize,
    pub rsync_exit_status: Option<i32>,
}

impl HookEnv {
    pub fn to_env_vars(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();

        vars.insert("RSYNC_MODULE_NAME".into(), self.rsync_module_name.clone());
        vars.insert("RSYNC_MODULE_PATH".into(), self.rsync_module_path.to_string_lossy().into());
        vars.insert("RSYNC_HOST_ADDR".into(), self.rsync_host_addr.clone());
        vars.insert("RSYNC_HOST_NAME".into(), self.rsync_host_name.clone());
        vars.insert("RSYNC_PID".into(), self.rsync_pid.to_string());
        vars.insert("RSYNC_REQUEST".into(), self.rsync_request.clone());
        vars.insert("RSYNC_ARG#".into(), self.rsync_arg_count.to_string());

        if let Some(ref user) = self.rsync_user_name {
            vars.insert("RSYNC_USER_NAME".into(), user.clone());
        }
        if let Some(status) = self.rsync_exit_status {
            vars.insert("RSYNC_EXIT_STATUS".into(), status.to_string());
        }

        vars
    }
}
```

---

## Section 71: Remote Binary Options

Options for controlling the remote rsync binary.

### Remote Binary Configuration

```rust
use std::path::PathBuf;

/// Remote binary configuration
pub struct RemoteBinaryConfig {
    /// Path to remote rsync binary
    pub rsync_path: Option<PathBuf>,
    /// Remote shell command
    pub rsh: String,
    /// Blocking I/O mode
    pub blocking_io: bool,
    /// SSH options
    pub ssh_options: Vec<String>,
}

impl Default for RemoteBinaryConfig {
    fn default() -> Self {
        Self {
            rsync_path: None,
            rsh: std::env::var("RSYNC_RSH").unwrap_or_else(|_| "ssh".to_string()),
            blocking_io: false,
            ssh_options: Vec::new(),
        }
    }
}

impl RemoteBinaryConfig {
    /// Build remote command
    pub fn build_remote_command(
        &self,
        host: &str,
        args: &[String],
        is_sender: bool,
    ) -> Command {
        let mut cmd = Command::new(&self.rsh);

        // Add SSH options
        for opt in &self.ssh_options {
            cmd.arg(opt);
        }

        // Add host
        cmd.arg(host);

        // Remote rsync path
        let rsync = self.rsync_path.as_deref()
            .unwrap_or(Path::new("rsync"));
        cmd.arg(rsync);

        // Server mode
        cmd.arg("--server");

        if is_sender {
            cmd.arg("--sender");
        }

        // Pass through args
        for arg in args {
            cmd.arg(arg);
        }

        cmd
    }

    /// Parse -e/--rsh option
    pub fn parse_rsh(&mut self, value: &str) {
        let parts: Vec<&str> = value.split_whitespace().collect();
        if let Some((shell, opts)) = parts.split_first() {
            self.rsh = shell.to_string();
            self.ssh_options = opts.iter().map(|s| s.to_string()).collect();
        }
    }
}

/// Environment variables affecting remote execution
pub const RSYNC_RSH: &str = "RSYNC_RSH";
pub const RSYNC_CONNECT_PROG: &str = "RSYNC_CONNECT_PROG";
pub const RSYNC_PROXY: &str = "RSYNC_PROXY";
```

---

## Section 72: Implied Directories

Handling of implied parent directories in transfers.

### Implied Directories Implementation

```rust
use std::path::{Path, PathBuf};
use std::collections::HashSet;

/// Track implied directories during transfer
pub struct ImpliedDirectories {
    /// Directories that were explicitly in file list
    explicit: HashSet<PathBuf>,
    /// Directories created implicitly
    implied: HashSet<PathBuf>,
    /// Whether to create implied dirs
    create_implied: bool,
}

impl ImpliedDirectories {
    pub fn new(create_implied: bool) -> Self {
        Self {
            explicit: HashSet::new(),
            implied: HashSet::new(),
            create_implied,
        }
    }

    /// Register explicit directory from file list
    pub fn add_explicit(&mut self, path: &Path) {
        self.explicit.insert(path.to_path_buf());
    }

    /// Ensure parent directories exist
    pub fn ensure_parents(&mut self, path: &Path) -> io::Result<()> {
        if !self.create_implied {
            return Ok(());
        }

        let mut current = PathBuf::new();
        for component in path.parent().unwrap_or(Path::new("")).components() {
            current.push(component);

            if !self.explicit.contains(&current) && !self.implied.contains(&current) {
                // Create implied directory
                if !current.exists() {
                    std::fs::create_dir(&current)?;
                    self.implied.insert(current.clone());
                }
            }
        }

        Ok(())
    }

    /// Check if directory is implied (not explicit)
    pub fn is_implied(&self, path: &Path) -> bool {
        self.implied.contains(path)
    }

    /// Get metadata handling for implied directory
    pub fn implied_metadata(&self, path: &Path) -> ImpliedMetadata {
        if self.is_implied(path) {
            ImpliedMetadata::Minimal
        } else {
            ImpliedMetadata::Full
        }
    }
}

pub enum ImpliedMetadata {
    /// Apply full metadata (explicit directory)
    Full,
    /// Apply minimal metadata (implied directory)
    Minimal,
}

/// Options affecting implied directories
pub struct ImpliedDirOptions {
    /// --dirs/-d: transfer directories
    pub transfer_dirs: bool,
    /// --no-implied-dirs: don't create parent dirs
    pub no_implied_dirs: bool,
    /// --mkpath: create destination path components
    pub mkpath: bool,
}

impl ImpliedDirOptions {
    pub fn should_create_implied(&self) -> bool {
        !self.no_implied_dirs
    }
}
```

---

## Section 73: UID/GID Mapping

Numeric ID handling and name/ID mapping.

### UID/GID Mapping Implementation

```rust
use std::collections::HashMap;

/// UID/GID mapping configuration
pub struct IdMapping {
    /// Use numeric IDs only
    pub numeric_ids: bool,
    /// UID name -> ID mapping
    uid_map: HashMap<String, u32>,
    /// GID name -> ID mapping
    gid_map: HashMap<String, u32>,
    /// Reverse UID map
    uid_names: HashMap<u32, String>,
    /// Reverse GID map
    gid_names: HashMap<u32, String>,
}

impl IdMapping {
    pub fn new(numeric_ids: bool) -> Self {
        Self {
            numeric_ids,
            uid_map: HashMap::new(),
            gid_map: HashMap::new(),
            uid_names: HashMap::new(),
            gid_names: HashMap::new(),
        }
    }

    /// Load system user database
    #[cfg(unix)]
    pub fn load_passwd(&mut self) -> io::Result<()> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};

        let file = File::open("/etc/passwd")?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 {
                if let Ok(uid) = parts[2].parse::<u32>() {
                    let name = parts[0].to_string();
                    self.uid_names.insert(uid, name.clone());
                    self.uid_map.insert(name, uid);
                }
            }
        }
        Ok(())
    }

    /// Load system group database
    #[cfg(unix)]
    pub fn load_group(&mut self) -> io::Result<()> {
        use std::fs::File;
        use std::io::{BufRead, BufReader};

        let file = File::open("/etc/group")?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 {
                if let Ok(gid) = parts[2].parse::<u32>() {
                    let name = parts[0].to_string();
                    self.gid_names.insert(gid, name.clone());
                    self.gid_map.insert(name, gid);
                }
            }
        }
        Ok(())
    }

    /// Map UID from remote to local
    pub fn map_uid(&self, remote_uid: u32, remote_name: Option<&str>) -> u32 {
        if self.numeric_ids {
            return remote_uid;
        }

        if let Some(name) = remote_name {
            if let Some(&local_uid) = self.uid_map.get(name) {
                return local_uid;
            }
        }

        // Fall back to numeric ID
        remote_uid
    }

    /// Map GID from remote to local
    pub fn map_gid(&self, remote_gid: u32, remote_name: Option<&str>) -> u32 {
        if self.numeric_ids {
            return remote_gid;
        }

        if let Some(name) = remote_name {
            if let Some(&local_gid) = self.gid_map.get(name) {
                return local_gid;
            }
        }

        remote_gid
    }

    /// Get name for UID
    pub fn uid_name(&self, uid: u32) -> Option<&str> {
        self.uid_names.get(&uid).map(|s| s.as_str())
    }

    /// Get name for GID
    pub fn gid_name(&self, gid: u32) -> Option<&str> {
        self.gid_names.get(&gid).map(|s| s.as_str())
    }
}

/// Wire format for name/ID exchange
pub fn write_id_mapping<W: Write>(
    writer: &mut W,
    id: u32,
    name: Option<&str>,
) -> io::Result<()> {
    writer.write_all(&id.to_le_bytes())?;

    if let Some(n) = name {
        let bytes = n.as_bytes();
        writer.write_all(&(bytes.len() as u8).to_le_bytes())?;
        writer.write_all(bytes)?;
    } else {
        writer.write_all(&[0u8])?;
    }

    Ok(())
}
```

---

## Section 74: Daemon Socket Options

TCP socket configuration for daemon connections.

### Socket Options Implementation

```rust
use std::net::TcpStream;
use std::time::Duration;

/// Socket configuration for daemon
pub struct SocketOptions {
    /// TCP_NODELAY
    pub nodelay: bool,
    /// SO_KEEPALIVE
    pub keepalive: bool,
    /// Keepalive interval (seconds)
    pub keepalive_interval: Option<u32>,
    /// SO_SNDBUF size
    pub sndbuf: Option<usize>,
    /// SO_RCVBUF size
    pub rcvbuf: Option<usize>,
    /// IP_TOS value
    pub tos: Option<u8>,
}

impl Default for SocketOptions {
    fn default() -> Self {
        Self {
            nodelay: true,
            keepalive: true,
            keepalive_interval: Some(30),
            sndbuf: None,
            rcvbuf: None,
            tos: None,
        }
    }
}

impl SocketOptions {
    /// Apply options to TCP stream
    #[cfg(unix)]
    pub fn apply(&self, stream: &TcpStream) -> io::Result<()> {
        use std::os::unix::io::AsRawFd;

        stream.set_nodelay(self.nodelay)?;

        let fd = stream.as_raw_fd();

        if let Some(size) = self.sndbuf {
            unsafe {
                let size = size as libc::c_int;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        if let Some(size) = self.rcvbuf {
            unsafe {
                let size = size as libc::c_int;
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    &size as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        if let Some(tos) = self.tos {
            unsafe {
                let tos = tos as libc::c_int;
                libc::setsockopt(
                    fd,
                    libc::IPPROTO_IP,
                    libc::IP_TOS,
                    &tos as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }

        Ok(())
    }

    /// Parse socket options from config string
    pub fn parse(value: &str) -> Self {
        let mut opts = Self::default();

        for part in value.split(',') {
            let part = part.trim();

            if let Some((key, val)) = part.split_once('=') {
                match key {
                    "sndbuf" => opts.sndbuf = val.parse().ok(),
                    "rcvbuf" => opts.rcvbuf = val.parse().ok(),
                    "tos" => opts.tos = val.parse().ok(),
                    "keepalive" => opts.keepalive_interval = val.parse().ok(),
                    _ => {}
                }
            } else {
                match part {
                    "nodelay" => opts.nodelay = true,
                    "keepalive" => opts.keepalive = true,
                    _ => {}
                }
            }
        }

        opts
    }
}
```

---

## Section 75: Whole File Transfer

The `--whole-file` option disables delta transfer.

### Whole File Implementation

```rust
/// Whole file transfer mode
#[derive(Clone, Copy, PartialEq)]
pub enum WholeFileMode {
    /// Auto-detect based on transfer type
    Auto,
    /// Always transfer whole files
    Always,
    /// Never transfer whole files (force delta)
    Never,
}

impl WholeFileMode {
    /// Determine if delta should be used
    pub fn use_delta(&self, is_local: bool, file_size: u64, block_size: usize) -> bool {
        match self {
            WholeFileMode::Always => false,
            WholeFileMode::Never => true,
            WholeFileMode::Auto => {
                // Local transfers: whole file is usually faster
                if is_local {
                    return false;
                }

                // Very small files: overhead not worth it
                if file_size < (block_size * 2) as u64 {
                    return false;
                }

                true
            }
        }
    }
}

/// Whole file sender
pub fn send_whole_file<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    file_size: u64,
) -> io::Result<u64> {
    // Send file size
    writer.write_all(&(file_size as u32).to_le_bytes())?;

    // Send file data
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;

    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buffer[..n])?;
        total += n as u64;
    }

    // Send end marker
    writer.write_all(&0i32.to_le_bytes())?;

    Ok(total)
}

/// Whole file receiver
pub fn receive_whole_file<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> io::Result<u64> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;

    loop {
        // Read block/literal length
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf)?;
        let len = i32::from_le_bytes(len_buf);

        if len == 0 {
            // End of file
            break;
        }

        if len < 0 {
            // Block reference (shouldn't happen in whole-file mode)
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unexpected block reference in whole-file mode",
            ));
        }

        // Read literal data
        let len = len as usize;
        if len > buffer.len() {
            buffer.resize(len, 0);
        }
        reader.read_exact(&mut buffer[..len])?;
        writer.write_all(&buffer[..len])?;
        total += len as u64;
    }

    Ok(total)
}
```

---

## Section 76: Append Mode

The `--append` and `--append-verify` options for resuming transfers.

### Append Mode Implementation

```rust
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Append mode configuration
#[derive(Clone, Copy)]
pub enum AppendMode {
    /// No append
    None,
    /// Append without verification
    Append,
    /// Append with checksum verification
    AppendVerify,
}

/// Handle append mode transfer
pub fn handle_append<R: Read>(
    mode: AppendMode,
    reader: &mut R,
    dest: &Path,
    expected_size: u64,
) -> io::Result<u64> {
    match mode {
        AppendMode::None => {
            // Normal transfer, create/overwrite file
            let mut file = File::create(dest)?;
            io::copy(reader, &mut file)
        }

        AppendMode::Append => {
            let existing_size = dest.metadata().map(|m| m.len()).unwrap_or(0);

            if existing_size >= expected_size {
                // File complete or larger
                return Ok(0);
            }

            // Skip already transferred bytes
            skip_bytes(reader, existing_size)?;

            // Open for append
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dest)?;

            io::copy(reader, &mut file)
        }

        AppendMode::AppendVerify => {
            let existing_size = dest.metadata().map(|m| m.len()).unwrap_or(0);

            if existing_size == 0 {
                // Fresh transfer
                let mut file = File::create(dest)?;
                return io::copy(reader, &mut file);
            }

            // Verify existing portion
            let mut existing = File::open(dest)?;
            let mut verified = 0u64;
            let mut buf1 = vec![0u8; 64 * 1024];
            let mut buf2 = vec![0u8; 64 * 1024];

            while verified < existing_size {
                let to_read = ((existing_size - verified) as usize).min(buf1.len());

                existing.read_exact(&mut buf1[..to_read])?;
                reader.read_exact(&mut buf2[..to_read])?;

                if buf1[..to_read] != buf2[..to_read] {
                    // Mismatch - rewrite from this point
                    let mut file = OpenOptions::new()
                        .write(true)
                        .open(dest)?;
                    file.seek(SeekFrom::Start(verified))?;
                    file.write_all(&buf2[..to_read])?;

                    // Continue with remaining data
                    return io::copy(reader, &mut file).map(|n| n + to_read as u64);
                }

                verified += to_read as u64;
            }

            // Existing data verified, append remainder
            let mut file = OpenOptions::new()
                .append(true)
                .open(dest)?;

            io::copy(reader, &mut file)
        }
    }
}

fn skip_bytes<R: Read>(reader: &mut R, count: u64) -> io::Result<()> {
    let mut remaining = count;
    let mut buf = vec![0u8; 64 * 1024];

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        reader.read_exact(&mut buf[..to_read])?;
        remaining -= to_read as u64;
    }

    Ok(())
}
```

---

## Section 77: Copy Dest Options

The `--copy-dest`, `--compare-dest`, and `--link-dest` options.

### Alternate Destination Implementation

```rust
use std::path::{Path, PathBuf};
use std::os::unix::fs::MetadataExt;

/// Alternate destination mode
#[derive(Clone, Copy)]
pub enum AltDestMode {
    /// --copy-dest: Copy from alternate if unchanged
    Copy,
    /// --compare-dest: Check alternate for unchanged
    Compare,
    /// --link-dest: Hard link from alternate if unchanged
    Link,
}

/// Alternate destination configuration
pub struct AltDest {
    pub mode: AltDestMode,
    pub paths: Vec<PathBuf>,
}

impl AltDest {
    /// Find file in alternate destinations
    pub fn find(&self, relative_path: &Path) -> Option<PathBuf> {
        for base in &self.paths {
            let full = base.join(relative_path);
            if full.exists() {
                return Some(full);
            }
        }
        None
    }

    /// Check if file matches alternate (for compare-dest)
    pub fn matches(&self, relative_path: &Path, source: &FileEntry) -> Option<PathBuf> {
        for base in &self.paths {
            let full = base.join(relative_path);
            if let Ok(meta) = full.metadata() {
                if meta.len() == source.size && meta.mtime() == source.mtime as i64 {
                    return Some(full);
                }
            }
        }
        None
    }

    /// Handle file with alternate destination
    pub fn handle(
        &self,
        relative_path: &Path,
        source: &FileEntry,
        dest: &Path,
    ) -> io::Result<AltDestAction> {
        match self.matches(relative_path, source) {
            Some(alt_path) => {
                match self.mode {
                    AltDestMode::Compare => {
                        // File unchanged from alternate, skip transfer
                        Ok(AltDestAction::Skip)
                    }
                    AltDestMode::Copy => {
                        // Copy from alternate
                        std::fs::copy(&alt_path, dest)?;
                        Ok(AltDestAction::Copied)
                    }
                    AltDestMode::Link => {
                        // Hard link to alternate
                        std::fs::hard_link(&alt_path, dest)?;
                        Ok(AltDestAction::Linked)
                    }
                }
            }
            None => Ok(AltDestAction::Transfer),
        }
    }
}

pub enum AltDestAction {
    /// Skip transfer (file matches alternate)
    Skip,
    /// File copied from alternate
    Copied,
    /// File hard-linked to alternate
    Linked,
    /// Normal transfer required
    Transfer,
}
```

---

## Section 78: Size Filters

The `--max-size` and `--min-size` options.

### Size Filter Implementation

```rust
/// Size specification (with optional suffix)
#[derive(Clone, Copy)]
pub struct SizeSpec {
    bytes: u64,
}

impl SizeSpec {
    /// Parse size specification
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("Empty size specification".to_string());
        }

        let (num_str, suffix) = if s.chars().last().unwrap().is_alphabetic() {
            let split = s.len() - 1;
            // Handle two-letter suffixes (KB, MB, etc.)
            let split = if s.len() > 2 && s.chars().nth(s.len() - 2).unwrap().is_alphabetic() {
                s.len() - 2
            } else {
                split
            };
            (&s[..split], Some(&s[split..]))
        } else {
            (s, None)
        };

        let base: f64 = num_str.parse()
            .map_err(|e| format!("Invalid number: {}", e))?;

        let multiplier: u64 = match suffix {
            None => 1,
            Some(s) => match s.to_uppercase().as_str() {
                "B" => 1,
                "K" | "KB" => 1024,
                "M" | "MB" => 1024 * 1024,
                "G" | "GB" => 1024 * 1024 * 1024,
                "T" | "TB" => 1024 * 1024 * 1024 * 1024,
                "P" | "PB" => 1024 * 1024 * 1024 * 1024 * 1024,
                _ => return Err(format!("Unknown size suffix: {}", s)),
            },
        };

        Ok(Self {
            bytes: (base * multiplier as f64) as u64,
        })
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// Size filter configuration
pub struct SizeFilter {
    pub min_size: Option<SizeSpec>,
    pub max_size: Option<SizeSpec>,
}

impl SizeFilter {
    /// Check if file passes size filter
    pub fn matches(&self, size: u64) -> bool {
        if let Some(ref min) = self.min_size {
            if size < min.bytes() {
                return false;
            }
        }
        if let Some(ref max) = self.max_size {
            if size > max.bytes() {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_spec_parse() {
        assert_eq!(SizeSpec::parse("100").unwrap().bytes(), 100);
        assert_eq!(SizeSpec::parse("1K").unwrap().bytes(), 1024);
        assert_eq!(SizeSpec::parse("1KB").unwrap().bytes(), 1024);
        assert_eq!(SizeSpec::parse("1M").unwrap().bytes(), 1024 * 1024);
        assert_eq!(SizeSpec::parse("1.5G").unwrap().bytes(), 1610612736);
    }
}
```

---

## Section 79: Modify Window

The `--modify-window` option for timestamp comparison tolerance.

### Modify Window Implementation

```rust
/// Modification time comparison with tolerance
pub struct ModifyWindow {
    /// Tolerance in seconds (default 0)
    tolerance: i64,
}

impl ModifyWindow {
    pub fn new(seconds: i64) -> Self {
        Self { tolerance: seconds }
    }

    /// Default (exact match)
    pub fn exact() -> Self {
        Self { tolerance: 0 }
    }

    /// FAT filesystem compatibility (2-second resolution)
    pub fn fat_compat() -> Self {
        Self { tolerance: 1 }
    }

    /// Check if times are equal within tolerance
    pub fn times_equal(&self, t1: i64, t2: i64) -> bool {
        (t1 - t2).abs() <= self.tolerance
    }

    /// Check if file needs update based on mtime
    pub fn needs_update(&self, source_mtime: i64, dest_mtime: i64) -> bool {
        !self.times_equal(source_mtime, dest_mtime)
    }

    /// Check if source is newer
    pub fn source_newer(&self, source_mtime: i64, dest_mtime: i64) -> bool {
        source_mtime > dest_mtime + self.tolerance
    }
}

impl Default for ModifyWindow {
    fn default() -> Self {
        Self::exact()
    }
}

/// Compare timestamps with modify window
pub fn compare_timestamps(
    source: i64,
    dest: i64,
    window: &ModifyWindow,
    check_newer: bool,
) -> TimestampComparison {
    if window.times_equal(source, dest) {
        TimestampComparison::Equal
    } else if check_newer && !window.source_newer(source, dest) {
        TimestampComparison::DestNewer
    } else {
        TimestampComparison::Different
    }
}

pub enum TimestampComparison {
    /// Times are equal (within tolerance)
    Equal,
    /// Times differ, transfer needed
    Different,
    /// Destination is newer (skip with --update)
    DestNewer,
}
```

---

This document is the contract between the internal agents and the external
behaviour of **oc-rsync**. Changes to binaries, crates, or CI workflows must be
reflected here so contributors and reviewers can reason about the system as a
whole.
