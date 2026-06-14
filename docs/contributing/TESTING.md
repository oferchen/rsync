# Testing Strategy

This document describes the testing philosophy, per-crate expectations, infrastructure,
and workflows used across the oc-rsync workspace.

## Philosophy

Tests are layered by scope and purpose:

1. **Property tests** - verify algorithmic correctness with randomized inputs (proptest/quickcheck).
2. **Golden byte tests** - pin exact wire format against captured upstream byte sequences.
3. **Interop tests** - run oc-rsync against upstream rsync binaries (3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3).
4. **Integration tests** - exercise end-to-end transfer pipelines using tempfile fixtures.
5. **Fuzz targets** - find parser crashes and logic errors via cargo-fuzz.
6. **SIMD parity tests** - ensure scalar and SIMD implementations produce identical output.

Every change must include tests. The project targets greater than 95% line coverage
(`cargo llvm-cov`), measured locally.

## Per-Crate Expectations

### checksums

- SIMD parity: scalar vs AVX2, SSE2, and NEON must produce identical digests for
  all inputs. See `tests/rolling_simd_parity.rs` and `src/rolling/tests/checksum/simd.rs`.
- Property tests for rolling checksum slide/rotate invariants (`tests/checksum/properties.rs`).
- Upstream compatibility: MD4/MD5 output must match known test vectors and upstream rsync digests.
- Strategy integration: verify runtime checksum selection logic.

### protocol

- Golden byte tests (`tests/golden_*.rs`) pin wire encoding for v28, v29, v32 handshakes,
  file lists, multiplex frames, and delta stats.
- Proptest round-trip for varint codec, delta script encoding, and file entry serialization.
- Fuzz targets: `fuzz_varint`, `fuzz_multiplex_frame`, `fuzz_delta`,
  `fuzz_legacy_greeting`, `file_entry_roundtrip`, `varint_roundtrip`,
  `multiplex_frame`, `negotiation_prologue`.
- Version negotiation tests cover protocol 27 through 32.

### filters

- Property tests: `proptest_rule_evaluation.rs`, `proptest_fuzz.rs`, `precedence_property.rs`
  verify first-match-wins semantics and rule ordering invariants.
- Pattern matching tests cover globs, directory-only patterns, negated rules, include/exclude
  combinations, clear rules, and edge cases.
- Fuzz targets: `fuzz_filter_chain`, `fuzz_filter_parse`.

### engine

- Integration tests with `tempfile::TempDir` fixtures: atomic rename, hardlink handling,
  sparse writes, delta transfer strategies, pipeline reorder.
- Parallel applier stress tests (`parallel_apply_dg3_stress.rs`,
  `parallel_apply_concurrent.rs`).
- Delete determinism property tests.
- Spill/reorder buffer integration (`spill_env_e2e.rs`, `pipeline_reorder_integration.rs`).

### daemon

- Config parsing unit tests for `oc-rsyncd.conf` directives.
- Connection lifecycle FSM tests.
- Concurrency stress tests (`connection_scaling_stress.rs`).
- Interop validation via `tools/ci/run_interop.sh` (daemon push/pull against upstream).

### fast\_io

- Platform-specific tests gated by `#[cfg(target_os = "linux")]` for io\_uring and
  `#[cfg(windows)]` for IOCP.
- io\_uring: probe fallback, linkat, send\_zc, mmap pressure, byte-identical output,
  data read round-trip.
- IOCP: high-concurrency stress, disk-full simulation.
- Tests degrade gracefully when the required kernel or platform is unavailable.

### metadata

- Permission and ownership round-trip tests (`uidgid_integration.rs`).
- Timestamp edge cases including Y2038 (`timestamp_2038.rs`).
- ACL handling on POSIX and Windows (`acl_handling.rs`, `acl_windows/tests/`).
- Cross-platform ACL round-trip (`windows_to_linux_acl_roundtrip.rs`).

### core, cli, transfer, bandwidth, compress, signature, flist, matching

- Unit and integration tests covering their respective concerns.
- All must compile and pass on Linux, macOS, and Windows.

## Test Infrastructure

### Temporary directories

Use `tempfile::TempDir` for all filesystem fixtures. The `test-support` crate provides
`create_tempdir()` with retry logic for Windows CI antivirus contention.

### Environment isolation

Tests that modify environment variables must use the `EnvGuard` pattern to restore
state on drop. The CI script `tools/ci/check_envguard.sh` lints for unguarded env
mutations.

### setup\_test\_dirs pattern

Many integration tests share a `setup_test_dirs()` helper that creates a source
directory with representative file trees (regular files, symlinks, directories,
devices) inside a `TempDir`.

### Quick-check pitfall

Rsync skips files with matching size and mtime. Tests must either backdate
destination files (using the `filetime` crate) or use different file sizes to
avoid false "no transfer" results.

## Running Tests Locally

Use cargo-nextest for all test execution. Never use `cargo test`.

```sh
# Run tests for a single crate
cargo nextest run -p <crate> --all-features

# Filter to specific test names
cargo nextest run -p <crate> --all-features -E 'test(<pattern>)'

# Run ignored tests (e.g. SSH integration)
cargo nextest run -p <crate> --all-features --run-ignored ignored-only
```

Do not run the full workspace test suite locally - that is reserved for CI. Run
only the crates you changed.

## CI Matrix

| Job | Platform | Scope | Required |
|-----|----------|-------|----------|
| nextest (stable) | Linux | Full workspace, all features | Yes |
| nextest (beta/nightly) | Linux | Full workspace, all features | No (informational) |
| Windows test | Windows | core, engine, cli | Yes |
| Windows IOCP | Windows | fast\_io, engine | Yes |
| Windows metadata | Windows | metadata, ACL round-trip | Yes |
| macOS test | macOS | core, engine, cli, metadata, apple-fs | Yes |
| Linux musl | Linux | Full workspace, no default features | Yes |
| interop | Linux | Upstream rsync daemon/SSH scenarios | Yes |
| interop (macOS) | macOS | Homebrew rsync smoke | Yes |
| interop (Windows) | Windows | MSYS2 rsync smoke | No (best-effort) |

Branch protection requires: fmt+clippy, nextest (stable), Windows (stable),
macOS (stable), Linux musl (stable).

### Nextest configuration

`.config/nextest.toml` defines:
- SSH tests run serially (`max-threads = 1`) to avoid pipe corruption.
- Daemon and sparse tests get 2 retries for TOCTOU and timing races.
- Buffer pool tests are serialized due to global `OnceLock` state.
- CI profile sets a 120-second per-test timeout with terminate-after-3.

## Interop Testing

The interop harness lives at `tools/ci/run_interop.sh`. It:

1. Downloads and builds upstream rsync for versions 3.0.9, 3.1.3, 3.4.1, 3.4.2, and 3.4.3.
2. Starts an oc-rsync daemon on a non-privileged port.
3. Runs push and pull scenarios (initial sync, delta update, checksums, compression,
   hardlinks, delete modes, numeric-ids, excludes, inplace, batch) against each
   upstream version.
4. Validates exit codes, transferred file content, and error messages.

A lighter `run_interop_smoke.sh` runs portable scenarios on macOS and Windows where
full daemon testing is not available.

To run locally (Linux only):

```sh
# Build oc-rsync first
cargo build

# Run full interop suite (downloads upstream versions on first run)
bash tools/ci/run_interop.sh
```

## Fuzz Testing

Fuzz targets use `cargo-fuzz` (libFuzzer). Three crates have fuzz directories:

- **Workspace-level** (`fuzz/fuzz_targets/`) - 24 targets covering protocol wire formats,
  file list decoding, daemon greeting, checksums, filters, compression, and differential
  testing against upstream behavior.
- **protocol** (`crates/protocol/fuzz/fuzz_targets/`) - 8 targets for varint, multiplex
  frames, delta scripts, file entries, and greeting parsing.
- **filters** (`crates/filters/fuzz/fuzz_targets/`) - 2 targets for filter chain
  evaluation and rule parsing.

### Running a fuzz target

```sh
# Install cargo-fuzz
cargo install cargo-fuzz

# Run a workspace-level target
cargo fuzz run <target_name>

# Run a crate-specific target
cd crates/protocol && cargo fuzz run fuzz_varint
```

### Adding a new fuzz target

1. Create `fuzz/fuzz_targets/<name>.rs` (or in the relevant crate's `fuzz/` directory).
2. Add the target to the crate's `fuzz/Cargo.toml` under `[[bin]]`.
3. The target function receives `&[u8]` and should exercise parsing/decoding logic
   without panicking on arbitrary input.
4. Run locally to confirm it starts and does not immediately crash on the empty corpus.

### Overnight fuzz workflows

CI runs `differential-fuzzer-overnight.yml` and `filter-fuzzer-overnight.yml` on a
schedule. The triage script `tools/ci/triage_fuzz_artifact.sh` helps reduce crash
artifacts to minimal reproducers.
