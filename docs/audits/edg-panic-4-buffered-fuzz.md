# EDG-PANIC.4: `BufferedMap` fuzz coverage

## Background

PR #5566 and PR #5569 closed the UTS-18.f / UTS-18.g panic class in
`crates/transfer/src/map_file/buffered.rs`:

1. **PR #5566** turned the `copy_within` and `&buf[start..end]` panic sites
   into fail-loud `io::ErrorKind::InvalidData` errors. The original abort:

   ```
   range end index 262144 out of range for slice of length 48128
   ```

2. **PR #5569** added a root-cause clamp at the single `window_len`
   assignment site in `load_window` so a legitimate 48128-byte basis file
   paired with the 262144-byte default window no longer trips PR #5566's
   guards.

This document records the fuzz target and corpus that lock the regression
in place going forward.

## Fuzz target

`fuzz/fuzz_targets/buffered_map.rs` drives both `BufferedMap` panic sites
through the public `MapStrategy::map_ptr` entry point that all four
untrusted callers (`transfer_ops/token_loop.rs`,
`transfer_ops/response.rs`, `receiver/transfer/sync.rs`,
`delta_apply/applicator.rs`) share.

The harness packs six `u64` LE words at the head of each input:

| Bytes  | Field           | Cap                    |
| ------ | --------------- | ---------------------- |
| 0..8   | `file_size`     | `512 KiB`              |
| 8..16  | `window_size`   | `512 KiB` (min `1`)    |
| 16..24 | `request_start` | `512 KiB`              |
| 24..32 | `request_len`   | `512 KiB`              |
| 32..40 | `second_start`  | `512 KiB`              |
| 40..48 | `second_len`    | `512 KiB`              |
| 48..   | file payload    | tiled to `file_size`   |

Two `map_ptr` calls per input cover the empty-cache load path (first
request) and the forward-overlap branch of `load_window` (second
request, which is the historically panicking path through
`copy_within`).

`map_ptr` MUST return `Ok` or `Err` — never panic. libFuzzer treats any
panic as a finding under `fuzz/artifacts/buffered_map/`.

## Corpus seeds

`fuzz/corpus/buffered_map/` ships three deterministic seeds derived from
the UTS-18 trigger scenarios:

| Seed                          | file_size | window_size | First request | Second request | Coverage |
| ----------------------------- | --------- | ----------- | ------------- | -------------- | -------- |
| `uts_18_overlap_shrink.bin`   | 48128     | 262144      | (0, 48128)    | (0, 48128)     | PR #5569 production crash ratio (file < `MAX_MAP_SIZE`) |
| `uts_18_truncated.bin`        | 0         | 4096        | (0, 0)        | (0, 0)         | Empty-payload / 48-byte header boundary |
| `uts_18_zero_len.bin`         | 1024      | 256         | (512, 0)      | (0, 0)         | `len == 0` short-circuit in `map_ptr` |

The seeds give libFuzzer a stable starting basin so mutation-driven
exploration can reach the overlap-shrink edge without first having to
randomly synthesise valid header bytes.

## CI integration

`buffered_map` is listed in `.github/workflows/fuzz-coverage-report.yml`
matrix and is auto-discovered by the smoke run in
`.github/workflows/fuzz-coverage.yml`. The nightly schedule (03:30 UTC)
runs each target for 5 minutes and uploads the lcov artifact for trend
analysis.

## Cross-references

- `crates/transfer/src/map_file/buffered.rs` — guarded `load_window`
  and `map_ptr` paths.
- PR #5566 — fail-loud `copy_within` + slice guards.
- PR #5569 — `window_len` clamp at the source.
- `docs/audits/edg-panic-5-unwrap-on-slice.md` — sibling audit covering
  the broader `.unwrap()` / `.expect()` slice-driven panic class.
