# Interoperability Status

This document is the single reference for oc-rsync's interoperability with
upstream rsync. Every claim is backed by automated CI tests that run on every
pull request and nightly.

For test infrastructure details, see [INTEROP.md](INTEROP.md) and
[interop_testing.md](interop_testing.md). For the full per-feature matrix, see
[user/interop-compatibility-matrix.md](user/interop-compatibility-matrix.md).

---

## Upstream Versions Tested

oc-rsync is tested bidirectionally against five upstream rsync releases spanning
protocol versions 28 through 32. Both directions - oc-rsync client pushing to
upstream daemon, and upstream client pulling from oc-rsync daemon - are
validated for every version on every CI run.

| Upstream Version | Protocol | How Obtained | CI Status |
|------------------|----------|--------------|-----------|
| rsync 2.6.9      | 28-29    | Always source-built | Non-blocking |
| rsync 3.0.9      | 30       | Ubuntu package or source | Required |
| rsync 3.1.3      | 31       | Ubuntu package or source | Required |
| rsync 3.4.1      | 32       | Debian package or source | Required |
| rsync 3.4.2      | 32       | Debian package or source | Required |
| rsync 3.4.3      | 32       | Source-built (security release) | Required |

Upstream binaries are obtained via multi-tier fallback: Debian/Ubuntu packages
first, then release tarballs from rsync.samba.org, then source build from
`./configure && make`.

---

## Protocol Version Support

oc-rsync advertises protocol 32 and negotiates downward when connecting to
older peers. All protocol versions from 28 through 32 are supported.

| Protocol | Introduced In | Wire Encoding | Default Checksum | Inc. Recurse | Status |
|----------|---------------|---------------|------------------|--------------|--------|
| 28       | rsync 2.6.0   | Legacy 4-byte LE | MD4           | No           | Supported |
| 29       | rsync 2.6.6   | Legacy 4-byte LE | MD4           | No           | Supported |
| 30       | rsync 3.0.0   | Varint        | MD5              | Yes          | Supported |
| 31       | rsync 3.1.0   | Varint        | MD5              | Yes          | Supported |
| 32       | rsync 3.4.0   | Varint        | MD5              | Yes          | Supported (primary) |

### Forced Protocol Testing

CI forces each protocol version (28-32) against the newest upstream binary to
verify backward compatibility:

| Forced Protocol | Transfer | Delete | Compress | Hardlinks | Filters |
|-----------------|----------|--------|----------|-----------|---------|
| `--protocol=28` | Pass     | Pass   | Pass     | Pass      | Limited (1) |
| `--protocol=29` | Pass     | Pass   | Pass     | Pass      | Limited (2) |
| `--protocol=30` | Pass     | Pass   | Pass     | Pass      | Pass |
| `--protocol=31` | Pass     | Pass   | Pass     | Pass      | Pass |
| `--protocol=32` | Pass     | Pass   | Pass     | Pass      | Pass |

(1) Protocol 28: merge-filter rules rejected by upstream (`exclude.c:1530
legal_len=1`). ACLs, xattrs, and zstd/lz4 unavailable.

(2) Protocol 29: ACLs, xattrs, and zstd/lz4 unavailable (require protocol 30+
wire format and vstring negotiation).

### Checksum Algorithm Negotiation

| Protocol | Default Checksum | Negotiated Options |
|----------|------------------|--------------------|
| 28-29    | MD4              | None (no negotiation) |
| 30-32    | MD5              | XXH128, XXH3, XXH64, MD5, MD4, SHA1 |

Checksum negotiation requires the `-e.LsfxCIvu` capability string in SSH
transfers. Without it, transfers fall back to MD5.

---

## Transfer Modes

All tested bidirectionally (oc-rsync client to upstream daemon, upstream client
to oc-rsync daemon) against 3.0.9, 3.1.3, and 3.4.x. Versions 3.4.2 and 3.4.3
share protocol 32 with 3.4.1 and pass the identical test matrix.

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Archive mode | `-av` | Pass | Pass | Pass |
| Recursive only | `-rv` | Pass | Pass | Pass |
| Whole file | `-avW` | Pass | Pass | Pass |
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

(1) Requires upstream built with zstd/lz4 support. Skipped when upstream lacks
the codec.

---

## Metadata, Links, Comparison, Delete, and Filters

### Metadata and Attributes

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Permissions | `-rlpv` | Pass | Pass | Pass |
| Numeric IDs | `-av --numeric-ids` | Pass | Pass | Pass |
| Devices | `-avD` | - | - | Pass |
| ACLs | `-avA` | Skipped (proto < 30) | Sender + receiver verified | Sender + receiver verified |
| Extended attrs | `-avX` | - | - | Verified when supported |
| Itemize changes | `-avi` | Pass | Pass | Pass |

### Links

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Symlinks | `-rlptv` | Pass | Pass | Pass |
| Hard links | `-avH` | Pass | Pass | Pass |
| Copy links | `-avL` | - | - | Pass |
| Safe links | `-rlptv --safe-links` | - | - | Pass |
| Hardlinks + relative | `-avHR` | - | - | Pass |
| Hardlinks + delete | `-avH --delete` | - | - | Pass |
| Hardlinks + checksum | `-avHc` | - | - | Pass |
| Cross-dir hardlinks | `-avH --inc-recursive` | - | - | Pass |

### Comparison and Selection

| Feature | Flags | 3.0.9 | 3.1.3 | 3.4.x |
|---------|-------|-------|-------|-------|
| Checksum mode | `-avc` | Pass | Pass | Pass |
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
| Merge filter | `-av -FF` (.rsync-filter) | - | - | Pass |
| Exclude from file | `-av --exclude-from=file` | - | - | Pass |
| Relative paths | `-avR` | Pass | Pass | Pass |
| Files from | `-av --files-from=list` | - | - | Pass |
| Delete + exclude | `-av --delete --exclude=*.log` | - | - | Pass |
| Delete excluded | `-av --delete-excluded` | - | - | Pass |

---

## Transport Modes

### Daemon Mode (rsync://)

| Direction | Versions Tested |
|-----------|-----------------|
| oc-rsync client -> upstream daemon (push) | 2.6.9, 3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3 |
| Upstream client -> oc-rsync daemon (pull) | 2.6.9, 3.0.9, 3.1.3, 3.4.1, 3.4.2, 3.4.3 |

### SSH Transport

| Direction | Status |
|-----------|--------|
| oc-rsync client -> upstream server (push) | Pass |
| oc-rsync client <- upstream server (pull) | Pass |
| No-change re-run (quick-check) | Pass |
| SSH with compression | Pass |

### Batch File Interop

| Direction | Status |
|-----------|--------|
| oc-rsync `--write-batch` -> upstream `--read-batch` | Pass |
| Upstream `--write-batch` -> oc-rsync `--read-batch` | Pass |
| oc-rsync `--write-batch -z` -> upstream `--read-batch` | Pass |
| Upstream `--write-batch -z` -> oc-rsync `--read-batch` | Pass |
| Upstream compressed delta batch self-roundtrip | Known failure (upstream bug) |

The upstream compressed delta batch failure is a bug in upstream rsync
(`token.c:608`): the batch writer tees deflated data without dictionary sync,
so upstream cannot read back its own compressed delta batch files. oc-rsync
reads these files correctly.

---

## Upstream rsync Testsuite

oc-rsync runs upstream rsync's own `testsuite/*.test` corpus (from rsync 3.4.2)
with oc-rsync substituted as `$RSYNC`. This is the canonical "does oc-rsync
look like rsync from upstream's perspective" check. The harness reuses
upstream's `rsync.fns`, `tls`, `getgroups`, and `support/lsh.sh` helper tools.

### Known Failures

Tests tracked in `tools/ci/upstream_testsuite_known_failures.conf`:

| Test | Reason |
|------|--------|
| `dir-sgid` | oc-rsync does not yet preserve inherited setgid bit on mkdir |

### Resolved (Previously Failing)

These upstream testsuite tests previously failed but now pass:

- `ssh-basic` - fixed by `build_capability_string_suffix()` providing the
  embeddable capability string (`-e.LsfxCIvu`)
- `devices` - passes via fakeroot on CI
- `chown` - passes via fakeroot on CI
- `protected-regular` - self-skips (exit 77) when not root

### Test Outcome Categories

| Outcome | Meaning |
|---------|---------|
| PASS    | Test passed (not in known-failure list) |
| FAIL    | Test failed unexpectedly (regression) |
| XFAIL   | Test failed as expected (in known-failure list) |
| UPASS   | Test passed but is in known-failure list (entry should be removed) |
| SKIP    | Test self-skipped (exit 77), typically requires root or special env |

---

## Standalone and Edge Case Tests

Beyond the per-version feature matrix, dedicated standalone scenarios validate
specific edge cases:

| Test | Status |
|------|--------|
| Pre/post xfer exec (daemon hooks) | Pass |
| Read-only module enforcement | Pass |
| Wrong password auth rejection | Pass |
| `--max-connections` admission | Pass |
| Unicode filenames | Pass |
| Shell metacharacters in paths | Pass |
| Empty directory preservation | Pass |
| Many files (100+) | Pass |
| Deep directory nesting | Pass |
| `--modify-window` tolerance | Pass |
| `--trust-sender` handling | Pass |
| `--partial-dir` staging | Pass |
| Copy dest with daemon | Pass |
| Link dest with daemon | Pass |
| Daemon filter directives (glob, anchored, include/exclude, doublestar, charclass) | Pass |
| Daemon filter push direction | Pass |
| Delta stats (NDX_DEL_STATS wire) | Pass |
| Zstd codec auto-negotiation | Pass |
| `--info=progress2` | Known limitation |
| 2 GB+ file transfer via daemon | Known limitation |
| `--files-from` with vanished files | Known limitation |
| `--iconv` charset conversion | Known limitation |

---

## Platform Coverage

### CI Matrix

| Platform | Workflow | Interop Scope | CI Status |
|----------|----------|---------------|-----------|
| Linux x86_64 (Ubuntu) | `ci.yml` + `_interop.yml` | Full multi-version daemon interop, SSH, protocol forcing, upstream testsuite | Required |
| Linux x86_64 (Ubuntu) | `interop-validation.yml` | Exit codes, messages, behavior, batch, filters, compression, INC_RECURSE | Required |
| macOS (latest) | `_interop-macos.yml` | Smoke: push, pull, quick-check, delta, list-only (Homebrew rsync) | Required |
| Windows (latest) | `_interop-windows.yml` | Smoke: push, pull, quick-check, delta (MSYS2 rsync) | Non-blocking |

### macOS Interop

Tested against Homebrew-provided upstream rsync (typically 3.4.x):

- Baseline upstream-to-upstream local copy - Pass
- oc-rsync sender + upstream receiver (push via daemon) - Pass
- Upstream sender + oc-rsync receiver (pull via daemon) - Pass
- Quick-check no-op re-run - Pass
- Delta update (both directions) - Pass
- `--list-only` output parity - Pass

Not covered on macOS (tested on Linux only): xattr/ACL parity, daemon on
privileged port, SSH loopback.

### Windows Interop

Tested in MSYS2 with the native oc-rsync.exe binary:

- Baseline upstream-to-upstream local copy - Pass
- oc-rsync push via upstream daemon - Pass
- Upstream pull via upstream daemon - Pass
- Quick-check no-op re-run - Pass
- Delta update - Pass

Not covered on Windows: oc-rsync daemon mode (unavailable on Windows),
xattr/ACL/hardlinks/symlinks (NTFS semantics differ), SSH loopback.

---

## Interop Validation Workflow

The dedicated `interop-validation.yml` workflow runs on push, PR, and nightly
schedule. It validates:

| Job | Scope |
|-----|-------|
| Exit Code Validation | oc-rsync exit codes match upstream for all 25 documented codes |
| Message Format Validation | Error/warning message formats match upstream patterns |
| Behavior Comparison | Side-by-side behavior comparison for common scenarios |
| Batch Mode Interop | Cross-implementation batch file read/write compatibility |
| Filter Rules Interop | Filter rule matching parity (exclude, include/exclude, merge, delete+filter) |
| Compression Codec Interop | zlibx, zstd, lz4, and auto-negotiation |
| INC_RECURSE Interop | Incremental recursion with deep trees, deletes, and large directory counts |

---

## Known Limitations

### oc-rsync Gaps

| Feature | Description |
|---------|-------------|
| ACLs | `-avA` interop is verified against upstream in both directions when the upstream binary advertises ACL support and the host filesystem supports POSIX ACLs: the oc client and the oc wire receiver both round-trip named-user/group entries and the mask exactly (the earlier receiver drop of named entries + mask was fixed; wire-decode round-trip is unit-tested), and it is skipped (not failed) when unsupported. POSIX access + default ACLs are covered on Linux/macOS/FreeBSD (macOS APFS has no default-ACL concept, matching upstream). Residual limitation: Windows DACL is supported but SACL/inheritance/protected bits are deferred (Tier-1C), and cross-model POSIX<->NFSv4<->DACL transfer is lossy exactly as it is in upstream (no cross-model translation) |
| Extended attrs | `-avX` round-trip is verified against upstream in both directions when the upstream binary advertises xattr support and the host filesystem supports user xattrs; skipped (not failed) otherwise |
| `--info=progress2` | Output format not fully implemented |
| `--iconv` | Charset conversion not implemented |
| 2 GB+ daemon transfer | Not yet validated end-to-end |
| `--files-from` vanished | Exit code handling for vanished files |
| `dir-sgid` | Setgid bit not preserved on directory creation |
| Windows daemon mode | Not available on Windows (by design) |

### Upstream-Imposed Limitations

These are not oc-rsync bugs - upstream rsync itself fails identically.

| Limitation | Protocol | Upstream Source |
|------------|----------|----------------|
| ACLs require proto 30+ | 28-29 | `compat.c:655-661` |
| Xattrs require proto 30+ | 28-29 | `compat.c:662-668` |
| zstd/lz4 require proto 30+ | 28-29 | `compat.c:556-564` |
| Merge-filter requires proto 29+ | 28 | `exclude.c:1530` |
| Compressed delta batch self-roundtrip | All | `token.c:608` |

---

## Behavioral Differences

These scenarios produce different behavior between oc-rsync and upstream rsync:

| Scenario | Difference |
|----------|------------|
| `--keep-dirlinks` (`-K`) with symlinks | oc-rsync creates extra nested directory |
| `--list-only` on local paths | oc-rsync returns exit code 23; upstream returns 0 |

---

## CI Workflows Reference

| Workflow | File | Trigger |
|----------|------|---------|
| CI (interop job) | `ci.yml` + `_interop.yml` | Push/PR |
| Interop Validation | `interop-validation.yml` | Push/PR/nightly |
| macOS Interop | `_interop-macos.yml` | Push/PR (called from ci.yml) |
| Windows Interop | `_interop-windows.yml` | Push/PR (called from ci.yml) |

### Test Scripts

| Script | Purpose |
|--------|---------|
| `tools/ci/run_interop.sh` | Primary CI interop runner (11K+ lines, all versions/features) |
| `tools/ci/run_interop_smoke.sh` | Portable smoke harness (macOS, Windows) |
| `tools/ci/run_upstream_testsuite.sh` | Upstream testsuite runner against oc-rsync |
| `tools/ci/known_failures.conf` | Known failure registry for interop suite |
| `tools/ci/upstream_testsuite_known_failures.conf` | Known failure registry for upstream testsuite |

---

## How to Verify Locally

```bash
# Run the full interop suite (builds upstream binaries automatically)
bash tools/ci/run_interop.sh

# Build upstream binaries without running tests
bash tools/ci/run_interop.sh build-only

# Run the portable smoke harness (works on Linux/macOS/Windows)
OC_RSYNC=target/release/oc-rsync UPSTREAM_RSYNC=rsync \
  bash tools/ci/run_interop_smoke.sh

# Run upstream rsync's own testsuite against oc-rsync
bash tools/ci/run_upstream_testsuite.sh
```

Upstream rsync source is downloaded to `target/interop/upstream-src/`. If
already present, the scripts reuse the cached build.
