# oc-rsync Interoperability Tests

This document defines the upstream compatibility test scenarios used to validate oc-rsync’s parity with rsync 3.0.9, 3.1.3, and 3.4.1.

---

## Upstream Binaries Tested

- `rsync-3.0.9`  
- `rsync-3.1.3`  
- `rsync-3.4.1`

---

## Test Matrix

| Scenario                            | Description                              | Result    |
|-------------------------------------|------------------------------------------|-----------|
| Local copy (archive mode)           | `-av /src /dest`                          | ✅        |
| Sparse file round-trip              | Zero run → hole                           | ⚠️ layout |
| Remote copy via SSH (sender/recv)   | `-av host:/src /dest`                     | ✅        |
| Daemon transfer (host::module)      | `-av rsync://host/module/ /dest`         | ⚠️ auth   |
| Filters (include/exclude/filter)    | Deep ruleset match                        | ✅        |
| Compression level                   | `-z --compress-level=9`                  | ✅        |
| Metadata flags                      | `-aHAX --numeric-ids`                    | ⚠️ ACL    |
| Delete options                      | `--delete-excluded`, etc.                | ✅        |
| File list diffing                   | Match order, mtime, permission checks     | ✅        |
| Exit code match                     | Match known upstream codes                | ✅        |

---

## Test Infrastructure

- Compares:
  - Exit code  
  - Stdout/stderr (normalized)  
  - Final file system tree  
  - File content hashes  
  - Block allocation (sparse)  

- Run via `xtask test-interop`

---

## Known Gaps

- Daemon auth secrets not enforced  
- ACLs partially implemented  
- Sparse hole layout not guaranteed to match block-for-block yet  
- Filter `--filter='merge ...'` edge cases pending

---
