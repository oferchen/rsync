# Interoperability Compatibility Matrix

This document describes oc-rsync's tested interoperability with upstream rsync
across protocol versions, upstream releases, transfer modes, and platforms.
Every claim below is backed by automated CI tests that run on every pull
request and nightly.

---

## Quick Reference

| Upstream Version | Protocol | Daemon Push | Daemon Pull | SSH | Status |
|------------------|----------|-------------|-------------|-----|--------|
| rsync 2.6.9      | 28-29    | Pass        | Pass        | -   | Non-blocking |
| rsync 3.0.9      | 30       | Pass        | Pass        | -   | Required CI |
| rsync 3.1.3      | 31       | Pass        | Pass        | -   | Required CI |
| rsync 3.4.1      | 32       | Pass        | Pass        | Pass | Required CI |
| rsync 3.4.2      | 32       | Pass        | Pass        | Pass | Required CI |
| rsync 3.4.3      | 32       | Pass        | Pass        | Pass | Required CI |

**Legend**: "Push" = oc-rsync client sends to upstream daemon. "Pull" =
upstream client pulls from oc-rsync daemon. Both directions are tested for
every version.

---

## Protocol Version Support

oc-rsync supports protocol versions 28 through 32. It advertises protocol 32
and negotiates downward when connecting to older peers.

| Protocol | Introduced In | Default Checksum | Compat Flags | Inc. Recurse | Status |
|----------|---------------|------------------|--------------|--------------|--------|
| 28       | rsync 2.6.0   | MD4              | Legacy 4-byte LE | No       | Supported |
| 29       | rsync 2.6.6   | MD4              | Legacy 4-byte LE | No       | Supported |
| 30       | rsync 3.0.0   | MD5              | Varint       | Yes          | Supported |
| 31       | rsync 3.1.0   | MD5              | Varint       | Yes          | Supported |
| 32       | rsync 3.4.0   | MD5              | Varint       | Yes          | Supported (primary) |

### Forced Protocol Testing

CI forces each protocol version (28-32) against the newest upstream binary
(3.4.3 or 3.4.2) to verify backward compatibility. Results:

| Forced Protocol | Transfer | Delete | Compress | Hardlinks | Filters | Status |
|-----------------|----------|--------|----------|-----------|---------|--------|
| `--protocol=28` | Pass     | Pass   | Pass     | Pass      | Limited | Supported |
| `--protocol=29` | Pass     | Pass   | Pass     | Pass      | Limited | Supported |
| `--protocol=30` | Pass     | Pass   | Pass     | Pass      | Pass    | Supported |
| `--protocol=31` | Pass     | Pass   | Pass     | Pass      | Pass    | Supported |
| `--protocol=32` | Pass     | Pass   | Pass     | Pass      | Pass    | Supported |

Protocol 28-29 limitations (upstream-imposed, not oc-rsync bugs):
- ACLs and xattrs require protocol 30+ wire format
- Compression algorithm selection (zstd, lz4) requires protocol 30+ vstring
  negotiation
- Merge-filter rules require protocol 29+ (upstream `exclude.c:1530`
  `legal_len=1` at protocol 28)

### Checksum Algorithm Negotiation

| Protocol | Default Strong Checksum | Negotiated Options |
|----------|------------------------|--------------------|
| 28-29    | MD4                    | None (no negotiation) |
| 30-32    | MD5                    | XXH128, XXH3, XXH64, MD5, MD4, SHA1 |

Checksum negotiation requires the `-e.LsfxCIvu` capability string in SSH
transfers. Without it, transfers fall back to MD5.

---

## Feature Compatibility by Upstream Version

Each feature below is tested bidirectionally: upstream client pushing to
oc-rsync daemon, and oc-rsync client pushing to upstream daemon. Rsync 3.4.2
and 3.4.3 use the same protocol (32) as 3.4.1 and pass the identical test
matrix.

### Transfer Modes

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Archive mode | `-av` | Pass | Pass | Pass |
| Recursive only | `-rv` | Pass | Pass | Pass |
| Whole file | `-avW` | Pass | Pass | Pass |
| Whole file replace | `-avW` (stale dest) | - | - | Pass |
| Delta transfer | `-av --no-whole-file -I` | Pass | Pass | Pass |
| Inplace | `-av --inplace` | Pass | Pass | Pass |
| Compression (zlib) | `-avz` | Pass | Pass | Pass |
| Compress level 1 | `-avz --compress-level=1` | - | - | Pass |
| Compress level 9 | `-avz --compress-level=9` | - | - | Pass |
| Compressed delta | `-avz --no-whole-file -I` | - | - | Pass |
| Compress zstd | `-avz --compress-choice=zstd` | - | - | Pass (1) |
| Compress lz4 | `-avz --compress-choice=lz4` | - | - | Pass (1) |
| Sparse files | `-avS` | Pass | Pass | Pass |
| Append mode | `-av --append` | - | - | Pass |
| Partial transfer | `-av --partial` | - | - | Pass |
| Bandwidth limit | `-av --bwlimit=10000` | - | - | Pass |
| Delay updates | `-av --delay-updates` | - | - | Pass |
| Dry run | `-avn` | - | - | Pass |
| Inc. recursive | `-av --inc-recursive` | Pass | Pass | Pass |

(1) Requires upstream built with zstd/lz4 support (`libzstd-dev`/`liblz4-dev`
at configure time). Skipped when upstream lacks the codec.

### Metadata and Attributes

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Permissions | `-rlpv` | Pass | Pass | Pass |
| Numeric IDs | `-av --numeric-ids` | Pass | Pass | Pass |
| Devices | `-avD` | - | - | Pass |
| ACLs | `-avA` | Known limitation | Known limitation | Known limitation |
| Extended attrs | `-avX` | - | - | Known limitation |
| Itemize changes | `-avi` | Pass | Pass | Pass |

ACL and xattr limitations: transfer succeeds but metadata fidelity depends
on upstream build options (`--enable-acl-support`, `--enable-xattr-support`)
and platform support.

### Links

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Symlinks | `-rlptv` | Pass | Pass | Pass |
| Hard links | `-avH` | Pass | Pass | Pass |
| Copy links | `-avL` | - | - | Pass |
| Safe links | `-rlptv --safe-links` | - | - | Pass |
| Hardlinks + relative | `-avHR` | - | - | Pass |
| Hardlinks + delete | `-avH --delete` | - | - | Pass |
| Hardlinks + numeric IDs | `-avH --numeric-ids` | - | - | Pass |
| Hardlinks + checksum | `-avHc` | - | - | Pass |
| Hardlinks + existing | `-avH --existing` | - | - | Pass |
| Hardlinks + inc. recurse | `-avH --inc-recursive` | - | - | Pass |
| Cross-dir hardlinks | `-avH --inc-recursive` | - | - | Pass |

### Comparison and Selection

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Checksum mode | `-avc` | Pass | Pass | Pass |
| Checksum skip (identical) | `-avc` (pre-populated) | - | - | Pass |
| Checksum content detect | `-avc` (same size, diff content) | - | - | Pass |
| Size only | `-av --size-only` | - | - | Pass |
| Ignore times | `-av --ignore-times` | - | - | Pass |
| Update mode | `-av --update` | - | - | Pass |
| Existing only | `-av --existing` | - | - | Pass |
| One file system | `-avx` | - | - | Pass |

### Delete and Backup

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Delete | `-av --delete` | Pass | Pass | Pass |
| Delete after | `-av --delete-after` | - | - | Pass |
| Delete during | `-av --delete-during` | - | - | Pass |
| Delete + inc. recurse | `-av --inc-recursive --delete` | - | - | Pass |
| Max delete | `-av --delete --max-delete=1` | - | - | Pass |
| Backup | `-av --backup` | - | - | Pass |
| Backup dir | `-av --backup --backup-dir=.backups` | - | - | Pass |
| Compare dest | `-av --compare-dest=ref` | - | - | Pass |
| Link dest | `-av --link-dest=ref` | - | - | Pass |

### Filters and Paths

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Exclude pattern | `-av --exclude=*.log` | Pass | Pass | Pass |
| Include/exclude precedence | `--include=*.txt --exclude=*` | - | - | Pass |
| Filter rule | `-av --exclude=*.tmp` | - | - | Pass |
| Merge filter | `-av -FF` (.rsync-filter) | - | - | Pass |
| Exclude from file | `-av --exclude-from=file` | - | - | Pass |
| Relative paths | `-avR` | Pass | Pass | Pass |
| Files from | `-av --files-from=list` | - | - | Pass |
| Delete + exclude | `-av --delete --exclude=*.log` | - | - | Pass |
| Delete excluded | `-av --delete-excluded` | - | - | Pass |
| Delete + P filter (protect) | `-av --delete -f 'P *.log'` | - | - | Pass |
| Delete + R filter (risk) | `-av --delete -f 'R *.log'` | - | - | Pass |

---

## Bidirectional Coverage

### Daemon Mode (rsync:// protocol)

Every version is tested in both directions on every CI run:

| Direction | Description | Versions Tested |
|-----------|-------------|-----------------|
| oc-rsync client -> upstream daemon | oc-rsync pushes files to upstream rsync daemon | 2.6.9, 3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3 |
| Upstream client -> oc-rsync daemon | Upstream rsync pulls files from oc-rsync daemon | 2.6.9, 3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3 |
| rsync 2.6.9 client -> oc-rsync daemon | 2.6.9 as client, oc-rsync as daemon (RP28.e.2) | 2.6.9 |
| oc-rsync client -> rsync 2.6.9 daemon | oc-rsync as client, 2.6.9 as daemon (RP28.f.2) | 2.6.9 |

### SSH Transport

SSH interop is tested with upstream rsync on the PATH via loopback:

| Direction | Status |
|-----------|--------|
| oc-rsync client -> upstream server (push) | Pass |
| oc-rsync client <- upstream server (pull) | Pass |
| oc-rsync SSH no-change re-run | Pass |
| oc-rsync SSH with compression | Pass |
| oc-rsync SSH with iconv | Pass |

### Batch File Interop

Batch files written by one implementation can be read by the other:

| Direction | Status |
|-----------|--------|
| oc-rsync `--write-batch` -> upstream `--read-batch` | Pass |
| Upstream `--write-batch` -> oc-rsync `--read-batch` | Pass |
| oc-rsync daemon `--write-batch` -> `--read-batch` replay | Pass |
| oc-rsync `--write-batch -z` -> oc-rsync `--read-batch` | Pass |
| oc-rsync `--write-batch -z` -> upstream `--read-batch` | Pass |
| Upstream `--write-batch -z` -> oc-rsync `--read-batch` | Pass |
| Upstream compressed delta batch self-roundtrip | Known failure (upstream bug) |

The upstream compressed delta batch self-roundtrip failure is an upstream rsync
bug: `token.c:608` tees deflated data to the batch fd without dictionary sync,
so upstream cannot read back its own compressed delta batches. oc-rsync reads
these files correctly.

---

## Platform Coverage

### CI Test Matrix

| Platform | Workflow | Interop Scope | Status |
|----------|----------|---------------|--------|
| Linux x86_64 (Ubuntu) | `ci.yml`, `_interop.yml` | Full multi-version (2.6.9, 3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3), SSH, daemon, standalone | Required |
| Linux x86_64 (Ubuntu) | `interop-validation.yml` | Exit codes, messages, behavior, batch, filters, compression, inc. recurse | Required |
| macOS (latest) | `_interop-macos.yml` | Smoke: push, pull, quick-check, delta, list-only (Homebrew rsync) | Required |
| Windows (latest) | `_interop-windows.yml` | Smoke: push, pull, quick-check, delta (MSYS2 rsync, best-effort) | Non-blocking |

### macOS Interop Details

The macOS smoke harness (`tools/ci/run_interop_smoke.sh`) tests against
Homebrew-provided upstream rsync (typically 3.4.x):

| Scenario | Status |
|----------|--------|
| Baseline upstream -> upstream local copy | Pass |
| oc-rsync sender + upstream receiver (push via daemon) | Pass |
| Upstream sender + oc-rsync receiver (pull via daemon) | Pass |
| Quick-check no-op re-run | Pass |
| Delta update (both directions) | Pass |
| `--list-only` output parity | Pass |

Not covered on macOS (tested on Linux instead):
- xattr/ACL parity (macOS HFS+/APFS semantics differ)
- Daemon mode on privileged port
- SSH loopback

### Windows Interop Details

The Windows smoke harness runs in MSYS2 with the native oc-rsync.exe binary:

| Scenario | Status |
|----------|--------|
| Baseline upstream -> upstream local copy | Pass |
| oc-rsync sender + upstream receiver (push via upstream daemon) | Pass |
| Upstream sender + oc-rsync receiver (pull via upstream daemon) | Pass |
| Quick-check no-op re-run | Pass |
| Delta update | Pass |

Not covered on Windows:
- oc-rsync daemon mode (not available on Windows)
- xattr/ACL/hardlinks/symlinks (NTFS semantics differ)
- SSH loopback
- `--list-only` format parity (Cygwin path differences)

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

## Standalone Interop Tests

Beyond the per-version feature matrix, these standalone scenarios validate
specific edge cases and advanced features:

| Test | Description | Status |
|------|-------------|--------|
| Batch write/read roundtrip | Cross-implementation batch file compatibility | Pass |
| Batch with compression | Compressed batch write/read roundtrip | Pass |
| Batch framing (multifile) | Multi-file batch framing correctness | Pass |
| Compressed batch delta | Delta transfers in compressed batch files | Pass |
| `--info=progress2` | Progress output format | Known limitation |
| 2 GB+ file transfer | Large file transfer via daemon | Known limitation |
| File vanished | `--files-from` with vanished files | Known limitation |
| Copy unsafe + safe links | Symlink safety interaction | Pass |
| Pre/post xfer exec | Daemon hooks | Pass |
| Read-only module | Module permission enforcement | Pass |
| Wrong password auth | Authentication rejection | Pass |
| `--iconv` charset | Character set conversion | Known limitation |
| `--iconv` upstream interop | iconv interop with upstream daemon | Known limitation |
| Hardlinks comprehensive | Deep hardlink scenarios | Pass |
| Inc. recurse comprehensive | Deep INC_RECURSE scenarios | Pass |
| Inc. recurse sender push | Sender-side INC_RECURSE | Pass |
| Unicode filenames | Non-ASCII path handling | Pass |
| Special characters | Shell metacharacters in paths | Pass |
| Empty directories | Empty directory preservation | Pass |
| Many files (100+) | Scalability with many small files | Pass |
| Deep nesting | Deeply nested directory trees | Pass |
| `--modify-window` | Timestamp comparison tolerance | Pass |
| `--trust-sender` | Trust-sender flag handling | Pass |
| `--partial-dir` | Partial directory staging | Pass |
| `--max-connections` | Daemon connection admission | Pass |
| Permissions only | Permission-only transfer (`-p`) | Pass |
| Timestamps only | Timestamp-only transfer (`-t`) | Pass |
| Zstd negotiation | Compression codec auto-negotiation | Pass |
| Delta stats | NDX_DEL_STATS wire correctness | Pass |
| Log format daemon | `--log-format=%i` daemon output | Pass |
| Server-side daemon filter | Daemon `filter` directive | Pass |
| Daemon filter (glob) | Glob patterns in daemon filters | Pass |
| Daemon filter (anchored) | Anchored patterns in daemon filters | Pass |
| Daemon filter (include/exclude) | Combined include/exclude daemon filters | Pass |
| Daemon filter (directive types) | All daemon filter directive types | Pass |
| Daemon filter (overlapping) | Overlapping daemon filter rules | Pass |
| Daemon filter (from files) | Daemon filter `merge` from files | Pass |
| Daemon filter (doublestar) | `**` patterns in daemon filters | Pass |
| Daemon filter (charclass) | Character class `[...]` in daemon filters | Pass |
| Daemon filter (question mark) | `?` wildcard in daemon filters | Pass |
| Daemon filter (push direction) | Daemon filters on push transfers | Pass |
| Link dest (standalone) | `--link-dest` with daemon | Pass |
| Copy dest (standalone) | `--copy-dest` with daemon | Pass |
| Numeric IDs (standalone) | `--numeric-ids` with daemon | Pass |

---

## Known Limitations

### oc-rsync Limitations

| Feature | Description | Tracked |
|---------|-------------|---------|
| ACLs | Transfer succeeds but ACL metadata may be incomplete on some platforms | Yes |
| Extended attrs | Transfer succeeds but xattr handling depends on platform | Yes |
| `--info=progress2` | Output format incomplete | Yes |
| `--iconv` | Charset conversion not fully implemented | Yes |
| 2 GB+ daemon transfer | Not yet validated end-to-end | Yes |
| `--files-from` vanished | Exit code handling for vanished files | Yes |
| Windows daemon mode | Not available on Windows | By design |

### Upstream-Imposed Limitations (not oc-rsync bugs)

| Limitation | Protocol | Upstream Source |
|------------|----------|----------------|
| ACLs require proto 30+ | 28-29 | `compat.c:655-661` |
| Xattrs require proto 30+ | 28-29 | `compat.c:662-668` |
| zstd/lz4 require proto 30+ | 28-29 | `compat.c:556-564` (no vstring negotiation) |
| Merge-filter requires proto 29+ | 28 | `exclude.c:1530` (`legal_len=1`) |
| Upstream compressed delta batch self-roundtrip | All | `token.c:608` (inflate without dict sync) |

---

## CI Workflows

| Workflow | File | Scope |
|----------|------|-------|
| CI (interop job) | `ci.yml` + `_interop.yml` | Full bidirectional daemon interop (3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3), SSH push/pull, protocol forcing (28-32), standalone tests, 2.6.9 push/pull cells, upstream testsuite |
| Interop Validation | `interop-validation.yml` | Exit code validation, message format validation, behavior comparison, batch mode, filter rules, compression codecs, INC_RECURSE. Runs on push, PR, and nightly schedule |
| macOS Interop | `_interop-macos.yml` | Portable smoke harness against Homebrew rsync |
| Windows Interop | `_interop-windows.yml` | Portable smoke harness against MSYS2 rsync (best-effort) |

---

## Upstream rsync Testsuite

oc-rsync is also validated against upstream rsync's own `testsuite/*.test`
corpus. The harness sources upstream's `rsync.fns` and helper tools, running
oc-rsync as `$RSYNC` - the canonical "does oc-rsync look like rsync from
upstream's perspective" check. Expected failures are tracked in
`tools/ci/upstream_testsuite_known_failures.conf`.

---

## How to Verify Locally

```bash
# Run the full interop suite (requires upstream rsync binaries)
bash tools/ci/run_interop.sh

# Run just the portable smoke harness (works on Linux/macOS/Windows)
OC_RSYNC=target/release/oc-rsync UPSTREAM_RSYNC=rsync \
  bash tools/ci/run_interop_smoke.sh

# Build upstream binaries without running tests
bash tools/ci/run_interop.sh build-only
```

Upstream rsync binaries are obtained automatically via multi-tier fallback:
Debian/Ubuntu packages, release tarballs from rsync.samba.org, or git clone
and source build.
