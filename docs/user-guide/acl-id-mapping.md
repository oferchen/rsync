# ACL ID mapping on the receiver

This guide explains how oc-rsync handles POSIX ACL entries whose UIDs or GIDs
do not resolve to a local user or group on the receiver, and how the behaviour
changed in PR #4742 (commit `07f81641f`) to match upstream rsync 3.4.2.

## What ACL ID mapping means

A POSIX.1e ACL can carry per-user (`u:NAME:rwx`) and per-group (`g:NAME:rwx`)
entries that name principals beyond the file owner and primary group. On the
wire, each named entry travels as a numeric UID/GID plus an optional name
suffix (the `XFLAG_NAME_FOLLOWS` field). When the receiver applies the ACL it
must decide which local UID/GID to install for each named entry:

1. If the sender included a name and the name resolves on the receiver
   (`getpwnam_r` / `getgrnam_r`), use the resolved local id.
2. Otherwise, install the entry with the raw numeric id that arrived on the
   wire and let the kernel accept or reject it.

Step 2 is the "unmappable id" case this guide is about: the numeric id from
the sender's host has no corresponding `/etc/passwd` or `/etc/group` entry on
the receiver, and the sender either shipped no name or shipped a name that
also fails to resolve locally.

## Behaviour before the fix (ACL-1 / PR #4742)

oc-rsync's `is_unsupported_error` previously swallowed `EPERM` from the
filesystem's `setfacl` call. When a non-root receiver tried to install an ACL
containing an id it could not manage, the kernel returned `EPERM`, the entire
ACL apply was treated as "filesystem does not support ACLs", and the ACL was
silently dropped from the file. The receiver also never emitted the upstream
`DEBUG_GTE(OWN, 2)` "uid %u(%s) maps to %u" line that documents which ids
were remapped, so the loss was invisible without strace or post-transfer
inspection.

Concrete symptom: copying a file with `setfacl -m u:1234:rwx file` from host A
(where UID 1234 exists) to host B (where it does not), under a non-root
receiver, left `file` on host B without the `u:1234:rwx` entry and without
any warning. Cross-system ACL transfers lost data quietly.

## Behaviour after the fix

oc-rsync now mirrors upstream rsync 3.4.2 `acls.c::recv_ida_entries` and
`uidlist.c::recv_add_id` (upstream commit gating the `am_root` check off):

- Every named ACL entry routes through `resolve_ida_id`
  (`crates/metadata/src/acl_exacl/reconstruct.rs:133-187`).
- When the sender shipped a name, the receiver tries `getpwnam_r` /
  `getgrnam_r` and uses the resolved local id when present; otherwise the
  raw wire id passes through verbatim (`id2 = id` at upstream
  `uidlist.c:282`).
- Named entries are never dropped during ACL reconstruction.
- `is_unsupported_error` (`crates/metadata/src/acl_exacl/error.rs`) now
  swallows only the codes upstream's `no_acl_syscall_error` swallows -
  `ENOSYS`, `ENOTSUP`, `EINVAL`, `ENODATA` (plus `ENOENT` on macOS for the
  documented directory-ACL quirk). `EPERM` and Linux `ENOENT` surface to the
  caller, so a non-root receiver that cannot install an id sees a real
  error rather than a silent ACL drop.
- The `DEBUG_GTE(OWN, 2)` mapping line is emitted via
  `protocol::acl::trace_acl_uid_remap` / `trace_acl_gid_remap`, gated at
  `--debug=own2`.

The receiver hands the ACL to the kernel with the raw numeric id when no
mapping is known. POSIX ACL filesystems accept this: the kernel stores the
numeric id and `getfacl` will display it as a bare number rather than a name.

## What this means in practice

### Root receiver, root sender

ACLs round-trip identically. All ids are mappable, all names resolve, no
behavioural change versus the pre-fix path.

### Non-root receiver, mappable ids

The receiver resolves names through local NSS where it can, and installs the
local id. No change in observable behaviour - this was already the working
path.

### Non-root receiver, unmappable ids

Pre-fix: the entire ACL was dropped, transfer succeeded silently.

Post-fix: the ACL is installed with the raw numeric id. `getfacl` will show
the id as a number. The entry exists, the kernel accepts it, and the
filesystem records it. If the kernel rejects the id (a true `EPERM`), the
receiver now reports the error instead of swallowing it.

### Cross-system migration

`--numeric-ids` is **no longer required** for ACL fidelity. The default path
already preserves unmappable ids by falling back to the raw wire id, which
matches what `--numeric-ids` would do for the ownership path. Use
`--numeric-ids` when you want to suppress name lookups for ownership too;
use `--usermap` / `--groupmap` when you want explicit translation tables.

## Reading the result with `getfacl`

```sh
# Default: displays the resolved name when one exists, else the bare number.
getfacl file

# Forces numeric display for every id, useful for verifying that the wire id
# was preserved end-to-end.
getfacl --numeric file
```

If a numeric id appears where you expected a name, the id was unmappable on
the receiver. The filesystem accepted it, and the entry will continue to
match any future user or group that adopts that id.

## Related flags

| Flag | Scope | Effect |
|------|-------|--------|
| `-A`, `--acls` | ACL preservation | Wire-ships POSIX ACLs; the fix described here applies when this is set. |
| `--numeric-ids` | Ownership and ACLs | Suppresses name lookups on the receiver; sends raw ids on the wire. ACL fidelity now matches `--numeric-ids` behaviour by default for unmappable ids. |
| `--usermap=SRC:DST,...` | Ownership and ACLs | Explicit per-user id remap, applied before `getpwnam_r`. Use when source and destination disagree on which name should hold which id. |
| `--groupmap=SRC:DST,...` | Ownership and ACLs | Group equivalent of `--usermap`. |
| `--chown=USER:GROUP` | Ownership only | Forces a single destination owner; does not affect ACL entries. |

`--usermap` / `--groupmap` take precedence over the NSS lookup performed by
`resolve_ida_id`. They run earlier in the receive pipeline; the ACL
reconstruction path sees the already-remapped id.

## Upgrade note

When upgrading from a version older than PR #4742, expect new ACL entries to
appear on receivers where ACLs were previously being silently dropped:

- Files transferred under a non-root receiver where the sender ACL named ids
  the receiver could not resolve will, after upgrade, retain those entries
  with their raw numeric ids.
- This is the documented post-fix behaviour, not a regression. It matches
  upstream rsync 3.4.2 exactly.
- If the new entries are unwanted, strip them on the source with `setfacl
  -x` before transfer, or filter the path with `--filter`/`--exclude` and
  re-create the ACL out of band on the receiver.

If you depended on the pre-fix silent-drop behaviour, the closest equivalent
is a post-transfer cleanup that strips entries whose ids fail to resolve.
There is no flag that re-enables the silent-drop path; preserving wire ids is
the upstream-compatible default.

## References

- Fix commit: `07f81641f` "fix(metadata): remap unmappable POSIX ACL IDs to
  receiver instead of silent drop" (PR #4742).
- Source of truth: upstream rsync 3.4.2 `acls.c::recv_ida_entries` (the
  `am_root` gate removal that closes upstream #618) and
  `uidlist.c::recv_add_id` / `match_uid` / `match_gid`. Local copy at
  `target/interop/upstream-src/rsync-3.4.1/acls.c`; the 3.4.2 diff is
  reproduced in `docs/audits/upstream-3.4.2-acl-non-root-parity.md`.
- Implementation: `crates/metadata/src/acl_exacl/reconstruct.rs`
  (`rsync_acl_to_entries`, `resolve_ida_id`) and
  `crates/metadata/src/acl_exacl/error.rs` (`is_unsupported_error`).
- Wire receive path: `crates/protocol/src/acl/wire/recv.rs`
  (`recv_ida_entries`).
- Debug emission: `--debug=own2` triggers
  `protocol::acl::trace_acl_uid_remap` / `trace_acl_gid_remap`, matching
  upstream `DEBUG_GTE(OWN, 2)` at `uidlist.c:287-291`.
