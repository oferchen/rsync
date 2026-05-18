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
**589 `unsafe { ... }` blocks** across the workspace (up from the previous 571).
**568 blocks carry a SAFETY comment**; **21 still lack one** for a workspace
coverage of **96.43%** (up from 90.9%).

| Metric                                  | Previous (571) | Current (589) | Delta   |
|-----------------------------------------|---------------:|--------------:|--------:|
| Total `unsafe { ... }` blocks           |            571 |           589 |   +18   |
| Blocks with a SAFETY comment            |            519 |           568 |   +49   |
| Blocks missing a SAFETY comment         |             52 |            21 |   -31   |
| Workspace coverage (with SAFETY)        |          90.9% |        96.43% |  +5.5pp |

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
| `daemon`         |      2 | The remaining two live in the connection-scaling stress test and should be migrated to a shared test helper. |
| `embedding`      |      3 | Replace with `platform::env::EnvGuard`. |
| `branding`       |      2 | Replace with `platform::env::EnvGuard`. (SAFETY comments already added in earlier cycle.) |

## Missing SAFETY comments

`CLAUDE.md` requires every `unsafe { ... }` block to be preceded by a SAFETY
comment explaining the invariants the caller relies on. The current outstanding
count is **21 blocks** across two crates.

| Crate            | Missing (prev) | Missing (now) | Notes |
|------------------|---------------:|--------------:|-------|
| `checksums`      | 31             | 0             | Fixed: SAFETY comments now cover the SIMD test wrappers (target-feature gates). |
| `core`           | 8              | 8             | Unchanged. Two integration test files and the SIGWINCH installer. |
| `windows-gnu-eh` | 13             | 13            | Unchanged. `LoadLibrary`/`GetProcAddress` resolver chain still missing per-block SAFETY notes. |

Eleven of the thirteen crates that host unsafe code now have zero outstanding
violations. The two remaining crates account for all 21 missing notes.

### New unsafe blocks since the previous audit

The previous audit observed 571 blocks; the workspace now has 589 (+18 net).
The new additions are concentrated in `fast_io` (+8) and `metadata` (+10), and
**every newly-introduced unsafe block was added with a SAFETY comment**. The
net delta therefore introduces zero new violations. Specifically:

- `fast_io`: +8 blocks distributed across newly added/decomposed modules
  (`io_uring/registered_buffers/*` split, IOCP TransmitFile primitive,
  vmsplice writer, BGID high-water-mark instrumentation, ASYNC_CANCEL, SEND_ZC,
  linked SQE chains, macOS kqueue primitive, ThreadLocalRingPool, session ring
  pool). All shipped with SAFETY comments at submission time.
- `metadata`: +10 blocks in the Windows DACL/SACL SDDL round-trip path
  (`metadata` PR series #2307, #2308). All shipped with SAFETY comments.
- `daemon`: -1 block (doctest example removed during decomposition).

### Outstanding violation inventory (file:line)

The 21 remaining sites, copied verbatim from the audit script output:

`crate: core` (8 violations):

- `crates/core/src/client/tests/module_list_auth.rs:128`
- `crates/core/src/client/tests/module_list_auth.rs:137`
- `crates/core/src/client/tests/module_list_auth.rs:146`
- `crates/core/src/client/tests/module_list_auth.rs:157`
- `crates/core/src/client/tests/module_list_auth.rs:162`
- `crates/core/src/signal/unix.rs:200`
- `crates/core/tests/client_integration.rs:27`
- `crates/core/tests/client_integration.rs:38`

`crate: windows-gnu-eh` (13 violations):

- `crates/windows-gnu-eh/src/lib.rs:100`
- `crates/windows-gnu-eh/src/lib.rs:132`
- `crates/windows-gnu-eh/src/lib.rs:146`
- `crates/windows-gnu-eh/src/lib.rs:150`
- `crates/windows-gnu-eh/src/lib.rs:158`
- `crates/windows-gnu-eh/src/lib.rs:163`
- `crates/windows-gnu-eh/src/lib.rs:167`
- `crates/windows-gnu-eh/src/lib.rs:172`
- `crates/windows-gnu-eh/src/lib.rs:176`
- `crates/windows-gnu-eh/src/lib.rs:182`
- `crates/windows-gnu-eh/src/lib.rs:183`
- `crates/windows-gnu-eh/src/lib.rs:192`
- `crates/windows-gnu-eh/src/lib.rs:193`

### Recommended fix shape

Each block should be preceded by a single-line SAFETY comment immediately
above the `unsafe { ... }` opening brace, summarising the caller-visible
invariant in one line. The form is:

```rust
// SAFETY: <one-line invariant statement>.
unsafe {
    ...
}
```

Suggested invariant skeletons per call site:

- `core/client/tests/module_list_auth.rs` (libc helpers): cite the test
  serial slot (`ENV_MUTEX` / `serial_test`) that pins the process to one
  thread for the duration of the libc call, plus the POD nature of any
  output struct.
- `core/signal/unix.rs:200` (`signal(SIGWINCH, ...)`): cite that the handler
  is `extern "C" fn` with no captured state, satisfies async-signal-safety,
  and is installed once during startup before any reader thread runs.
- `core/tests/client_integration.rs:27,38` (macOS `attrlist` +
  `getattrlist`): cite the `repr(C)` POD layout (`mem::zeroed` initialisation
  is valid) and the kernel-side population guarantees from `getattrlist(2)`.
- `windows-gnu-eh/src/lib.rs` (13 resolver chain blocks): cite the
  module-handle lifetime (`GetModuleHandleA` /  `LoadLibraryA` returning a
  process-lifetime handle), the `transmute` precondition that the
  `GetProcAddress` symbol resolves to the exact `extern "system"` signature
  declared in the `type` alias, and the `OnceLock` cache ensuring the symbol
  is resolved exactly once with subsequent reads being plain pointer copies.

## Historical fix log (prior audit cycles)

The following files were updated with SAFETY justifications across prior
audit cycles. Listed here for traceability; this refresh did not modify any
source files.

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
- `crates/checksums/src/**` (SIMD test wrappers, follow-up cycle)

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

1. **windows-gnu-eh resolver chain** (13 blocks). Document the
   `OnceLock`-cached `GetProcAddress` lifecycle once per resolver helper
   using the SAFETY skeleton above.
2. **core test scaffolding** (8 blocks). Annotate the libc / signal sites
   in place, or migrate them into a shared helper crate already on the
   permitted list (`metadata` for libc id-lookups, `fast_io` for
   signal-handler installation).
3. **Policy follow-up**: either migrate the unsafe code in `platform`,
   `windows-gnu-eh`, `core`, `cli`, `flist`, `daemon`, `embedding`, and
   `branding` into the permitted crates, or extend the `CLAUDE.md` allowlist
   with explicit, narrow exceptions.

## Reproducing the audit

```sh
python3 tools/audit/unsafe_safety_comment_audit.py
```

The script prints per-crate block counts, total counts, and a violations table
grouped by crate. Run after edits to confirm the violation count is dropping.
