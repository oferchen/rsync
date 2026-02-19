# CLAUDE.md

## Project

oc-rsync — Rust reimplementation of rsync, wire-compatible with upstream 3.4.1 (protocol 32). Binary: `oc-rsync`. Rust 1.88.0 (pinned in rust-toolchain.toml). Use model: Opus 4.6 (`claude-opus-4-6`).

## Git Workflow

Master is protected. Never push directly to master.

1. Create a feature branch: `git checkout -b feature/description`
2. Push the branch: `git push -u origin feature/description`
3. Create a PR: `gh pr create`
4. CI must pass before merge. Required checks: fmt+clippy, nextest (stable), Windows (stable), macOS (stable), Linux musl (stable).
5. PRs require 1 approving review (admin can bypass).
6. Merge via GitHub (`gh pr merge`).

Commit messages use conventional prefixes: `feat:`, `fix:`, `perf:`, `docs:`, `chore:`, `style:`, `test:`, `refactor:`. Keep the first line under 72 characters.

PR titles MUST use the same conventional prefix. A labeler workflow auto-applies GitHub labels based on the PR title prefix for release note categorization:

| Prefix | Label | Release Category |
|--------|-------|-----------------|
| `feat:` | `enhancement` | Features |
| `perf:` | `performance` | Performance |
| `fix:` | `bug` | Bug Fixes |
| `ci:` | `ci` | CI/CD |
| `docs:` | `documentation` | Documentation |
| `chore:` | `chore` | Other Changes |
| `style:` | `style` | Other Changes |
| `test:` | `test` | Other Changes |
| `refactor:` | `refactor` | Other Changes |

## Build & Test

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --no-deps -D warnings
cargo nextest run --workspace --all-features
```

Requires `cargo-nextest`. Configuration in `.config/nextest.toml`.

## Cross-Platform

Code must compile and pass tests on Linux, macOS, and Windows.

- Use `#[cfg(unix)]` / `#[cfg(windows)]` for platform-specific code.
- Watch for unused imports/variables behind `#[cfg()]` gates on other platforms.
- Use no-op stubs for unsupported platforms/features: `#[cfg(not(target_os = "linux"))]` blocks returning `Ok(None)` or `Ok(())`. Same pattern for feature gates: `#[cfg(not(feature = "zstd"))]` returns a graceful fallback or error, never silently omits functionality.
- Test flakiness from rsync's quick-check (same mtime+size = skip): backdate destination files or use different file sizes in tests.

## Code Quality

- **Standard library first.** Prefer `std` over external crates unless there's a documented, significant advantage (e.g., `crossbeam-channel` over `std::sync::mpsc` for measurably lower syscall overhead).
- **No placeholders.** No `todo!`, `unimplemented!`, `FIXME`, `XXX`, or stub functions. Every change ships production-ready.
- **No deprecated APIs.** Migrate promptly when dependencies deprecate functionality.
- **Efficiency.** Optimize hot paths, avoid unnecessary allocations, use appropriate data structures.
- **Modularity.** Each function and module does one thing well. Keep functions short.
- **Elegance.** Prefer clear, expressive code. Remove dead code. Simplify complex logic.
- **Comment hygiene.** Comments explain WHY, never WHAT — the code already shows what it does.
  - Use `///` rustdoc on all public types, traits, functions, and methods. Describe purpose, invariants, and edge cases.
  - Delete restatement comments that echo the code (e.g., `// increment counter` above `counter += 1`).
  - Delete outdated comments that no longer reflect the code.
  - Delete debug checkpoint comments (`// DEBUG`, `// TEMP`, `// XXX`).
  - Reference upstream C source when explaining non-obvious behaviour (e.g., `// upstream: token.c:send_token()`).
  - Internal (`//`) comments are for tricky logic, safety invariants, or upstream protocol quirks — not narrating obvious control flow.
- **No TODO/FIXME in production code.** Use issue tracking instead.

Enforced by `tools/enforce_limits.sh` (LoC caps, comment ratios) and `tools/no_placeholders.sh`.

## Design Patterns

Apply these patterns consistently across the codebase:

- **Strategy Pattern** — Interchangeable algorithms at runtime. Used for checksum selection (Adler32/MD4/MD5/XXH3), compression codecs (zlib/zstd), and protocol wire encoding (legacy 4-byte LE vs modern varint).
- **Builder Pattern** — Complex object construction with validation. Used for `FileEntryBuilder`, `CoreConfig`, `TransferConfigBuilder`, `FilterChain`.
- **State Machine Pattern** — Explicit connection and transfer lifecycle states with validated transitions. Used for daemon connections (`Greeting → ModuleSelect → Authenticating → Transferring → Closing`) and transfer phases (`Handshake → FilterExchange → FileListTransfer → DeltaTransfer → Finalization → Complete`).
- **Chain of Responsibility** — Filter rule evaluation: `FilterChain` evaluates rules in order, first match wins.
- **Dependency Inversion** — Traits define interfaces, implementations are swappable. High-level modules depend on abstractions (`RollingChecksum`, `StrongChecksum`, `Compressor` traits), not concrete types.
- **Single Responsibility** — Each crate handles one concern (checksums, protocol, filters, engine, transport). No monolithic structs mixing concerns.

## Architecture

```
cli → core → engine, daemon, transport, logging
              core → protocol → checksums, filters, compress, bandwidth → metadata
```

- **cli**: CLI parsing (Clap v4), help, output formatting.
- **core**: Orchestration facade. All transfers use `core::session()` and `CoreConfig`. Both CLI and daemon go through here. No `unwrap`/`expect` on fallible paths.
- **engine**: Delta pipeline, rolling+strong checksum scheduling, block-match, temp-file commit, local-copy executor with sparse support. Buffer reuse, vectored I/O.
- **protocol**: Wire protocol (v28–32), multiplex `MSG_*` frames, version negotiation. Golden byte tests for wire format.
- **daemon**: TCP listener, `@RSYNCD:` negotiation, auth, `oc-rsyncd.conf`, systemd integration. Mode of `oc-rsync`, not a separate binary.
- **transport**: SSH stdio passthrough, `rsync://` TCP, timeouts/back-pressure.
- **checksums**: Rolling `rsum` + strong checksums (MD4/MD5/XXH3). SIMD fast paths (AVX2, SSE2, NEON) with scalar fallbacks and parity tests.
- **filters**: `--filter`, includes/excludes, `.rsync-filter`. Property tests and snapshot goldens.
- **metadata**: perms/uid/gid, ns-mtime, devices/FIFOs/symlinks, ACLs (`-A`), xattrs (`-X`).

## Module Decomposition

Large modules are split into focused submodules. New code must extend existing submodules, never recreate monolithic files:

- `crates/engine/src/local_copy/executor/file/` — copy, links, transfer submodules.
- `crates/cli/src/frontend/execution/drive/` — options, config, filters, summary, metadata.
- `crates/bandwidth/src/limiter/` — change, core, sleep.
- `crates/engine/src/local_copy/dir_merge/parse/` — types, line, merge, modifiers.
- `xtask/src/commands/docs/` and `xtask/src/commands/package/` — split by concern.

## Error Handling

- Use `thiserror` for error type derivation.
- Map all errors to upstream-compatible exit codes (1:1 with upstream rsync).
- Include path context in I/O errors via extension trait.
- Role trailers (`[sender]`, `[receiver]`, `[generator]`, `[server]`, `[client]`, `[daemon]`) mirror upstream semantics.
- Error message format: `... (code N) at <repo-rel-path>:<line> [<role>=<version>]`

## Performance

- SIMD-accelerated hot paths with runtime feature detection cached via `OnceLock`:
  - `x86`/`x86_64`: AVX2 and SSE2.
  - `aarch64`: NEON.
  - Other architectures: scalar fallback only.
  - SIMD and scalar implementations must stay in lockstep; parity tests are mandatory.
- Buffer pool (`BufferPool` with RAII `PooledBuffer`) to reduce allocations.
- Memory-mapped I/O for large files.
- Sparse writing with 16-byte `u128` zero-run detection, single seek-per-zero-run invariant.
- `delta/script.rs::apply_delta` caches basis offset for sequential `COPY` tokens.
- Lock-free SPSC channel (`pipeline/spsc.rs`) for network→disk pipeline: zero syscalls, pure userspace spin-wait on `crossbeam_queue::ArrayQueue`.
- SSH capability string `-e.LsfxCIvu` in remote invocation enables checksum negotiation (XXH3/XXH128 instead of MD5). Without it, SSH transfers fall back to software MD5 (~34% CPU on aarch64).

## Testing

- **Property tests** for algorithmic correctness (rolling checksum, SIMD vs scalar parity).
- **Golden byte tests** for wire format compatibility in `crates/protocol/tests/golden/`.
- **Interop tests** against upstream rsync 3.0.9, 3.1.3, 3.4.1.
- **Environment isolation** via `EnvGuard` for tests that modify environment variables.
- Test fixtures use `tempfile::TempDir` with `setup_test_dirs()` pattern.

### Success Metrics

| Metric | Target |
|--------|--------|
| Test coverage | > 80% line coverage (`cargo llvm-cov`) |
| Interop success rate | 100% with supported versions |
| Performance vs upstream C | Faster or within 5% across all modes (local 3x+, daemon 2x+, SSH on par) |
| Memory overhead | < 10% vs upstream (peak RSS) |
| API documentation | 100% public items documented |

## Upstream Rsync as Source of Truth

All implementations must mirror upstream rsync behaviour. The upstream C source code is the ONLY authoritative reference for protocol behaviour, wire formats, and algorithmic details. **Code over documentation. Code over memory.**

- **Read the C source first.** Before implementing any protocol feature, capability flag, or wire format, read the corresponding upstream C code. Do not rely on man pages, blog posts, or third-party descriptions — they are frequently incomplete or wrong.
- **Match upstream semantics exactly.** If upstream sends `-e.LsfxCIvu` in server args, we send `-e.LsfxCIvu`. If upstream uses MD5 as fallback when checksum negotiation fails, we do the same. Deviations cause silent incompatibilities.
- **Verify against upstream behaviour.** When in doubt, run upstream rsync with `strace`/`dtruss` or `-vvv` and compare wire output byte-for-byte.
- **Reference upstream source in comments.** When code implements non-obvious upstream behaviour, cite the file and function (e.g., `// upstream: compat.c:720 set_allow_inc_recurse()`).

Local upstream source: `target/interop/upstream-src/rsync-3.4.1/`. If missing, fetch it:

```sh
mkdir -p target/interop/upstream-src && cd target/interop/upstream-src
curl -L https://download.samba.org/pub/rsync/src/rsync-3.4.1.tar.gz | tar xz
```

Or run the full interop harness which downloads all tested versions (3.0.9, 3.1.3, 3.4.1):

```sh
bash tools/ci/run_interop.sh
```

## Containers (Podman)

Container runtime is **podman** (not docker). Two containers are used for Linux benchmarking and profiling:

- **`localhost/oc-rsync-bench:latest`** — Arch Linux (`base-devel`) benchmark image (9 GB).
  - Rust toolchain, oc-rsync source at `/build/oc-rsync`, upstream rsync 3.4.1 built from source, oc-rsync v0.5.4 pre-built.
  - Embedded `run_benchmark.py` at `/usr/local/bin/`. Configurable via `BENCH_RUNS` env var.
  - User `dev` with sudo. Workdir `/build/oc-rsync`.
- **`rsync-profile`** — Long-running container based on `rust:latest` (Debian).
  - Workspace bind-mount: `/Users/ofer/devel/rsync:/workspace`.
  - Has upstream `rsync 3.4.1` and `oc-rsync-dev` binary.
  - Started with `podman run --name rsync-profile -v ... rust:latest sleep infinity`.
  - Exec into it: `podman exec -it rsync-profile bash`.

Benchmark scripts: `scripts/benchmark.sh`, `scripts/benchmark_hyperfine.sh`, `scripts/benchmark_remote.sh`.
Interop daemon harness: `scripts/rsync-interop-server.sh` + `tools/ci/run_interop.sh`.

## Release Process

1. Create release branch: `git checkout -b release/vX.Y.Z`
2. Bump version in workspace `Cargo.toml` (`version` and `workspace.metadata.oc_rsync.rust_version`).
3. Update README.md version string.
4. Commit, push, create PR, wait for CI, merge.
5. Tag on master: `git tag vX.Y.Z && git push origin vX.Y.Z`
6. Create GitHub release using `.github/RELEASE_TEMPLATE.md` as the body template.
7. CI automatically: builds artifacts (stable/beta/nightly), Docker image, Homebrew formula PR, benchmark chart PNG (uploaded as release asset), and appends benchmark results to release notes.

Release notes template: `.github/RELEASE_TEMPLATE.md`. Auto-categorization config: `.github/release.yml`.

## Known Pitfalls

Lessons learned from development history:

### Git & Release
- **Ambiguous refs:** If a tag and branch share a name (e.g., `master`), `git push` is ambiguous. Use `refs/heads/master` or `refs/tags/v0.5.5` explicitly.
- **Benchmark appends to release body:** The benchmark workflow (`benchmark.yml`) appends results to the release body on every tag push. Multiple pushes = duplicate sections. To get a clean release: `gh release delete vX.Y.Z`, recreate, then let CI run once.
- **Force push with branch protection:** Branch protection allows force push (needed for retagging). But be careful: force-pushing tags requires `git push --force origin vX.Y.Z`.

### Cross-Platform Compilation
- **`#[cfg(unix)]` test modules:** If all tests in a module are `#[cfg(unix)]`, gate the entire module with `#[cfg(unix)]` to avoid unused import warnings on Windows. Don't just gate individual tests.
- **Unused `mut` on Windows:** Variables that are only mutated inside `#[cfg(unix)]` blocks cause `unused_mut` warnings on Windows. Use `let _ = &var;` or `#[allow(unused_mut)]`.
- **Rustdoc links on re-exports:** Module doc `[`TypeName`]` links don't resolve when the type is re-exported through `lib.rs`. Use backtick-only `` `TypeName` `` instead of doc links.

### Test Flakiness
- **Quick-check skips transfers:** Rsync's quick-check algorithm skips files with matching size + mtime. Tests that create source and destination files within the same second may see no transfer. Fix: backdate destination files (`filetime` crate) or use different file sizes.
- **Sparse test sensitivity:** Sparse file tests depend on filesystem block allocation. Use `du --apparent-size` vs `du` comparisons, not absolute sizes.

### SSH Process Management
- **Zombie prevention:** `SshChildHandle` from `SshConnection::split()` has a `Drop` impl that reaps the child process. Callers should still explicitly call `.wait()`, but Drop prevents zombies on early return or panic.
- **Exit code propagation:** SSH child exit codes must be mapped to rsync `ExitCode` values via `map_child_exit_status()`. Exit 127 = command not found, 255 = connection failed, signal death = killed. Worst (highest) exit code wins when both transfer and child fail.

### Performance
- **`--direct-write`:** Bypasses temp-file creation and rename on initial copies. Only safe when destination doesn't exist yet.
- **io_uring:** Available on Linux 5.6+ with automatic fallback to standard I/O. The `fast_io` crate handles detection.
- **Buffer pool contention:** `BufferPool` uses `Mutex<Vec<Vec<u8>>>`. Under high concurrency, consider per-thread pools or lock-free alternatives if this becomes a bottleneck.
- **Sender syscall overhead:** Rust's `read_to_end()` on `File` internally calls `fstat` + `stream_position` + EOF probe, adding 3 extra syscalls per file vs `read_exact()` with a known size. Per-file `writer.flush()` in the transfer loop defeats buffer batching, causing 1 `sendto` per file instead of upstream's ~10-files-per-write pattern. Both were fixed to match upstream rsync's syscall profile (see `generator.rs`).

### Containers & Bind Mounts
- **CRITICAL: Never use `rm -rf` with variable expansion inside containers that have host bind mounts.** A shell quoting error in a benchmark `--setup` command caused `rm -rf` to wipe the host-mounted workspace (`/workspace` → `/Users/ofer/devel/rsync`), destroying the entire local repository including uncommitted work and `.git`. Recovery was only possible because the code was pushed to GitHub. **Mitigations:** (1) Always use a dedicated, non-bind-mounted directory for destructible benchmark data. (2) Never pass `rm -rf` through multiple layers of shell quoting (heredocs, subshells, `podman exec`). (3) Write benchmark cleanup as a script file, not inline commands. (4) Commit all work before running benchmarks in containers with bind mounts.

## Further Details

See AGENTS.md for full agent roles, API references with code examples, crate dependency graph, external dependency table, implementation roadmap, and decision checklists.
