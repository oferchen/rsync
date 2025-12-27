# oc-rsync Architecture Overview

This document outlines the crate boundaries, role separation, and design patterns enforced across the oc-rsync workspace.

**Last Updated**: 2025-12-26

---

## Crates and Responsibilities

| Crate | Responsibility | Key Files |
|-------|----------------|-----------|
| `cli` | CLI parsing, argument handling, exit code routing | `frontend/arguments/parsed_args.rs` |
| `core` | Client/server orchestration, transfer coordination | `client/run.rs`, `server/generator.rs`, `server/receiver.rs` |
| `engine` | Delta transfer, local copy, batch files, signatures | `delta/`, `local_copy/`, `signature.rs` |
| `protocol` | Wire format, multiplex codec, negotiation, flist | `multiplex/codec.rs`, `negotiation/`, `flist/` |
| `daemon` | Daemon mode, config parsing, session management | `daemon.rs`, `config.rs`, `daemon/sections/` |
| `filters` | Filter rules (include/exclude), pattern matching | `set.rs`, `rule.rs`, `compiled.rs` |
| `checksums` | Rolling (SIMD) and strong checksums (XXH3, MD5, etc.) | `rolling/`, `strong/` |
| `compress` | Compression algorithms (zlib, zstd, lz4) | `zlib.rs`, `zstd.rs`, `lz4.rs` |
| `metadata` | Permissions, ownership, ACLs, xattrs, timestamps | `apply.rs`, `acl_support.rs`, `xattr.rs` |
| `bandwidth` | Rate limiting with token bucket algorithm | `limiter/core.rs` |
| `flist` | File list building and traversal | `builder.rs`, `file_list_walker.rs` |
| `rsync_io` | Socket/pipe I/O, session management | `binary/`, `daemon/`, `ssh/` |
| `logging` | Output formatting, message sinks | `lib.rs` |
| `logging-sink` | Log sink implementations | `lib.rs` |
| `embedding` | Self-exec orchestration (used by `--server`) | `lib.rs` |
| `branding` | Version strings, program names | `lib.rs` |
| `apple-fs` | macOS-specific filesystem features | `lib.rs` |
| `windows-gnu-eh` | Windows GNU exception handling | `lib.rs` |

---

## Crate Dependency Graph

```
cli → engine → protocol, filters, checksums, bandwidth → core
       ↓
    flist, metadata, compress
       ↓
   rsync_io → daemon
```

---

## Role Execution Pipeline

```
main.rs → cli → core → role match
         ↘          ↘
     --server     --daemon
         ↓            ↓
    engine/core   daemon/session
```

---

## Key Design Patterns

| Pattern | Applied In | Purpose |
|---------|------------|---------|
| **Strategy** | `checksums`, `compress`, `protocol/codec.rs` | Version-aware encoding (NdxCodec, ProtocolCodec) |
| **State Machine** | `daemon/session`, `protocol/negotiation` | Connection lifecycle, protocol transitions |
| **Builder** | `protocol/flist`, `cli/command_builder` | Frame construction, argument building |
| **Chain of Responsibility** | `filters/set.rs` | Filter rule evaluation |
| **Factory** | `core/client`, `core/server` | Role instantiation |
| **Adapter** | `daemon/config` | Map rsyncd.conf to runtime config |
| **Token Bucket** | `bandwidth/limiter` | Rate limiting |

---

## Protocol Implementation

### Wire Format
- **Multiplex**: 4-byte LE header (tag in high byte, 24-bit length) + payload
- **Varint**: Protocol 30+ uses variable-length integers
- **NDX**: File-list index encoding (legacy 4-byte LE or modern delta)

### Checksum Support
- **Rolling**: SIMD-accelerated Adler-32 variant (AVX2, SSE2, NEON)
- **Strong**: XXH3-64 (default), XXH3-128, XXH64, MD5, MD4, SHA1

### Compression Support
- **zlib**: Default, levels 1-9
- **zstd**: Optional feature, levels 1-22
- **lz4**: Optional feature, levels 1-12

---

## Clean Code Rules

- One module = one concern
- No cross-crate import cycles
- No CLI logic outside `cli/`
- No branding hardcoded (use `branding` crate)
- Prefer `thiserror` for error types
- Use `eprintln!` carefully in daemon mode (see AGENTS.md)

---

## Testing

```bash
# Full validation suite
cargo fmt --all -- --check && \
cargo clippy --workspace --all-targets --all-features --no-deps -- -D warnings && \
cargo nextest run --workspace --all-features && \
cargo xtask docs
```

---
