# AGENTS.md — Roles, Responsibilities, APIs, and Error/Message Conventions

This document defines the internal actors (“agents”), their responsibilities, APIs, invariants, and how user-visible messages (including Rust source file remapping) are produced. All binaries must route user-visible behavior through these agents via the `core` facade.

---

## Global Conventions

- **Canonical branding metadata** is sourced from `[workspace.metadata.oc_rsync]`
  in `Cargo.toml`. The branded binaries are **`oc-rsync`** and **`oc-rsyncd`**,
  the daemon configuration lives under `/etc/oc-rsyncd/` (for example
  `/etc/oc-rsyncd/oc-rsyncd.conf` and `/etc/oc-rsyncd/oc-rsyncd.secrets`), the
  published version string is `3.4.1-rust`, and the authoritative source
  repository URL is <https://github.com/oferchen/rsync>. Any user-facing surface
  (rustdoc examples, CLI help, documentation, packaging manifests, CI logs) must
  derive these values from the shared metadata via the `xtask branding` helpers
  or equivalent library APIs rather than hard-coding constants.
- **Error Message Suffix (C→Rust remap)**:
  Format: `... (code N) at <repo-rel-path>:<line> [<role>=3.4.1-rust]`
  Implemented in `crates/core/src/message.rs` via:
  - `role: Role` enum (`Sender/Receiver/Generator/Server/Client/Daemon`) chosen at call-site.  
  - `source_path: &'static str = file!()`; `source_line: u32 = line!()`; normalized to repo-relative.  
  - Central constructor: `Message::error(code, text).with_role(role).with_source(file!(), line!())`.
- **Roles in trailers** mirror upstream semantics exactly.  
- **All info/warn/error/progress strings** are centralized in `core::message::strings` for snapshot tests.
- **Remote fallback guardrails**: before spawning upstream helpers, the client
  and daemon must confirm that the selected fallback binary exists on `PATH`
  (or via explicit overrides) and is executable, surfacing a branded
  diagnostic when the check fails so operators can install upstream `rsync` or
  set `OC_RSYNC_FALLBACK` appropriately. Use the shared
  `rsync_core::fallback::fallback_binary_available` and
  `rsync_core::fallback::describe_missing_fallback_binary` helpers to keep the
  guard rails consistent across binaries. The availability helper now caches
  its result for each `(binary, PATH[, PATHEXT])` tuple to avoid repeated
  filesystem scans; any updates must preserve this memoisation and ensure
  tests adjust environment variables via the existing `EnvGuard` utilities so
  cache entries stay coherent.
- **Workspace-wide nextest configuration**: `.config/nextest.toml` pins
  `[profile.default.package] graph = "workspace"` so a bare
  `cargo nextest run` executes the entire workspace without additional flags.
  Contributors must keep this file in sync with any future profile
  adjustments so local invocations match CI coverage.
- **Standard-library-first implementations**: Prefer the Rust standard
  library and well-supported crates that are actively maintained. Avoid
  deprecated APIs, pseudo-code, or placeholder logic; every change must ship
  production-ready behaviour with comprehensive tests or parity checks.
- **CPU-accelerated hot paths**: The rolling checksum pipeline uses
  architecture-specific SIMD fast paths (AVX2 and SSE2 on `x86`/`x86_64`, NEON on
  `aarch64`) that fall back to the scalar implementation for other targets.
  Runtime feature detection is cached via `OnceLock` so repeated checksum calls
  avoid the overhead of re-querying `is_x86_feature_detected!`/
  `is_aarch64_feature_detected!`, and any updates
  must preserve that memoisation alongside the scalar fallback. Any updates to
  `crates/checksums`—especially `rolling::checksum::accumulate_chunk`—must keep
  the SIMD and scalar implementations in lockstep, reuse the shared scalar
  helper for edge cases, and extend the parity tests
  (`avx2_accumulate_matches_scalar_reference`,
  `sse2_accumulate_matches_scalar_reference`, and
  `neon_accumulate_matches_scalar_reference`) whenever new optimisations are
  introduced. The SIMD reducers rely on `horizontal_sum_epi64` to collapse
  64-bit partial sums without spilling to the stack; any future changes to the
  AVX2/SSE2 accumulation paths should reuse that helper so the scalar and SIMD
  implementations remain identical. The sparse-writer fast path in
  `crates/engine/src/local_copy/executor/file/sparse.rs` now batches zero-run
  detection into 16-byte `u128` comparisons before falling back to the scalar
  prefix scan; `zero_run_length_matches_scalar_reference` keeps the vectorised
  path and scalar reference in lockstep. Updates must preserve the single
  seek-per-zero-run invariant and keep those tests (and the sparse copy
  integrations) green. The bandwidth parser in `crates/bandwidth`
  likewise leans on `memchr` to locate decimal separators and exponent markers
  so ASCII scans stay vectorised; updates must keep the byte-oriented fast path
  aligned with the exhaustive parser tests. Additional CPU offloading should
  follow the same
  pattern of runtime feature detection (where applicable) paired with
  deterministic tests that compare against the scalar reference implementation.
  The vectored rolling checksum updater coalesces small `IoSlice` buffers into
  a 128-byte stack scratch space before dispatching so SIMD back-ends can run on
  aggregated input; any regression must keep that scratch path and its
  associated unit test (`update_vectored_coalesces_small_slices`) intact.
  Multi-byte rolling updates (`RollingChecksum::roll_many`) now rely on the
  weighted-delta aggregation introduced in
  `crates/checksums/src/rolling/checksum.rs`, which collapses per-byte loops
  into a handful of arithmetic reductions while retaining an escape hatch to
  the scalar `roll` path for exotic slice lengths. Future optimisations must
  preserve the aggregated arithmetic and extend the long-sequence regression
  test (`roll_many_matches_single_rolls_for_long_sequences`) so both code paths
  remain in parity. The `VersionInfoConfig::with_runtime_capabilities` helper
  surfaces the SIMD detection result (via
  `rsync_checksums::simd_acceleration_available`) so `--version` output tracks
  the acceleration active at runtime; update the helper whenever new
  architecture-specific paths are introduced.
- **Environment guardrails for tests**: When exercising fallback overrides or
  other environment-sensitive logic in unit tests, use the existing
  `EnvGuard` helpers (for example, `crates/daemon/src/tests/support.rs` or the
  scoped guard in `crates/daemon/src/daemon/sections/tests.rs`) so variables
  are restored even if the test panics.
- **CI workflow contract**: `.github/workflows/ci.yml` orchestrates the
  reusable workflows in `.github/workflows/lint.yml`,
  `.github/workflows/test-linux.yml`, and
  `.github/workflows/cross-compile.yml`. The cross-compile dispatcher
  delegates to platform-specific workflows under
  `.github/workflows/build-linux.yml`, `.github/workflows/build-macos.yml`,
  and `.github/workflows/build-windows.yml`, each of which owns the matrix for
  its operating system. The test workflow continues to run exclusively on
  Linux, while the platform jobs emit Linux (x86_64/aarch64), macOS
  (x86_64/aarch64), and Windows (x86_64/aarch64) artifacts. The Windows x86
  entry remains in the matrix as a disabled placeholder for future enablement.
  Automation inside CI steps must rely on Rust tooling (`cargo`, `cargo xtask`)
  rather than ad-hoc shell or Python scripts, and additional
  validation/packaging logic should be surfaced via `xtask` subcommands so both
  local and CI runs stay in sync.
- **Documentation validation guardrail**: `cargo xtask docs --validate` now
  asserts that `.github/workflows/cross-compile.yml` references the three
  platform workflows and that each of those files mirrors the cross-compilation
  platforms declared under `[workspace.metadata.oc_rsync.cross_compile]`. The
  validator keeps the Windows x86/aarch64 entries present but disabled, and
  verifies that every matrix entry advertises the expected `target`,
  `build_command`, `build_daemon`, `uses_zig`, `needs_cross_gcc`, and
  `generate_sbom` values for its platform. Each platform job must also disable
  `fail-fast`, set `max-parallel` to a value greater than one, and expose a
  `strategy.matrix` block that matches the workspace metadata so builds run in
  parallel. The validator rejects duplicate matrix entries, flags unexpected
  targets, and enforces that `test-linux` is the only test job (running on
  `ubuntu-latest`) to keep the test suite Linux-only as required by CI.
  Contributors must update both the manifest metadata and CI matrices together
  so the validation continues to pass. The same validation pass also enforces
  that `docs/COMPARE.md`
  references the branded `oc-rsync` binaries, daemon configuration path
  (`/etc/oc-rsyncd/oc-rsyncd.conf`), and published version string
  (`3.4.1-rust`) sourced from workspace metadata so release documentation
  remains consistent with packaged artifacts.
- **`xtask docs` decomposition**: the former monolithic `docs.rs` command
  handler now lives in `xtask/src/commands/docs/` with dedicated modules for
  argument parsing (`cli.rs`), command execution (`executor.rs`), and
  validation (`validation/`). Keep new helpers scoped to these purpose-built
  modules so the hygiene guard continues to pass and future contributors can
  extend validation logic without regressing the line-count cap.
- **`xtask package` decomposition**: the packaging command resides in
  `xtask/src/commands/package/` split across `args.rs`, `build.rs`,
  `tarball.rs`, and `tests.rs`. Extend argument handling or packaging logic in
  those focused modules rather than introducing a monolithic
  `package.rs`, and keep each file below the hygiene thresholds.
- **CLI execution decomposition**: The `crates/cli/src/frontend/execution`
  module is being split into dedicated submodules (`options`, `module_listing`,
  `validation`, etc.) so the formerly monolithic `execution.rs` file can be
  reduced below the hygiene thresholds. New helpers should continue to live in
  those purpose-specific modules (or additional siblings) instead of returning
  to `drive/mod.rs`, and future iterations should keep migrating logical
  segments (for example, fallback handling and config assembly) until every
  file stays under the 600-line cap. The `drive/` directory now owns the
  high-level orchestration flow through dedicated helpers (`options.rs`
  handles info/debug/bandwidth/compress parsing, `config.rs` assembles the
  client builder, `filters.rs` wires include/exclude rules, `summary.rs`
  renders progress/output, `fallback.rs` prepares remote invocation arguments,
  and `metadata.rs` derives preservation flags). Extend these modules directly
  instead of inflating `mod.rs` again.
- **Drive workflow layering**: The orchestrator now lives under
  `crates/cli/src/frontend/execution/drive/workflow/` with
  `preflight.rs`, `fallback_plan.rs`, and `operands.rs` hosting the
  argument validation, remote-fallback assembly, and usage rendering logic,
  respectively. Keep new flow-control helpers in these focused modules (or
  additional siblings) so `workflow/mod.rs` remains under the hygiene cap and
  primarily coordinates the sequence rather than re-implementing the details.
- **CLI argument parser decomposition**: `crates/cli/src/frontend/arguments`
  has been converted into a module tree (`program_name.rs`, `bandwidth.rs`,
  `parsed_args.rs`, `env.rs`, and `parser.rs`) to keep every file below the
  hygiene ceiling. New parsing helpers or data structures must join the
  appropriate submodule (or an additional sibling) so `parser.rs` stays focused
  on orchestration while data types and environment glue remain isolated.
- **Filter rule decomposition**: The CLI filter utilities now live in
  `crates/cli/src/frontend/filter_rules/` with focused modules for arguments,
  CVS exclusions, directive parsing, merge handling, and source loading. New
  helpers must join the appropriate submodule rather than reintroducing a
  monolithic `filter_rules.rs`; keep each file below the hygiene threshold and
  follow the existing layering when extending filter behaviour.
- **Local copy file executor decomposition**: The massive
  `crates/engine/src/local_copy/executor/file.rs` implementation has been
  reorganized into the `file/` module tree (`copy/`, `links`, `transfer`, etc.)
  so each responsibility stays below the hygiene limit. Future changes to file
  transfer behaviour must extend the appropriate helper module rather than
  reintroducing monolithic logic in a single file.
- **Bandwidth limiter decomposition**: The throttling implementation now lives
  in `crates/bandwidth/src/limiter/` with dedicated modules for configuration
  (`change.rs`), runtime behaviour (`core.rs`), and sleep utilities
  (`sleep.rs`). New pacing logic or helpers must integrate with these modules
  instead of growing a single large source file.
- **Dir-merge parser decomposition**: The former
  `crates/engine/src/local_copy/dir_merge/parse.rs` has been split into the
  `parse/` module directory (`types.rs`, `line.rs`, `merge.rs`,
  `dir_merge.rs`, `modifiers.rs`) so no single file exceeds the hygiene cap.
  When extending dir-merge parsing, add helpers to the relevant focused module
  instead of reassembling the original monolithic file.

---

## Agents Overview

### 1) Client (CLI Frontend)
- **Binary**: `src/bin/oc-rsync.rs`
- **Depends on**: `cli`, `core`, `transport`  
- **Responsibilities**:
  - Parse CLI (Clap v4) and render upstream-parity help/misuse.
  - Build `CoreConfig` via Builder and call `core::run_client()`.
  - Route `--msgs2stderr`, `--out-format`, `--info/--debug` to `logging`.
  - When invoked without transfer operands, emit the usage banner to **stdout** before surfacing the canonical "missing source operands" error so tests remain deterministic and compatible with scripts that expect upstream ordering.
- **Invariants**:
  - Never access protocol/engine directly; only via `core`.
  - `--version` reflects feature gates and prints `3.4.1-rust`.

**Key API**:
```rust
pub fn main() -> ExitCode {
    let (cfg, fmt) = cli::parse_args();         // custom help renderer parity
    core::run_client(cfg, fmt).into()
}
````

*Decomposition note*: flag parsing lives in `crates/cli/src/frontend/arguments.rs`; keep new switches in that module to avoid regressing the hygiene guard on `mod.rs`.

---

### 2) Daemon (rsyncd)

* **Binary**: `src/bin/oc-rsyncd.rs`
* **Depends on**: `daemon`, `core`, `transport`, `logging`
* **Responsibilities**:

  * Listen on TCP; legacy `@RSYNCD:` negotiation for pre-30; binary handshake otherwise.
  * Apply `oc-rsyncd.conf` semantics (auth users, secrets 0600, chroot, caps).
  * Enforce daemon `--bwlimit` as default and **cap**.
  * sd_notify ready/status; systemd unit/env-file integration.
* **Invariants**:

  * Never bypass `core` for transfers or metadata.

**Key API**:

```rust
pub fn main() -> ExitCode {
    let conf = daemon::load_config();
    daemon::serve(conf) // loops; spawns sessions; per-session calls into core
}
```

*Modularity notes*: the daemon implementation is now decomposed across
`crates/daemon/src/daemon/sections/*.rs` and pulled into `daemon.rs` via
`include!` blocks so no single file exceeds the hygiene caps. New work that
touches the daemon should follow this layout, adding additional section files as
needed rather than growing existing ones.

---

### 3) Core (Facade)

* **Crate**: `crates/core`
* **Depends on**: `protocol`, `engine`, `meta`, `filters`, `compress`, `checksums`, `logging`, `transport`
* **Responsibilities**:

  * Single facade for orchestration: file walking, selection, delta pipeline, metadata, xattrs/ACLs, messages/progress.
  * Enforce centralization: all transfers use `core::session()`; both CLI and daemon go through here.
  * Error/message construction (including Rust source suffix + role trailers).
* **Invariants**:

  * No `unwrap/expect`; stable error enums → exit code mapping.

**Key API**:

```rust
pub struct CoreConfig { /* builder-generated */ }
pub fn run_client(cfg: CoreConfig, fmt: logging::Format) -> Result<(), CoreError>;
pub fn run_daemon_session(ctx: DaemonCtx, req: ModuleRequest) -> Result<(), CoreError>;
```

---

### 4) Protocol (Handshake & Multiplex)

* **Crate**: `crates/protocol`
* **Responsibilities**:

  * Version negotiation (32–28), constants copied from upstream.
  * Envelope read/write; multiplex `MSG_*`; legacy `@RSYNCD:` fallback.
  * Golden byte streams and fuzz tests.
* **Key API**:

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
  * Block-match/literal emission per upstream heuristics.
  * `--inplace/--partial` behavior; temp-file commit.
* **Perf**: buffer reuse; vectored I/O; cache-friendly layouts.
  * `delta/script.rs::apply_delta` caches the current basis offset so
    sequential `COPY` tokens avoid redundant seeks. The helper advances the
    tracked position with `u64::checked_add` on every buffered read and
    returns an `InvalidInput` error on overflow. Future optimisations must keep
    this monotonic tracking intact and continue reusing the shared copy buffer
    to minimise syscall churn on large delta streams.

#### Local Copy Module Layout

- `crates/engine/src/local_copy/` is decomposed into focused modules. The
  `executor/` directory now contains `cleanup`, `directory`, `file`,
  `reference`, `sources`, `special`, and `util` submodules. Shared helpers such
  as hard-link tracking, metadata synchronization, and operand parsing live in
  sibling files (`hard_links.rs`, `metadata_sync.rs`, `operands.rs`).
- New work touching the local copy path **must** follow this structure instead
  of growing a single monolithic file. Prefer adding small modules and keep
  re-exports in `executor/mod.rs` limited to items required by other modules or
  tests. Test-only helpers are gated behind `cfg(test)` to keep release builds
  warning-free.
- When splitting further, update this section to document the new module and
  adjust the curated re-export list so that only intentional surface area is
  exposed.
- `local_copy/context.rs` keeps the `CopyContext` inherent impl decomposed via
  `include!` into `context_impl/impl_part*.rs` files, each ≤400 LoC. Extend the
  implementation by adding new part files (and updating the include list) when
  a segment would otherwise exceed the hygiene caps—never grow the root file
  beyond the centralized preamble/postamble.

---

### 6) Walk (File List)

* **Crate**: `crates/walk`
* **Responsibilities**:

  * Deterministic traversal; relative-path enforcement; path-delta compression.
  * Sorted lexicographic order; repeated-field elision.

---

### 7) Filters (Selection Grammar)

* **Crate**: `crates/filters`
* **Responsibilities**:

  * Parser/merger for `--filter`, includes/excludes, `.rsync-filter`.
  * Property tests & snapshot goldens.

---

### 8) Meta (Metadata/XAttrs/ACLs)

* **Crate**: `crates/meta`
* **Responsibilities**:

  * Apply/record perms/uid/gid/ns-mtime/links/devices/FIFOs/symlinks.
  * `-A/--acls` implies `--perms`; gated features & diagnostics.
  * `-X/--xattrs` namespace rules; feature gating.

---

### 9) Compress (zlib/zstd)

* **Crate**: `crates/compress`
* **Responsibilities**:

  * Upstream defaults/negotiation; parity with `-z` & `--compress-level`.
  * Throughput/ratio benchmarks.

---

### 10) Checksums

* **Crate**: `crates/checksums`
* **Responsibilities**:

  * Rolling `rsum`; MD4/MD5/xxhash (protocol-selected).
  * Property tests (window slide, truncation, seeds).

---

### 11) Transport

* **Crate**: `crates/transport`
* **Responsibilities**:

  * ssh stdio passthrough; `rsync://` TCP; stdio mux.
  * Timeouts/back-pressure; daemon cap enforcement.

---

### 12) Logging & Messages

* **Crate**: `crates/logging`
* **Responsibilities**:

  * Map `--info/--debug`; `--msgs2stderr`; `--out-format`.
  * Central construction of user-visible messages via `core::message`.
  * Exit-code mapping; progress and summary parity.

---

## Exit Codes & Roles

* Exit codes map 1:1 to upstream; enforced by integration tests.
* Each agent sets its role for message trailers:

  * Client sender path → `[sender]`
  * Client receiver path → `[receiver]`
  * Generator on receive side → `[generator]`
  * Daemon process context → `[server]`/`[daemon]` as upstream does

---

## Security & Timeouts

* Path normalization & traversal prevention identical to upstream (relative paths only unless explicitly allowed).
* Timeouts applied at transport and protocol layers; back-pressure respected.
* `secrets file` permissions (0600) enforced with upstream-style diagnostics.

---

## Interop & Determinism

* Loopback CI matrix across protocols 32–28 with upstream versions 3.0.9/3.1.3/3.4.1.
* Upstream references are cloned from `https://github.com/RsyncProject/rsync` tags
  (`v3.0.9`, `v3.1.3`, `v3.4.1`) by `tools/ci/run_interop.sh`, which runs
  `./prepare-source` when necessary before configuring and installing the
  binaries. Contributors must keep the tag list in sync with the versions tested
  by the parity harness.
* Deterministic output: `LC_ALL=C`, `COLUMNS=80`; normalized metadata ordering; stable progress formatting.
* Error messages include Rust source suffix as specified; snapshot tests assert presence/shape, not specific line numbers.

---

## Lint & Hygiene Agents

### 2.2 `enforce_limits` Agent
- **Script:** `tools/enforce_limits.sh`
- **Purpose:** Enforce **LoC caps** (target ≤400; hard **≤600** lines) and comment policy.
- **Config:** `MAX_RUST_LINES` (default `600`).
- **Run locally:**
  ```sh
  MAX_RUST_LINES=600 bash tools/enforce_limits.sh
  ```

### 2.4 `no_placeholders` Agent
- **Script:** `tools/no_placeholders.sh`
- **Purpose:** Ban `todo!`, `unimplemented!`, `FIXME`, `XXX`, and obvious placeholder panics in Rust sources.
- **Run locally:**
  ```sh
  bash tools/no_placeholders.sh
  ```

---

## 3) Build & Test Agents

### 3.1 `lint` Agent (fmt + clippy)
- **Invoker:** CI job `lint` (see workflow).  
- **Purpose:** Enforce formatting and deny warnings.
- **Run locally:**
  ```sh
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -Dwarnings
  ```

### 3.2 `test-linux` Agent (coverage-gated)
- **Purpose:** Run unit/integration tests and enforce **≥95%** line/block coverage.
- **Run locally (example):**
  ```sh
  rustup component add llvm-tools-preview
  cargo install cargo-llvm-cov
  cargo llvm-cov --workspace --lcov --output-path coverage.lcov --fail-under-lines 95
  ```
- **Artifacts:** `coverage.lcov`

### 3.3 `build-matrix` Agent
- **Purpose:** Release builds for Linux/macOS/Windows (x86_64 + aarch64 as applicable).  
- **Run locally (Linux example):**
  ```sh
  cargo build --release --workspace
  ```

### 3.4 `package-linux` Agent (+ SBOM)
- **Purpose:** Build `.deb`, `.rpm`, and generate CycloneDX SBOM.
- **Run locally (examples):**
  ```sh
  cargo install cargo-deb cargo-rpm
  cargo deb --no-build
  cargo rpm build
  cargo install cyclonedx-bom || true
  cyclonedx-bom -o target/sbom/rsync.cdx.json
  ```

---



