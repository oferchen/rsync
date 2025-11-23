# oc-rsync Parity Checklist (vs rsync 3.4.1)

This matrix tracks the feature-by-feature parity status of `oc-rsync` against upstream `rsync 3.4.1`.

## ✅ Legend

- ✅ Complete  
- ⚠️ Partial (exists but not full)  
- ❌ Missing  
- 🧪 Covered by interop tests  
- 🛠️ Planned in backlog  

---

## 1. Modes & Transport

| Feature                        | Status | Notes                              |
|-------------------------------|--------|------------------------------------|
| Local copy                    | ✅     | `src/ dest/`                       |
| Remote via SSH                | ✅     | `host:path`                        |
| `--server` role invocation    | ✅     | Internal; flag parsing validated   |
| Daemon mode (`--daemon`)      | ⚠️     | Basic module serving only          |
| `rsync://host/module` syntax  | ⚠️     | Supported but not fully tested     |

---

## 2. CLI Semantics

| Flag / Feature                | Status | Notes                                      |
|------------------------------|--------|--------------------------------------------|
| `-a` (archive mode)          | ✅     | Aggregates core metadata flags             |
| `--checksum`                 | ✅     | Works; performance tested                  |
| `--delete*` flags            | ✅     | Full suite matched                         |
| `--backup*` options          | ✅     | Suffix, dir, timing tested                 |
| `--sparse`                   | ⚠️     | Works; upstream hole layout not verified   |
| `--append*`                  | ✅     | Verified with interop                      |
| `--partial`, `--inplace`     | ✅     | Works per upstream behaviour               |
| `--compress`                 | ✅     | zlib-compatible; verified                  |
| `--xattrs`, `--acls`         | ⚠️     | Preserved, ACL partially implemented       |
| `--chmod`, `--numeric-ids`   | ✅     | Verified locally                           |
| `--info`, `--debug`, `--out-format` | ⚠️  | Format parsing complete; match pending     |

---

## 3. Filter Engine

| Rule Type                    | Status | Notes                         |
|-----------------------------|--------|-------------------------------|
| `--include` / `--exclude`   | ✅     | Grammar conforms              |
| `--filter`                  | ⚠️     | Some rule edge cases pending |
| `--files-from`              | ✅     | `--from0` also supported      |

---

## 4. Metadata & Filesystem

| Metadata Type               | Status | Notes                                  |
|----------------------------|--------|----------------------------------------|
| Permissions                | ✅     | `--perms`, `--chmod`                   |
| Ownership (UID/GID)        | ✅     | `--owner`, `--group`, `--numeric-ids` |
| Symlinks                   | ✅     | Fully round-tripped                    |
| Hardlinks                  | ✅     | Verified in link-dest tests            |
| Timestamps                 | ✅     | Atime/mtime preserved                  |
| ACLs                       | ⚠️     | Partial; tests WIP                     |
| Extended attributes (xattr)| ⚠️     | Preserved on Linux; verify elsewhere   |
| Devices / specials         | ✅     | `-D` tested                            |
| Sparse holes               | ⚠️     | Block counts match, but hole layout TBD|

---

## 5. Daemon Features

| Feature                        | Status | Notes                           |
|-------------------------------|--------|---------------------------------|
| Module definitions            | ✅     | `path`, `comment`, `uid`, etc. |
| Host allow/deny               | ⚠️     | Parsing implemented             |
| Secrets file auth             | ❌     | Not yet enforced                |
| Max connections               | ❌     | To be implemented               |
| Chroot and privilege drop     | ⚠️     | Drop to `uid` supported         |

---

## 6. Protocol & Compatibility

| Capability / Behavior        | Status | Notes                            |
|-----------------------------|--------|----------------------------------|
| Protocol 32 compliance      | ✅     | Interop OK                       |
| Capability negotiation      | ⚠️     | Some upstream bits missing       |
| Sender/receiver FSM         | ✅     | Validated via interop            |
| Message tags                | ✅     | All known tags supported         |
| Multiplexed streams         | ✅     | Works for stdout/stderr/data     |
| Interop with upstream 3.4.1 | ✅     | Bidirectional verified           |
| Interop with 3.1.3 / 3.0.9  | ⚠️     | Basic fallback only              |

---
