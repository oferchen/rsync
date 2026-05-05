# CLI Tunability Flags (#1824-#1830)

Status: Design (consolidated cover for tasks #1824-#1830)
Audience: cli, fast_io, checksums, engine maintainers
Scope: introduce a coherent family of CLI flags that surface internal
performance tunables already exercised by the codebase but currently
reachable only through environment variables, build features, or
compile-time defaults

## 1. Motivation

Seven internal subsystems have grown independent runtime knobs over the
last several releases. Each knob is real, exercised by tests, and
necessary to escape a specific performance pathology. None is reachable
from the CLI today. Operators hitting any of these pathologies must
either rebuild with a different feature set or set an environment
variable that the daemon and SSH child processes do not always inherit.

The seven knobs and their existing back-end APIs are:

- **I/O backend selection.** The `fast_io` crate already dispatches
  between io_uring (`crates/fast_io/src/io_uring.rs`), IOCP
  (`crates/fast_io/src/iocp.rs`), `sendfile` /
  `splice` / `copy_file_range` (`crates/fast_io/src/sendfile.rs`,
  `crates/fast_io/src/splice.rs`), and a portable buffered fallback.
  Selection is driven by `IoUringPolicy` and `IocpPolicy`
  (`crates/fast_io/src/lib.rs:404` and `:437`) plus a fallback chain
  documented in `lib.rs:48-67`. There is no CLI surface; users who
  want to bypass io_uring on a 5.6+ kernel where it misbehaves must
  rebuild without the `io_uring` feature. (#1821 wired the
  `IoBackend` dispatch trait; #1824 surfaces the selector.)
- **SIMD backend selection.** `crates/checksums/src/simd_batch/md5_dispatcher.rs:80`
  exposes `Dispatcher::detect()` which walks a precedence list
  (AVX-512 -> AVX2 -> SSE4.1 -> SSSE3 -> SSE2 -> NEON -> WASM SIMD ->
  scalar). The `md4`, `xxh3`, and rolling-checksum dispatchers follow
  the same pattern. CPUID detection has shipped with bugs before
  (e.g., the AES-GCM probe regression tracked at #1629); without a
  bypass, an operator on hardware that mis-reports a feature has no
  workaround except recompiling with `-C target-cpu=...`. (#1825.)
- **Reflink / copy-on-write toggling.** `fast_io::platform_copy`
  (`crates/fast_io/src/platform_copy/dispatch.rs:11`) tries `FICLONE`
  on Linux, `clonefile` on macOS, and `FSCTL_DUPLICATE_EXTENTS_TO_FILE`
  on Windows ReFS, before falling back to `copy_file_range` and then
  `std::fs::copy`. The reflink path is fast but interacts badly with
  some monitoring agents and snapshot tools; an explicit off-switch
  matches the `cp --reflink=never` UX. (#1826.)
- **Zero-copy socket transfer.** `fast_io::sendfile` and
  `fast_io::splice` are the file-to-socket and socket-to-file
  zero-copy paths. They are always selected when available. SOCK_ZC /
  `MSG_ZEROCOPY` is a separately tracked follow-up; the toggle
  documented here is the same lever already used internally to fall
  back to buffered read/write for diagnosis. (#1827.)
- **Sparse hole detection mode.** `fast_io::zero_detect` provides the
  SIMD zero-byte detector. The receiver also has access to
  `lseek(SEEK_DATA)` / `SEEK_HOLE` on Linux/macOS and to
  `FSCTL_QUERY_ALLOCATED_RANGES` on Windows. The current default
  pipes through the SIMD detector unconditionally; certain backup
  basis files prefer the kernel hole map. (#1828.)
- **Thread-pool sizes.** Rayon's pool defaults to
  `std::thread::available_parallelism()` and is read by the engine via
  `rayon::current_num_threads()` at multiple sites
  (`crates/transfer/src/delta_pipeline.rs:324`,
  `crates/engine/src/concurrent_delta/work_queue/capacity.rs:34`).
  Tokio's pool is configured similarly inside `transport`. The
  adaptive sizer proposed in `docs/design/adaptive-thread-pool-sizing.md`
  pushes both pools toward feedback-driven sizes; this knob is the
  pinning override that disables the adaptive path when an operator
  needs a deterministic core count. (#1829.)
- **io_uring SQE depth.** `IoUringConfig::sq_entries` defaults to 64
  for general use and 256 for `for_large_files()`
  (`crates/fast_io/src/io_uring_stub.rs:75-115`). Workloads with deep
  pipelines or many concurrent files want a larger ring; embedded
  targets with constrained RLIMIT_MEMLOCK want a smaller one. (#1830.)

The pattern across all seven is the same: an internal `Default::default()`
or `detect()` call picks a value that is correct for 95% of users, and
the remaining 5% need an override. This design defines the seven CLI
flags as a single coherent set so the cli, parsing, validation, help-
text, and test layers can land together rather than seven separate
half-PRs.

There is **zero wire-protocol impact**. Every knob is process-internal.
The conflict matrix in section 4 documents this explicitly.

## 2. Per-Flag Design Table

| Flag | Default value | Valid values | Internal API touched | Validation rule | Interaction with other flags |
|------|---------------|--------------|----------------------|-----------------|------------------------------|
| `--io-backend=MODE` | `auto` | `auto`, `io_uring`, `kqueue`, `iocp`, `poll`, `epoll` | `IoUringPolicy`, `IocpPolicy`, `fast_io::traits::FileReader/FileWriter` factories | Reject if `MODE` not in the enumerated set. Warn-and-fallback if `MODE` is unsupported on this OS or this build. | `--io-uring-depth` only meaningful when backend resolves to `io_uring`; otherwise warn. |
| `--simd=MODE` | `auto` | `auto`, `avx512`, `avx2`, `sse4`, `neon`, `none` | `Dispatcher::detect()` in `crates/checksums/src/simd_batch/*` and rolling SIMD | Reject if `MODE` not in the enumerated set. Hard-fail when an explicit ISA is named and the running CPU lacks it. | None. SIMD is independent of I/O backend, COW, and zero-copy. |
| `--cow` / `--no-cow` | enabled (try reflink) | bool toggle | `fast_io::platform_copy::DefaultPlatformCopy::try_*` chain | Tri-state per `tri_state_flag_positive_first`; `--no-cow` wins on tie. | `--inplace` overrides any reflink decision (no clone for in-place writes); `--whole-file` benefits most from `--cow`. |
| `--zero-copy` / `--no-zero-copy` | enabled | bool toggle | `fast_io::sendfile::send_file_to_fd`, `fast_io::splice::try_splice_to_file` | Tri-state per `tri_state_flag_positive_first`; `--no-zero-copy` wins on tie. | `--bwlimit` and the multiplex token frame both force a fallback to read/write irrespective of this flag (see section 4.4). |
| `--sparse-detect=MODE` | `auto` | `auto`, `seek`, `map`, `none` | `fast_io::zero_detect::detect_zero_run`, `lseek(SEEK_DATA)`, `FSCTL_QUERY_ALLOCATED_RANGES` | Reject if `MODE` not in the enumerated set. `seek` requires Linux/macOS; `map` requires Windows ReFS or `FIEMAP`; warn-and-fallback otherwise. | `--sparse` / `--no-sparse` controls *whether* sparse handling runs at all; `--sparse-detect` controls *how* it runs when it does. |
| `--rayon-threads=N` / `--tokio-threads=N` | `available_parallelism()` per pool | `1..=4096` | `rayon::ThreadPoolBuilder::num_threads`, `tokio::runtime::Builder::worker_threads` | Reject 0 and values above 4096. Warn if `N > 4 * cpu_count`. | Pinning either pool disables the adaptive sizer (see `docs/design/adaptive-thread-pool-sizing.md`) for that pool only. |
| `--io-uring-depth=N` | 64 (or 256 for large-file profile) | power-of-two in `1..=4096` | `IoUringConfig::sq_entries` | Reject 0 and values above 4096. Reject non-power-of-two. | Ignored unless the resolved I/O backend is `io_uring`; then a warning is emitted. |

Defaults are written to mirror the existing internal defaults so the
flags are zero-impact when omitted.

## 3. Detailed Per-Flag Spec

### 3.1 `--io-backend=MODE` (#1824)

**Parse rule.** Single-value option. Clap definition:

```rust
Arg::new("io-backend")
    .long("io-backend")
    .value_name("MODE")
    .help("Force a specific I/O backend (auto, io_uring, kqueue, iocp, poll, epoll).")
    .num_args(1)
    .value_parser(["auto", "io_uring", "kqueue", "iocp", "poll", "epoll"])
    .action(ArgAction::Set)
```

The value parser uses Clap's `PossibleValuesParser`-equivalent string
list so invalid values surface as a Clap parse error (consistent with
`--checksum-choice` and `--compress-choice`).

**Value resolution at runtime.** A new enum lives in
`crates/cli/src/frontend/execution/options.rs`:

```rust
pub enum IoBackendChoice {
    Auto,
    IoUring,
    Kqueue,
    Iocp,
    Poll,
    Epoll,
}
```

The frontend translates `IoBackendChoice` into the existing
`IoUringPolicy` and `IocpPolicy` plus a new `PortableBackend` enum
inside `fast_io`:

| Choice | Linux build | macOS build | Windows build |
|--------|-------------|-------------|---------------|
| `auto` | io_uring -> epoll -> poll | kqueue -> poll | iocp -> poll |
| `io_uring` | force io_uring; error if unavailable | warn-fallback | warn-fallback |
| `kqueue` | warn-fallback | force kqueue | warn-fallback |
| `iocp` | warn-fallback | warn-fallback | force iocp |
| `poll` | force poll | force poll | force poll |
| `epoll` | force epoll | warn-fallback | warn-fallback |

The "force" path maps onto `IoUringPolicy::Enabled` and the analogous
`IocpPolicy::Enabled`. The "warn-fallback" path emits a single
diagnostic line at startup (matching the `OC_RSYNC_BUFFER_POOL_STATS`
log style) and uses `auto`. The fallback is intentionally non-fatal so
that wrapper scripts written for one OS keep working when shipped on
another, mirroring upstream rsync's behaviour for `--remote-option`.

**Override semantics.** When the user names an explicit backend that
the OS lacks, the policy is warn-and-fallback. When the user names a
backend that the build lacks (e.g., `--io-backend=io_uring` on a
non-`io_uring`-feature build), the same warn-and-fallback applies. When
the user names `auto` and *no* backend is available, the fallback is
the buffered standard-library path; this is the same behaviour the
codebase already provides today, and there is no error condition.

**Error mode.** Hard exit only on a parse failure (`MODE` not in the
enumerated set). All runtime mismatches are warn-and-fallback. The
exit-code mapping is the existing `ExitCode::SyntaxOrUsage` (1) for
parse errors, identical to how upstream rsync exits when an option
value fails to parse.

### 3.2 `--simd=MODE` (#1825)

**Parse rule.**

```rust
Arg::new("simd")
    .long("simd")
    .value_name("MODE")
    .help("Force a specific SIMD backend (auto, avx512, avx2, sse4, neon, none).")
    .num_args(1)
    .value_parser(["auto", "avx512", "avx2", "sse4", "neon", "none"])
    .action(ArgAction::Set)
```

**Value resolution at runtime.** `Dispatcher::detect()` is augmented
with `Dispatcher::detect_with_override(SimdChoice)`. The override path
short-circuits the precedence walk and instead asserts that the
requested ISA is available, returning an error if not. The frontend
calls this once at process start; the chosen backend is cached in the
existing `OnceLock` that already memoizes `Dispatcher::detect()`.

The CPUID detection bug history (#1629 AES-GCM CPU detection
regression) is the exact case this knob exists to escape. The override
path therefore performs *two* validations:

1. The ISA the user named is recognised by the `is_x86_feature_detected!`
   probe.
2. A 32-byte canary input round-trips through the implementation under
   the chosen backend and matches the scalar reference. This catches
   the "feature is reported but the kernel disabled it via XSAVE" case
   that #1629 hit.

If validation fails, the process exits with `ExitCode::Protocol`
(error-code 4 in upstream's mapping) and a message that names both the
requested ISA and the canary mismatch. Hard exit is the right error
mode here because silently downgrading a checksum choice the user
explicitly demanded would mask the very pathology the flag exists to
diagnose.

**Override semantics.** `--simd=none` forces the scalar fallback even
on hardware that has AVX-512. This is the bisection knob: when a
checksum mismatch is reported, the operator can rerun with
`--simd=none` to confirm whether the bug is in the SIMD code or
elsewhere.

**Error mode.** Hard exit on (a) parse failure, (b) requested ISA
absent on this CPU, (c) canary round-trip mismatch. No warn-and-
fallback; the asymmetry against `--io-backend` is intentional because
checksum correctness is non-negotiable.

### 3.3 `--cow` / `--no-cow` (#1826)

**Parse rule.** Tri-state pair, identical pattern to the existing
`--sparse` / `--no-sparse` and `--compress` / `--no-compress` pairs:

```rust
Arg::new("cow")
    .long("cow")
    .help("Try copy-on-write reflinks (FICLONE/clonefile/FSCTL_DUPLICATE_EXTENTS).")
    .action(ArgAction::SetTrue)
    .overrides_with("no-cow"),
Arg::new("no-cow")
    .long("no-cow")
    .help("Disable copy-on-write reflinks; always fall through to copy_file_range or std::fs::copy.")
    .action(ArgAction::SetTrue)
    .overrides_with("cow"),
```

Resolution uses `tri_state_flag_positive_first` from
`crates/cli/src/frontend/arguments/parser/flags.rs:11`, matching every
other paired flag in the codebase.

**Value resolution at runtime.** Default is `true` (try reflinks). The
chosen value is plumbed into a new `PlatformCopy` constructor:

```rust
DefaultPlatformCopy::with_policy(PlatformCopyPolicy::TryReflink)
DefaultPlatformCopy::with_policy(PlatformCopyPolicy::SkipReflink)
```

`SkipReflink` causes the dispatch chain in
`crates/fast_io/src/platform_copy/dispatch.rs` to skip the
`try_ficlone` / `try_clonefile` / `try_refs_reflink` heads and go
straight to `copy_file_range` (Linux) or `std::fs::copy`.

**Override semantics.** `--cow` on a filesystem that does not support
reflinks (ext4 on Linux, HFS+ on macOS, NTFS on Windows) falls back
silently. The reflink heads already return `Unsupported` in those
cases; the dispatch chain handles fallback transparently. No warning
is emitted because the fallback is the documented behaviour and is
identical to today's pre-flag default.

**Error mode.** No error path. The flag is a hint; the kernel has the
last word.

### 3.4 `--zero-copy` / `--no-zero-copy` (#1827)

**Parse rule.** Tri-state pair:

```rust
Arg::new("zero-copy")
    .long("zero-copy")
    .help("Use sendfile/splice for zero-copy socket transfer where supported.")
    .action(ArgAction::SetTrue)
    .overrides_with("no-zero-copy"),
Arg::new("no-zero-copy")
    .long("no-zero-copy")
    .help("Disable zero-copy socket transfer; always use buffered read/write.")
    .action(ArgAction::SetTrue)
    .overrides_with("zero-copy"),
```

**Value resolution at runtime.** The frontend stores the boolean in
`CoreConfig` and the engine consults it before invoking
`fast_io::sendfile::send_file_to_fd` and
`fast_io::splice::try_splice_to_file`. The existing internal threshold
(`SENDFILE_THRESHOLD = 64 KB`, see
`crates/fast_io/src/sendfile.rs:48`) is preserved; the flag is an
upper veto, not a lower bound override.

**Override semantics.** Two cases that already force a fallback
override `--zero-copy=true`:

- The transport is the multiplex socket carrying `MSG_*` frames. The
  multiplex framing is incompatible with `sendfile` because the
  payload must be wrapped in a length-prefixed envelope. The flag has
  no effect on multiplex sockets; it only affects the file-to-pipe
  fast paths used during local copies and SSH stdio passthrough.
- `--bwlimit` is active. The bandwidth limiter
  (`crates/bandwidth/src/limiter/`) inspects every chunk before
  release; bypassing it would defeat the rate cap. When
  `--bwlimit > 0` the engine forces buffered I/O and ignores
  `--zero-copy`. A diagnostic at `-vv` documents the override.

**Error mode.** No error path. The override is silent except at `-vv`.

### 3.5 `--sparse-detect=MODE` (#1828)

**Parse rule.**

```rust
Arg::new("sparse-detect")
    .long("sparse-detect")
    .value_name("MODE")
    .help("Sparse hole detection mode (auto, seek, map, none).")
    .num_args(1)
    .value_parser(["auto", "seek", "map", "none"])
    .action(ArgAction::Set)
```

**Value resolution at runtime.** A new `SparseDetectMode` enum in
`crates/engine/src/local_copy/sparse.rs`:

| Mode | Linux | macOS | Windows |
|------|-------|-------|---------|
| `auto` | `lseek(SEEK_DATA)` if `st_blocks * 512 < st_size` else SIMD | `lseek(SEEK_DATA)` else SIMD | `FSCTL_QUERY_ALLOCATED_RANGES` else SIMD |
| `seek` | `lseek(SEEK_DATA/HOLE)` only | `lseek(SEEK_DATA/HOLE)` only | warn-fallback to `auto` |
| `map` | `FIEMAP` ioctl | warn-fallback to `auto` | `FSCTL_QUERY_ALLOCATED_RANGES` only |
| `none` | SIMD scan only | SIMD scan only | SIMD scan only |

The default `auto` heuristic mirrors GNU `cp --sparse=auto`: the
allocated-blocks shortcut is the cheapest signal that a file might be
sparse; if it indicates "fully allocated", we skip hole detection
entirely.

**Override semantics.** `--sparse-detect=seek` on Windows warns and
falls back to `auto`. `--sparse-detect=map` on macOS warns and falls
back to `auto`. `--sparse-detect=none` always works. `--sparse-detect`
is layered under `--sparse` / `--no-sparse`: if sparse handling is
disabled, this flag has no effect and a warning is emitted at `-vv`
when the user supplies it alongside `--no-sparse`.

**Error mode.** Hard exit only on parse failure. Mode-platform
mismatches are warn-and-fallback.

### 3.6 `--rayon-threads=N` / `--tokio-threads=N` (#1829)

**Parse rule.** Two integer-valued options:

```rust
Arg::new("rayon-threads")
    .long("rayon-threads")
    .value_name("N")
    .help("Override the number of rayon worker threads (default: available CPU cores).")
    .num_args(1)
    .value_parser(clap::value_parser!(u32).range(1..=4096))
    .action(ArgAction::Set),
Arg::new("tokio-threads")
    .long("tokio-threads")
    .value_name("N")
    .help("Override the number of tokio worker threads (default: available CPU cores).")
    .num_args(1)
    .value_parser(clap::value_parser!(u32).range(1..=4096))
    .action(ArgAction::Set),
```

**Value resolution at runtime.** When `--rayon-threads=N` is present
the frontend calls `rayon::ThreadPoolBuilder::new().num_threads(N
as usize).build_global()` exactly once, before any rayon-using crate
runs. When absent, the existing default-global behaviour is preserved
(rayon picks up `std::thread::available_parallelism()`).

The same pattern applies to `--tokio-threads=N` against the
transport's tokio runtime constructor in
`crates/transport/src/runtime.rs` (or equivalent). The runtime is
constructed exactly once and the user-supplied count is passed to
`tokio::runtime::Builder::worker_threads(N as usize)`.

**Override semantics.** Either pinning flag *also* disables the
adaptive sizer for that pool (see
`docs/design/adaptive-thread-pool-sizing.md` section 4.4). The
adaptive sizer treats `--rayon-threads=N` as equivalent to
`transfer-worker-threads = <fixed>`: the count is honoured and the
sizer never grows or shrinks past it.

**Error mode.** Hard exit on parse failure (out of `1..=4096`). A
warning at `-vv` if `N > 4 * cpu_count` because oversubscription past
4x typically indicates a misconfiguration (the value is honoured; the
warning is informational).

### 3.7 `--io-uring-depth=N` (#1830)

**Parse rule.**

```rust
Arg::new("io-uring-depth")
    .long("io-uring-depth")
    .value_name("N")
    .help("Override the io_uring submission queue depth (default: 64; must be power of two).")
    .num_args(1)
    .value_parser(clap::value_parser!(u32).range(1..=4096))
    .action(ArgAction::Set),
```

The power-of-two requirement is enforced by a custom validator that
calls `n.is_power_of_two()` after the integer parse. On failure the
clap error message names the next-larger and next-smaller powers of
two as suggestions, matching the upstream `--block-size` validator
style.

**Value resolution at runtime.** The frontend writes the value into
`IoUringConfig::sq_entries` before any of the
`reader_from_path` / `writer_from_file` factories run. The default
profile heuristic (64 for general use, 256 for large files) is
overridden at the point where the profile is selected.

**Override semantics.** Ignored when the resolved I/O backend is not
`io_uring`. A `-vv` warning is emitted in that case so the operator
knows the flag had no effect. On Linux without the `io_uring` build
feature the same warning fires.

The `RLIMIT_MEMLOCK` ceiling on registered buffers limits the maximum
practical depth; the hard ceiling of 4096 is below the default RLIMIT
on most distros (16 MiB / 4 KB pages = 4096). Operators on tighter
limits will see an `ENOMEM` from `io_uring_setup` and the existing
fallback chain returns a buffered reader; that is unchanged.

**Error mode.** Hard exit on parse failure (non-power-of-two,
out-of-range).

## 4. Conflict Matrix

The matrix records every pair where one flag's effect is constrained,
ignored, or overridden by another. Empty cells mean "independent; no
interaction expected". Cells with text are documented and tested.

|                       | `--io-backend` | `--simd` | `--cow` | `--zero-copy` | `--sparse-detect` | `--rayon-threads` | `--tokio-threads` | `--io-uring-depth` |
|-----------------------|----------------|----------|---------|---------------|-------------------|-------------------|-------------------|--------------------|
| `--io-backend`        | -              |          |         |               |                   |                   |                   | gates io-uring-depth |
| `--simd`              |                | -        |         |               |                   |                   |                   |                    |
| `--cow`               |                |          | -       |               |                   |                   |                   |                    |
| `--zero-copy`         | see 4.4        |          |         | -             |                   |                   |                   |                    |
| `--sparse-detect`     |                |          |         |               | -                 |                   |                   |                    |
| `--rayon-threads`     |                |          |         |               |                   | -                 |                   |                    |
| `--tokio-threads`     |                |          |         |               |                   |                   | -                 |                    |
| `--io-uring-depth`    | gates 4.5      |          |         |               |                   |                   |                   | -                  |

External-flag interactions (rows below the diagonal that involve
upstream-rsync flags rather than the new family):

- **`--inplace` overrides `--cow`.** Reflinks require a fresh
  destination inode; `--inplace` writes through the existing inode.
  The dispatch chain in `platform_copy/dispatch.rs` already short-
  circuits when the destination exists; this is a documentation-only
  interaction.
- **`--bwlimit > 0` overrides `--zero-copy`.** The rate limiter
  inspects every chunk; bypassing it via `sendfile` would silently
  defeat the rate cap. Section 3.4 documents the override.
- **`--no-sparse` makes `--sparse-detect` a no-op.** Section 3.5
  documents the layering.
- **`--whole-file` benefits most from `--cow`.** No conflict; whole-
  file copies are exactly the case where reflinks have the largest
  measurable speedup. Documentation only.

### 4.1 `--io-backend=io_uring` on macOS

Warn-and-fallback to `auto` (kqueue+poll). The user gets a single
startup line:

```
io_uring not available on this platform; falling back to kqueue
```

### 4.2 `--cow` on a non-COW filesystem

Silent fallback to `copy_file_range` / `std::fs::copy`. The flag is a
hint and the kernel has the last word. No warning, because the fallback
is identical to the pre-flag default and warning would be noisy on
mixed-filesystem trees.

### 4.3 `--simd=avx512` when the CPU lacks AVX-512

Hard exit with `ExitCode::Protocol` (4) and a message:

```
oc-rsync: --simd=avx512 requested but the running CPU does not advertise
the avx512f and avx512bw features. Use --simd=auto to detect, or
--simd=none to force scalar.
```

The asymmetry against `--io-backend` is documented in section 3.2.

### 4.4 `--io-backend=poll` and `--zero-copy=true`

Independent; no interaction. The poll backend uses non-zero-copy
buffered I/O for its event loop, but the `sendfile` path is still
available for the file-to-pipe data plane. Documented in the matrix
explicitly so that future maintainers do not assume an unstated
constraint.

### 4.5 `--io-uring-depth=N` and `--io-backend != io_uring`

Warn at `-vv`:

```
oc-rsync: --io-uring-depth=N has no effect when --io-backend resolves
to <other>; ignoring.
```

The flag is honoured if a later option flips the backend back to
io_uring; the warning fires once at startup based on the resolved
backend at config-build time.

## 5. Help-Text Wording

Single-line `--help` summaries, matching upstream rsync's style of
imperative-mood verb phrases under 78 characters:

```
--io-backend=MODE       force a specific I/O backend (auto, io_uring,
                        kqueue, iocp, poll, epoll)
--simd=MODE             force a specific SIMD backend (auto, avx512,
                        avx2, sse4, neon, none)
--cow                   try copy-on-write reflinks where supported
--no-cow                disable copy-on-write reflinks
--zero-copy             use sendfile/splice for zero-copy where supported
--no-zero-copy          disable zero-copy socket transfer
--sparse-detect=MODE    sparse hole detection mode (auto, seek, map, none)
--rayon-threads=N       override rayon worker thread count
--tokio-threads=N       override tokio worker thread count
--io-uring-depth=N      override io_uring submission queue depth
```

Long-form help text (printed under each flag with `--help`) names the
default explicitly:

```
  --io-backend=MODE
      Force a specific I/O backend. The default is "auto", which picks
      the best backend available on this platform: io_uring on Linux
      5.6+, IOCP on Windows, kqueue on macOS, otherwise poll. Naming a
      backend that this OS or this build does not support warns at
      startup and falls back to "auto".
```

This style matches the existing `--checksum-choice`, `--compress-choice`,
and `--bwlimit` entries in
`crates/cli/src/frontend/command_builder/sections/`.

## 6. Implementation Order

The seven flags depend on different parts of the codebase. Suggested
phasing places the cheapest, most-isolated wins first and the largest
plumbing changes last.

1. **Phase 1: tri-state pairs (`--cow`, `--zero-copy`).** These two
   flags reuse the `tri_state_flag_positive_first` helper that already
   exists; they thread a single boolean through `CoreConfig`. No new
   dependency, no new crate boundary. Lands in one PR.
2. **Phase 2: `--simd=MODE`.** Augments `Dispatcher::detect()` with an
   override path. The canary-validation gate (section 3.2) is the only
   non-trivial new code. Independent of the other flags. Second PR.
3. **Phase 3: `--sparse-detect=MODE`.** Adds the `SparseDetectMode`
   enum and rewires the receiver's hole-detection path. Layered under
   the existing `--sparse` flag; isolation makes this independently
   reviewable. Third PR.
4. **Phase 4: `--rayon-threads=N` and `--tokio-threads=N`.** These
   pin pool sizes and disable the adaptive sizer per pool. Order of
   operations matters: rayon's `build_global` must happen before any
   rayon use, and tokio's runtime must be built before any
   `transport` use. The frontend's `main` becomes the single
   construction site. Fourth PR.
5. **Phase 5: `--io-backend=MODE`.** This is the largest plumbing
   change because it widens the existing `IoUringPolicy` /
   `IocpPolicy` pair into a portable `IoBackend` enum. The trait
   wiring at #1821 is the prerequisite; if it is not landed, this
   phase blocks until it is. Fifth PR.
6. **Phase 6: `--io-uring-depth=N`.** Trivial once phase 5 is in: the
   override threads through `IoUringConfig::sq_entries`. Combined
   with phase 5 in the same release branch but split into a separate
   PR for review locality.

Phases 1, 2, 3, and 4 are independent of each other and of the trait
wiring; they can land in parallel. Phases 5 and 6 are sequential.

## 7. Test Strategy

One CLI-level integration test per flag, four assertions each:
default behaviour, explicit value, invalid value rejection,
interaction with one other flag. Tests live next to the existing
`crates/cli/src/frontend/tests/parse_args_*.rs` files following the
same naming pattern.

### 7.1 `--io-backend`

```
parse_args_recognises_io_backend.rs
- default_omitted_resolves_to_auto()
- explicit_io_uring_sets_force_policy_on_linux()
- explicit_invalid_value_rejected()
- explicit_io_uring_warns_on_macos_and_falls_back_to_auto()
```

The fourth case asserts on the structured warning emitted to stderr
under `OC_RSYNC_LOG_FORMAT=structured`, not on raw text.

### 7.2 `--simd`

```
parse_args_recognises_simd.rs
- default_omitted_resolves_to_dispatcher_detect()
- explicit_none_forces_scalar_backend()
- explicit_invalid_value_rejected()
- explicit_avx512_on_non_avx512_cpu_hard_exits_with_protocol_code()
```

The fourth case is the asymmetry test against `--io-backend`. It
runs on every CI runner; on AVX-512 boxes it skips with
`#[cfg_attr(target_feature = "avx512f", ignore)]`.

### 7.3 `--cow` / `--no-cow`

```
parse_args_recognises_cow.rs
- default_omitted_resolves_to_try_reflink()
- no_cow_skips_reflink_heads_in_dispatch_chain()
- positive_then_negative_resolves_to_no_cow()
- inplace_overrides_cow_attempt()
```

The fourth case is the documented `--inplace` interaction; it asserts
that the dispatch chain never calls `try_ficlone` when `--inplace` is
active.

### 7.4 `--zero-copy` / `--no-zero-copy`

```
parse_args_recognises_zero_copy.rs
- default_omitted_resolves_to_zero_copy_enabled()
- no_zero_copy_skips_sendfile_path()
- positive_then_negative_resolves_to_no_zero_copy()
- bwlimit_overrides_zero_copy_attempt()
```

### 7.5 `--sparse-detect`

```
parse_args_recognises_sparse_detect.rs
- default_omitted_resolves_to_auto()
- explicit_seek_uses_lseek_data()
- explicit_invalid_value_rejected()
- no_sparse_makes_sparse_detect_a_noop()
```

### 7.6 `--rayon-threads` / `--tokio-threads`

```
parse_args_recognises_thread_pool_overrides.rs
- default_omitted_uses_available_parallelism()
- explicit_value_pins_pool_size()
- explicit_zero_rejected()
- explicit_pin_disables_adaptive_sizer_for_that_pool()
```

The fourth case requires the adaptive sizer landed (phase 4 of
`docs/design/adaptive-thread-pool-sizing.md`); until then the
assertion checks only that `OC_RSYNC_ADAPTIVE_THREADS=0` is implicit.

### 7.7 `--io-uring-depth`

```
parse_args_recognises_io_uring_depth.rs
- default_omitted_uses_64_or_256_per_profile()
- explicit_power_of_two_pins_sq_entries()
- explicit_non_power_of_two_rejected()
- explicit_value_warns_when_io_backend_is_not_io_uring()
```

Each test uses the existing `parse_args` test harness from
`crates/cli/src/frontend/arguments/parser/tests.rs` and the
`StructuredLogCapture` helper to assert on warnings.

Cross-flag interaction tests live in
`crates/cli/tests/cli_tunability_interactions.rs` (one new
integration-test file). One test per row of the conflict matrix where
the cell text says "see 4.x".

## 8. Non-Goals

- **No wire-protocol features.** Every flag is process-internal. The
  conflict matrix is closed under "this affects only the local
  process". No new capability bytes, no new MSG frames, no new daemon
  greeting tokens. The `feedback_no_wire_protocol_features` directive
  applies in full.
- **No exposing every internal knob.** Several internal knobs are
  deliberately not surfaced:
  - `OC_RSYNC_BUFFER_POOL_SIZE` stays an env var because operators
    rarely tune it and the default tuning already covers the 95%
    case (`crates/engine/src/local_copy/buffer_pool/global.rs:49`).
  - `BufferRingConfig::ring_size` and `bgid` stay internal because
    they live below the user-visible io_uring depth and overriding
    them in isolation would underconstrain the ring.
  - The bandwidth limiter's per-token interval, the SPSC channel's
    spin-wait threshold, and the sparse detector's SIMD lane width
    all stay internal.
  - Threshold constants like `SENDFILE_THRESHOLD` and
    `PARALLEL_STAT_THRESHOLD` stay compile-time constants.
- **No breaking changes to the upstream rsync flag namespace.** Every
  new flag occupies a name that upstream rsync 3.4.1 does not use.
  Concretely: `options.c` in
  `target/interop/upstream-src/rsync-3.4.1/options.c` defines no
  long option named `--io-backend`, `--simd`, `--cow`, `--no-cow`,
  `--zero-copy`, `--no-zero-copy`, `--sparse-detect`,
  `--rayon-threads`, `--tokio-threads`, or `--io-uring-depth`. The
  family is fully namespaced under names upstream has not claimed.
- **No `--unsafe` or `--no-checks` umbrella flag.** Each tunable is
  surfaced individually. An umbrella flag would invite operators to
  set it once and forget; per-knob granularity is the documented
  policy.
- **No `OC_RSYNC_*` env var equivalents for the new flags.** The
  existing env vars (`OC_RSYNC_BUFFER_POOL_SIZE`,
  `OC_RSYNC_FORCE_NO_COMPRESS_TEST`, etc.) remain for back-compat.
  New tunables ship as CLI flags only because env vars do not
  propagate cleanly through the SSH child and the daemon worker
  pool. The daemon-side equivalent is the `oc-rsyncd.conf`
  directives, which a separate design note will cover if and when
  there is operator demand.

## 9. Risks

- **Flag count creep.** Seven flags is a lot of new surface. The
  mitigation is grouping under a single design note and a single
  release; users see the family as a coherent whole rather than as
  seven independent additions. The help-text section is one page.
- **Validation footguns.** `--simd=avx512` on a CPU without AVX-512
  hard-exits, while `--io-backend=io_uring` on macOS warns and
  falls back. The asymmetry is documented in section 3.2 and 3.5;
  the rationale is that checksum correctness is non-negotiable while
  I/O-backend choice is a performance hint. Reviewers must agree on
  this asymmetry before merge.
- **Adaptive-sizer integration.** `--rayon-threads` and
  `--tokio-threads` interact with the adaptive sizer
  (`docs/design/adaptive-thread-pool-sizing.md`). If that design is
  not yet landed when the threads flags ship, phase 4 of this
  design pre-creates the disable hook so the adaptive sizer can
  later observe a pinned pool without surprise.
- **Power-of-two requirement on `--io-uring-depth`.** Some users
  will set `100` and be confused by the parse error. The clap error
  message names the nearest powers of two as suggestions
  (matching the `--block-size` validator pattern at upstream
  rsync's `parse_size_arg`). This is the lowest-friction approach;
  silently rounding would surprise more.
- **Test runtime.** Each flag has four integration tests; seven
  flags means ~28 new integration tests. They run inside the
  existing `parse_args` harness which is fast (microseconds per
  case); total CI delta is well under one second.
- **Help-text clutter.** The seven flags are placed in a new
  "Tunability" section in `--help` output, after "Connection options"
  and before "Output options", to avoid bloating any existing
  section.

## 10. Decision

Land the design now. Implementation proceeds per section 6.
Phases 1-4 are independent and reviewer-parallelizable. Phases 5
and 6 land sequentially after #1821 (`IoBackend` trait wiring) is
in. Each phase ships its own PR with the four-assertion test file
described in section 7. The conflict matrix in section 4 is a
contract: every cell with text gets a regression test under
`crates/cli/tests/cli_tunability_interactions.rs`.

The `OC_RSYNC_*` env-var policy stays as documented in section 8;
no new env vars are introduced by this design.

## 11. References

- Existing CLI parser: `crates/cli/src/frontend/arguments/parser/mod.rs`,
  `crates/cli/src/frontend/arguments/parser/flags.rs`.
- Existing flag definitions:
  `crates/cli/src/frontend/command_builder/sections/build_base_command/transfer.rs`,
  `crates/cli/src/frontend/command_builder/sections/transfer_behavior_options.rs`,
  `crates/cli/src/frontend/command_builder/sections/connection_and_logging_options.rs`.
- Internal APIs being surfaced:
  `crates/fast_io/src/lib.rs:404` (`IoUringPolicy`),
  `crates/fast_io/src/lib.rs:437` (`IocpPolicy`),
  `crates/fast_io/src/io_uring_stub.rs:54-118` (`IoUringConfig`),
  `crates/fast_io/src/platform_copy/dispatch.rs` (CoW dispatch chain),
  `crates/fast_io/src/sendfile.rs`,
  `crates/fast_io/src/splice.rs`,
  `crates/fast_io/src/zero_detect.rs`,
  `crates/checksums/src/simd_batch/md5_dispatcher.rs:80-118`
  (`Dispatcher::detect`).
- Related design notes:
  `docs/design/adaptive-thread-pool-sizing.md`,
  `docs/design/buffer-pool-sharding.md`,
  `docs/design/iouring-session-ring-pool.md`,
  `docs/design/io-uring-rayon-composition.md`.
- Upstream reference for flag-namespace claims:
  `target/interop/upstream-src/rsync-3.4.1/options.c`.
