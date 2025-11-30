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

## Agents Overview

### 1) Client & Daemon Entrypoint (CLI Binary)

- **Binary**: `src/bin/oc-rsync.rs`
- **Depends on**: `cli`, `core`, `transport`, `daemon`, `logging`
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

### 2.2 `enforce_limits` Agent

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

### 2.4 `no_placeholders` Agent

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

This document is the contract between the internal agents and the external
behaviour of **oc-rsync**. Changes to binaries, crates, or CI workflows must be
reflected here so contributors and reviewers can reason about the system as a
whole.

```
