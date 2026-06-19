# IUBP-5: io_uring buffer-ring sizing tunable (follow-up implementation note)

## Status

Follow-up implementation task. This note records the design surface for the
operator-facing knob recommended by the IUBP-1/2/3 audit at
`docs/design/iubp-buffer-ring-audit.md`. The knob is **not implemented**
today; this note closes the IUBP debt claim by capturing the design so the
follow-up can land without re-deriving it.

Closes the project-memory debt claim "buffer-ring sizing is hard-coded;
should be compile-time vs runtime tunable" by confirming the gap and
specifying the fix shape.

## Current state

The provided buffer ring (PBUF_RING) is sized through
`BufferRingConfig` (plain-data struct in `io_uring_common`) with these
defaults:

| Field         | Default     | Location                                         |
|---------------|-------------|--------------------------------------------------|
| `ring_size`   | `64`        | `crates/fast_io/src/io_uring_common.rs:462-470`  |
| `buffer_size` | `64 * 1024` | `crates/fast_io/src/io_uring_common.rs:462-470`  |
| `bgid`        | `0`         | `crates/fast_io/src/io_uring_common.rs:462-470`  |

Total pinned per ring: `64 x 64 KiB = 4 MiB`.

Validation lives in `crates/fast_io/src/io_uring/buffer_ring/mod.rs:107-115`:
`ring_size` must be a non-zero power of two; `buffer_size` must be non-zero.
The overflow guard at `mod.rs:215-221` rejects `buffer_size * ring_entries`
products that exceed `usize::MAX`.

The only operator knob in this area today is `OC_RSYNC_DISABLE_IOURING`
(`crates/fast_io/src/io_uring/config.rs:38`), which forces the entire
io_uring path off. There is no env var, Cargo feature, or CLI flag that
adjusts `ring_size` or `buffer_size`.

## Why a knob helps

- Daemon operators on memory-constrained hosts want a smaller pinned
  footprint than the `4 MiB`-per-ring default, especially when the
  per-thread-ring split (IUR-3) multiplies that across worker threads.
- Operators on high-IOPS NVMe boxes want larger rings (more in-flight
  buffers) to keep the queue full and avoid checkout misses.
- Both groups would otherwise patch the default and rebuild, which is
  hostile to packaged binaries.

## Proposed knob

Two parallel env vars, parsed at `BufferRingConfig::default()` (or at a
new `BufferRingConfig::from_env()` helper called by every caller of
`BufferRing::new` in production code):

| Env var                          | Field         | Validation                                                                 |
|----------------------------------|---------------|----------------------------------------------------------------------------|
| `OC_RSYNC_IO_URING_BUF_COUNT`    | `ring_size`   | Power of two, `>= 1`, `<= 65536` (the kernel `u16` tail ceiling).          |
| `OC_RSYNC_IO_URING_BUF_SIZE`     | `buffer_size` | Non-zero, `buf_size * ring_size <= usize::MAX / 2` to leave mmap headroom. |

Parse rules (matching `OC_RSYNC_REORDER_RING_CAP` from ROB-11):

- Unset or parse error: fall through to the compile-time default. Do not
  fail the transfer.
- Out-of-range or constraint violation: log once at `warn`, then fall
  through to the compile-time default.
- Both vars optional; either can be set independently.

No CLI flag. This is a low-frequency tuning surface, and a CLI flag would
clutter `--help` for the 99% of users who never touch it.

## What this note does not do

- It does not implement the knob. That is a separate follow-up task.
- It does not add runtime kernel-feature detection beyond the existing
  `check_kernel_version` PBUF_RING gate. Adaptive sizing tied to opcode
  availability belongs in its own task if profiling later shows the
  default is wasteful on a specific kernel tier.
- It does not change per-thread-ring sizing policy under IUR-3. Each
  per-thread ring inherits the same `BufferRingConfig`; the env knob
  applies uniformly.

## References

- IUBP-1/2/3 audit: `docs/design/iubp-buffer-ring-audit.md`
- Existing env-var precedent (`OC_RSYNC_REORDER_RING_CAP`, ROB-11): see
  `crates/engine/src/reorder/` for the parse-and-clamp pattern.
- io_uring availability gate: `crates/fast_io/src/io_uring/config.rs:38`.
