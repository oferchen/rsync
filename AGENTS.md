# AGENTS.md — Roles, Responsibilities, APIs, and Error/Message Conventions

This document defines the internal actors (“agents”), their responsibilities, APIs, invariants, and how user-visible messages (including Rust source file remapping) are produced. All binaries must route user-visible behavior through these agents via the `core` facade.

---

## Global Conventions

- **Error Message Suffix (C→Rust remap)**:  
  Format: `... (code N) at <repo-rel-path>:<line> [<role>=3.4.1-rust]`  
  Implemented in `crates/core/src/message.rs` via:
  - `role: Role` enum (`Sender/Receiver/Generator/Server/Client/Daemon`) chosen at call-site.  
  - `source_path: &'static str = file!()`; `source_line: u32 = line!()`; normalized to repo-relative.  
  - Central constructor: `Message::error(code, text).with_role(role).with_source(file!(), line!())`.
- **Roles in trailers** mirror upstream semantics exactly.  
- **All info/warn/error/progress strings** are centralized in `core::message::strings` for snapshot tests.

---

## Agents Overview

### 1) Client (CLI Frontend)
- **Binary**: `src/bin/oc-rsync.rs`
- **Depends on**: `cli`, `core`, `transport`  
- **Responsibilities**:
  - Parse CLI (Clap v4) and render upstream-parity help/misuse.
  - Build `CoreConfig` via Builder and call `core::run_client()`.
  - Route `--msgs2stderr`, `--out-format`, `--info/--debug` to `logging`.
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



