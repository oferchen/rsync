# Hardware Priority Order

Tracks issue #2121.

## 1. Goal

At runtime, oc-rsync auto-selects between hardware fast paths for SIMD,
file I/O, compression, and SSH ciphers. This document captures the
precedence each subsystem follows so users can predict which backend
will be picked on their host - and override it via CLI flags when the
default is wrong (debugging, benchmarking, regression bisects).

Selection runs once per process at startup. Results are cached in
`OnceLock` so repeated calls share the probe. CLI overrides bypass the
probe entirely and pin a specific backend; an unsupported override is
a hard error rather than a silent fallback.

## 2. SIMD precedence (rolling + strong checksum)

Probed via `is_x86_feature_detected!` / `std::arch::is_aarch64_feature_detected!`
at first use, with the result cached.

- **x86_64**: AVX-512 -> AVX2 -> SSE4.1 -> SSSE3 -> SSE2 -> scalar.
- **aarch64**: NEON (plus optional Crypto Extensions for AES-GCM) -> scalar.
- **other targets**: scalar fallback only.

SIMD and scalar implementations are kept in lockstep via parity tests
(`crates/checksums/tests/simd_parity.rs`). A SIMD bug never produces a
silently corrupt transfer - the parity test gate breaks the build.

## 3. I/O backend precedence

Each platform tries its native fast path first and falls back to portable
`std::io` when the kernel, filesystem, or runtime lacks support.

- **Linux**: `io_uring` (kernel >= 5.6) with capability probes for
  `IORING_REGISTER_PBUF_RING` (#2043), `RENAMEAT2` (#1922), and
  `LINKAT` (#1923) -> epoll + `std::io`. Each probe failure narrows
  what `io_uring` is used for; full failure drops to portable I/O.
- **macOS**: `dispatch_io` for streaming reads, `fcopyfile` for whole
  files, `clonefile` for APFS reflinks -> `std::io`.
- **Windows**: IOCP for async writes -> `CopyFileEx` for whole-file
  copies -> `std::io`.

The `fast_io` crate centralises detection and exposes a safe public
API so callers never see the underlying unsafe FFI.

## 4. Compression precedence (capability-negotiated)

Compression is chosen during the protocol handshake. Both peers must
advertise support; the highest mutually-supported codec wins.

- **protocol 32 with iconv capability**: zstd -> lz4 -> zlib (zlib-ng
  when compiled with the `zlib-ng` feature).
- **protocol < 30**: zlib only.

Codec choice is negotiated per-session; `--compress-choice` forces a
specific codec and fails the handshake if the peer does not advertise it.

## 5. Cipher precedence (SSH transport)

Picked from the SSH library's preference list, reordered based on host
CPU features detected at startup.

- **x86_64 with AES-NI**: AES-GCM-256 -> ChaCha20-Poly1305.
- **aarch64 with Crypto Extensions**: AES-GCM-256 -> ChaCha20-Poly1305.
- **aarch64 without Crypto Extensions**: ChaCha20-Poly1305 -> AES-GCM-256
  (ChaCha is faster on cores without AES instructions).

Server-side cipher allowlists still apply; if neither side accepts
the preferred cipher, negotiation falls through the rest of the SSH
library's default list.

## 6. Override knobs

Every auto-selected layer has a CLI flag to pin the backend explicitly.
These are the only supported way to override priority order; environment
variables are not a stable interface.

| Flag | Values |
|------|--------|
| `--simd` | `auto`, `avx512`, `avx2`, `sse4.1`, `ssse3`, `sse2`, `neon`, `scalar` |
| `--io-backend` | `auto`, `io_uring`, `epoll`, `dispatch_io`, `iocp`, `std` |
| `--compress-choice` | `auto`, `zstd`, `lz4`, `zlib`, `zlib-ng`, `none` |
| `--ssh-cipher` | `auto`, `aes-gcm-256`, `chacha20-poly1305`, plus any cipher exposed by the underlying SSH library |

`auto` (the default everywhere) runs the precedence chains documented
above. Any other value pins that backend; a pinned backend that is not
available on the host fails fast with a diagnostic naming the missing
capability rather than silently falling back.
