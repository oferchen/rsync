# zsync large-dataset benchmark (10 GB sparse VM image)

Tracks issue #2082. Companion to the in-process Criterion benchmark
`crates/matching/benches/zsync_medium_dataset.rs` (100 MiB / 5%, #2081).
This scenario exercises the full match-index + rolling-hash pipeline on
a corpus large enough to stress L3 cache, disk I/O scheduling, and the
delta-emission backpressure path simultaneously.

The benchmark is gated behind `OC_RSYNC_LARGE_BENCH=1` so it never
runs in default CI. It is intended for release qualification jobs only.

## Scenario specification

| Property | Value |
|----------|-------|
| Basis size | 10 GiB (configurable via `BENCH_SIZE_GB`) |
| Sector size | 64 KiB (configurable via `BENCH_SECTOR_KB`) |
| Total sectors | 163,840 |
| Fill ratio | 0.35 (35% of sectors hold random data; the rest are filesystem holes) |
| Modify fraction | ~1% of basis bytes (configurable via `BENCH_MODIFY_PCT`) |
| Modification layout | 1000 scattered patches of ~100 KiB each at deterministic offsets |
| rsync block size | 128 KiB (configurable via `BLOCK_SIZE`) |
| rsync flags | `--inplace --no-whole-file --stats --block-size=$BLOCK_SIZE` |
| Generation seed | LCG starting from `2654435761` (basis) and `1442695041` (patches), deterministic across runs |

The corpus mimics a "moderately used VM disk image": large allocated
logical size, sparse zero regions, and dense bursts of random sector
content. The 1% modification rate matches the typical day-to-day churn
on a long-running VM (config tweaks, log appends, package installs).
This is the regime where zsync-style match-index optimisations pay off:
the rolling hash should find a copy match for ~99% of the target stream
and emit literal data only at the patched offsets.

## Running

```sh
# Local (Linux):
OC_RSYNC_LARGE_BENCH=1 ./scripts/zsync_bench_large_dataset.sh

# Release-qualification CI wrapper (same behavior, with prereq checks):
OC_RSYNC_LARGE_BENCH=1 ./tools/ci/run_zsync_large_bench.sh

# Skip the upstream comparison (oc-rsync only):
OC_RSYNC_LARGE_BENCH=1 UPSTREAM_RSYNC=skip ./scripts/zsync_bench_large_dataset.sh

# Smaller smoke test (1 GiB instead of 10 GiB):
OC_RSYNC_LARGE_BENCH=1 BENCH_SIZE_GB=1 ./scripts/zsync_bench_large_dataset.sh
```

Scratch space: `${TMPDIR:-/tmp}/oc-rsync-zsync-large.$$`. Removed on
exit. The script never invokes `rm -rf` on a path expanded from a
non-PID-suffixed variable, so it is safe to run inside the
`rsync-profile` container even with bind-mounted workspaces.

## Expected runtime and resource use

On a typical Linux runner (8 cores, 16 GiB RAM, NVMe SSD):

| Phase | Wall clock (typical) |
|-------|----------------------|
| Basis generation (10 GiB sparse, 35% filled) | 60 - 120 s |
| Target generation (cp + 1000 patches) | 20 - 40 s |
| oc-rsync delta sync | 60 - 180 s |
| upstream rsync delta sync (optional) | 90 - 240 s |
| Total | 4 - 9 minutes (single-digit minutes) |

Disk: peak ~22 GiB of scratch (basis + two target copies, all sparse;
allocated size is ~3.5 GiB given the 35% fill ratio plus the patched
sectors).

Memory: oc-rsync RSS dominated by the signature index for a 10 GiB
basis at 128 KiB blocks (~80K blocks x ~32 bytes per entry = ~2.5 MiB
on the wire; in-memory representation typically <50 MiB). Peak RSS
target: <500 MiB.

## How to interpret results

The script emits a TSV row per binary into
`target/benchmarks/zsync_large_<timestamp>.tsv`:

```
binary       wall_s   peak_rss_kb   transferred_bytes
oc-rsync     78.412   312456        118231040
upstream     142.038  198432        119876608
```

Three signals matter for the release decision:

1. **Wall clock.** oc-rsync should be at parity or faster than
   upstream. A regression versus the previous release tag (compare TSVs
   across runs) is a release blocker if it exceeds 10%.
2. **Transferred bytes.** Both binaries should send close to the
   1% modification rate (~100 - 150 MiB for a 10 GiB / 1% scenario;
   the overhead above 100 MiB comes from block-boundary alignment of
   the patches and the file-list / signature framing). A material
   divergence (say, 2x more bytes than upstream) indicates the
   match-index path is missing copy opportunities. Cross-reference
   against `bithash_rejection.rs` and `prune_duplicate_heavy.rs` to
   identify whether the regression is in the bithash filter or the
   strong-checksum verification.
3. **Peak RSS.** Should stay below the ~500 MiB target. A blow-up
   here typically points at `BufferPool` count-based budgeting
   (see `project_bufferpool_count_cap.md`) or a signature-index
   leak.

## Companion microbenchmark results (lxhost, 2026-07-13)

Authoritative re-bench on the aarch64 Linux validation host (16 cores) of
the Criterion cells wired into `bench-zsync-matching.yml`. These are the
in-process matching microbenchmarks, run alongside the large-dataset
scenario above.

### Parallel delta scan (`parallel_delta_scan`)

256 MiB duplicate-free basis, opt-in parallel sender-delta scan across a
rising worker count. Every configuration is wire-byte-identical to the
sequential (1-chunk) scan for a duplicate-free basis.

| Workers | Wall clock | Throughput | Speedup vs sequential |
|---------|-----------|-----------|-----------------------|
| 1 (sequential) | 1.858 s | 137.8 MiB/s | 1.00x |
| 2 | 0.931 s | 275.1 MiB/s | 2.00x |
| 4 | 0.582 s | 439.5 MiB/s | 3.19x |
| 8 | 0.556 s | 460.1 MiB/s | 3.34x |
| 16 | 0.549 s | 466.6 MiB/s | 3.39x |

Scaling is near-linear to 4 workers, then flattens as the scan becomes
memory-bandwidth bound. The knee is around 8 workers (~3.3x); beyond that
extra workers add little. Default remains off - the path only engages
behind its opt-in flag on a duplicate-free basis.

### Other matching cells

| Cell | Peak throughput |
|------|-----------------|
| `zsync_optimizations` (medium corpus) | ~677 MiB/s |
| `bithash_rejection` (micro) | ~5.19 GiB/s |
| `compact_keys_cache` (micro) | ~418 Melem/s |

## Integration with release qualification

The release pipeline should run:

```sh
OC_RSYNC_LARGE_BENCH=1 ZSYNC_LARGE_BENCH_REQUIRED=1 \
    ./tools/ci/run_zsync_large_bench.sh
```

on the same Linux runner used for the v0.5.x benchmark suite. The TSV
output is archived as a release artifact and compared against the
previous tag's run. Numbers feed into the release notes under the
"Performance" section.

For the in-process matching microbenchmarks (cheap, run on every PR),
see `crates/matching/benches/zsync_medium_dataset.rs` (100 MiB) and the
companion bithash / seq-match / prune cells.
