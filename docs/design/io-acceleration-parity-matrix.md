# Cross-platform I/O acceleration parity matrix

This document inventories every kernel-assisted / zero-copy I/O acceleration
primitive in oc-rsync and records its status on Linux, macOS, and Windows. It is
a factual map of what is wired into production paths today, what is built but not
yet wired, and what is absent. Use it to decide where acceleration work has the
most leverage and to avoid advertising capabilities that are not actually engaged
in default builds.

Status legend:

- **WIRED** - a production code path calls it.
- **BUILT-UNWIRED** - implemented in `fast_io`, but no production caller (tests
  only).
- **STUBBED** - only a no-op / fallback stub exists.
- **ABSENT** - not present.

`n/a` means the primitive does not apply to that platform.

## Feature defaults

Workspace `Cargo.toml` (`bin`) default features include `copy_file_range`,
`io_uring`, `iocp`, plus the codec/metadata features. `crates/fast_io/Cargo.toml`
default is `["io_uring", "iocp", "sqpoll-mlock-basis"]`.

Acceleration features that are **not** in any default set (opt-in only):

| feature | crate | gates |
|---|---|---|
| `iouring-data-writes` | fast_io | io_uring registered-buffer disk write dispatch |
| `iouring-data-reads` | fast_io | io_uring READ_FIXED basis-read slurp |
| `iouring-send-zc` | fast_io | `IORING_OP_SEND_ZC` net send |
| `vmsplice` | fast_io | vmsplice disk writer |
| `dontcache` | fast_io | `RWF_DONTCACHE` read + write |
| `transmitfile` | fast_io | Windows `TransmitFile` |
| `mmap-free-basis`, `adaptive-basis-dispatch` | fast_io | experimental basis-read |

`copy_file_range` is in defaults but is a no-op alias: the module is always
compiled with runtime detection.

## Disk write

| primitive | Linux | macOS | Windows | feature | default | prod-wired |
|---|---|---|---|---|---|---|
| io_uring write (`IoUringDiskBatch`, plain `WRITE`) | WIRED | n/a | n/a | `io_uring` | yes | yes - `transfer/.../disk_commit/process.rs` `make_writer` -> `Writer::IoUring`; impl `fast_io/.../io_uring/disk_batch.rs` |
| io_uring whole-file write (`write_file_with_io_uring`) | BUILT-UNWIRED in default; WIRED with feature | n/a | n/a | `iouring-data-writes` | no | gated caller `engine/.../execute/iouring.rs` (local-copy Direct only) |
| IOCP write (`IocpDiskBatch`, overlapped `WriteFile`) | n/a | n/a | WIRED | `iocp` | yes | yes - `disk_commit/process.rs` `Writer::Iocp` |
| GCD `dispatch_io` write | n/a | ABSENT | n/a | - | - | macOS writer is `F_NOCACHE` + `writev` (`MacosWriter`), not GCD |
| `RWF_DONTCACHE` writer (`DontcacheFileWriter`, `pwritev2`) | WIRED behind feature | n/a | n/a | `dontcache` | no | call site `disk_commit/process.rs`; inert in default (probe returns false) |
| vmsplice writer | WIRED behind feature | n/a | n/a | `vmsplice` | no | call site `disk_commit/process.rs` |
| `copy_file_range` | WIRED | n/a (clonefile path) | n/a | alias | yes | yes - via platform_copy dispatcher |
| reflink (FICLONE / clonefile / ReFS) | WIRED (FICLONE) | WIRED (clonefile/fcopyfile) | WIRED (ReFS / `CopyFileExW`) | runtime-detected | n/a | yes - `CopyMethod::{Ficlone,Clonefile,ReFsReflink}` |
| sparse writing | WIRED | WIRED | WIRED | runtime | n/a | yes - `SparseState`, seek-based hole punching |

## Disk read

| primitive | Linux | macOS | Windows | feature | default | prod-wired |
|---|---|---|---|---|---|---|
| io_uring read (`read_file_with_io_uring`, READ_FIXED) | WIRED behind feature | n/a | n/a | `iouring-data-reads` | no | caller `engine/.../concurrent_delta/strategy.rs` (basis slurp); default path is `BufReader` |
| IOCP read | n/a | n/a | BUILT | `iocp` | yes | receiver disk reads not routed through IOCP; effectively no prod caller |
| GCD read | n/a | ABSENT | n/a | - | - | no `dispatch_io` |
| `RWF_DONTCACHE` read (`dontcache_read_exact`, `preadv2`) | WIRED behind feature | n/a | n/a | `dontcache` | no | call site `transfer/.../map_file/buffered.rs`; inert in default |
| mmap (`MapFile` / adaptive / mmap strategy) | WIRED | WIRED | n/a (unix-only) | runtime | n/a | yes - `transfer/.../map_file/` |
| sequential-read-hint (`posix_fadvise` Linux / `F_NOCACHE` macOS) | WIRED | WIRED | n/a | runtime | n/a | yes - `transfer/.../map_file/buffered.rs` |

### mmap vs SQPOLL on the basis read

Two big-I/O optimisations compete on the delta basis read: mmap of the basis
file and an io_uring SQPOLL ring reading it. They interact but are **not**
simultaneously active in a default build, which routinely confuses readers about
which one is engaged.

- **SQPOLL is opt-in.** `IoUringConfig::sqpoll` defaults to `false` on every
  constructor, and the production reader/writer paths build from
  `IoUringConfig::default()`. A stock transfer never requests the SQPOLL kthread,
  so the default large-basis read runs on **mmap** (io_uring `READ_FIXED` basis
  slurp is itself gated behind the non-default `iouring-data-reads` feature).
- **The mmap+SQPOLL refusal is a backstop, not the default gate.** Pairing a
  SQPOLL kthread with a file-backed mmap is a kernel hazard (the kthread has no
  user `mm` context, so cold-page faults bounce to `task_work` and concurrent
  truncation surfaces as in-kernel `SIGBUS`; see
  `docs/audits/io-uring-sqpoll-mmap-interaction.md`). `IoUringConfig::build_ring`
  refuses SQPOLL when `mmap_basis_active` is set **only** if the default
  `sqpoll-mlock-basis` feature is compiled out.
- **With the default feature set they coexist.** `sqpoll-mlock-basis` (a default
  feature) pins each basis window via `mlock(2)` before submission
  (`fast_io/src/sqpoll_basis.rs`), closing the fault race, so an operator who
  opts into SQPOLL keeps it even against an mmap'd basis. Mutual exclusion only
  reappears if that feature is disabled.

Net: in the default configuration mmap is the live basis-read acceleration and
SQPOLL is off - they never run together because SQPOLL is not requested, not
because the code forbids the pairing.

## Network send

Every kernel-async network-send path is built but not wired: production transport
is plain blocking `TcpStream`.

| primitive | Linux | macOS | Windows | feature | default | prod-wired |
|---|---|---|---|---|---|---|
| sendfile (`send_file_to_fd`) | BUILT-UNWIRED | BUILT-UNWIRED | (TransmitFile impl) | runtime | - | no prod caller |
| io_uring `SEND_ZC` (`ZeroCopySender`) | BUILT-UNWIRED behind feature | n/a | n/a | `iouring-send-zc` | no | no prod caller |
| splice file->pipe->socket | net-send direction ABSENT (splice module is recv->disk) | n/a | n/a | - | - | recv-side `recv_fd_to_file` is BUILT-UNWIRED |
| `MSG_ZEROCOPY` | ABSENT | n/a | n/a | - | - | no `SO_ZEROCOPY` anywhere |
| `TransmitFile` | n/a | n/a | BUILT-UNWIRED | `transmitfile` | no | reachable only via unwired `PlatformSendFile` |
| kTLS TX | ABSENT | ABSENT | ABSENT | - | - | not applicable - oc has no in-binary TLS |

## Network recv

| primitive | Linux | macOS | Windows | feature | default | prod-wired |
|---|---|---|---|---|---|---|
| io_uring recv single-shot (`IoUringSocketReader`) | BUILT-UNWIRED | n/a | n/a | `io_uring` | yes (built) | no prod caller |
| io_uring recv multishot / PBUF | ABSENT (multishot); buffer-ring infra BUILT-UNWIRED | n/a | n/a | `io_uring` | yes | socket path uses single-shot only |
| IOCP `WSARecv` (`IocpSocketReader`) | n/a | n/a | BUILT-UNWIRED | `iocp` | yes (built) | no prod caller |
| GCD / kqueue recv | n/a | BUILT-UNWIRED (generic `EVFILT_READ` readiness only) | n/a | runtime | - | `KqueueLoop` is generic readiness, no recv specialization |
| kTLS RX | ABSENT | ABSENT | ABSENT | - | - | - |
| RIO (Windows registered I/O) | n/a | n/a | BUILT-UNWIRED | `iocp` | yes (built) | env probe `OC_RSYNC_WINDOWS_RIO`; no prod caller |

## Accept

| primitive | Linux | macOS | Windows | feature | default | prod-wired |
|---|---|---|---|---|---|---|
| `AcceptEngine` seam | WIRED | WIRED | WIRED | runtime | n/a | yes - `daemon/.../server_runtime/accept_engine.rs`; `SingleListenerEngine` + `MultiListenerEngine`, both std `TcpListener::accept` |
| io_uring `IORING_OP_ACCEPT` | ABSENT | n/a | n/a | - | - | named as a future plug-in only |
| kqueue accept | n/a | ABSENT | n/a | - | - | `KqueueLoop` exists, no accept engine |
| IOCP `AcceptEx` | n/a | n/a | ABSENT | - | - | - |

The `AcceptEngine` trait (added in the NACC-3 work) is the seam the per-platform
accept engines plug into without touching the shared accept loop; the engines
themselves are not yet implemented.

## Socket tuning

This is the most production-complete category.

| primitive | Linux | macOS | Windows | feature | default | prod-wired |
|---|---|---|---|---|---|---|
| `SO_REUSEPORT` | WIRED | WIRED | n/a | runtime | n/a | yes - `daemon/.../listener.rs` (`reuse_port_supported()` + `set_reuse_port`) |
| `TCP_QUICKACK` | WIRED | n/a (Linux-only) | n/a | runtime | n/a | yes - client + daemon after connect/accept, re-armed in handshake |
| TCP Fast Open (server + connect) | WIRED | n/a (server: Linux/FreeBSD) | STUBBED | runtime | n/a | yes - server `listener.rs`, client `connect/direct.rs` |
| `TCP_NOTSENT_LOWAT` | WIRED | WIRED | n/a | runtime | n/a | yes - client `connect/mod.rs`, daemon `listener.rs` |
| `TCP_CORK` / `TCP_NOPUSH` (`set_tcp_cork`) | BUILT-UNWIRED | BUILT-UNWIRED | STUBBED | runtime | n/a | no prod caller (tests only) |
| `SO_MAX_PACING_RATE` | WIRED | n/a (Linux-only) | n/a | runtime | n/a | yes - `module_list/tcp_perf.rs`, pacing = bwlimit |
| TCP congestion / BBR (`TCP_CONGESTION`) | ABSENT | ABSENT | ABSENT | - | - | - |

## Takeaways

- **Disk acceleration is the most production-complete area.** io_uring `WRITE`
  (Linux) and IOCP `WriteFile` (Windows) are default-on; sparse, reflink
  (FICLONE / clonefile / ReFS), `copy_file_range`, mmap, and the
  sequential-read-hint are all wired without a feature gate (runtime-detected).
  macOS disk writes use `F_NOCACHE` + `writev`, not GCD `dispatch_io` (absent).
- **Every kernel-async network path is built-unwired or absent.** sendfile,
  io_uring `SEND_ZC`, `TransmitFile`, RIO, the io_uring / IOCP socket readers,
  and the kqueue readiness layer all exist in `fast_io` with zero production
  callers; production transport is plain blocking `TcpStream`. `MSG_ZEROCOPY`,
  kTLS (TX and RX), and io_uring multishot / PBUF recv are fully absent. Wiring
  any of these first requires plumbing the concrete socket fd through the
  transfer server boundary, where it is currently erased to `&mut dyn Read` /
  `dyn Write` (the NSV-1 / NRX-1 prerequisite).
- **Accept** has only the std-based `AcceptEngine` seam; the io_uring / kqueue /
  IOCP accept engines are absent (named as future plug-ins).
- **Socket tuning** is broadly wired: `SO_REUSEPORT`, `TCP_QUICKACK`, Fast Open,
  `TCP_NOTSENT_LOWAT`, and `SO_MAX_PACING_RATE`. Only `TCP_CORK` / `TCP_NOPUSH`
  is built-unwired, and `TCP_CONGESTION` / BBR is absent.
- **Opt-in accel with wired-but-inert call sites:** `iouring-data-writes`,
  `iouring-data-reads`, `vmsplice`, and `dontcache` (read + write) have real
  production call sites guarded by `#[cfg(feature = ...)]` and/or runtime
  `*_supported()` probes returning `false`, so default builds never exercise
  them. Promoting any of these to default-on is a bench-gated decision.
