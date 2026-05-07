# `#[must_use]` coverage audit on public Result/Option-returning functions

Tracking: code-quality hardening. Adding `#[must_use]` to fallible public
APIs prevents silently dropped errors and ignored optional values, both of
which mask correctness regressions.

Last verified: 2026-05-06 against `docs/must-use-coverage-2123` branch.

## Methodology

A static scanner walks every `crates/<crate>/src/**/*.rs` file, matches
`pub fn` declarations whose return type contains `Result<...>`,
`io::Result<...>` or `Option<...>`, and checks whether the immediately
preceding attribute block contains `#[must_use]`. The scanner skips blank
lines, doc comments, and unrelated attributes when walking back, so an
attribute placed above doc comments still counts.

The scanner script lives at `tools/audit/must_use_audit.py` and emits a
structured stream consumed when refreshing this document. The numbers
below are aggregated from `cargo`-free static analysis only - they do not
attempt to evaluate macro expansions, generated code, or trait method
implementations (only free `pub fn`s and `impl` block functions are
counted).

Notes:

- Trait method implementations on a public type are counted because they
  declare `pub fn` syntactically; in practice `#[must_use]` on a trait
  method is best applied at the trait definition.
- Functions that already carry `#[must_use]` via the `must_use` lint at
  the type level (for example `Result<MyType, Error>` where `MyType` is
  `#[must_use]`) are not double-counted as covered.

## Per-crate `Result`-returning `pub fn` coverage

| Crate | `Result` `pub fn` | Missing `#[must_use]` | Coverage |
|-------|-------------------|-----------------------|----------|
| `apple-fs` | 20 | 20 | 0.0% |
| `bandwidth` | 4 | 4 | 0.0% |
| `batch` | 40 | 40 | 0.0% |
| `branding` | 8 | 8 | 0.0% |
| `checksums` | 15 | 15 | 0.0% |
| `cli` | 82 | 82 | 0.0% |
| `compress` | 32 | 32 | 0.0% |
| `core` | 93 | 89 | 4.3% |
| `daemon` | 33 | 33 | 0.0% |
| `embedding` | 7 | 7 | 0.0% |
| `engine` | 141 | 140 | 0.7% |
| `fast_io` | 183 | 181 | 1.1% |
| `filters` | 10 | 10 | 0.0% |
| `flist` | 27 | 27 | 0.0% |
| `logging` | 4 | 4 | 0.0% |
| `logging-sink` | 8 | 8 | 0.0% |
| `match` | 3 | 3 | 0.0% |
| `metadata` | 102 | 102 | 0.0% |
| `platform` | 30 | 30 | 0.0% |
| `protocol` | 257 | 256 | 0.4% |
| `rsync_io` | 103 | 103 | 0.0% |
| `signature` | 8 | 8 | 0.0% |
| `test-support` | 0 | 0 | n/a |
| `transfer` | 145 | 143 | 1.4% |
| `windows-gnu-eh` | 0 | 0 | n/a |

Workspace totals: 1355 `Result`-returning `pub fn`s, 1323 missing
`#[must_use]` (2.4% covered).

## Per-crate `Option`-returning `pub fn` coverage

| Crate | `Option` `pub fn` | Missing `#[must_use]` | Coverage |
|-------|-------------------|-----------------------|----------|
| `apple-fs` | 6 | 6 | 0.0% |
| `bandwidth` | 6 | 3 | 50.0% |
| `batch` | 3 | 3 | 0.0% |
| `branding` | 1 | 1 | 0.0% |
| `checksums` | 6 | 4 | 33.3% |
| `cli` | 25 | 25 | 0.0% |
| `compress` | 7 | 6 | 14.3% |
| `core` | 100 | 96 | 4.0% |
| `daemon` | 80 | 78 | 2.5% |
| `embedding` | 0 | 0 | n/a |
| `engine` | 95 | 71 | 25.3% |
| `fast_io` | 23 | 1 | 95.7% |
| `filters` | 0 | 0 | n/a |
| `flist` | 4 | 4 | 0.0% |
| `logging` | 0 | 0 | n/a |
| `logging-sink` | 1 | 1 | 0.0% |
| `match` | 12 | 12 | 0.0% |
| `metadata` | 12 | 12 | 0.0% |
| `platform` | 8 | 8 | 0.0% |
| `protocol` | 70 | 69 | 1.4% |
| `rsync_io` | 15 | 15 | 0.0% |
| `signature` | 2 | 2 | 0.0% |
| `test-support` | 0 | 0 | n/a |
| `transfer` | 19 | 17 | 10.5% |
| `windows-gnu-eh` | 0 | 0 | n/a |

Workspace totals: 495 `Option`-returning `pub fn`s, 432 missing
`#[must_use]` (12.7% covered).

## Top 30 highest-priority missing annotations

Priority is biased toward `Result`-returning fallible paths in the
dependency-inversion core crates (`core`, `protocol`, `engine`,
`transfer`, `checksums`, `filters`, `metadata`, `daemon`, `cli`,
`signature`). Hot-path fallible functions whose dropped error would
silently corrupt state, mis-frame the wire protocol, or leak temp files
are highest impact.

| # | Kind | Location | Function |
|---|------|----------|----------|
| 1 | Result | `crates/core/src/timeout/tracker.rs:110` | `check_io_timeout` |
| 2 | Result | `crates/core/src/timeout/tracker.rs:158` | `check_connect_timeout` |
| 3 | Result | `crates/core/src/message/message_impl/render.rs:25` | `to_bytes` |
| 4 | Result | `crates/core/src/message/message_impl/render.rs:31` | `to_line_bytes` |
| 5 | Result | `crates/core/src/message/message_impl/render.rs:37` | `render_to_writer` |
| 6 | Result | `crates/core/src/message/message_impl/render.rs:45` | `render_line_to_writer` |
| 7 | Result | `crates/core/src/message/message_impl/render.rs:52` | `append_to_vec` |
| 8 | Result | `crates/core/src/message/message_impl/render.rs:59` | `append_line_to_vec` |
| 9 | Result | `crates/core/src/message/message_impl/render.rs:65` | `render_to_writer_inner` |
| 10 | Result | `crates/core/src/message/message_impl/render.rs:75` | `to_bytes_with_scratch_inner` |
| 11 | Result | `crates/core/src/message/message_impl/render.rs:86` | `append_to_vec_with_scratch_inner` |
| 12 | Result | `crates/core/src/message/message_impl/scratch_render.rs:68` | `to_bytes_with_scratch` |
| 13 | Result | `crates/core/src/message/message_impl/scratch_render.rs:73` | `to_line_bytes_with_scratch` |
| 14 | Result | `crates/core/src/message/message_impl/scratch_render.rs:78` | `render_to_writer_with_scratch` |
| 15 | Result | `crates/core/src/message/message_impl/scratch_render.rs:87` | `render_line_to_writer_with_scratch` |
| 16 | Result | `crates/core/src/message/message_impl/scratch_render.rs:96` | `append_to_vec_with_scratch` |
| 17 | Result | `crates/core/src/message/message_impl/scratch_render.rs:105` | `append_line_to_vec_with_scratch` |
| 18 | Result | `crates/core/src/message/segments/io.rs:37` | `write_to` |
| 19 | Result | `crates/core/src/message/segments/buffer.rs:48` | `try_extend_vec` |
| 20 | Result | `crates/core/src/message/segments/buffer.rs:84` | `extend_vec` |
| 21 | Result | `crates/core/src/message/segments/buffer.rs:119` | `copy_to_slice` |
| 22 | Result | `crates/core/src/message/segments/buffer.rs:161` | `to_vec` |
| 23 | Result | `crates/core/src/version/metadata.rs:108` | `write_standard_banner` |
| 24 | Result | `crates/core/src/version/report/renderer.rs:186` | `write_human_readable` |
| 25 | Result | `crates/core/src/client/progress.rs:167` | `new` |
| 26 | Result | `crates/core/src/client/config/iconv.rs:25` | `parse` |
| 27 | Result | `crates/core/src/client/config/compress_env.rs:19` | `force_no_compress_from_env` |
| 28 | Result | `crates/core/src/client/config/skip_compress.rs:35` | `parse_skip_compress_list` |
| 29 | Result | `crates/core/src/client/config/skip_compress.rs:58` | `skip_compress_from_env` |
| 30 | Result | `crates/core/src/client/config/bandwidth.rs:70` | `parse` |

These 30 are concentrated in `crates/core` because that crate is the
orchestration facade for both CLI and daemon flows: every dropped
`Result` here is a missed timeout, a partially rendered message, or a
silently mis-parsed config value that surfaces only later in the wire
stream. Fixing them is the highest leverage first batch.

## Hot-spots beyond the top 30

Total missing annotations grouped by crate (Result + Option):

| Crate | Missing total |
|-------|---------------|
| `protocol` | 325 |
| `engine` | 211 |
| `core` | 185 |
| `fast_io` | 182 |
| `transfer` | 160 |
| `rsync_io` | 118 |
| `metadata` | 114 |
| `daemon` | 111 |
| `cli` | 107 |
| `batch` | 43 |
| `compress` | 38 |
| `platform` | 38 |
| `flist` | 31 |
| `apple-fs` | 26 |
| `checksums` | 19 |
| `match` | 15 |
| `filters` | 10 |
| `signature` | 10 |
| `branding` | 9 |
| `logging-sink` | 9 |
| `bandwidth` | 7 |
| `embedding` | 7 |
| `logging` | 4 |

Representative samples used to scope the recommendation work:

- `protocol` - `crates/protocol/src/error.rs:53` `peer_versions`,
  `crates/protocol/src/error.rs:65` `unsupported_version`,
  `crates/protocol/src/error.rs:77` `malformed_legacy_greeting`.
- `engine` - `crates/engine/src/async_io/batch.rs:113` `copy_files`,
  `crates/engine/src/async_io/copier.rs:91` `copy_file`,
  `crates/engine/src/async_io/copier.rs:108` `copy_file_with_progress`.
- `transfer` - `crates/transfer/src/temp_cleanup.rs:95`
  `cleanup_stale_temp_files`, `crates/transfer/src/flags.rs:178` `parse`.
- `metadata` - `crates/metadata/src/apply_batch.rs:62`
  `apply_file_metadata`, `crates/metadata/src/copy_as.rs:48`
  `parse_copy_as_spec`, `crates/metadata/src/copy_as.rs:185`
  `switch_effective_ids`.
- `daemon` - `crates/daemon/src/systemd.rs:43` `ready`,
  `crates/daemon/src/systemd.rs:61` `status`,
  `crates/daemon/src/systemd.rs:75` `stopping`.
- `checksums` - `crates/checksums/src/crc32c.rs:143` `crc32c_file`,
  `crates/checksums/src/cpu_features.rs:165` `set_simd_override`.
- `filters` - `crates/filters/src/chain.rs:304` `enter_directory`,
  `crates/filters/src/set.rs:84` `from_rules`,
  `crates/filters/src/set.rs:221` `from_rules_with_cvs`.
- `signature` - `crates/signature/src/async_gen.rs:253`
  `request_signature`, `crates/signature/src/async_gen.rs:280`
  `wait_for_result`.
- `compress` - `crates/compress/src/zstd.rs:38` `new`,
  `crates/compress/src/zstd.rs:43` `finish`,
  `crates/compress/src/zstd.rs:67` `with_sink`.
- `bandwidth` - `crates/bandwidth/src/parse.rs:45`
  `parse_bandwidth_argument`, `crates/bandwidth/src/parse.rs:264`
  `parse_bandwidth_limit`.

## Recommendation

Land annotations in priority-ordered batches. Each batch is small enough
to land as a focused PR with a `chore:` or `style:` prefix and is
upstream-neutral - `#[must_use]` is purely a Rust attribute, no wire
format changes.

1. **Batch 1 - `core` rendering and timeout (32 functions).** Cover
   every entry in the Top 30 plus the remaining `core` `Result`
   functions. Highest leverage because both CLI and daemon paths funnel
   through `core`.
2. **Batch 2 - `protocol` constructors and parsers (~100 functions).**
   Annotate every fallible decoder, frame writer, and varint helper.
   Dropping a Result here mis-frames the wire stream.
3. **Batch 3 - `engine` delta and async I/O surface (~140 functions).**
   `copy_file`, `copy_files`, signature/match calls, all delta-script
   helpers. A dropped error here can corrupt files on disk.
4. **Batch 4 - `transfer`, `metadata`, `signature` (~270 functions).**
   These crates own per-file mutation; annotation prevents silent
   half-applied metadata or unverified signatures.
5. **Batch 5 - `daemon`, `cli` (~218 functions).** User-visible config
   parsing, systemd readiness, per-connection lifecycle - all benefit
   from compile-time enforcement.
6. **Batch 6 - remaining crates (`fast_io`, `rsync_io`, `compress`,
   `checksums`, `filters`, `bandwidth`, `branding`, `apple-fs`,
   `platform`, `match`, `flist`, `batch`, `embedding`, `logging`,
   `logging-sink`).** Sweep the long tail.

For each batch:

- Apply `#[must_use]` to the function (preferred) when the result type
  is generic or shared across crates.
- Apply `#[must_use]` to the **type** (in the crate that owns it) when a
  single result enum (`io::Result`, `Result<(), Error>`, etc.) is
  returned by many functions - this is more concise but requires the
  type to live in a crate the project owns.
- For builder-style `pub fn`s that return `Self` for chaining, keep the
  existing `#[must_use]` (most are already covered today). Skip
  `Option::map`-style adapter wrappers that exist solely to forward.
- Audit the resulting compile output - `cargo clippy --workspace
  --all-targets --all-features --no-deps -- -D warnings` already runs in
  CI and will catch any newly-flagged `unused_must_use` warnings if
  internal callers were dropping results. Fix call sites by binding to
  `_` only when the dropped error is genuinely unobservable
  (documented), otherwise propagate with `?`.

## Already-clean crates

| Crate | `Result` `pub fn` | `Option` `pub fn` | Status |
|-------|-------------------|-------------------|--------|
| `test-support` | 0 | 0 | no public Result/Option fns |
| `windows-gnu-eh` | 0 | 0 | no public Result/Option fns |

No production crate currently has 100% `#[must_use]` coverage on both
kinds. `fast_io` is the closest at 95.7% Option coverage and 1.1%
Result coverage; the gap is dominated by io_uring submission helpers.

## Refresh procedure

```sh
python3 tools/audit/must_use_audit.py
```

The scanner is hermetic, requires no `cargo` build, and runs in under a
second on the full workspace. Re-run after every batch lands and update
the tables above to track coverage progress.
