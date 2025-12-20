# Upstream rsync Comparison Analysis

This document provides a comprehensive comparison between oc-rsync (Rust implementation) and upstream rsync 3.4.1. The upstream source code at `target/interop/upstream-src/rsync-3.4.1/` is the source of truth for expected behavior.

## 1. CLI Arguments Comparison

### Core CLI Structure

| Aspect | Upstream (C) | oc-rsync (Rust) |
|--------|-------------|-----------------|
| Parser | popt library | Clap v4 |
| Architecture | Monolithic `options.c` (3,138 lines) | Modular components in `crates/cli/` |
| State | Global variables | Type-safe `ParsedArgs` structure (158 fields) |
| Short options | popt expansion | Explicit `expand_short_options()` |

### Supported Options by Category

| Category | Key Options | Status |
|----------|-----------|--------|
| **Archive/Recursion** | `-a`, `-r`, `-R`, `-d`, `--inc-recursive` | Full |
| **Deletion** | `--delete`, `--delete-before`, `--delete-during`, `--delete-delay`, `--delete-after`, `--delete-excluded`, `--max-delete` | Full |
| **Transfer Modes** | `--whole-file`, `-W`, `--inplace`, `-i`, `--partial`, `--append`, `--append-verify` | Full |
| **Metadata** | `-p`, `-o`, `-g`, `-t`, `-O`, `--omit-link-times` | Full |
| **Links** | `-l`, `-L`, `--copy-dirlinks`, `-k`, `-K`, `-p`, `--copy-unsafe-links` | Full |
| **Hard Links** | `-H` | Full |
| **Devices/Specials** | `--devices`, `--specials`, `-D`, `--copy-devices`, `--write-devices` | Full |
| **Checksums** | `-c`, `--checksum-seed`, `--checksum-choice` | Full |
| **Compression** | `-z`, `--compress-level`, `--compress-choice`, `--skip-compress` | Full |
| **Filtering** | `-f`, `--exclude`, `--include`, `--exclude-from`, `--include-from`, `--filter`, `--files-from` | Full |
| **Bandwidth** | `--bwlimit`, `--max-size`, `--min-size` | Full |
| **Backup** | `-b`, `--backup-dir`, `--suffix` | Full |
| **Daemon/Server** | `--daemon`, `--server`, `--port`, `--password-file`, `--auth-user` | Full |
| **SSH/Transport** | `-e`, `--rsync-path`, `--connect-program`, `--address`, `--sockopts`, `-4/-6` | Full |
| **Logging/Output** | `-v`, `-q`, `--stats`, `--progress`, `--itemize-changes`, `--out-format`, `--log-file`, `--msgs2stderr` | Full |
| **Info/Debug** | `--info`, `--debug` | Full |
| **Conversion** | `--iconv`, `--no-iconv` | Full (feature-gated) |
| **ACL/xattr** | `-A`, `-X`, `--numeric-ids` | Full (feature-gated) |

### Key Differences in CLI Handling

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| Dry-run inference | Complex logic | `--list-only` implies `--dry-run` |
| Delete validation | Runtime checks | Early validation in parser |
| Default values | Global variables | Function-based defaults |
| Error handling | Return codes + side effects | Clap error objects |

---

## 2. Protocol Constants Comparison

### Message Tags (MSG_* defines)

| Tag | Upstream Value | Rust Enum | Purpose |
|-----|----------------|-----------|---------|
| MSG_DATA | 0 | `MessageCode::Data` | Raw file data |
| MSG_ERROR_XFER | 1 | `MessageCode::ErrorXfer` | Fatal transfer error |
| MSG_INFO | 2 | `MessageCode::Info` | Informational log |
| MSG_ERROR | 3 | `MessageCode::Error` | Non-fatal error |
| MSG_WARNING | 4 | `MessageCode::Warning` | Warning message |
| MSG_ERROR_SOCKET | 5 | `MessageCode::ErrorSocket` | Socket/pipe error |
| MSG_LOG | 6 | `MessageCode::Log` | Daemon log only |
| MSG_CLIENT | 7 | `MessageCode::Client` | Client message |
| MSG_ERROR_UTF8 | 8 | `MessageCode::ErrorUtf8` | UTF-8 conversion error |
| MSG_REDO | 9 | `MessageCode::Redo` | Reprocess file-list index |
| MSG_STATS | 10 | `MessageCode::Stats` | Transfer stats |
| MSG_IO_ERROR | 22 | `MessageCode::IoError` | Source I/O error |
| MSG_IO_TIMEOUT | 33 | `MessageCode::IoTimeout` | Daemon timeout |
| MSG_NOOP | 42 | `MessageCode::NoOp` | No-op (protocol 30+) |
| MSG_ERROR_EXIT | 86 | `MessageCode::ErrorExit` | Error exit sync (protocol 31+) |
| MSG_SUCCESS | 100 | `MessageCode::Success` | File updated |
| MSG_DELETED | 101 | `MessageCode::Deleted` | File deleted |
| MSG_NO_SEND | 102 | `MessageCode::NoSend` | File open failed |

**Location**: `crates/protocol/src/envelope/message_code.rs`

### Transmission Flags (XMIT_* defines)

| Flag | Upstream | Rust | Protocol |
|------|----------|------|----------|
| XMIT_TOP_DIR | 1<<0 | `TopDir` | All |
| XMIT_SAME_MODE | 1<<1 | `SameMode` | All |
| XMIT_EXTENDED_FLAGS | 1<<2 | `ExtendedFlags` | >=28 |
| XMIT_SAME_UID | 1<<3 | `SameUid` | All |
| XMIT_SAME_GID | 1<<4 | `SameGid` | All |
| XMIT_SAME_NAME | 1<<5 | `SameName` | All |
| XMIT_LONG_NAME | 1<<6 | `LongName` | All |
| XMIT_SAME_TIME | 1<<7 | `SameTime` | All |
| XMIT_SAME_RDEV_MAJOR | 1<<8 | `SameRdevMajor` | >=28 (devices) |
| XMIT_NO_CONTENT_DIR | 1<<8 | `NoContentDir` | >=30 (dirs) |
| XMIT_HLINKED | 1<<9 | `HLinked` | >=28 (non-dirs) |
| XMIT_USER_NAME_FOLLOWS | 1<<10 | `UserNameFollows` | >=30 |
| XMIT_GROUP_NAME_FOLLOWS | 1<<11 | `GroupNameFollows` | >=30 |
| XMIT_HLINK_FIRST | 1<<12 | `HLinkFirst` | >=30 |
| XMIT_MOD_NSEC | 1<<13 | `ModNsec` | >=31 |
| XMIT_SAME_ATIME | 1<<14 | `SameAtime` | Command-gated |
| XMIT_CRTIME_EQ_MTIME | 1<<17 | `CrtimeEqMtime` | Command-gated |

**Location**: `crates/protocol/src/flist/flags.rs`

### Protocol Versions

| Constant | Upstream | oc-rsync |
|----------|----------|---------|
| PROTOCOL_VERSION | 32 | 32 |
| MIN_PROTOCOL_VERSION | 20 | 20 |
| OLD_PROTOCOL_VERSION | 25 | 25 |
| MAX_PROTOCOL_VERSION | 40 | 32 |
| SUBPROTOCOL_VERSION | 0 | 0 |

**Location**: `crates/protocol/src/version/constants.rs`

---

## 3. Checksum Algorithm Support

### Strong Checksums

| Algorithm | Upstream | oc-rsync | Digest Size |
|-----------|----------|---------|-------------|
| MD4 | Default (old) | `md4.rs` | 16 bytes |
| MD5 | Default | `md5.rs` | 16 bytes |
| XXH64 | Modern | `xxhash.rs` | 8 bytes |
| XXH3/64 | Modern | `xxhash.rs` | 8 bytes |
| XXH3/128 | Modern | `xxhash.rs` | 16 bytes |
| SHA1 | Optional | `sha1.rs` | 20 bytes |
| SHA256 | Optional | `sha256.rs` | 32 bytes |
| SHA512 | Optional | `sha512.rs` | 64 bytes |

**Location**: `crates/checksums/src/strong/`

### Rolling Checksum

Both implementations use identical 32-bit rolling checksum with CHAR_OFFSET=0.

**Rust SIMD acceleration**: AVX2, SSE2, NEON with scalar fallback.

**Location**: `crates/checksums/src/rolling/`

---

## 4. Compression Support

| Algorithm | Upstream | oc-rsync | Levels |
|-----------|----------|---------|--------|
| zlib | Always | `zlib.rs` | 1-9 (default 6) |
| zstd | - | `zstd.rs` (feature) | 1-22 (default 3) |
| LZ4 | Optional | `lz4.rs` | Speed/ratio |

**Location**: `crates/compress/src/`

---

## 5. Metadata Handling

### Permissions and Ownership

| Feature | Flag | Status |
|---------|------|--------|
| Mode preservation | `-p/--perms` | Full |
| UID/GID preservation | `-o`, `-g` | Full |
| Time preservation | `-t/--times` | Full |
| atime preservation | `--atimes` | Full (protocol-gated) |
| Creation time | `--crtimes` | Full (protocol 31+) |
| Sparse files | `-S/--sparse` | Full |
| Mode modification | `--chmod` | Full |
| Execute bit | `--executability` | Full |
| UID/GID mapping | `--usermap/--groupmap` | Full |
| Numeric IDs | `--numeric-ids` | Full |

**Location**: `crates/metadata/src/`

### ACL Support

| Aspect | Status |
|--------|--------|
| POSIX ACL parsing | Feature-gated |
| Error handling | Error if enabled but unavailable |
| Location | `crates/metadata/src/acl_support.rs` |

### Extended Attributes

| Aspect | Status |
|--------|--------|
| Namespace filtering | Full (user/system/trusted) |
| Exclude patterns | `--xattrs-exclude` supported |
| Location | `crates/metadata/src/xattr.rs` |

---

## 6. Delta/Matching Algorithm

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| Architecture | `match.c` + `sender.c` | `crates/engine/src/delta/` |
| Default block size | 700 | 700 |
| Algorithm | Weak first, then strong | Weak first, then strong |
| Optimization | Per-byte matching | Vectorized accumulation |

---

## 7. Daemon Implementation

### Configuration

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| Config location | `/etc/rsyncd.conf` | `/etc/oc-rsyncd/oc-rsyncd.conf` |
| Secrets location | `/etc/rsyncd.secrets` | `/etc/oc-rsyncd/oc-rsyncd.secrets` |
| Permission check | 0600 | 0600 |
| Binary invocation | `rsync --daemon` | `oc-rsync --daemon` |
| Service integration | systemd/inetd | systemd with sd_notify (feature-gated) |

### Module Features

Both support:
- Module definitions (path, comment, list, read-only)
- Authentication (users, secrets)
- Chroot/root-relative paths
- Max connections, timeout, bandwidth limits

---

## 8. Implementation Differences

### Architectural

| Area | Upstream (C) | oc-rsync (Rust) |
|------|--------------|-----------------|
| Memory safety | Manual | Automatic (ownership) |
| Error handling | Return codes + globals | Result types + enums |
| Concurrency | Threads + manual sync | Async-ready, atomic ops |
| Type system | C types | Traits + enums |
| Build system | autoconf/make | Cargo |

### Rust-Specific Features

1. **No fallback** - Native implementation only
2. **SIMD optimization** - Checksums, sparse detection
3. **Centralized messages** - `core::message::strings`
4. **Role-based trailers** - sender/receiver/generator/server/daemon/client
5. **Version reporting** - Includes SIMD capability detection

---

## 9. Code Organization Mapping

| Upstream File | oc-rsync Location | Purpose |
|---------------|-------------------|---------|
| `options.c` | `crates/cli/` | CLI parsing |
| `main.c` | `src/bin/oc-rsync.rs` | Entry point |
| `sender.c` | `crates/engine/` | Delta generation |
| `receiver.c` | `crates/core/src/server/receiver.rs` | Delta application |
| `generator.c` | `crates/core/src/server/generator.rs` | File selection |
| `daemon.c` | `crates/daemon/` | Daemon mode |
| `io.c` | `crates/transport/` | Multiplexing |
| `match.c` | `crates/checksums/src/rolling/` | Block matching |
| `checksum.c` | `crates/checksums/src/strong/` | Hashing |
| `acls.c` | `crates/metadata/src/acl_support.rs` | ACL support |
| `xattr.c` | `crates/metadata/src/xattr.rs` | xattr support |
| `rsync.h` | `crates/protocol/src/` | Constants, types |
| `flist.c` | `crates/walk/` | File list building |
| `compat.c` | `crates/protocol/src/compat.rs` | Compatibility flags |
| `clientserver.c` | `crates/daemon/` | Daemon protocol |
| `authenticate.c` | `crates/core/src/auth/` | Authentication |
| `log.c` | `crates/logging/` | Logging |

---

## 10. Interoperability Testing

### Tested Protocol Versions
- Protocol 28-32

### Tested Upstream Versions
- rsync 3.0.9
- rsync 3.1.3
- rsync 3.4.1

### Test Categories
- Golden byte streams
- Property tests
- SIMD vs scalar parity
- File list round-trips
- Compression negotiation

---

## 11. Conclusion

oc-rsync maintains full feature parity with upstream rsync 3.4.1 for:
- All CLI arguments and options
- Protocol versions 28-32
- Checksum and compression algorithms
- Metadata preservation (permissions, ACLs, xattrs)
- Delta transfer algorithm
- Daemon mode and module system

Key differentiators:
- Pure Rust with memory safety
- No system rsync fallback
- SIMD-optimized hot paths
- Modular, testable architecture
- Type-safe error handling
