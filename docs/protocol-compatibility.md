# Protocol Compatibility Matrix

This document describes oc-rsync's wire protocol compatibility with upstream
rsync across protocol versions 28-32 and upstream releases 3.0.9, 3.1.3, and
3.4.1. All claims are backed by automated CI tests that run on every pull
request and nightly.

---

## Protocol Version Support

oc-rsync supports protocol versions 28 through 32. The implementation
advertises protocol 32 and negotiates downward when connecting to older peers.

| Protocol | Introduced In | Checksum | Compat Flags | Inc. Recurse | Status |
|----------|---------------|----------|--------------|--------------|--------|
| 28       | rsync 2.6.0   | MD4      | N/A          | No           | Supported |
| 29       | rsync 2.6.6   | MD4      | N/A          | No           | Supported |
| 30       | rsync 3.0.0   | MD5      | Varint       | Yes          | Supported |
| 31       | rsync 3.1.0   | MD5      | Varint       | Yes          | Supported |
| 32       | rsync 3.4.0   | MD5      | Varint       | Yes          | Supported (primary) |

Protocol 28-29 use the legacy 4-byte LE wire encoding for compatibility flags.
Protocol 30+ uses varint encoding and supports the binary negotiation handshake
introduced in rsync 3.0.0.

### Checksum Algorithm Negotiation

| Protocol | Default Strong Checksum | Negotiated Options (protocol 30+) |
|----------|------------------------|-----------------------------------|
| 28-29    | MD4                    | N/A (no negotiation)              |
| 30-32    | MD5                    | XXH128, XXH3, XXH64, MD5, MD4, SHA1 |

Checksum negotiation requires the `-e.LsfxCIvu` capability string in SSH
transfers. Without it, transfers fall back to MD5.

---

## Upstream Version Compatibility

### Bidirectional Daemon Interop

Every CI run tests both directions for each upstream version:
- **Pull**: upstream rsync client pulls from oc-rsync daemon
- **Push**: oc-rsync client pushes to upstream rsync daemon

| Upstream Version | Protocol | Pull (upstream -> oc) | Push (oc -> upstream) |
|------------------|----------|-----------------------|-----------------------|
| rsync 3.0.9      | 30       | Pass                  | Pass                  |
| rsync 3.1.3      | 31       | Pass                  | Pass                  |
| rsync 3.4.1      | 32       | Pass                  | Pass                  |

### SSH Transport Interop

SSH interop is tested with upstream rsync 3.4.1 on the PATH:

| Direction                          | Status |
|------------------------------------|--------|
| oc-rsync client -> upstream server | Pass   |
| oc-rsync client <- upstream server | Pass   |
| oc-rsync local SSH (loopback)      | Pass   |

### Batch File Interop

Batch files written by one implementation can be read by the other:

| Direction                                  | Status       |
|--------------------------------------------|--------------|
| upstream --write-batch -> oc-rsync --read-batch | Known failure |
| oc-rsync --write-batch -> upstream --read-batch | Known failure |
| oc-rsync daemon --write-batch -> --read-batch   | Known failure |

---

## Feature Compatibility by Upstream Version

The table below lists every feature tested in the comprehensive interop suite.
Each feature is tested bidirectionally (upstream client -> oc-rsync daemon and
oc-rsync client -> upstream daemon) against all three upstream versions.

### Transfer Modes

| Feature              | Flags                     | 3.0.9 | 3.1.3 | 3.4.1 |
|----------------------|---------------------------|-------|-------|-------|
| Archive mode         | `-av`                     | Pass  | Pass  | Pass  |
| Recursive only       | `-rv`                     | Pass  | Pass  | Pass  |
| Whole file           | `-avW`                    | Pass  | Pass  | Pass  |
| Delta transfer       | `-av --no-whole-file -I`  | Pass  | Pass  | Pass  |
| Inplace              | `-av --inplace`           | Pass  | Pass  | Pass  |
| Compression          | `-avz`                    | Pass  | Pass  | Pass  |
| Compress level 1     | `-avz --compress-level=1` | Pass  | Pass  | Pass  |
| Compress level 9     | `-avz --compress-level=9` | Pass  | Pass  | Pass  |
| Compressed delta     | `-avz --no-whole-file -I` | Pass  | Pass  | Pass  |
| Sparse files         | `-avS`                    | Pass  | Pass  | Pass  |
| Append mode          | `-av --append`            | Pass  | Pass  | Pass  |
| Partial transfer     | `-av --partial`           | Pass  | Pass  | Pass  |
| Bandwidth limit      | `-av --bwlimit=10000`     | Pass  | Pass  | Pass  |
| Delay updates        | `-av --delay-updates`     | Pass  | Pass  | Pass  |
| Dry run              | `-avn`                    | Pass  | Pass  | Pass  |
| Inc. recursive       | `-av --inc-recursive`     | Pass  | Pass  | Pass  |

### Metadata and Attributes

| Feature              | Flags                     | 3.0.9 | 3.1.3 | 3.4.1 |
|----------------------|---------------------------|-------|-------|-------|
| Permissions          | `-rlpv`                   | Pass  | Pass  | Pass  |
| Numeric IDs          | `-av --numeric-ids`       | Pass  | Pass  | Pass  |
| Devices              | `-avD`                    | Pass  | Pass  | Pass  |
| ACLs                 | `-avA`                    | Known limitation | Known limitation | Known limitation |
| Extended attrs       | `-avX`                    | Known limitation | Known limitation | Known limitation |

### Links

| Feature              | Flags                     | 3.0.9 | 3.1.3 | 3.4.1 |
|----------------------|---------------------------|-------|-------|-------|
| Symlinks             | `-rlptv`                  | Pass  | Pass  | Pass  |
| Hard links           | `-avH`                    | Pass  | Pass  | Pass  |
| Copy links           | `-avL`                    | Pass  | Pass  | Pass  |
| Safe links           | `-rlptv --safe-links`     | Pass  | Pass  | Pass  |
| Hardlinks + relative | `-avHR`                   | Pass  | Pass  | Pass  |

### Comparison and Selection

| Feature              | Flags                     | 3.0.9 | 3.1.3 | 3.4.1 |
|----------------------|---------------------------|-------|-------|-------|
| Checksum mode        | `-avc`                    | Pass  | Pass  | Pass  |
| Checksum skip        | `-avc` (identical files)  | Pass  | Pass  | Pass  |
| Size only            | `-av --size-only`         | Pass  | Pass  | Pass  |
| Ignore times         | `-av --ignore-times`      | Pass  | Pass  | Pass  |
| Update mode          | `-av --update`            | Pass  | Pass  | Pass  |
| Existing only        | `-av --existing`          | Pass  | Pass  | Pass  |
| One file system      | `-avx`                    | Pass  | Pass  | Pass  |

### Delete and Backup

| Feature              | Flags                     | 3.0.9 | 3.1.3 | 3.4.1 |
|----------------------|---------------------------|-------|-------|-------|
| Delete               | `-av --delete`            | Pass  | Pass  | Pass  |
| Delete after         | `-av --delete-after`      | Pass  | Pass  | Pass  |
| Delete during        | `-av --delete-during`     | Pass  | Pass  | Pass  |
| Max delete           | `-av --delete --max-delete=1` | Pass | Pass | Pass |
| Backup               | `-av --backup`            | Pass  | Pass  | Pass  |
| Compare dest         | `-av --compare-dest=ref`  | Pass  | Pass  | Pass  |
| Link dest            | `-av --link-dest=ref`     | Pass  | Pass  | Pass  |

### Filters and Paths

| Feature              | Flags                     | 3.0.9 | 3.1.3 | 3.4.1 |
|----------------------|---------------------------|-------|-------|-------|
| Exclude pattern      | `-av --exclude=*.log`     | Pass  | Pass  | Pass  |
| Relative paths       | `-avR`                    | Pass  | Pass  | Pass  |
| Files from           | `-av --files-from=list`   | Pass  | Pass  | Pass  |

### Output

| Feature              | Flags                     | 3.0.9 | 3.1.3 | 3.4.1 |
|----------------------|---------------------------|-------|-------|-------|
| Itemize changes      | `-avi`                    | Known limitation | Known limitation | Known limitation |

### Protocol Forcing

| Feature              | Flags                     | 3.4.1 |
|----------------------|---------------------------|-------|
| Force protocol 28    | `-av --protocol=28`       | Known limitation (daemon transfers) |
| Force protocol 29    | `-av --protocol=29`       | Known limitation (daemon transfers) |
| Force protocol 30    | `-av --protocol=30`       | Pass  |
| Force protocol 31    | `-av --protocol=31`       | Pass  |
| Force protocol 32    | `-av --protocol=32`       | Pass  |

---

## Known Limitations and Failures

### Tracked Known Failures

These are documented in the `KNOWN_FAILURES` array in `tools/ci/run_interop.sh`
and are not treated as CI regressions.

| Key                           | Direction        | Description |
|-------------------------------|------------------|-------------|
| `oc:acls`                     | oc -> upstream   | Upstream daemon may reject ACL capabilities if built without `--enable-acl-support`. |
| `oc:xattrs`                   | oc -> upstream   | Upstream daemon may reject xattr capabilities if built without `--enable-xattr-support`. |
| `oc:itemize`                  | oc -> upstream   | Fixed: client-side itemize callback emits `<f` lines for push transfers. |
| `up:protocol-31`              | upstream -> oc   | rsync 3.0.9 does not support protocol 31 (expected). |
| `up:acls`                     | upstream -> oc   | Upstream daemon builds may not have ACL support enabled. |
| `up:xattrs`                   | upstream -> oc   | Upstream daemon builds may not have xattr support enabled. |
| `up:itemize`                  | upstream -> oc   | Fixed: daemon parses `--log-format=%i` and emits MSG_INFO itemize frames; test regex accepts both directions. |
| `standalone:write-batch-read-batch` | both       | Batch file format interop incomplete. |
| `standalone:info-progress2`   | oc-rsync         | `--info=progress2` output format incomplete. |
| `standalone:large-file-2gb`   | upstream -> oc   | 2GB+ file transfer via daemon not yet validated. |
| `standalone:file-vanished`    | oc-rsync         | `--files-from` with vanished files exit code handling. |
| `standalone:iconv`            | oc-rsync         | `--iconv` charset conversion not yet implemented. |
| Protocols 28-29 (forced)      | both             | Forced-protocol daemon transfers at v28/v29 not fully supported. |

### Resolved Known Failures

These were previously tracked as known failures and have since been fixed:

- `up:checksum`, `oc:checksum` - always-checksum mode implemented.
- `up:delete` - `apply_long_form_args` now parses `--delete`/`--delete-before`.
- `up:symlinks`, `oc:symlinks` - `create_symlinks()` in receiver.
- `oc:delete`, `oc:numeric-ids`, `oc:exclude` - correct compact flag semantics and long-form args.
- `up:compress`, `oc:compress` - `TokenReader` integration in `run_sync` path.
- `up:size-only` - `do_compression` check no longer matched `z` in `--size-only` long-form arg.

### Behavioral Differences (from behavior scenarios)

| Scenario                | Status  | Notes |
|-------------------------|---------|-------|
| `copy_symlink_keep_dirlinks` | Skipped | oc-rsync creates extra nested `realdir/realdir` with `-K`. |
| `list_only`             | Skipped | oc-rsync returns exit code 23 for `--list-only` on local paths; upstream returns 0. |
| `files_from`            | Skipped | oc-rsync does not yet filter by `--files-from` list; copies all source files. |

---

## Tested Transfer Modes

| Mode                  | Description                                     | CI Coverage |
|-----------------------|-------------------------------------------------|-------------|
| Local copy            | `oc-rsync -av src/ dest/`                       | Integration tests, behavior comparison |
| Daemon push (oc -> up)| `oc-rsync -av src/ rsync://host/module/`        | Bidirectional interop per version |
| Daemon pull (up -> oc)| `upstream-rsync -av src/ rsync://host/module/`  | Bidirectional interop per version |
| SSH push              | `oc-rsync -av src/ host:dest/`                  | SSH interop in CI |
| SSH pull              | `oc-rsync -av host:src/ dest/`                  | SSH interop in CI |
| Batch write           | `oc-rsync --write-batch=file src/ dest/`        | Standalone tests (known limitation) |
| Batch read            | `oc-rsync --read-batch=file dest/`              | Standalone tests (known limitation) |

---

## CI Workflows

Interop testing is distributed across several CI workflows:

| Workflow                        | File                                  | Scope |
|---------------------------------|---------------------------------------|-------|
| CI (interop job)                | `.github/workflows/ci.yml`            | Basic bidirectional daemon + SSH interop for 3.0.9, 3.1.3, 3.4.1. Comprehensive scenarios for all features. Protocol forcing for v28-v32. Standalone tests. |
| Interop Validation              | `.github/workflows/interop-validation.yml` | Exit code validation, message format validation, behavior comparison, batch mode interop. Runs on push, PR, and nightly. |
| Interop Tests (reusable)        | `.github/workflows/_interop.yml`      | Reusable workflow called by CI. Builds upstream binaries, sets up SSH loopback, runs full interop suite. |

---

## References

- Protocol wire format: [docs/PROTOCOL.md](PROTOCOL.md)
- Feature matrix: [docs/feature_matrix.md](feature_matrix.md)
- Interop test details: [docs/INTEROP.md](INTEROP.md)
- Interop testing guide: [docs/interop_testing.md](interop_testing.md)
- Interop test script: `tools/ci/run_interop.sh`
- Upstream rsync source: `target/interop/upstream-src/rsync-3.4.1/`
