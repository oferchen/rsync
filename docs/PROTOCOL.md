# oc-rsync Protocol 32 Implementation

This document describes the wire protocol negotiation, framing, roles, and state machine logic used to implement upstream rsync 3.4.1 Protocol 32.

---

## Protocol Version

- oc-rsync must always emit: `protocol version = 32`
- Must accept fallback negotiation to 28–32
- Fails with explicit error for unsupported versions

---

## Capability Bitmask

Capabilities must be announced/negotiated:

| Capability Bit       | Meaning                              |
|----------------------|--------------------------------------|
| `xattrs`             | Preserve extended attributes         |
| `acls`               | Preserve POSIX ACLs                  |
| `symlink-times`      | Timestamp preservation for symlinks  |
| `iconv`              | Supports character set conversion    |
| `delete-during`      | Mid-transfer deletes allowed         |
| `partial-dir`        | Use partial temp directories         |
| `msgs2stderr`        | Info/debug sent to stderr channel    |

---

## Message Tags

| Tag              | Direction       | Purpose                                  |
|------------------|-----------------|------------------------------------------|
| `MSG_DATA`       | Sender → Receiver | File contents                            |
| `MSG_RECV`       | Receiver → Sender | Block requests                           |
| `MSG_INFO`       | Both            | Informational messages                   |
| `MSG_ERROR`      | Both            | Exit triggers                             |
| `MSG_DONE`       | Sender          | File complete marker                      |

Each message is:

```
[length: u32] [tag: u8] [payload...]
```

---

## Roles

| Role     | Description                              |
|----------|------------------------------------------|
| Sender   | Emits file list and blocks               |
| Receiver | Applies deltas and writes to disk        |

---

## Multiplexed Channel Semantics

- stdout = default channel  
- stderr = multiplexed when supported (`--msgs2stderr`)  
- Flush must respect upstream framing behaviour

---

## Framing Tests

Required:

- Upstream → oc-rsync frame parsing  
- oc-rsync → upstream receiver tests  
- Out-of-order frame recovery  
- Error handling on invalid tags

---
