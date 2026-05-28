# io_uring per-kernel feature-support matrix (IKV-10)

Synthesized from the IKV-1 through IKV-6 audit outputs. Each row is an
opcode or register operation dispatched by `crates/fast_io/src/io_uring/`;
each column is a kernel version at a tier boundary. The table shows which
opcodes are available at each kernel version and what fallback engages when
they are not.

IKV-7/8/9 CI cells (kernel-specific smoke tests) are in progress
separately and will add empirical evidence rows when available.

## Source of truth

- **In-tree constants:** `crates/fast_io/src/io_uring_common.rs`
  (`LINKAT_MIN_KERNEL`, `STATX_MIN_KERNEL`, `ASYNC_CANCEL_MIN_KERNEL`,
  `ASYNC_CANCEL_FD_MIN_KERNEL`) and
  `crates/fast_io/src/io_uring/buffer_ring/registration.rs`
  (`MIN_PBUF_RING_KERNEL`).
- **Hard floor:** `MIN_KERNEL_VERSION = (5, 6)` in
  `crates/fast_io/src/io_uring/config.rs:19`.
- **Full opcode inventory:** `docs/audit/iouring-opcode-kernel-floor.md`
  (IKV-1).
- **README tier table:** IKV-4.

## Per-kernel feature-support table

Legend: Y = supported, - = not available, **fb** = graceful fallback
engages at runtime.

| Opcode / Feature | 5.1 | 5.5 | 5.6 | 5.11 | 5.15 | 5.19 | 6.0 |
|---|---|---|---|---|---|---|---|
| io_uring path enabled | - | - | Y | Y | Y | Y | Y |
| `NOP` | Y | Y | Y | Y | Y | Y | Y |
| `FSYNC` | Y | Y | Y | Y | Y | Y | Y |
| `READ_FIXED` | Y | Y | Y | Y | Y | Y | Y |
| `WRITE_FIXED` | Y | Y | Y | Y | Y | Y | Y |
| `POLL_ADD` | Y | Y | Y | Y | Y | Y | Y |
| `ASYNC_CANCEL` (user-data) | - | Y | Y | Y | Y | Y | Y |
| `LINK_TIMEOUT` | - | Y | Y | Y | Y | Y | Y |
| `READ` | - | - | Y | Y | Y | Y | Y |
| `WRITE` | - | - | Y | Y | Y | Y | Y |
| `SEND` | - | - | Y | Y | Y | Y | Y |
| `RECV` | - | - | Y | Y | Y | Y | Y |
| `STATX` | - | - | **fb** | Y | Y | Y | Y |
| `RENAMEAT` | - | - | **fb** | Y | Y | Y | Y |
| `LINKAT` | - | - | **fb** | **fb** | Y | Y | Y |
| `REGISTER_PBUF_RING` | - | - | **fb** | **fb** | **fb** | Y | Y |
| `ASYNC_CANCEL` (fd-targeted) | - | - | **fb** | **fb** | **fb** | Y | Y |
| `SEND_ZC` | - | - | **fb** | **fb** | **fb** | **fb** | Y |

Notes on fallback cells marked **fb**:

- Kernels below 5.6 never reach any io_uring code path; the hard floor
  gate in `config.rs` disables the entire backend.
- On 5.6-5.10, `STATX` and `RENAMEAT` are probed at startup and fall back
  to libc `statx(2)` / `renameat2(2)`. The receiver loses batched stat
  calls and reverts to serial metadata operations.
- On 5.6-5.14, `LINKAT` is probed and falls back to libc `linkat(2)`.
  Hardlink-heavy transfers lose io_uring batching.
- On 5.6-5.18, `REGISTER_PBUF_RING` is unavailable; the reader and writer
  skip the kernel-side buffer ring and use owned buffers with plain `READ`
  / `WRITE`. `ASYNC_CANCEL` fd-targeted form downgrades to user-data
  cancel (the 5.5 form via `CancelBuilder`).
- On 5.6-5.19, `SEND_ZC` is unavailable and the socket writer uses plain
  `SEND`. Additionally, even on 6.0+ the default build omits `SEND_ZC`
  unless compiled with the `iouring-send-zc` cargo feature - the zero-copy
  send path is opt-in regardless of kernel version.

## Tier summary

| Tier | Kernel range | What is available | Effect on oc-rsync |
|---|---|---|---|
| Unsupported | < 5.6 | No io_uring | Standard `read(2)`/`write(2)` and platform paths (IOCP on Windows). Fully functional, no performance penalty vs upstream C rsync. |
| Basic | 5.6 - 5.10 | `READ`, `WRITE`, `READ_FIXED`, `WRITE_FIXED`, `SEND`, `RECV`, `FSYNC`, `NOP`, `POLL_ADD`, `ASYNC_CANCEL`, `LINK_TIMEOUT` | Data-plane io_uring enabled. Metadata ops (`STATX`, `RENAMEAT`, `LINKAT`) fall back to libc. |
| Extended | 5.11 - 5.14 | Basic + `STATX`, `RENAMEAT` | Metadata fast paths (batched stat, atomic rename) move into the ring. |
| Mature | 5.15 - 5.18 | Extended + `LINKAT` | Hardlink creation via io_uring. All production-relevant opcodes available. |
| PBUF-ring | 5.19 - 5.x | Mature + `REGISTER_PBUF_RING`, fd-targeted `ASYNC_CANCEL` | Kernel-side buffer ring reduces user-kernel buffer copies. Precise fd-scoped cancellation. |
| Full | >= 6.0 | PBUF-ring + `SEND_ZC` (opt-in) | Zero-copy socket send. Requires the `iouring-send-zc` cargo feature; default builds downgrade to plain `SEND`. |

## Runtime probe mechanism (IKV-3)

Every opcode above the 5.6 hard floor is probed at runtime before first
use. The probe pattern is consistent across all opcode modules:

1. **Kernel version gate** - `config.rs::is_io_uring_available()` reads
   `uname(2)`, parses the release string, and checks against
   `MIN_KERNEL_VERSION = (5, 6)`. The result is cached in process-wide
   atomics (`IO_URING_AVAILABLE`, `IO_URING_CHECKED`) so subsequent calls
   are a single relaxed load. Below 5.6 the entire io_uring code path is
   bypassed.

2. **Per-opcode probe** - Each opcode module (`linkat.rs`, `renameat2.rs`,
   `statx.rs`, `send_zc.rs`) creates a temporary 4-entry ring, calls
   `ring.submitter().register_probe()`, and checks
   `probe.is_supported(OPCODE)`. The result is cached in a `OnceLock<bool>`
   (or equivalent atomic) so the probe runs at most once per process.

3. **Per-opcode MIN_KERNEL constants** - Constants in `io_uring_common.rs`
   and `buffer_ring/registration.rs` encode the minimum kernel for each
   opcode. These serve as documentation and as early-exit checks in the
   probe functions (if the parsed kernel version is below the constant,
   the probe short-circuits to `false` without building a ring).

4. **Fallback dispatch** - When a probe returns `false`, the call site
   returns `io::ErrorKind::Unsupported`. The caller (typically in
   `io_uring_ops.rs` or the stub modules) catches this error and
   dispatches the equivalent libc syscall instead.

5. **Environment override** - `OC_RSYNC_DISABLE_IOURING=1` forces
   `is_io_uring_available()` to return `false` regardless of kernel
   support. Used for testing the standard-I/O fallback path on hosts that
   would otherwise pass the kernel probe.

## Mapping to in-tree constants

| Constant | Value | File |
|---|---|---|
| `MIN_KERNEL_VERSION` | (5, 6) | `crates/fast_io/src/io_uring/config.rs:19` |
| `ASYNC_CANCEL_MIN_KERNEL` | (5, 5) | `crates/fast_io/src/io_uring_common.rs:73` |
| `STATX_MIN_KERNEL` | (5, 11) | `crates/fast_io/src/io_uring_common.rs:61` |
| `LINKAT_MIN_KERNEL` | (5, 15) | `crates/fast_io/src/io_uring_common.rs:37` |
| `ASYNC_CANCEL_FD_MIN_KERNEL` | (5, 19) | `crates/fast_io/src/io_uring_common.rs:77` |
| `MIN_PBUF_RING_KERNEL` | (5, 19) | `crates/fast_io/src/io_uring/buffer_ring/registration.rs:24` |

## Cross-references

- **IKV-1** - Full per-opcode dispatch-site inventory:
  `docs/audit/iouring-opcode-kernel-floor.md`
- **IKV-2** - MIN_KERNEL constants verified at each dispatch site
- **IKV-3** - Runtime probe matrix in `fast_io::io_uring` modules
- **IKV-4** - README kernel-tier table (README.md lines 151-164)
- **IKV-5** - Man-page per-opcode fallback documentation
- **IKV-6** - Release-notes scaffold for io_uring kernel-floor disclaimer
- **IKV-7/8/9** - CI cells for specific kernel versions (in progress)
