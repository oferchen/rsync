# Implementation Gaps Analysis

This document identifies missing, incomplete, or incompatible flags and behaviors between oc-rsync and upstream rsync 3.4.1. The upstream source at `target/interop/upstream-src/rsync-3.4.1/` is the source of truth.

## Summary

| Category | Count | Severity |
|----------|-------|----------|
| Missing CLI Options | 32 | Mixed |
| Missing Protocol Features | 3 | Medium |
| Behavioral Differences | 5 | Low |
| Extra Options (Rust-only) | 6 | N/A |

---

## 1. Missing CLI Options

### 1.1 Critical - Time Preservation Options

| Option | Short | Purpose | Priority |
|--------|-------|---------|----------|
| `--atimes` | `-U` | Preserve access times | HIGH |
| `--crtimes` | `-N` | Preserve creation times (macOS/Windows) | MEDIUM |

**Analysis**: Protocol support for `XMIT_SAME_ATIME` and nanosecond times exists, but CLI flags are missing. The version report already advertises `supports_atimes: true`.

**Files to modify**:
- `crates/cli/src/frontend/command_builder/sections/` - Add CLI options
- `crates/cli/src/frontend/arguments/parsed_args.rs` - Add fields
- `crates/core/src/client/config/` - Wire to client config

---

### 1.2 Critical - Daemon/Server Mode Options

| Option | Purpose | Status |
|--------|---------|--------|
| `--daemon` | Run as daemon | **EXISTS in daemon crate, missing from CLI** |
| `--server` | Server mode (remote end) | Missing |
| `--sender` | Sender role marker | Missing |
| `--config` | Daemon config file | **EXISTS in daemon crate** |
| `--detach` / `--no-detach` | Daemon detach control | **EXISTS in daemon crate** |
| `--dparam` / `-M` | Daemon parameter override | Missing |

**Analysis**: Daemon options exist in `crates/daemon/src/` but are not exposed through the main CLI binary's unified command structure.

---

### 1.3 High Priority - Security Options

| Option | Purpose | Priority |
|--------|---------|----------|
| `--fake-super` | Store/restore privileged attrs via xattrs | HIGH |
| `--trust-sender` | Trust sender's file list | MEDIUM |
| `--munge-links` | Munge symlinks for safety | MEDIUM |

**Analysis**: These are security-related options that affect how rsync handles privileged operations and symlinks in daemon mode.

---

### 1.4 Medium Priority - Transfer Behavior

| Option | Purpose | Notes |
|--------|---------|-------|
| `--copy-as` | Copy files as USER:GROUP | Privileged operation |
| `--ignore-errors` | Delete even with I/O errors | Affects delete behavior |
| `--early-input` | Read file before transfer | Advanced feature |
| `--max-alloc` | Memory allocation limit | Safety feature |

---

### 1.5 Low Priority - Aliases and Shortcuts

| Option | Equivalent | Notes |
|--------|-----------|-------|
| `--cc` | `--checksum-choice` | Shorthand alias |
| `--zc` | `--compress-choice` | Shorthand alias |
| `--zl` | `--compress-level` | Shorthand alias |
| `--del` | `--delete-during` | Shorthand alias |
| `--i-r` | `--inc-recursive` | Shorthand alias |
| `--i-d` | `--implied-dirs` | Shorthand alias |
| `--old-d` | `--old-dirs` | Legacy alias |
| `--time-limit` | `--stop-after` | Legacy name |
| `--log-format` | `--out-format` | Deprecated alias |
| `--ignore-non-existing` | `--existing` | Alias |

**Analysis**: These are convenience aliases. Lower priority but needed for full compatibility.

---

### 1.6 Low Priority - Compression Variants

| Option | Purpose | Notes |
|--------|---------|-------|
| `--old-compress` | Force old zlib compression | Protocol compat |
| `--new-compress` | Force new compression | Protocol compat |

---

### 1.7 Low Priority - Advanced/Rare Options

| Option | Purpose | Notes |
|--------|---------|-------|
| `--stderr` | Redirect stderr handling | Output routing |
| `--old-args` | Legacy argument handling | Backwards compat |
| `--secluded-args` | Protect arguments | Security |
| `--qsort` | Use qsort for file lists | Performance tuning |

---

## 2. Missing Protocol Features

### 2.1 Access Time Support

**Upstream behavior** (options.c line ~102):
```c
{"atimes", 'U', POPT_ARG_NONE, 0, 'U', 0, 0 },
```

When `-U` is used:
- `preserve_atimes` is set
- `XMIT_SAME_ATIME` flag is used in file list encoding
- Access times are preserved during transfer

**Current state**: Protocol flag `XMIT_SAME_ATIME` exists but is not wired to CLI or transfer logic.

### 2.2 Creation Time Support

**Upstream behavior** (options.c line ~106):
```c
{"crtimes", 'N', POPT_ARG_VAL, &preserve_crtimes, 1, 0, 0 },
```

When `-N` is used:
- `preserve_crtimes` is set
- `XMIT_CRTIME_EQ_MTIME` flag indicates if crtime equals mtime
- Creation times are preserved (macOS, Windows)

**Current state**: Protocol flag exists, not wired.

### 2.3 Symlink Munging

**Upstream behavior** (options.c line ~170):
```c
{"munge-links", 0, POPT_ARG_VAL, &munge_symlinks, 1, 0, 0 },
```

Transforms symlinks to prevent traversal attacks in daemon mode. Not implemented.

---

## 3. Behavioral Differences

### 3.1 Daemon Mode Entry

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| Entry point | `rsync --daemon` | `oc-rsync --daemon` (separate parsing path) |
| Config default | `/etc/rsyncd.conf` | `/etc/oc-rsyncd/oc-rsyncd.conf` |
| Fork behavior | Forks to background by default | Currently no-op/no-detach only |

### 3.2 Delete with Errors

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| `--ignore-errors` | Continues delete even with I/O errors | Not implemented |

### 3.3 Argument Protection

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| `--protect-args` / `-s` | Protects arguments from shell expansion | Exists but behavior may differ |
| `--old-args` | Forces legacy argument handling | Missing |

### 3.4 Memory Limits

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| `--max-alloc` | Limits memory allocation | Not implemented |

### 3.5 Compression Selection

| Aspect | Upstream | oc-rsync |
|--------|----------|---------|
| `--old-compress` | Forces old zlib compression | Missing |
| `--new-compress` | Forces new compression | Missing |

---

## 4. Extra Options (Rust-only)

These options exist in oc-rsync but not in upstream rsync 3.4.1:

| Option | Purpose | Notes |
|--------|---------|-------|
| `--connect-program` | Custom connection program | Extended functionality |
| `--no-copy-links` | Explicit negation | Consistency |
| `--no-copy-unsafe-links` | Explicit negation | Consistency |
| `--no-executability` | Explicit negation | Consistency |
| `--no-fsync` | Explicit negation | Consistency |
| `--no-keep-dirlinks` | Explicit negation | Consistency |

These are acceptable extensions that provide explicit negation forms for consistency.

---

## 5. Priority Implementation Order

### Phase 1: Critical Gaps (High Priority)

1. **Add `--atimes` / `-U`**
   - Add CLI option
   - Wire to client config
   - Enable protocol flag usage

2. **Add `--crtimes` / `-N`**
   - Add CLI option
   - Wire to client config
   - Enable protocol flag usage

3. **Implement daemon fork behavior**
   - Make `--no-detach` actually work
   - Implement proper daemonization

### Phase 2: Security Features (Medium Priority)

4. **Add `--fake-super`**
   - Store privileged attributes via xattrs

5. **Add `--munge-links`**
   - Symlink safety in daemon mode

6. **Add `--trust-sender`**
   - Trust sender's file list

### Phase 3: Compatibility Aliases (Low Priority)

7. **Add shorthand aliases**
   - `--cc`, `--zc`, `--zl`
   - `--del`, `--i-r`, `--i-d`
   - `--time-limit`, `--log-format`

8. **Add legacy compatibility**
   - `--old-args`
   - `--old-compress`, `--new-compress`
   - `--old-dirs`

### Phase 4: Advanced Features (Low Priority)

9. **Add `--ignore-errors`**
10. **Add `--copy-as`**
11. **Add `--early-input`**
12. **Add `--max-alloc`**
13. **Add `--stderr`**

---

## 6. Testing Requirements

For each missing option, add:

1. **Unit tests** - Option parsing
2. **Integration tests** - Feature behavior
3. **Interop tests** - Compatibility with upstream rsync

---

## 7. Files to Modify

### CLI Layer
- `crates/cli/src/frontend/command_builder/sections/` - Add options
- `crates/cli/src/frontend/arguments/parsed_args.rs` - Add fields
- `crates/cli/src/frontend/execution/drive/` - Wire options

### Core Layer
- `crates/core/src/client/config/` - Add config fields
- `crates/core/src/server/` - Handle options

### Protocol Layer
- `crates/protocol/src/flist/` - Wire time flags
- `crates/protocol/src/wire/` - Encoding support

### Daemon Layer
- `crates/daemon/src/` - Fork behavior, security options
