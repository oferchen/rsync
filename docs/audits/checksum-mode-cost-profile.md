# Profile checksum-mode (`-c` / `--checksum`) computation cost

Tracking issue: oc-rsync task #1041.

## Summary

`--checksum` (`-c`) replaces size+mtime quick-check with a full content
hash on every regular file, on both sides of the transfer. Dispatch is
wire-faithful with upstream but never profiled. This audit locates
dispatch, itemises per-file cost, defines a criterion+perf plan,
compares against upstream, and lists three wire-compatible wins.

## 1. Dispatch site in `crates/transfer/`

Parsed into `TransferFlags::checksum` (`crates/transfer/src/flags.rs:39,232,270`).
Generator-side population of per-entry checksums in the file list lives
at `crates/transfer/src/generator/mod.rs:551-560`:

```rust
if self.config.flags.checksum {
    let factory = ChecksumFactory::from_negotiation(...);
    writer = writer.with_always_checksum(factory.digest_length());
}
```

Receiver-side dispatch is at
`crates/transfer/src/receiver/transfer/candidates.rs:111`, lifting the
flag into `Option<ChecksumAlgorithm>` and forwarding to
`quick_check_matches` (line 145) and `try_reference_dest` (line 182).
Hot path: `file_checksum_matches` in
`crates/transfer/src/receiver/quick_check.rs:225-250`:

```rust
let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
let mut buf = [0u8; 64 * 1024];
while remaining > 0 { file.read_exact(&mut buf[..to_read])?; hasher.update(...); }
```

Mirrors `generator.c:626 quick_check_ok()` and `checksum.c:402
file_checksum()` (no rolling, no seed).

## 2. Per-file cost breakdown

For every regular file whose size matches the source entry:

1. `fs::File::open` (1 `open` syscall + path resolution).
2. Read entire file in 64 KiB chunks via `read_exact` -
   `ceil(size / 65536)` `read` syscalls. No `mmap`, no readahead hint.
3. `ChecksumVerifier::update` per chunk - MD5/MD4/XXH3/XXH128
   compression over the whole file. Algorithm chosen by negotiation.
4. `finalize_into` and truncated comparison against `expected.len()`.
5. Implicit close on drop.

Cost: **O(file_size)** CPU plus **O(file_size / 65536)** syscalls. No
buffer pool reuse - the 64 KiB stack array is re-zeroed per call. SIMD
fast paths (AVX2/SSE2/NEON) apply only to XXH3/XXH128; MD5/MD4 are
scalar.

Receiver runs this inside the sequential Phase C loop in
`build_files_to_transfer` (line 134). Sender does the same work during
file-list construction. **Both ends read every candidate file
end-to-end**: wall-clock cost of `-c` is at least
`2 * sum(file_size) / min(sender_bw, receiver_bw)` plus hash CPU.

## 3. Profile plan

Two synthetic workloads exercising the small-file and large-file regimes:

| Workload | Files | Size each | Total | Stresses |
|---|---|---|---|---|
| `many_small` | 100 000 | 4 KiB | ~400 MiB | syscall + open overhead |
| `few_large`  | 1 000   | 100 MiB | ~100 GiB | hash CPU + read bandwidth |

Harness:

- **criterion** - new bench `crates/transfer/benches/checksum_mode.rs`
  generating corpora into `tempfile::TempDir` once per group, benching
  `build_files_to_transfer` end-to-end with `-c` on/off across MD5,
  MD4, XXH3 (negotiated), XXH128.
- **perf flame graph** (Linux): `perf record -F 999 -g -- oc-rsync -ac
  src/ dst/` then `perf script | inferno-flamegraph > flame.svg`, run
  inside the `rsync-profile` podman container.
- **macOS**: `cargo instruments -t "Time Profiler"` for parity.

Metrics: wall time, `perf stat -e task-clock,cycles,instructions,
cache-misses,page-faults`, `strace -c` syscalls. Compare against
upstream rsync 3.4.1 on the same corpora.

## 4. Comparison against upstream rsync `-c`

Same I/O shape: upstream `checksum.c:402 file_checksum()` opens, reads
via `map_ptr`/`map_file` (`fileio.c`) in 256 KiB windows (mmap when the
FS supports it, else `read`), hashes, closes. Same algorithm dispatch
via the negotiated `xfer_sum_struct` (`checksum.c:288 sum_init`). Two
oc-rsync gaps:

- Upstream `map_ptr` can `mmap`; oc-rsync always uses a 64 KiB read
  loop.
- Upstream MD5 is hand-tuned C; oc-rsync MD5/MD4 are scalar Rust
  without SIMD. XXH3 parity is good.

No wire-protocol divergence; file-list checksum payload identical under
protocols 30-32.

## 5. Optimisation candidates

All wire-compatible (no protocol changes):

1. **Parallel stat-and-checksum.** Phase B already parallelises `stat`
   via `parallel_io::map_blocking`. Extend Phase C so
   `file_checksum_matches` runs on the same rayon pool keyed by
   `parallel_thresholds.stat`, with a small-file cutoff (e.g. > 1 MiB)
   to avoid overhead on `many_small`. Expected win: linear speed-up on
   `few_large` up to disk bandwidth.
2. **`mmap` for large files.** Replace the read-loop with
   `memmap2::Mmap` above a threshold (e.g. 1 MiB), matching upstream's
   `map_ptr`. Eliminates the user-space copy and 64 KiB stack zeroing;
   enables kernel readahead. Keep the read-loop fallback for
   non-mappable FDs and Windows non-file handles.
3. **XXH3 fast-path under proto 32.** When negotiation lands on
   XXH3/XXH128, use the SIMD-accelerated `xxh3` path already in
   `ChecksumVerifier`. Audit `ChecksumFactory::from_negotiation` to
   confirm XXH3 is preferred whenever the peer advertises it; document
   the fallback chain (XXH128 -> XXH3 -> MD5 -> MD4) so scalar MD5 only
   runs against pre-3.2 peers.

Rolling up: parallel + mmap should bring `few_large` `-c` within 5% of
`cp` plus hash CPU; XXH3 negotiation lifts the MD5 CPU ceiling on the
cross-version matrix.
