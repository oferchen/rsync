# Unsafe Block SAFETY Comment Audit

## Scope

Workspace-wide audit of `unsafe { ... }` expression blocks for SAFETY comments
and conformance with the unsafe-code policy declared in `CLAUDE.md`.

Methodology:

1. Enumerate every `unsafe { ... }` block (not `unsafe fn`, `unsafe trait`, or
   `unsafe impl`) under `crates/`.
2. Skip occurrences inside doc-comment fragments (`///`, `//!`) - those are
   example bodies, not real compilation units.
3. For each block, scan up to 15 non-blank lines above for a SAFETY justification
   (case-insensitive `// SAFETY:`, `// Safety:`, `// safety:`). The scan
   tolerates intermediate `if`/`else`/`match` arms so a single SAFETY note can
   cover several `unsafe { ... }` arms of the same control-flow construct.
4. Bucket each violation as either `missing` (no comment found) or
   `placeholder` (TODO/FIXME/empty body).
5. Cross-reference each crate against the permitted-unsafe list in
   `CLAUDE.md`.

The audit script is checked in at `tools/audit/unsafe_safety_comment_audit.py`.

## Permitted vs. forbidden crates

`CLAUDE.md` lists the crates that may host unsafe code (directly or via
`#[allow(unsafe_code)]` on specific functions):

| Status         | Crates                                                |
|----------------|-------------------------------------------------------|
| Permitted      | `fast_io`, `metadata`, `checksums`, `engine`, `protocol` |
| Forbidden      | every other workspace crate                          |

The long-term direction calls for consolidating all unsafe code into `fast_io`
and exposing safe public APIs from the other permitted crates.

## Workspace totals

After the quick-win fixes shipped with this audit, the workspace contains
**571 `unsafe { ... }` blocks** (down from 572 once a stale `//!` doctest
fragment was excluded from the scan):

| Crate            | Blocks | Files | Permitted? | Notes                          |
|------------------|-------:|------:|------------|--------------------------------|
| `fast_io`        |    314 |    55 | yes        | io_uring, IOCP, sendfile, splice, mmap, syscall batch |
| `metadata`       |     66 |     6 | yes        | POSIX id lookups, timestamps, ACL/xattr stubs |
| `platform`       |     56 |     7 | no         | Windows console / service / privilege APIs and POSIX env helpers |
| `checksums`      |     54 |    14 | yes        | SIMD rolling hash and MD4/MD5 batched lanes |
| `engine`         |     29 |    11 | yes        | buffer pool atomics, deferred sync, clonefile, ACL tests |
| `windows-gnu-eh` |     13 |     1 | no         | LoadLibrary/GetProcAddress shim for `__register_frame_info` |
| `core`           |     12 |     4 | no         | signal handler installation, integration test scaffolding |
| `cli`            |     11 |     3 | no         | env-guard helpers for clap integration tests |
| `flist`          |      8 |     2 | no         | `fstatat` + `statx` syscall wrappers used during batched stat |
| `daemon`         |      3 |     2 | no         | doctest example + `getrusage` stress test |
| `embedding`      |      3 |     1 | no         | env-guard helpers for in-process embed tests |
| `branding`       |      2 |     1 | no         | env-guard helpers for brand-detection tests |
| `protocol`       |      1 |     1 | yes        | `Vec::set_len` in `read_payload_into` (already documented) |

## Policy violations (forbidden crates that contain unsafe)

The following crates host `unsafe { ... }` blocks despite being outside the
`CLAUDE.md` allowlist. Each one is a candidate either for migration into a
permitted crate, replacement with a safe wrapper crate, or a documented
exception in `CLAUDE.md` (preferred for tiny POSIX shims that exist purely to
serialise environment-variable mutations during tests).

| Crate            | Blocks | Recommendation |
|------------------|-------:|----------------|
| `platform`       |     56 | Migrate Windows console/service/privilege shims into `fast_io` and gate them through safe wrappers (`ctrlc`, `windows-rs`). Env-guard helpers can stay if `CLAUDE.md` is updated to list `platform` as the canonical home for POSIX env serialisation. |
| `windows-gnu-eh` |     13 | Compile-time fallback for `*-windows-gnu` only; gate the entire crate behind `#[cfg(all(target_os = "windows", target_env = "gnu"))]` and document an explicit exception in `CLAUDE.md`. The shim cannot be replaced by a safe wrapper because it patches the gnu personality routine. |
| `core`           |     12 | Move the SIGWINCH handler in `signal/unix.rs` into `platform` (POSIX side) or directly into `fast_io`. Test-only unsafe (`module_list_auth`, `client_integration`) should be moved into a shared helper crate already on the permitted list. |
| `cli`            |     11 | Test-only env-guard helpers. Either re-use the helper that already lives in `platform::env::EnvGuard` or add `cli` test modules to the `CLAUDE.md` exception list. |
| `flist`          |      8 | Wrap `statx`/`fstatat` syscalls behind a safe API exposed from `fast_io::syscall_batch` (`fast_io` already exposes statx helpers in `io_uring/statx.rs`). The current direct `libc::syscall` calls duplicate functionality. |
| `daemon`         |      3 | The lone production block is in a doctest example (`//! # unsafe { ... }`) which is auto-excluded from this audit; the remaining two live in the connection-scaling stress test and should be migrated to a shared test helper. |
| `embedding`      |      3 | Replace with `platform::env::EnvGuard`. |
| `branding`       |      2 | Replace with `platform::env::EnvGuard`. (Fixed in this PR; SAFETY comments added.) |

## Missing SAFETY comments

`CLAUDE.md` requires every `unsafe { ... }` block to be preceded by a SAFETY
comment explaining the invariants the caller relies on. After the quick-win
fixes in this PR plus the `fast_io` follow-up the violation count dropped from
**356** down to **52**.

| Crate            | Missing (before) | Missing (after) | Notes |
|------------------|-----------------:|----------------:|-------|
| `branding`       | 2                | 0               | Fixed: env-guard helpers in `tests.rs`. |
| `cli`            | 8                | 0               | Fixed: env-guard helpers in `frontend/arguments/env.rs` and `frontend/tests/common.rs`. |
| `core`           | 12               | 8               | Fixed: `client/config/compress_env.rs` env-guard helpers. Remaining: `tests/client_integration.rs` macOS `getattrlist`, `signal/unix.rs`, `client/tests/module_list_auth.rs` libc helpers. |
| `embedding`      | 3                | 0               | Fixed: env-guard helpers in `lib.rs`. |
| `engine`         | 21               | 0               | Fixed: env-guard helpers in `local_copy/executor/file/partial.rs`, `local_copy/tests/partial_transfers.rs`, `local_copy/prefetch.rs`, `local_copy/tests/execute_sparse.rs`, `local_copy/tests/mod.rs` (already had `Safety:` lower-case comments now recognised by the scanner). |
| `flist`          | 8                | 0               | Fixed: `batched_stat/dir_stat.rs` and `batched_stat/statx_support.rs` (`fstatat`/`statx` syscalls + zeroed POD buffers). |
| `metadata`       | 16               | 0               | Fixed: `permission_tests.rs` (`umask`), `copy_as.rs` (`geteuid`/`getegid`), `apply/timestamps.rs` (zeroed `attrlist`). |
| `platform`       | 14               | 0               | Fixed: `signal.rs` (`SetConsoleCtrlHandler`), `env.rs` (test-only env mutations under `TEST_LOCK`). |
| `fast_io`        | 222              | 0               | Fixed: io_uring SQE submission helpers (`batching.rs`, `file_reader.rs`, `file_writer.rs`, `socket_reader.rs`), buffer-ring pointer arithmetic (`buffer_ring.rs`), POD statx buffers (`statx.rs`), `uname` call (`config.rs`), registered-buffer slice helpers, fd-based socket adapters, and the splice/sendfile test scaffolding. The 13 ring-buffer blocks in `buffer_ring.rs` now carry full per-block invariants documenting the kernel-shared mmap region. |
| `checksums`      | 31               | 31              | Outstanding. All 31 are test calls to `unsafe fn digest_xN(&inputs)` and `unsafe fn accumulate_chunk_*`. Recommendation: add a single SAFETY block per test function referencing the relevant `target_feature` precondition (`SSE2` is x86_64 baseline, NEON is mandatory on aarch64, AVX2/AVX-512/SSSE3/SSE4.1 are runtime-detected before the test runs). Mechanical follow-up. |
| `windows-gnu-eh` | 13               | 13              | Outstanding. The crate's documentation covers the load-and-cache pattern but individual blocks lack SAFETY notes. Recommendation: add a SAFETY note at the top of each `extern "system"` resolver explaining (1) module-handle lifetime semantics, (2) function-pointer transmute conditions, and (3) thread-safety of the `OnceLock` cache. |

After this follow-up: **571 unsafe blocks, 52 still missing SAFETY
comments** (-304, -85.4% from the original 356). Eight of the eleven listed
crates now have zero outstanding violations.

## Fixes applied in this PR

The following files were updated with SAFETY justifications:

- `crates/branding/src/branding/tests.rs`
- `crates/cli/src/frontend/arguments/env.rs`
- `crates/cli/src/frontend/tests/common.rs`
- `crates/core/src/client/config/compress_env.rs`
- `crates/embedding/src/lib.rs`
- `crates/engine/src/local_copy/executor/file/partial.rs`
- `crates/engine/src/local_copy/prefetch.rs`
- `crates/engine/src/local_copy/tests/execute_sparse.rs`
- `crates/engine/src/local_copy/tests/partial_transfers.rs`
- `crates/fast_io/src/io_uring/batching.rs`
- `crates/fast_io/src/io_uring/buffer_ring.rs`
- `crates/fast_io/src/io_uring/config.rs`
- `crates/fast_io/src/io_uring/file_reader.rs`
- `crates/fast_io/src/io_uring/file_writer.rs`
- `crates/fast_io/src/io_uring/registered_buffers/registry.rs`
- `crates/fast_io/src/io_uring/registered_buffers/tests/registry.rs`
- `crates/fast_io/src/io_uring/socket_factory.rs`
- `crates/fast_io/src/io_uring/socket_reader.rs`
- `crates/fast_io/src/io_uring/statx.rs`
- `crates/fast_io/src/io_uring/tests.rs`
- `crates/fast_io/src/io_uring_stub/socket_factory.rs`
- `crates/fast_io/src/io_uring_stub/tests.rs`
- `crates/fast_io/src/sendfile.rs`
- `crates/fast_io/src/splice.rs`
- `crates/fast_io/tests/io_uring_mmap_pressure.rs`
- `crates/fast_io/tests/splice_integration.rs`
- `crates/flist/src/batched_stat/dir_stat.rs`
- `crates/flist/src/batched_stat/statx_support.rs`
- `crates/metadata/src/apply/timestamps.rs`
- `crates/metadata/src/copy_as.rs`
- `crates/metadata/src/permission_tests.rs`
- `crates/platform/src/env.rs`
- `crates/platform/src/signal.rs`

Common patterns documented:

- **Env-guard helpers** (10 sites). `std::env::{set_var, remove_var}` became
  `unsafe` in Rust 2024 because POSIX `getenv`/`setenv` are not thread-safe.
  Every fixed site now cites the `ENV_MUTEX` (or nextest serial slot) that
  serialises the mutation and therefore restores the soundness invariant.
- **POD `mem::zeroed` syscall buffers** (`libc::stat`, `libc::statx`,
  `libc::attrlist`). Cited as POD `repr(C)` structures whose all-zero pattern
  is a valid initial state, followed by kernel-side population.
- **`umask`, `geteuid`, `getegid`, `lseek`** test calls. Documented as either
  pure accessors with no inputs or thread-safety preconditions backed by the
  test framework's serial slot.
- **io_uring SQE submission** (`batching.rs`, `file_reader.rs`,
  `file_writer.rs`, `socket_reader.rs`). Each block cites that the entry
  references a buffer and fd that outlive `submit_and_wait`, so the kernel
  retains a valid view until completion.
- **io_uring buffer-ring pointer arithmetic** (`buffer_ring.rs`). 13 blocks
  manipulate the kernel-shared mmap region. Each cites the bounds proof for
  the offset (entry index < ring_size, buffer offset < arena size) and the
  alignment guarantee (page-aligned mmap + multiple-of-2 entry size).
- **fd-based socket adapters** (`socket_factory.rs`). Document that the
  caller owns the fd's lifetime and the buffer matches `read(2)`/`write(2)`'s
  ABI.
- **Splice/sendfile test scaffolding** (~120 sites). All matching the pattern
  "fd opened by `pipe`/`socketpair`, closed exactly once, buffer satisfies
  syscall ABI."

## Follow-up tasks

1. **checksums SIMD tests** (31 blocks). Add per-test-function SAFETY blocks
   citing the relevant target-feature gate.
2. **windows-gnu-eh resolver chain** (13 blocks). Document the
   `OnceLock`-cached `GetProcAddress` lifecycle once at the top of the
   resolver helpers.
3. **core / cli / daemon residue** (8 blocks). Migrate test scaffolding into a
   shared helper crate already on the permitted list, or annotate in place.
4. **Policy follow-up**: either migrate the unsafe code in `platform`,
   `windows-gnu-eh`, `core`, `cli`, `flist`, `daemon`, `embedding`, and
   `branding` into the permitted crates, or extend the `CLAUDE.md` allowlist
   with explicit, narrow exceptions.

## Reproducing the audit

```sh
python3 tools/audit/unsafe_safety_comment_audit.py
```

The script prints per-crate block counts, total counts, and a violations table
grouped by crate. Run after edits to confirm the violation count is dropping.
