# io_uring bench workload size inventory (IUB-1)

Tracking: IUB-1 (#2854). Companion tasks IUB-2/IUB-3 will design bench cells
at 2 GB+ / 100 K-file+ scales after this inventory locks down what is
already in-tree.

## Purpose

Map every io_uring bench cell currently shipped in this repo - its file,
the cell name, the workload size, the file-count profile, the disk-class
assumption baked into the harness, the feature flags it exercises, and
any last-measured speedup vs the standard-I/O baseline. The output feeds
IUB-2/IUB-3, which will close the gap at 2 GB+ workloads where the
current cells are too small for io_uring to pay off.

## Background

The headline release-benchmark workload sits at 148.3 MB across 10 000
files (see `docs/design/inc-recurse-sender-reenable-audit.md:182` and
`docs/ssh-transport-decision-matrix.md:20`). At that scale, the io_uring
data-path benches that ship today report roughly 1.00x against their
stdlib baseline - per-op kernel-async dispatch cost is comparable to the
syscalls it eliminates, and the working set fits inside page cache on a
CI runner. IUD-4 (#2364) and IUD-9 (#2369) established the prototype and
production-wrapper benches respectively; both target the same 10 x 1 GiB
shape and are env-gated off by default.

No IUD-4 or IUD-9 measured numbers are committed to this repo. Neither
the `CHANGELOG.md` entry for IUD-9 (#4398) nor the inline doc comments
on `nvme_data_path*.rs` carry a numeric baseline; the benches must be
run by hand under `OC_RSYNC_BENCH_NVME_DATA_PATH=1` on a Linux 5.6+ host
to produce a number. The "1.00x at 148 MB" datum cited by IUB-1 lives in
the project memory note `project_iouring_marginal_at_small_bench_scale`,
not in any tracked file.

## Inventory table

Columns: `file` (path) | `cell` (criterion bench-id) |
`workload_size` (bytes per iter, computed from constants) |
`file_count` (files per iter) | `disk_class` (assumed storage) |
`features` (cargo features + env gates required) |
`last_measured_speedup_if_known`.

| file | cell | workload_size | file_count | disk_class | features | last_measured_speedup_if_known |
|------|------|---------------|------------|------------|----------|-------------------------------|
| `crates/fast_io/benches/io_optimizations.rs:165-220` | `io_uring/standard_io/64kb` + `io_uring/io_uring/64kb` | 64 KiB | 1 (read) | unspecified (`tempdir`, CI ramdisk likely) | `fast_io` features `io_uring` (Linux only) | not committed |
| `crates/fast_io/benches/io_optimizations.rs:165-220` | `io_uring/standard_io/1mb` + `io_uring/io_uring/1mb` | 1 MiB | 1 (read) | unspecified (`tempdir`) | `io_uring` | not committed |
| `crates/fast_io/benches/io_optimizations.rs:165-220` | `io_uring/standard_io/10mb` + `io_uring/io_uring/10mb` | 10 MiB | 1 (read) | unspecified (`tempdir`) | `io_uring` | not committed |
| `crates/fast_io/benches/io_optimizations.rs:225-282` | `io_uring_writes/standard_io/64kb` + `io_uring_writes/io_uring/64kb` | 64 KiB | 1 (write) | unspecified (`tempdir`) | `io_uring` | not committed |
| `crates/fast_io/benches/io_optimizations.rs:225-282` | `io_uring_writes/standard_io/1mb` + `io_uring_writes/io_uring/1mb` | 1 MiB | 1 (write) | unspecified (`tempdir`) | `io_uring` | not committed |
| `crates/fast_io/benches/iouring_per_file_vs_shared.rs:80-303` | `iouring_per_file_vs_shared/per_file_ring` | 4 KiB x 100 000 = 400 MiB total | 100 000 (many-files write) | unspecified (`tempdir`); env override via dir choice | `io_uring`; env gate `OC_RSYNC_BENCH_IOURING_RING=1`; Linux 5.6+ | not committed |
| `crates/fast_io/benches/iouring_per_file_vs_shared.rs:80-303` | `iouring_per_file_vs_shared/shared_ring` | 4 KiB x 100 000 = 400 MiB total | 100 000 (many-files write) | unspecified (`tempdir`) | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1`; Linux 5.6+ | not committed |
| `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs:104-353` | `iouring_sqpoll_vs_regular/stdfs` | 4 KiB x 100 000 = 400 MiB total | 100 000 (many-files write) | unspecified (`tempdir`) | `io_uring` | not committed |
| `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs:104-353` | `iouring_sqpoll_vs_regular/iouring_regular` | 4 KiB x 100 000 = 400 MiB total | 100 000 (many-files write) | unspecified (`tempdir`) | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1`; Linux 5.6+ | not committed |
| `crates/fast_io/benches/iouring_sqpoll_vs_regular.rs:104-353` | `iouring_sqpoll_vs_regular/iouring_sqpoll` | 4 KiB x 100 000 = 400 MiB total | 100 000 (many-files write) | unspecified (`tempdir`) | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1` + `OC_RSYNC_BENCH_IOURING_SQPOLL=1`; Linux 5.13+ unprivileged or 5.6-5.12 with `CAP_SYS_NICE` | not committed |
| `crates/fast_io/benches/iocp_vs_iouring_matched.rs:190-352` (linux_cells) | `iocp_vs_iouring_matched/std_baseline/{4096,65536,1048576}` | 4 KiB / 64 KiB / 1 MiB x 1000 = 4 MiB / 64 MiB / 1 GiB total per row | 1 000 (many-files write) | unspecified (`tempdir`); compares to Windows-host run | `io_uring` (Linux); env gate `OC_RSYNC_BENCH_IOURING_RING=1` | not committed |
| `crates/fast_io/benches/iocp_vs_iouring_matched.rs:190-352` | `iocp_vs_iouring_matched/iouring_default/{4096,65536,1048576}` | 4 KiB / 64 KiB / 1 MiB x 1000 | 1 000 (many-files write) | unspecified | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1` | not committed |
| `crates/fast_io/benches/iocp_vs_iouring_matched.rs:190-352` | `iocp_vs_iouring_matched/iouring_concurrent_ops_8/{4096,65536,1048576}` | 4 KiB / 64 KiB / 1 MiB x 1000 | 1 000 (many-files write) | unspecified | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1` | not committed |
| `crates/fast_io/benches/iocp_vs_iouring_matched.rs:190-352` | `iocp_vs_iouring_matched/iouring_sqpoll/{4096,65536,1048576}` | 4 KiB / 64 KiB / 1 MiB x 1000 | 1 000 (many-files write) | unspecified | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1` + `OC_RSYNC_BENCH_IOURING_SQPOLL=1` | not committed |
| `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs:130-end` | `mmap/sequential/4MiB` | 4 MiB | 1 (single-file basis read) | unspecified (`tempdir`) | `io_uring` (compile gate); mmap cell needs no env gate | not committed |
| `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs:130-end` | `mmap/sequential/64MiB` | 64 MiB | 1 (single-file basis read) | unspecified | `io_uring` | not committed |
| `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs:130-end` | `mmap/sequential/1GiB` | 1 GiB | 1 (single-file basis read) | unspecified | `io_uring`; env gate `OC_RSYNC_BENCH_LARGE=1` | not committed |
| `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs:130-end` | `read_fixed_sqpoll/sequential/4MiB` | 4 MiB | 1 (single-file basis read) | unspecified | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1` + `OC_RSYNC_BENCH_IOURING_SQPOLL=1`; Linux 5.13+ (or 5.6-5.12 with `CAP_SYS_NICE`) | not committed |
| `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs:130-end` | `read_fixed_sqpoll/sequential/64MiB` | 64 MiB | 1 (single-file basis read) | unspecified | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1` + `OC_RSYNC_BENCH_IOURING_SQPOLL=1` | not committed |
| `crates/fast_io/benches/mmap_vs_read_fixed_basis.rs:130-end` | `read_fixed_sqpoll/sequential/1GiB` | 1 GiB | 1 (single-file basis read) | unspecified | `io_uring`; `OC_RSYNC_BENCH_IOURING_RING=1` + `OC_RSYNC_BENCH_IOURING_SQPOLL=1` + `OC_RSYNC_BENCH_LARGE=1` | not committed |
| `crates/fast_io/benches/nvme_data_path.rs:140-end` | `nvme_data_path/stdlib_write/10x1GiB` (IUD-4) | 1 GiB x 10 = 10 GiB | 10 (large-file write stream) | NVMe assumed (`OC_RSYNC_BENCH_NVME_PATH` opt; falls back to `tempdir`/ramdisk) | `io_uring`; env gate `OC_RSYNC_BENCH_NVME_DATA_PATH=1`; Linux 5.6+ | not committed (#4381 / #2364) |
| `crates/fast_io/benches/nvme_data_path.rs:140-end` | `nvme_data_path/iouring_write_fixed/10x1GiB` (IUD-4) | 10 GiB | 10 (large-file write stream) | NVMe assumed | `io_uring`; `OC_RSYNC_BENCH_NVME_DATA_PATH=1` | not committed (#4381 / #2364) |
| `crates/fast_io/benches/nvme_data_path_production.rs:120-end` | `production/stdlib_write/10x1GiB` (IUD-9) | 10 GiB | 10 (large-file write stream) | NVMe assumed | `iouring-data-writes` + `iouring-data-reads`; `OC_RSYNC_BENCH_NVME_DATA_PATH=1`; Linux 5.6+ | not committed (#4398 / #2369) |
| `crates/fast_io/benches/nvme_data_path_production.rs:120-end` | `production/iouring_write/10x1GiB` (IUD-9) | 10 GiB | 10 (large-file write stream) | NVMe assumed | `iouring-data-writes` + `iouring-data-reads`; `OC_RSYNC_BENCH_NVME_DATA_PATH=1` | not committed (#4398 / #2369) |
| `crates/fast_io/benches/nvme_data_path_production.rs:120-end` | `production/stdlib_read/10x1GiB` (IUD-9) | 10 GiB | 10 (large-file read stream) | NVMe assumed | `iouring-data-writes` + `iouring-data-reads`; `OC_RSYNC_BENCH_NVME_DATA_PATH=1` | not committed (#4398 / #2369) |
| `crates/fast_io/benches/nvme_data_path_production.rs:120-end` | `production/iouring_read/10x1GiB` (IUD-9) | 10 GiB | 10 (large-file read stream) | NVMe assumed | `iouring-data-writes` + `iouring-data-reads`; `OC_RSYNC_BENCH_NVME_DATA_PATH=1` | not committed (#4398 / #2369) |
| `crates/fast_io/benches/ius_3_send_zc_vs_send.rs:128-183` | `ius_3_send_zc_vs_send/send_plain/small_chunks_16KiB_x_10000` | 16 KiB x 10 000 = 160 MiB on the wire | 10 000 calls over 1 TCP loopback pair | loopback (no disk) | `io_uring`; env gate `OC_RSYNC_BENCH_IUS_3=1`; Linux 5.6+ | not committed |
| `crates/fast_io/benches/ius_3_send_zc_vs_send.rs:128-183` | `ius_3_send_zc_vs_send/send_plain/medium_chunks_256KiB_x_1000` | 256 KiB x 1000 = 256 MiB | 1 000 calls | loopback | `io_uring`; `OC_RSYNC_BENCH_IUS_3=1` | not committed |
| `crates/fast_io/benches/ius_3_send_zc_vs_send.rs:128-183` | `ius_3_send_zc_vs_send/send_plain/large_chunks_1MiB_x_100` | 1 MiB x 100 = 100 MiB | 100 calls | loopback | `io_uring`; `OC_RSYNC_BENCH_IUS_3=1` | not committed |
| `crates/fast_io/benches/ius_3_send_zc_vs_send.rs:128-183` | `ius_3_send_zc_vs_send/send_plain/mixed_chunks_4KiB_to_1MiB_x_1000` | 4 KiB-1 MiB random x 1000 (deterministic LCG) | 1 000 calls | loopback | `io_uring`; `OC_RSYNC_BENCH_IUS_3=1` | not committed |
| `crates/fast_io/benches/ius_3_send_zc_vs_send.rs:128-183` | `ius_3_send_zc_vs_send/send_zc/{same four shapes}` | as above | 10 000 / 1 000 / 100 / 1 000 calls | loopback | `iouring-send-zc` (also gated on runtime `send_zc_supported()`, Linux 6.0+); `OC_RSYNC_BENCH_IUS_3=1` | not committed |
| `scripts/benchmark_io_optimizations.sh:79-97` | Wraps `cargo bench -p fast_io --features mmap,io_uring -- --save-baseline phase1_optimizations` (runs every `io_optimizations.rs` cell above) | inherits from `io_optimizations.rs` (64 KiB / 1 MiB / 10 MiB read; 64 KiB / 1 MiB write) | 1 file per cell | unspecified | `io_uring`, `mmap`; Linux 5.6+ | not committed |
| `scripts/benchmark_io_optimizations.sh:103-113` | Wraps `cargo bench -p transfer --bench map_file_benchmark` and `token_buffer_benchmark` (mmap + buffer, no direct io_uring dispatch) | inherits from those benches | varies | unspecified | none io_uring-specific | not committed |

## Related but not strictly io_uring

These cells exercise paths adjacent to io_uring (rayon dispatch, platform
copy primitives, copy_file_range threshold) but do not call into the
io_uring submission code. Recorded here so IUB-2/IUB-3 do not duplicate
them when designing the 2 GB+ cells.

- `crates/transfer/benches/par_bridge_vs_deque.rs` - 100 K small-file
  rayon dispatch shape. No io_uring dispatch.
- `crates/transfer/benches/sync_channel_overhead.rs` - 32 / 256 / 4096
  byte payloads through sync vs crossbeam channels. No io_uring
  dispatch.
- `crates/engine/benches/per_op_thresholds.rs` - parallel-stat sweep
  over 32 / 64 / 128 / 256 files plus `copy_file_range` at 4 KiB /
  64 KiB / 1 MiB. `copy_file_range` rides on `fast_io::copy_file_contents`
  which can dispatch through io_uring when available.
- `crates/fast_io/benches/platform_copy.rs` - 4 KiB / 64 KiB / 1 MiB /
  16 MiB single-file copy through `DefaultPlatformCopy`. Linux dispatch
  may select io_uring under the hood but the bench does not isolate it.
- `crates/fast_io/benches/platform_copy_gap.rs` - 4 KiB x 100 / 64 KiB
  x 100 / 1 MiB x 100 / 16 MiB x 10 / 256 MiB x 2 cells. Same dispatcher
  caveat as `platform_copy.rs`.
- `crates/fast_io/benches/iocp_vs_stdio.rs` - 4 KiB / 64 KiB / 1 MiB x
  1000, Windows-only IOCP path. No io_uring dispatch.
- `crates/fast_io/benches/splice_pipe.rs` - 64 KiB / 1 MiB / 16 MiB
  splice/vmsplice. Pipe path, not io_uring.

## Observations for IUB-2 and IUB-3

1. The only cells that exceed 1 GiB total per iteration today are
   `mmap_vs_read_fixed_basis::*/1GiB` (env-gated) and the two
   `nvme_data_path*::*/10x1GiB` benches (both env-gated, both default
   off). Every other io_uring cell sits well below the 2 GB+ /
   100 K-file+ scale that the IUB initiative targets.
2. No cell stamps `disk_class = NVMe` as a hard requirement; the
   `nvme_data_path*` benches accept an `OC_RSYNC_BENCH_NVME_PATH`
   override but fall back to `tempdir` (typically a ramdisk on CI) when
   unset. IUB-2/IUB-3 should make NVMe an explicit precondition for
   the cells they design.
3. Three env-var gates already define the io_uring bench escalation
   ladder: `OC_RSYNC_BENCH_IOURING_RING=1` (any ring), then
   `OC_RSYNC_BENCH_IOURING_SQPOLL=1` (SQPOLL), then
   `OC_RSYNC_BENCH_NVME_DATA_PATH=1` (10 GiB working sets) or
   `OC_RSYNC_BENCH_LARGE=1` (1 GiB basis reads). New IUB cells should
   reuse this convention rather than coining new ones.
4. No baseline numbers - IUD-4 (#2364, #4381) and IUD-9 (#2369, #4398) -
   are committed to the repo. IUB-2/IUB-3 will need to run the existing
   `nvme_data_path*` benches as part of their own scope to confirm the
   "~1.00x at 148 MB" claim from the memory note and establish a
   numeric reference point for the new 2 GB+ cells.
5. Five of the seven io_uring bench files (`iouring_per_file_vs_shared`,
   `iouring_sqpoll_vs_regular`, `iocp_vs_iouring_matched` Linux cells,
   `mmap_vs_read_fixed_basis`, both `nvme_data_path*`) are env-gated off
   in default `cargo bench -p fast_io` invocations. Only
   `io_optimizations.rs` and the in-process Windows / fallback cells run
   without a gate. IUB-2/IUB-3 cells will inherit env gating to keep CI
   cheap.
