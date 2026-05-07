# `unwrap()` / `expect()` Audit in Library Crates

Tracking issue: [#2122](https://github.com/oferchen/oc-rsync/issues/2122).

## 1. Baseline

```sh
rg -n "\.unwrap\(\)|\.expect\(" crates/ | grep -v test | grep -v bench | wc -l
```

Total raw matches: **5757** across 1293 source files. The naive `grep -v test`
filter still keeps inline `#[cfg(test)] mod tests` blocks, which dominate the
count. Manual sampling of high-count files shows the majority of matches are
in test bodies, doc-comment examples, or `#[test]` helpers rather than
production paths. After excluding inline `mod tests` and rustdoc samples,
production-only sites land near 80 workspace-wide.

### Per-crate distribution (top 12)

| Crate     | Raw | Notes                                                    |
|-----------|----:|----------------------------------------------------------|
| protocol  | 934 | wire-format encoder/decoder roundtrip tests              |
| engine    | 904 | `local_copy` / pipeline / filter-program test modules    |
| fast_io   | 832 | platform copy / iouring / reflink test scaffolding       |
| metadata  | 468 | xattr / acl / ownership unit tests                       |
| transfer  | 441 | session / stats fixture tests                            |
| rsync_io  | 369 | session-state doctest examples                           |
| core      | 357 | builder / orchestration test modules                     |
| cli       | 279 | clap integration tests, frontend snapshot tests          |
| checksums | 169 | SIMD parity tests                                        |
| compress  | 162 | codec roundtrip tests                                    |
| daemon    | 124 | auth / listener tests + one `0.0.0.0:873`.parse() literal |
| flist     | 108 | wire-format tests                                        |

Smaller crates: `filters` 98, `signature` 91, `bandwidth` 82, `logging` 58,
`batch` 55, `match` 53, `branding` 44, `apple-fs` 44, `platform` 40,
`logging-sink` 32, `embedding` 13.

## 2. Categorization

Sampling production sites in each crate places every call into one of three
buckets:

1. **Genuine invariants (keep with `expect("invariant: ...")`)**.
   - `crates/bandwidth/src/async_limiter.rs:58` -
     `NonZeroU64::new(bytes_per_second).expect("bytes_per_second must be greater than zero")`.
   - `crates/signature/src/parallel.rs:94` -
     `NonZeroUsize::new(strong_len).expect("strong digest length requested by layout must be non-zero")`.
   - `crates/daemon/src/daemon/async_session/listener.rs:63` -
     `"0.0.0.0:873".parse().unwrap()` (infallible string literal -> `SocketAddr`).
   - Mutex `lock().unwrap()` in pools where no panic path holds the guard.
2. **Startup-only / process-exits**. CLI parse, config-file load, daemon bind.
   A panic at startup converts to exit code 1 via the panic hook before any
   transfer begins. Acceptable as-is.
3. **Lazy (convert to `Result`)**. Smallest bucket. Candidates surface in the
   engine pipeline glue (`local_copy/pipelined_state.rs`, controller dequeue
   helpers) where `dequeue_ready().unwrap()` could become
   `try_dequeue() -> Option<Entry>` and propagate cleanly.

Test-body matches (most of the raw 5757) are out of scope: panicking in a
unit test is the correct failure signal.

## 3. Top 10 crates by raw count

`protocol`, `engine`, `fast_io`, `metadata`, `transfer`, `rsync_io`, `core`,
`cli`, `checksums`, `compress`. Test bodies dominate every entry.

## 4. Triage criteria

Keep with `.expect("invariant: ...")` when:

- Argument has been validated by the caller (`NonZero*`, parsed literal, enum
  exhausted by a preceding `match`).
- Poisoned-mutex case is unreachable because no panic path holds the lock.
- Site runs only at startup; failure would otherwise call `process::exit`.

Convert to `Result` when:

- The value is derived from external input (filesystem, network, env).
- The error is reachable through normal API misuse.
- Existing callers already return `io::Result` / `anyhow::Result`.

The `.expect("invariant: ...")` form replaces bare `.unwrap()` everywhere a
site is kept, so the docstring records why it is infallible.

## 5. Action plan

- Land per-crate cleanup PRs in the order: `engine`, `transfer`, `core`,
  `daemon`, `protocol`, then the remainder. Each PR converts lazy sites and
  rewrites kept sites to `.expect("invariant: ...")`.
- After each PR re-run the baseline command and record the delta in this
  file until production-only sites drop below 20 workspace-wide.
- At that threshold, enable workspace-wide
  `clippy::unwrap_used = "warn"` and `clippy::expect_used = "warn"` in the
  `[workspace.lints.clippy]` block of the root `Cargo.toml`, with
  `#[cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]` on
  test modules and doctests.
- Promote both lints to `deny` in the final PR after a clean CI run.
