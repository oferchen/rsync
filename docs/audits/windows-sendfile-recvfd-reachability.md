# Windows reachability trace for `send_file_to_fd` / `recv_fd_to_file`

Follow-up to `docs/audits/windows-fast-io-stubs.md` (WIN-S.LAND.1, PR #5551).
This document discharges WIN-S.LAND.1.a by tracing the call graph from
`crates/transfer/`, `crates/protocol/`, `crates/core/`, `crates/cli/`,
`crates/daemon/`, and `crates/engine/` to determine whether the
Windows-targeted stubs in `crates/fast_io/src/sendfile/` and
`crates/fast_io/src/splice/syscalls.rs` are reachable in production when
the workspace is compiled for `target_os = "windows"`.

The two stubs under examination are reproduced below for reference.

```text
crates/fast_io/src/sendfile/mod.rs:193-196   send_file_to_fd        (#[cfg(not(unix))])
crates/fast_io/src/sendfile/mod.rs:235-243   send_file_to_fd_with_policy (#[cfg(not(unix))])
crates/fast_io/src/splice/syscalls.rs:356-362 recv_fd_to_file        (#[cfg(not(unix))])
crates/fast_io/src/splice/syscalls.rs:271-277 try_vmsplice_to_file   (#[cfg(not(target_os = "linux"))])
crates/fast_io/src/splice/syscalls.rs:280-286 try_splice_to_file     (#[cfg(not(target_os = "linux"))])
```

## Search methodology

```sh
# 1. Direct callers in production crates.
grep -rn "send_file_to_fd\|send_file_to_fd_with_policy\|recv_fd_to_file\|\
try_splice_to_file\|try_vmsplice_to_file" \
  crates/transfer/ crates/protocol/ crates/core/ \
  crates/cli/ crates/daemon/ crates/engine/ xtask/ \
  --include="*.rs"

# 2. Any import of the parent modules.
grep -rn "fast_io::sendfile\|fast_io::splice" \
  crates/transfer/ crates/protocol/ crates/core/ \
  crates/cli/ crates/daemon/ crates/engine/ xtask/ \
  --include="*.rs"

# 3. Public re-exports from fast_io.
grep -n "pub use" crates/fast_io/src/lib.rs | \
  grep -E "send_file_to_fd|recv_fd_to_file|try_splice|try_vmsplice"
```

Result of every query against production crates: **zero hits**. The
only matches are inside `crates/fast_io/` itself (definitions, tests,
documentation, and the IOCP comment cross-reference at
`crates/fast_io/src/iocp/transmit_file.rs:8`).

## Caller table

| Symbol | Source location | `target_os` gate | External callers (production crates) | External callers (tests / docs) | Reachable on Windows? |
|---|---|---|---|---|---|
| `send_file_to_fd` (non-unix stub) | `sendfile/mod.rs:193` | `cfg(not(unix))` | none | none (tests are `cfg(unix)`) | **No** |
| `send_file_to_fd_with_policy` (non-unix stub) | `sendfile/mod.rs:235` | `cfg(not(unix))` | none | none (re-exported at `lib.rs:275` but unreferenced) | **No** |
| `send_file_to_writer` | `sendfile/mod.rs:107` | unconditional | none | `sendfile/tests.rs` only | n/a |
| `recv_fd_to_file` (non-unix stub) | `splice/syscalls.rs:356` | `cfg(not(unix))` | none | `splice/tests/non_linux.rs` only | **No** |
| `try_splice_to_file` (non-Linux stub) | `splice/syscalls.rs:280` | `cfg(not(target_os = "linux"))` | none | `splice/tests/non_linux.rs` only | **No** |
| `try_vmsplice_to_file` (non-Linux stub) | `splice/syscalls.rs:271` | `cfg(not(target_os = "linux"))` | none | `splice/tests/non_linux.rs` only | **No** |

## Why the receive / send paths bypass these stubs

- **Receive direction (`crates/transfer/src/transfer_ops/response.rs`):**
  imports only `fast_io::FileWriter`. Bytes arrive through
  `protocol::demux` -> `MultiplexReader` -> `TokenReader` ->
  `apply_delta`, which writes via the `FileWriter` trait. No socket
  -> file fast path is invoked anywhere in this pipeline. On Windows
  the buffered `read`/`write` path therefore runs unchanged; the
  non-unix `recv_fd_to_file` stub is never called.
- **Send direction (sender file body):** the sender reads file bytes
  through `MmapReader` / `FileReader` and writes them through the
  multiplex framer (`MSG_DATA` frames). No code path acquires a raw
  socket fd to hand to `send_file_to_fd*`. The Windows `TransmitFile`
  primitive at `crates/fast_io/src/iocp/transmit_file.rs` is a
  separate API and is itself unreferenced outside the `fast_io`
  crate; that is tracked as a distinct gap.
- **Public re-export:** `crates/fast_io/src/lib.rs:275` unconditionally
  re-exports `send_file_to_fd_with_policy`, which means the Windows
  stub is part of the public API surface. No caller in the workspace
  takes that handle; the symbol is reachable only through external
  consumers of the `fast_io` crate, of which there are none in this
  repository.

## Per-stub verdict

### `send_file_to_fd(non-unix)` at `sendfile/mod.rs:193`

`send_file_to_writer(source, &mut io::sink(), length)` discards bytes.
The PR #5551 audit flagged this as P0-LATENT because the failure mode
is silent data loss rather than an error. The reachability trace finds
**no production caller in any workspace crate**. The stub is dead
code on Windows.

**Verdict: P3 (dead code on Windows).** Downgrade from P0-LATENT.

### `send_file_to_fd_with_policy(non-unix)` at `sendfile/mod.rs:235`

Same `io::sink` discard. Same trace result. No production caller.

**Verdict: P3 (dead code on Windows).** Downgrade from P0-LATENT.

### `recv_fd_to_file(non-unix)` at `splice/syscalls.rs:356`

Already returns `io::ErrorKind::Unsupported` rather than discarding
bytes; the failure mode would have been loud, not silent. No
production caller in any workspace crate.

**Verdict: P3 (dead code on Windows).** Downgrade from P0-LATENT.

### `try_splice_to_file(non-Linux)` and `try_vmsplice_to_file(non-Linux)` at `splice/syscalls.rs:271, 280`

Both return `Unsupported`. No production caller. The Linux-only
`recv_fd_to_file` invokes `try_splice_to_file`, but that variant is
itself unreferenced outside `fast_io`.

**Verdict: P3 (dead code on every target).**

## Follow-up disposition

The two `WIN-S.12.b` and `WIN-S.12.c` tasks proposed by PR #5551
(wire IOCP socket helpers and replace the `io::sink` stub) are no
longer warranted as P0 work. Recommended follow-ups instead:

1. **WIN-S.LAND.1.b (cleanup, P3):** delete the non-unix
   `send_file_to_fd`, `send_file_to_fd_with_policy`, and
   `recv_fd_to_file` stubs. Replace them with a single
   `#[cfg(not(unix))]` `compile_error!` shim, or drop the public
   re-export at `lib.rs:275` and gate `send_file_to_fd_with_policy`
   on `cfg(unix)`. Dead code that nominally silently discards file
   bytes should not survive in the tree even if it has no caller
   today; a future caller could regress this.
2. **WIN-S.LAND.1.c (separate concern, P1 stays open):** the
   IOCP `TransmitFile` primitive at
   `crates/fast_io/src/iocp/transmit_file.rs` is unreferenced
   outside its own crate. Wiring it into the Windows daemon
   transmit path is the real Windows zero-copy gap and is tracked
   independently of this audit.

## Verification commands

```sh
# Caller scan over production crates.
grep -rn "send_file_to_fd\|recv_fd_to_file\|try_splice_to_file\|try_vmsplice_to_file" \
  crates/transfer/ crates/protocol/ crates/core/ \
  crates/cli/ crates/daemon/ crates/engine/ xtask/ \
  --include="*.rs"
# expected: no output

# Module-import scan.
grep -rn "use fast_io::sendfile\|use fast_io::splice" \
  crates/transfer/ crates/protocol/ crates/core/ \
  crates/cli/ crates/daemon/ crates/engine/ xtask/ \
  --include="*.rs"
# expected: no output
```
