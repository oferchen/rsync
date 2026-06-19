# IUBP: io_uring Provided Buffer Ring (PBUF_RING) sizing audit

## Scope

This document audits how the io_uring provided buffer ring (PBUF_RING) is
sized in `crates/fast_io/src/io_uring/buffer_ring/`, cross-checks the sizing
choice against the kernel-version feature matrix that IKV-1/IKV-2/IKV-3
established (`crates/fast_io/src/io_uring_common.rs`,
`crates/fast_io/src/io_uring/config.rs`,
`crates/fast_io/src/io_uring/buffer_ring/registration.rs`), and resolves the
IUBP-1/2/3 debt claim from project memory that buffer-ring sizing might be
compile-time-only and out of step with kernel capabilities.

Tasks closed by this doc: IUBP-1, IUBP-2, IUBP-3.

## 1. Constants found

The PBUF_RING module exposes sizing through a single config struct, not
through `BUF_SIZE` / `BUF_COUNT` constants buried in the ring code. The
runtime gate is a separate kernel-version constant.

| Symbol                  | Value                | Location                                                                          | Role                                                                  |
|-------------------------|----------------------|-----------------------------------------------------------------------------------|-----------------------------------------------------------------------|
| `BufferRingConfig`      | struct (3 fields)    | `crates/fast_io/src/io_uring_common.rs:453-460`                                   | Plain-data sizing handed to `BufferRing::new` / `try_new`             |
| `BufferRingConfig.ring_size`   | `u32`         | `crates/fast_io/src/io_uring_common.rs:455`                                       | Number of entries in the ring (must be power of 2)                    |
| `BufferRingConfig.buffer_size` | `u32`         | `crates/fast_io/src/io_uring_common.rs:457`                                       | Size of each individual buffer in bytes                               |
| `BufferRingConfig.bgid`        | `u16`         | `crates/fast_io/src/io_uring_common.rs:459`                                       | Buffer group ID                                                       |
| `Default for BufferRingConfig` | `ring_size=64, buffer_size=64*1024, bgid=0` | `crates/fast_io/src/io_uring_common.rs:462-470` | Default sizing: 64 entries x 64 KiB = 4 MiB pinned per ring           |
| `MIN_PBUF_RING_KERNEL`         | `(5, 19)`     | `crates/fast_io/src/io_uring/buffer_ring/registration.rs:24`                      | Minimum kernel for `IORING_REGISTER_PBUF_RING` opcode 22              |
| `IORING_REGISTER_PBUF_RING`    | `22`          | `crates/fast_io/src/io_uring/buffer_ring/registration.rs:15`                      | Registration opcode                                                   |
| `IORING_UNREGISTER_PBUF_RING`  | `23`          | `crates/fast_io/src/io_uring/buffer_ring/registration.rs:18`                      | Unregistration opcode                                                 |
| `IORING_OFF_PBUF_RING`         | `0x80000000`  | `crates/fast_io/src/io_uring/buffer_ring/registration.rs:21`                      | mmap offset for the ring region                                       |

The validator in `crates/fast_io/src/io_uring/buffer_ring/mod.rs:107-115`
enforces only two invariants: `ring_size` non-zero + power of two, and
`buffer_size` non-zero. There is no upper bound on either field beyond the
overflow guard at `mod.rs:215-221` (`buf_size * ring_entries`).

## 2. Sizing path: compile-time, env-tunable, or runtime-detected?

| Question                                                         | Answer  |
|------------------------------------------------------------------|---------|
| Is the default a compile-time const?                             | Yes, via `impl Default for BufferRingConfig`. |
| Is the default tunable via env var?                              | No. There is no `OC_RSYNC_IO_URING_BUF_*` parser. |
| Is the default tunable via CLI?                                  | No. |
| Is the chosen size runtime-detected against kernel capabilities? | No - only the binary "PBUF_RING supported yes/no" check at `registration.rs:69-79` runs. There is no probe that adjusts `ring_size` or `buffer_size` based on kernel features. |
| Can a caller pass a non-default sizing?                          | Yes - any caller of `BufferRing::new(ring, cfg)` may build a custom `BufferRingConfig`. The crate API is not the bottleneck; the absence of an env / CLI knob is. |

Net: sizing is **compile-time only at the public default**, with a
caller-supplied override available at the API surface but no operator-facing
knob.

## 3. Kernel-version matrix cross-check

PBUF_RING itself is gated by `MIN_PBUF_RING_KERNEL = (5, 19)`. The audited
sizing (`ring_size=64`, `buffer_size=65536`) interacts with three kernel
limits:

| Kernel tier | PBUF_RING avail. | Per-ring limit                                  | Default fits?       | Notes |
|-------------|------------------|--------------------------------------------------|---------------------|-------|
| < 5.19      | No (opcode 22 ENOENT) | n/a                                         | n/a                 | `check_kernel_version` rejects with `BufferRingError::KernelTooOld`. Caller falls back to classic provide-buffers (opcode 31, kernel 5.6+) per the `mod.rs:44-59` fallback chain. |
| 5.19 - 5.20 | Yes              | `ring_entries` must be power of two, no documented upper bound beyond `u16::MAX` (kernel writes `tail` as `u16` at `mod.rs:298`) | Yes - 64 << 65535 | First-generation PBUF_RING. No INC support. |
| 6.0 - 6.x   | Yes              | Same `u16` tail; INC (incremental consumption) added in 6.x but not used here | Yes                 | No additional constraint on our default. |

Practical kernel ceiling for the existing field types:

- `BufferRingConfig.ring_size: u32` is cast to `usize` at `mod.rs:201`.
  The kernel ring tail is a `u16` written at `mod.rs:298` (`tail_ptr` writes
  `ring_entries as u16`). Effective ceiling: `ring_size <= 65536`.
- `BufferRingConfig.buffer_size: u32` is cast to `usize`. The overflow
  guard at `mod.rs:215-221` rejects `buffer_size * ring_entries > usize::MAX`
  with `OutOfMemory`.
- Page-alignment requirement at `mod.rs:222-228` requires `total_buf_size`
  to fit a valid `Layout`. For 4 KiB pages and `usize=u64`, this is not a
  binding constraint at any sensible size.

**No tier where the default `64 * 64 KiB = 4 MiB` exceeds a kernel limit.**
The default sits well below the `u16` tail ceiling and the layout overflow
guard. The memory note's concern that sizing could be incompatible with
older kernels is unfounded for the PBUF_RING path itself; the entire path
is gated off below 5.19 by `check_kernel_version`.

## 4. Recommendation

| Sub-task | Question | Answer |
|----------|----------|--------|
| IUBP-1   | Where are the constants?                  | `BufferRingConfig` (struct, default 64 x 64 KiB) + `MIN_PBUF_RING_KERNEL=(5,19)`. Not buried as `BUF_SIZE`/`BUF_COUNT` consts; sized through the config struct. |
| IUBP-2   | Compile-time vs runtime?                  | Compile-time default; runtime override at the API level but no env / CLI knob. |
| IUBP-3   | Does sizing exceed any kernel-tier limit? | No. Default fits within the `u16` ring-tail and layout-overflow envelopes on every kernel that supports PBUF_RING (5.19+). Pre-5.19 kernels never reach this code path. |

### Adopt: keep compile-time default, add a single env override

The default `(64, 64 KiB)` is conservative, kernel-safe across every
supported tier, and matches the typical io_uring tutorial value. Runtime
auto-tuning based on kernel features is not warranted - the only meaningful
upper bound (`u16` tail) is well above any default we would pick, and the
lower bound (`buffer_size > 0`, `ring_size` power of two) is already
validated.

Operator-facing tunability is the gap worth closing. Recommend a single
env override that parses at config-build time and clamps to the validator
constraints:

- `OC_RSYNC_IO_URING_BUF_COUNT` - overrides `ring_size`. Validated as
  power-of-two and `<= 65536`. Falls through to the compile-time default
  on parse error or invalid value.
- `OC_RSYNC_IO_URING_BUF_SIZE`  - overrides `buffer_size`. Validated as
  non-zero and `buf_size * ring_size <= usize::MAX / 2` to leave headroom
  for the ring-region mmap. Falls through to the compile-time default
  on parse error.

Both parse identically to the existing `OC_RSYNC_REORDER_RING_CAP` knob
(ROB-11). No CLI flag is recommended - this is a low-frequency tuning
surface for daemon operators on memory-constrained hosts or for callers
who want larger pinned-buffer pools on high-IOPS NVMe boxes.

Do **not** add runtime kernel-feature detection beyond the existing
`check_kernel_version`. Adaptive ring sizing tied to opcode availability
(e.g. shrinking buffer count on older kernels that have PBUF_RING but
lack INC) belongs in a separate task if profiling later shows the default
is wasteful on those kernels.

### Out of scope

- INC (incremental consumption) buffer behaviour (kernel 6.x).
- Per-thread ring sizing under the IUR-3 per-thread-ring split. Each
  per-thread ring gets its own `BufferRingConfig` and inherits the default
  unless the caller overrides; the env knob applies uniformly.
- BGID allocation policy (covered by BGE-4 / BGW series).

## 5. Closes

- IUBP-1 (#4722) - constants inventoried at `crates/fast_io/src/io_uring_common.rs:453-470` and `crates/fast_io/src/io_uring/buffer_ring/registration.rs:14-24`.
- IUBP-2 (#4723) - sizing is compile-time default + caller override at the API level; no env / CLI knob exists today.
- IUBP-3 (#4724) - cross-check against the IKV-1/2/3 kernel matrix found no tier where the default `64 * 64 KiB` exceeds a kernel limit.

A follow-up implementation task (IUBP-4) is warranted to add the
`OC_RSYNC_IO_URING_BUF_COUNT` and `OC_RSYNC_IO_URING_BUF_SIZE` env knobs
recommended in section 4 if operator demand surfaces. The debt claim
itself (sizing might be silently incompatible with kernel capabilities)
is closed.
