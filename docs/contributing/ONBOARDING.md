# Contributor Onboarding Guide

This guide helps new contributors get productive with oc-rsync quickly.

## Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| Rust | 1.88.0 (pinned in `rust-toolchain.toml`) | Build toolchain |
| cargo-nextest | Latest | Test runner (required - never use `cargo test`) |
| podman | Latest | Container-based interop testing and benchmarks |

Install Rust via [rustup](https://rustup.rs/). The pinned toolchain installs automatically on first build. Install nextest:

```sh
cargo install cargo-nextest --locked
```

## Repository Structure

oc-rsync is a workspace of focused crates. Each crate has a single responsibility:

### Core Pipeline

| Crate | Purpose |
|-------|---------|
| `cli` | CLI parsing (Clap v4), help text, output formatting |
| `core` | Orchestration facade - all transfers route through `core::session()` |
| `engine` | Delta pipeline, block matching, temp-file commit, local-copy executor |
| `protocol` | Wire protocol (v28-32), multiplex MSG_* frames, version negotiation |
| `transport` | SSH stdio passthrough, `rsync://` TCP, timeouts and back-pressure |

### Data Processing

| Crate | Purpose |
|-------|---------|
| `checksums` | Rolling rsum + strong checksums (MD4/MD5/XXH3), SIMD fast paths |
| `signature` | Block signature generation and block-size calculation |
| `matching` | Delta matching algorithm - finds matching blocks between files |
| `filters` | `--filter`, includes/excludes, `.rsync-filter` rule evaluation |
| `compress` | Compression codecs (zlib/zstd) |
| `flist` | File list building, sorting, and flat-list storage |

### Platform and I/O

| Crate | Purpose |
|-------|---------|
| `metadata` | Permissions, uid/gid, timestamps, devices, symlinks, ACLs, xattrs |
| `fast_io` | io_uring, IOCP, reflink ioctls, platform copy dispatch |
| `rsync_io` | Buffered I/O primitives for the rsync wire protocol |
| `platform` | Platform detection and capability queries |
| `apple-fs` | macOS-specific filesystem operations (clonefile, etc.) |

### Infrastructure

| Crate | Purpose |
|-------|---------|
| `daemon` | TCP listener, `@RSYNCD:` negotiation, auth, config, systemd integration |
| `batch` | Batch file read/write for offline transfers |
| `bandwidth` | Rate limiting for `--bwlimit` |
| `logging` | Structured logging, role trailers, verbosity |
| `logging-sink` | Log sink abstraction (separate from logging to break cycles) |
| `branding` | Binary name and version constants |
| `transfer` | Transfer orchestration between sender/receiver/generator |
| `embedding` | Embeddable library interface |
| `test-support` | Shared test utilities (not published) |

### Dependency Graph (simplified)

```
cli -> core -> engine, daemon, transport, logging
               core -> protocol -> checksums, filters, compress, bandwidth -> metadata
```

## Build and Test

### Build the workspace

```sh
cargo build --workspace
```

### Run tests for a specific crate

```sh
cargo nextest run -p <crate> --all-features
```

Filter to a specific test pattern:

```sh
cargo nextest run -p engine --all-features -E 'test(delta)'
```

### Lint checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings
```

### Important rules

- **Never run the full nextest suite locally** - it is reserved for CI.
- **Never use `cargo test`** - always use `cargo nextest run`.
- **Run targeted tests only** - test the crate you changed.

## Key Design Patterns

### Strategy Pattern - Checksums

Interchangeable algorithms selected at runtime via traits:

- `RollingChecksum` trait - Adler32 implementation
- `StrongChecksum` trait - MD4, MD5, XXH3 implementations
- `Compressor` trait - zlib, zstd implementations

### Builder Pattern - Configuration

Complex objects constructed with validation:

- `FileEntryBuilder` - builds file list entries
- `CoreConfig` / `TransferConfigBuilder` - transfer configuration
- `FilterChain` - filter rule construction

### State Machine Pattern - Connection Lifecycle

Explicit states with validated transitions:

- Daemon: `Greeting -> ModuleSelect -> Authenticating -> Transferring -> Closing`
- Transfer: `Handshake -> FilterExchange -> FileListTransfer -> DeltaTransfer -> Finalization -> Complete`

### Chain of Responsibility - Filters

`FilterChain` evaluates rules in order - first match wins. Rules are include/exclude patterns evaluated sequentially against each path.

## How to Add a Feature

1. **Read upstream C source first.** The C code at `target/interop/upstream-src/rsync-3.4.1/` is the only source of truth for protocol behavior. Do not rely on man pages or third-party descriptions.

2. **Create a feature branch:**
   ```sh
   git checkout -b feat/description master
   ```

3. **Implement.** Follow existing patterns. Match upstream semantics exactly. Reference upstream source in comments for non-obvious behavior (e.g., `// upstream: token.c:send_token()`).

4. **Write tests.** Every change needs tests. Aim for >95% line coverage of new code.

5. **Check locally:**
   ```sh
   cargo build --workspace
   cargo nextest run -p <crate> --all-features -E 'test(<pattern>)'
   ```

6. **Push and create PR:**
   ```sh
   git push -u origin feat/description
   gh pr create --title "feat: short description" --body "Summary of changes"
   ```

7. **Wait for CI.** All required checks must pass before merge.

### Commit message format

Use conventional prefixes: `feat:`, `fix:`, `perf:`, `docs:`, `chore:`, `style:`, `test:`, `refactor:`. Keep the first line under 72 characters.

## CI Requirements

All of these must pass before a PR can merge:

| Check | What it validates |
|-------|-------------------|
| fmt+clippy | `cargo fmt --check` and `cargo clippy` with `-D warnings` |
| nextest (stable) | Full workspace test suite on Linux |
| Windows (stable) | Platform-specific compilation and targeted tests |
| macOS (stable) | Platform-specific compilation and targeted tests |
| Linux musl (stable) | Static linking compatibility |

PRs require one approving review (admin can bypass).

## Upstream Rsync Reference

The upstream C source is the authoritative reference for all protocol behavior.

### Fetch it

```sh
mkdir -p target/interop/upstream-src && cd target/interop/upstream-src
curl -L https://download.samba.org/pub/rsync/src/rsync-3.4.1.tar.gz | tar xz
```

Or run the interop harness which downloads all tested versions (3.0.9, 3.1.3, 3.4.1, 3.4.2):

```sh
bash tools/ci/run_interop.sh
```

### Key upstream files

| File | Contains |
|------|----------|
| `main.c` | Entry point, option parsing dispatch |
| `sender.c` | Sender-side transfer logic |
| `receiver.c` | Receiver-side file reconstruction |
| `generator.c` | File list generation, quick-check |
| `match.c` | Block matching algorithm |
| `token.c` | Delta token encoding/decoding |
| `io.c` | Multiplexed I/O, buffering |
| `flist.c` | File list wire format |
| `exclude.c` | Filter rule evaluation |
| `compat.c` | Protocol version negotiation |
| `authenticate.c` | Daemon authentication |
| `clientserver.c` | Daemon connection setup |

## Common Pitfalls

### Quick-check test flakiness

Rsync skips files with matching size + mtime. Tests that create source and destination files within the same second may see no transfer. Fix: backdate destination files using the `filetime` crate or use different file sizes.

### cfg-gate hazards

- If all tests in a module are `#[cfg(unix)]`, gate the entire module to avoid unused-import warnings on Windows.
- Variables mutated only inside `#[cfg(unix)]` blocks cause `unused_mut` on Windows - use `#[allow(unused_mut)]` or restructure.
- Provide no-op stubs for unsupported platforms: `#[cfg(not(target_os = "linux"))]` blocks returning `Ok(None)` or `Ok(())`.

### Platform-feature gates in preflight

Per the WPC-3 defect (PR #5564): a preflight `#[cfg]` gate that limits a feature to `unix` only, when the metadata or transfer backend actually exists for `windows`, silently disables shipped functionality at the CLI boundary. The flag is rejected before the wired backend can run.

**Rule**: preflight gates for platform features must list every platform that has a working backend, AND the Cargo feature must propagate to the backend crate.

```rust
// Correct - matches every platform with a backend:
#[cfg(not(all(any(unix, windows), feature = "xattr")))]
fn reject_xattrs(...) { ... }

// Wrong - silently disables --xattrs on Windows even though
// crates/metadata/src/xattr_windows.rs ships the FindFirstStreamW backend:
#[cfg(not(all(unix, feature = "xattr")))]
fn reject_xattrs(...) { ... }
```

**Checklist before adding or editing a platform-feature gate**:

1. Confirm the backend exists in `crates/metadata/` (or wherever) for every platform you want to allow.
2. Update the preflight `#[cfg]` gate to list each supported platform.
3. Verify `crates/core/Cargo.toml` propagates the feature to the backend crate - e.g., `xattr = ["metadata/xattr", "transfer/xattr"]`, not just `xattr = ["transfer/xattr"]`. The propagation invariant is locked in by `crates/cli/tests/feature_propagation.rs`.
4. Add a parameterized preflight test per PR #5576's pattern (one `#[cfg]`-gated arm per `(platform, feature)` pair, both positive accept and negative reject).

Reference PRs: #5564 (WPC-3 xattrs reality fix), #5576 (parameterized preflight regression matrix), #5585 (metadata long-path wiring).

### Buffer pool test serialization

`BufferPool` uses a global `OnceLock`. Tests that modify pool capacity need `EnvGuard` for isolation. Run pool-related tests with targeted patterns, not as part of the full suite.

### No unsafe in most crates

All crates use `#![deny(unsafe_code)]`. Only `metadata`, `fast_io`, `checksums`, `engine`, and `protocol` may contain unsafe blocks - and only with explicit `#[allow(unsafe_code)]` on specific functions.

### No placeholders

Never use `todo!()`, `unimplemented!()`, `FIXME`, or stub functions. Every change ships production-ready.

## Getting Help

- Read `docs/ARCHITECTURE.md` for the high-level system design.
- Read `docs/PROTOCOL.md` for wire protocol details.
- Read `docs/INTEROP.md` for interop testing approach.
- Check `docs/design/` for detailed design documents on specific subsystems.
