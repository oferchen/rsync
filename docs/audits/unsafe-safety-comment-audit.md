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

## Workspace summary

Latest re-run of `tools/audit/unsafe_safety_comment_audit.py` reports
**589 `unsafe { ... }` blocks** across the workspace. **All 589 blocks now
carry a SAFETY comment**; **0 remain missing one** for a workspace coverage
of **100.00%** (up from 96.43% in the previous audit cycle).

| Metric                                  | Cycle 1 (571) | Cycle 2 (589) | Cycle 3 (589) |
|-----------------------------------------|--------------:|--------------:|--------------:|
| Total `unsafe { ... }` blocks           |           571 |           589 |           589 |
| Blocks with a SAFETY comment            |           519 |           568 |           589 |
| Blocks missing a SAFETY comment         |            52 |            21 |             0 |
| Workspace coverage (with SAFETY)        |         90.9% |        96.43% |       100.00% |

| Crate            | Blocks | Files | Permitted? | Notes                          |
|------------------|-------:|------:|------------|--------------------------------|
| `fast_io`        |    322 |    58 | yes        | io_uring, IOCP, sendfile, splice, mmap, syscall batch |
| `metadata`       |     76 |     6 | yes        | POSIX id lookups, timestamps, ACL/xattr stubs (Windows DACL/SACL SDDL round-trip added blocks) |
| `platform`       |     56 |     7 | no         | Windows console / service / privilege APIs and POSIX env helpers |
| `checksums`      |     54 |    14 | yes        | SIMD rolling hash and MD4/MD5 batched lanes |
| `engine`         |     29 |    11 | yes        | buffer pool atomics, deferred sync, clonefile, ACL tests |
| `windows-gnu-eh` |     13 |     1 | no         | LoadLibrary/GetProcAddress shim for `__register_frame_info` |
| `core`           |     12 |     4 | no         | signal handler installation, integration test scaffolding |
| `cli`            |     11 |     3 | no         | env-guard helpers for clap integration tests |
| `flist`          |      8 |     2 | no         | `fstatat` + `statx` syscall wrappers used during batched stat |
| `embedding`      |      3 |     1 | no         | env-guard helpers for in-process embed tests |
| `branding`       |      2 |     1 | no         | env-guard helpers for brand-detection tests |
| `daemon`         |      2 |     2 | no         | `getrusage` stress test scaffolding |
| `protocol`       |      1 |     1 | yes        | `Vec::set_len` in `read_payload_into` (already documented) |

## Round-by-round history

This audit has progressed through three documented cycles, taking workspace
SAFETY-comment coverage from below 80% to full coverage.

| Cycle | PR        | Total blocks | With SAFETY | Missing | Coverage | Headline change |
|------:|-----------|-------------:|------------:|--------:|---------:|------------------|
| 0     | baseline  |          572 |         216 |     356 |    37.8% | First scan; majority of `fast_io` SIMD and io_uring sites uncommented. |
| 1     | initial   |          571 |         519 |      52 |    90.9% | `branding`, `cli`, `embedding`, `engine`, `flist`, `metadata`, `platform`, `fast_io` (-222) reached zero outstanding. Stale doctest fragment excluded. |
| 2     | #4412     |          589 |         568 |      21 |   96.43% | `checksums` (-31) cleared; +18 net blocks added (`fast_io` +8 for buffer-ring decomposition / IOCP TransmitFile / vmsplice / BGID instrumentation / ASYNC_CANCEL / SEND_ZC / linked SQE chains / macOS kqueue primitive / ThreadLocalRingPool / session ring pool, `metadata` +10 for Windows DACL/SACL SDDL round-trip) all shipped with SAFETY notes at submission time. |
| 3     | #4440     |          589 |         589 |       0 |  100.00% | Annotated the remaining 21 sites: 8 in `core` (test scaffolding and the SIGWINCH installer, including the macOS kqueue/`getattrlist` integration-test pair) and 13 in `windows-gnu-eh` (LoadLibrary/GetProcAddress resolver chain). |

Note on the cycle 2 -> cycle 3 transition: PR #4440 paired the 21 remaining
annotations with a small fix to the macOS kqueue-fronted `getattrlist`
integration test, which both stabilised the test on Apple Silicon CI and let
the audit script reliably parse the two surrounding `unsafe { ... }` blocks.
After that fix the script's parser sees every site, and every site carries an
inline SAFETY justification.

## Policy violations (forbidden crates that contain unsafe)

The following crates host `unsafe { ... }` blocks despite being outside the
`CLAUDE.md` allowlist. Each one is a candidate either for migration into a
permitted crate, replacement with a safe wrapper crate, or a documented
exception in `CLAUDE.md` (preferred for tiny POSIX shims that exist purely to
serialise environment-variable mutations during tests). All listed blocks now
carry SAFETY comments; the recommendations below address the policy layer, not
the comment layer.

| Crate            | Blocks | Recommendation |
|------------------|-------:|----------------|
| `platform`       |     56 | Migrate Windows console/service/privilege shims into `fast_io` and gate them through safe wrappers (`ctrlc`, `windows-rs`). Env-guard helpers can stay if `CLAUDE.md` is updated to list `platform` as the canonical home for POSIX env serialisation. |
| `windows-gnu-eh` |     13 | Compile-time fallback for `*-windows-gnu` only; gate the entire crate behind `#[cfg(all(target_os = "windows", target_env = "gnu"))]` and document an explicit exception in `CLAUDE.md`. The shim cannot be replaced by a safe wrapper because it patches the gnu personality routine. |
| `core`           |     12 | Move the SIGWINCH handler in `signal/unix.rs` into `platform` (POSIX side) or directly into `fast_io`. Test-only unsafe (`module_list_auth`, `client_integration`) should be moved into a shared helper crate already on the permitted list. |
| `cli`            |     11 | Test-only env-guard helpers. Either re-use the helper that already lives in `platform::env::EnvGuard` or add `cli` test modules to the `CLAUDE.md` exception list. |
| `flist`          |      8 | Wrap `statx`/`fstatat` syscalls behind a safe API exposed from `fast_io::syscall_batch` (`fast_io` already exposes statx helpers in `io_uring/statx.rs`). The current direct `libc::syscall` calls duplicate functionality. |
| `embedding`      |      3 | Replace with `platform::env::EnvGuard`. |
| `daemon`         |      2 | Migrate the connection-scaling `getrusage` stress test to a shared test helper. |
| `branding`       |      2 | Replace with `platform::env::EnvGuard`. |

## Missing SAFETY comments

`CLAUDE.md` requires every `unsafe { ... }` block to be preceded by a SAFETY
comment explaining the invariants the caller relies on. The current outstanding
count is **0 blocks**.

| Crate            | Missing (cycle 2) | Missing (cycle 3) | Notes |
|------------------|------------------:|------------------:|-------|
| `core`           | 8                 | 0                 | Fixed (#4440): SAFETY blocks added to `client/tests/module_list_auth.rs` (5 libc sites), `signal/unix.rs:200` (`sigaction` installer), and `tests/client_integration.rs` (macOS `getattrlist` POD buffer + ABI call). |
| `windows-gnu-eh` | 13                | 0                 | Fixed (#4440): SAFETY blocks added per resolver step in `lib.rs` (`resolve_symbol`, `load_from_library`, `GetModuleHandleA`/`LoadLibraryA`, `GetProcAddress`, `transmute` to fn-pointer, and the two forwarding entry points). |

All thirteen crates that host unsafe code now report zero outstanding
violations.

### New unsafe blocks since PR #4440

The script run for this refresh finds **0 new violations**. Cycle 3 keeps the
same 589-block total observed in cycle 2: no `unsafe { ... }` blocks have been
introduced since #4440 merged, so nothing fell back below the comment bar. The
gating expectation for new code is unchanged - any PR adding an `unsafe { ... }`
block must ship the SAFETY comment in the same patch, and the audit script
should be re-run before merge.

### Reference SAFETY comment shape

Each block should be preceded by a single-line (or short multi-line) SAFETY
comment immediately above the `unsafe { ... }` opening brace, summarising the
caller-visible invariant. Examples from the cycle 3 fixes:

```rust
// SAFETY: Zero-initialises libc::sigaction structs (valid POD layout) before
// passing them to libc::sigaction; handler functions are async-signal-safe and
// only set atomic flags.
unsafe {
    let mut sa_int: libc::sigaction = std::mem::zeroed();
    // ...
}
```

```rust
// SAFETY: `module` is a valid handle returned by GetModuleHandleA/LoadLibraryA;
// `symbol` is a static NUL-terminated byte literal.
unsafe { GetProcAddress(module, symbol.as_ptr() as *const c_char) }
```

## Fixes applied across cycles

Cycle 1 (initial sweep):

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

Cycle 2 (#4412, doc refresh + checksums clean-up):

- `crates/checksums/src/**` (SIMD test wrappers covered by per-test SAFETY
  notes citing the relevant `target_feature` gate).

Cycle 3 (#4440, final 21):

- `crates/core/src/client/tests/module_list_auth.rs`
- `crates/core/src/signal/unix.rs`
- `crates/core/tests/client_integration.rs`
- `crates/windows-gnu-eh/src/lib.rs`

Common patterns documented:

- **Env-guard helpers** (10 sites). `std::env::{set_var, remove_var}` became
  `unsafe` in Rust 2024 because POSIX `getenv`/`setenv` are not thread-safe.
  Every fixed site now cites the `ENV_MUTEX` (or nextest serial slot) that
  serialises the mutation and therefore restores the soundness invariant.
- **POD `mem::zeroed` syscall buffers** (`libc::stat`, `libc::statx`,
  `libc::attrlist`, `libc::sigaction`). Cited as POD `repr(C)` structures
  whose all-zero pattern is a valid initial state, followed by kernel-side
  population.
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
- **Windows resolver chain** (13 sites). Document the static NUL-terminated
  byte literal precondition for symbol/library names, the `OnceLock`-cached
  `GetProcAddress` lifecycle, and the ABI match required by each
  `transmute::<*mut (), fn(..)>` conversion.

## Follow-up tasks

1. **Policy follow-up**: either migrate the unsafe code in `platform`,
   `windows-gnu-eh`, `core`, `cli`, `flist`, `daemon`, `embedding`, and
   `branding` into the permitted crates, or extend the `CLAUDE.md` allowlist
   with explicit, narrow exceptions. Comment coverage is now 100%; the
   remaining work is structural, not documentary.
2. **Regression gate**: wire `tools/audit/unsafe_safety_comment_audit.py` into
   CI as a hard gate so any PR that adds an unannotated `unsafe { ... }` block
   fails before merge.

## Reproducing the audit

```sh
python3 tools/audit/unsafe_safety_comment_audit.py
```

The script prints per-crate block counts, total counts, and a violations table
grouped by crate. Run after edits to confirm the violation count stays at zero.
