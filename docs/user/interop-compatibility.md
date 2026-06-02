# Interoperability Compatibility Status

oc-rsync is tested bidirectionally against upstream rsync on every pull request,
on push to master, and on a nightly schedule. This document summarizes the
current compatibility status across upstream versions, protocol versions,
transport modes, platforms, and features.

For the full per-feature matrix with exact flags, see
[interop-compatibility-matrix.md](interop-compatibility-matrix.md).

---

## Supported Upstream Versions

| Upstream Version | Protocol | Daemon Push | Daemon Pull | SSH | CI Status |
|------------------|----------|-------------|-------------|-----|-----------|
| rsync 2.6.9      | 28-29    | Pass        | Pass        | -   | Non-blocking |
| rsync 3.0.9      | 30       | Pass        | Pass        | -   | Required |
| rsync 3.1.3      | 31       | Pass        | Pass        | -   | Required |
| rsync 3.4.1      | 32       | Pass        | Pass        | Pass | Required |
| rsync 3.4.2      | 32       | Pass        | Pass        | Pass | Required |
| rsync 3.4.3      | 32       | Pass        | Pass        | Pass | Required |

"Push" means oc-rsync client sends to upstream daemon. "Pull" means upstream
client pulls from oc-rsync daemon. Both directions are tested for every version.

Upstream binaries are obtained via multi-tier fallback: Debian/Ubuntu packages
first, then release tarballs from rsync.samba.org, then source build.

---

## Protocol Version Coverage

oc-rsync advertises protocol 32 and negotiates downward to protocol 28 when
connecting to older peers.

| Protocol | Introduced In | Wire Encoding | Default Checksum | Inc. Recurse |
|----------|---------------|---------------|------------------|--------------|
| 28       | rsync 2.6.0   | Legacy 4-byte LE | MD4           | No           |
| 29       | rsync 2.6.6   | Legacy 4-byte LE | MD4           | No           |
| 30       | rsync 3.0.0   | Varint        | MD5              | Yes          |
| 31       | rsync 3.1.0   | Varint        | MD5              | Yes          |
| 32       | rsync 3.4.0   | Varint        | MD5              | Yes          |

CI forces each protocol version (28-32) against the newest upstream binary to
verify backward compatibility. All pass, subject to upstream-imposed limitations
at older protocols (see "Known Limitations" below).

### Checksum Algorithm Negotiation

| Protocol | Default | Negotiated Options |
|----------|---------|--------------------|
| 28-29    | MD4     | None (no negotiation) |
| 30-32    | MD5     | XXH128, XXH3, XXH64, MD5, MD4, SHA1 |

Checksum negotiation requires the `-e.LsfxCIvu` capability string in SSH
transfers. Without it, transfers fall back to MD5.

---

## Test Scenarios

### Core Scenarios (All Versions: 3.0.9, 3.1.3, 3.4.x)

These scenarios run against every upstream version in both directions:

| Category | Scenarios | Status |
|----------|-----------|--------|
| Transfer | Archive, recursive, checksum, whole-file, delta, inplace, compress (zlib) | Pass |
| Metadata | Permissions, numeric IDs, symlinks, itemize changes | Pass |
| Links | Hard links | Pass |
| Delete | Delete | Pass |
| Filters | Exclude pattern, relative paths | Pass |
| Recursion | Incremental recursive (protocol 30+) | Pass |

### Extended Scenarios (3.4.x Only)

These run only against upstream 3.4.1, 3.4.2, and 3.4.3 to keep CI within
time limits:

| Category | Scenarios | Status |
|----------|-----------|--------|
| Transfer | Whole-file replace, compress level 1/9, compressed delta, zstd, lz4, sparse, append, partial, bandwidth limit, delay updates, dry run | Pass |
| Metadata | Devices, extended attributes, ACLs | Pass (with limitations) |
| Links | Copy links, safe links, hardlinks + relative/delete/checksum/existing/inc-recursive, cross-dir hardlinks | Pass |
| Comparison | Checksum skip, checksum content detect, size only, ignore times, update, existing, one file system | Pass |
| Delete | Delete after, delete during, delete + inc. recurse, max delete | Pass |
| Backup | Backup, backup dir, compare dest, link dest | Pass |
| Filters | Include/exclude precedence, filter rule, merge filter, exclude from file, files from, delete + exclude, delete excluded, delete + protect/risk filters | Pass |
| Protocol | Forced protocol 28-32 | Pass |

### Transport Modes

| Transport | Direction | Status |
|-----------|-----------|--------|
| Daemon (rsync://) | oc-rsync client to upstream daemon | Pass (all 6 versions) |
| Daemon (rsync://) | Upstream client to oc-rsync daemon | Pass (all 6 versions) |
| SSH | oc-rsync push to upstream server | Pass |
| SSH | oc-rsync pull from upstream server | Pass |
| SSH | No-change re-run (quick-check) | Pass |
| SSH | With compression | Pass |
| SSH | With iconv | Pass |
| Batch | oc-rsync write-batch to upstream read-batch | Pass |
| Batch | Upstream write-batch to oc-rsync read-batch | Pass |
| Batch | Compressed batch (both directions) | Pass |

### Standalone and Edge Case Tests

| Test | Status |
|------|--------|
| Pre/post xfer exec (daemon hooks) | Pass |
| Read-only module enforcement | Pass |
| Wrong password auth rejection | Pass |
| Max-connections admission | Pass |
| Unicode filenames | Pass |
| Shell metacharacters in paths | Pass |
| Empty directory preservation | Pass |
| Many files (100+) | Pass |
| Deep directory nesting | Pass |
| Modify-window tolerance | Pass |
| Trust-sender handling | Pass |
| Partial-dir staging | Pass |
| Daemon filter directives (glob, anchored, include/exclude, doublestar, charclass, question mark) | Pass |
| Daemon filter push direction | Pass |
| Delta stats (NDX_DEL_STATS wire) | Pass |
| Zstd codec auto-negotiation | Pass |
| INC_RECURSE comprehensive (deep trees, deletes, large dirs) | Pass |
| INC_RECURSE sender push | Pass |
| Hardlinks comprehensive (deep scenarios) | Pass |
| Batch framing (multifile) | Pass |
| Compressed batch delta interop | Pass |
| Copy-unsafe + safe-links interaction | Pass |

### Upstream rsync Testsuite

oc-rsync also runs upstream rsync's own `testsuite/*.test` corpus with oc-rsync
substituted as `$RSYNC`. This validates that oc-rsync is indistinguishable from
upstream rsync to upstream's own test infrastructure. The upstream testsuite
known failures list is currently empty - all tests pass or self-skip.

---

## Platform Coverage

| Platform | Interop Scope | CI Status |
|----------|---------------|-----------|
| Linux x86_64 (Ubuntu) | Full multi-version (all 6 versions), SSH, daemon, protocol forcing, upstream testsuite | Required |
| macOS (latest) | Smoke: push, pull, quick-check, delta, list-only (Homebrew rsync) | Required |
| Windows (latest) | Smoke: push, pull, quick-check, delta (MSYS2 rsync) | Non-blocking |

### Platform Feature Availability

| Feature | Linux | macOS | Windows |
|---------|:-----:|:-----:|:-------:|
| Full daemon mode | Yes | Yes | No |
| SSH transport | Yes | Yes | Yes |
| Symlinks | Yes | Yes | Requires Developer Mode |
| Hard links | Yes | Yes | Yes |
| POSIX ACLs | Yes | Yes | No (NTFS DACLs differ) |
| Extended attributes | Yes | Yes (different namespace) | No |
| Sparse files | Yes | Yes | Yes |
| io_uring async I/O | Yes (5.6+) | No | No |
| SIMD checksums | AVX2/SSE2 | NEON | AVX2/SSE2 |
| Compression (zlib/zstd/lz4) | Yes | Yes | Yes |
| Batch mode | Yes | Yes | Yes |

---

## Known Limitations

### oc-rsync Gaps

| Feature | Description |
|---------|-------------|
| ACLs | Transfer succeeds but ACL metadata may be incomplete depending on platform and upstream build options |
| Extended attrs | Transfer succeeds but xattr handling depends on platform |
| `--info=progress2` | Output format not fully implemented |
| `--iconv` | Charset conversion not fully implemented |
| 2 GB+ daemon transfer | Not yet validated end-to-end |
| `--files-from` vanished | Exit code handling for vanished files |
| Windows daemon mode | Not available on Windows (by design) |

### Behavioral Differences

| Scenario | Difference |
|----------|------------|
| `--keep-dirlinks` (`-K`) with symlinks | oc-rsync creates extra nested directory |
| `--list-only` on local paths | oc-rsync returns exit code 23; upstream returns 0 |

### Upstream-Imposed Limitations

These are not oc-rsync bugs - upstream rsync itself fails identically.

| Limitation | Affected Protocols | Upstream Source |
|------------|--------------------|----------------|
| ACLs require protocol 30+ | 28-29 | `compat.c:655-661` |
| Xattrs require protocol 30+ | 28-29 | `compat.c:662-668` |
| zstd/lz4 require protocol 30+ | 28-29 | `compat.c:556-564` (no vstring negotiation) |
| Merge-filter requires protocol 29+ | 28 | `exclude.c:1530` (`legal_len=1`) |
| Compressed delta batch self-roundtrip | All | `token.c:608` (upstream bug - oc-rsync reads these correctly) |

---

## CI Workflows

| Workflow | File | Trigger | Scope |
|----------|------|---------|-------|
| CI interop job | `ci.yml` + `_interop.yml` | Push/PR | Full bidirectional daemon (all versions), SSH, protocol forcing, upstream testsuite |
| Interop Validation | `interop-validation.yml` | Push/PR/nightly | Exit codes, messages, behavior, batch, filters, compression, INC_RECURSE |
| macOS Interop | `_interop-macos.yml` | Push/PR | Portable smoke against Homebrew rsync |
| Windows Interop | `_interop-windows.yml` | Push/PR | Portable smoke against MSYS2 rsync |
| Upstream Testsuite | `upstream-testsuite.yml` | Push/PR | Upstream `testsuite/*.test` with oc-rsync as `$RSYNC` |

---

## Running Interop Tests Locally

```bash
# Full interop suite (builds upstream binaries automatically)
bash tools/ci/run_interop.sh

# Build upstream binaries without running tests
bash tools/ci/run_interop.sh build-only

# Portable smoke harness (works on Linux/macOS/Windows)
OC_RSYNC=target/release/oc-rsync UPSTREAM_RSYNC=rsync \
  bash tools/ci/run_interop_smoke.sh

# Upstream rsync's own testsuite against oc-rsync
bash tools/ci/run_upstream_testsuite.sh
```

Upstream rsync source is downloaded to `target/interop/upstream-src/`. If
already present, the scripts reuse the cached build.

### Known Failures Configuration

Known failures are tracked in two configuration files:

- `tools/ci/known_failures.conf` - interop suite known failures (currently 1
  unconditional entry for the upstream batch bug, plus conditional entries for
  upstream protocol version limitations)
- `tools/ci/upstream_testsuite_known_failures.conf` - upstream testsuite known
  failures (currently empty - all tests pass)
